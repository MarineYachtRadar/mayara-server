use std::f64::consts::PI;
use std::time::Duration;

use num_traits::FromPrimitive;
use tokio::time::{Instant, interval};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::world::{Echo, EmulatorWorld, TURN_RADIUS, TurnProgress};
use super::{
    EMULATOR_RANGES, EMULATOR_SPOKE_LEN, EMULATOR_SPOKES, get_initial_position, spoke_step,
};
use crate::Cli;
use crate::radar::settings::{ControlId, ControlUpdate};
use crate::radar::spoke::GenericSpoke;
use crate::radar::{
    CommonRadar, DopplerMode, FRAC_NM_2, GeoPosition, KN_TO_MS, Power, RadarError, RadarInfo,
    SharedRadars,
};

// Rotation speed: ~24 RPM = 2.5 seconds per rotation
const ROTATION_TIME_MS: u64 = 2500;
const SPOKES_PER_BATCH: usize = 32; // Like Navico

/// Doppler speed threshold until a client sets one, in m/s (about 2 kn).
const DEFAULT_DOPPLER_THRESHOLD: f64 = 1.0;
/// Echo strength occupies the low nibble of a pixel; the legend's
/// Doppler colours sit above it.
const INTENSITY_MASK: u8 = 0x0f;
/// The speed threshold control's wire unit is cm/s.
const CM_PER_M: f64 = 100.0;

// Conversion constants
const DEG_TO_RAD: f64 = PI / 180.0;

/// Half the antenna's horizontal beam width, in radians. An echo spreads
/// over that much of the picture to either side, as on a real radar, so a
/// small boat far out shows as an arc rather than a speck or not at all.
const HALF_BEAM_WIDTH: f64 = 1.0 * DEG_TO_RAD;
/// An echo is never shorter than this many pixels along a spoke, as a
/// radar pulse has a length of its own.
const MIN_ECHO_PIXELS: f64 = 2.0;

pub(crate) struct EmulatorReportReceiver {
    common: CommonRadar,
    command_sender: Option<Command>,

    // Emulator state
    world: EmulatorWorld,
    /// Where the test scenario in `world` was built around
    world_origin: GeoPosition,
    boat_position: GeoPosition,
    boat_heading: f64,  // degrees
    boat_speed: f64,    // knots
    current_range: u32, // meters
    transmitting: bool,

    /// Another navigation source places the boat, so the emulator does not
    /// move it or publish its navigation.
    following: bool,

    // Turn state for course reversal loop
    boat_turn: Option<TurnProgress>,
    initial_lon: f64,

    // Spoke generation state
    current_spoke: u16,
    spoke_step: u16,
    doppler_mode: DopplerMode,
    doppler_threshold: f64, // m/s
    last_update: Instant,
    rotation_count: u32,
}

impl EmulatorReportReceiver {
    pub(crate) fn new(args: &Cli, info: RadarInfo, radars: SharedRadars) -> Self {
        let key = info.key();

        log::debug!("{}: Creating EmulatorReportReceiver", key);

        let (initial_pos, heading, speed) = get_initial_position(args);

        // Create command sender
        let command_sender = Some(Command::new(info.clone()));

        // Guard zone is set in create_emulator_radar() before this point,
        // either from persistence or as a default for ARPA testing

        let control_update_rx = info.control_update_subscribe();
        let arpa_tx = radars.get_arpa_tx();
        let common = CommonRadar::new(args, key, info, radars, control_update_rx, false, arpa_tx);

        // Create the world simulation
        let world = EmulatorWorld::new(initial_pos);

        EmulatorReportReceiver {
            common,
            command_sender,
            world,
            world_origin: initial_pos,
            boat_position: initial_pos,
            boat_heading: heading,
            boat_speed: speed,
            current_range: FRAC_NM_2 as u32, // Default 1/2 nm
            transmitting: false,
            following: false,
            boat_turn: None,
            initial_lon: initial_pos.lon(),
            current_spoke: 0,
            spoke_step: spoke_step(args),
            doppler_mode: DopplerMode::None,
            doppler_threshold: DEFAULT_DOPPLER_THRESHOLD,
            last_update: Instant::now(),
            rotation_count: 0,
        }
    }

    pub(super) async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
        // Set initial status to Transmit in emulator mode
        self.common
            .set_value(&ControlId::Power, Power::Transmit as i32 as f64);
        self.transmitting = true;

        // Set initial range value in controls
        self.common
            .set_value(&ControlId::Range, self.current_range as f64);
        self.common
            .set_value(&ControlId::Doppler, self.doppler_mode as i32 as f64);
        self.common.set_value(
            &ControlId::DopplerSpeedThreshold,
            self.doppler_threshold * CM_PER_M,
        );

        // Calculate interval for spoke batches
        // 2048 spokes / 32 per batch = 64 batches per rotation
        // 2500ms / 64 = ~39ms per batch; sparse spokes take fewer batches
        let spokes_sent = EMULATOR_SPOKES as u64 / self.spoke_step as u64;
        let batch_interval_ms = ROTATION_TIME_MS / (spokes_sent / SPOKES_PER_BATCH as u64);
        let mut spoke_interval = interval(Duration::from_millis(batch_interval_ms));

        // A source that already reports the boat places it before the
        // emulator publishes a position of its own
        self.follow_own_ship();
        self.update_navdata();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Ok(());
                }

                _ = spoke_interval.tick() => {
                    // The emulator is its own radar, so it is never silent.
                    self.common.info.mark_input();
                    if self.transmitting {
                        self.generate_spoke_batch();
                    }
                    self.update_boat_position();
                    self.update_navdata();
                }

                r = self.common.control_update_rx.recv() => {
                    if let Ok(cu) = r {
                        self.handle_control_update(cu).await;
                    }
                }
            }
        }
    }

    async fn handle_control_update(&mut self, control_update: ControlUpdate) {
        let cv = &control_update.control_value;

        match cv.id {
            ControlId::Power => {
                if let Some(ref value) = cv.value {
                    match Power::from_value(value) {
                        Ok(Power::Transmit) => {
                            log::info!("{}: Starting transmission", self.common.key);
                            self.transmitting = true;
                            self.common
                                .set_value(&ControlId::Power, Power::Transmit as i32 as f64);
                        }
                        Ok(Power::Standby) => {
                            log::info!("{}: Stopping transmission (standby)", self.common.key);
                            self.transmitting = false;
                            self.common
                                .set_value(&ControlId::Power, Power::Standby as i32 as f64);
                        }
                        Ok(Power::Off) => {
                            log::info!("{}: Power off", self.common.key);
                            self.transmitting = false;
                            self.common
                                .set_value(&ControlId::Power, Power::Off as i32 as f64);
                        }
                        Ok(Power::Preparing | Power::Fault) | Err(_) => {}
                    }
                }
            }
            ControlId::Range => {
                if let Some(ref value) = cv.value
                    && let Some(range_val) = value.as_f64()
                {
                    let range = clamp_range(range_val);
                    log::debug!("{}: Range changed to {} m", self.common.key, range);
                    self.current_range = range;
                    self.common.set_value(&ControlId::Range, range as f64);
                }
            }
            ControlId::Doppler => {
                if let Some(mode) = cv.value.as_ref().and_then(|v| v.as_f64()) {
                    self.doppler_mode =
                        DopplerMode::from_i32(mode as i32).unwrap_or(DopplerMode::None);
                    self.common
                        .set_value(&ControlId::Doppler, self.doppler_mode as i32 as f64);
                }
            }
            ControlId::DopplerSpeedThreshold => {
                if let Some(threshold) = cv.value.as_ref().and_then(|v| v.as_f64()) {
                    self.doppler_threshold = threshold;
                    self.common
                        .set_value(&ControlId::DopplerSpeedThreshold, threshold * CM_PER_M);
                }
            }
            _ => {
                // Forward other control updates to command sender
                let _ = self
                    .common
                    .process_control_update(control_update, &mut self.command_sender)
                    .await;
            }
        }
    }

    fn update_boat_position(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);
        self.last_update = now;
        let elapsed_secs = elapsed.as_secs_f64();

        if !self.follow_own_ship() {
            self.move_boat(elapsed_secs);
        }

        // Always update world (moving targets) even when boat is stationary
        self.world.update(elapsed_secs);
    }

    /// Move the emulator's own boat along its course, turning it round at
    /// the ends of its run.
    fn move_boat(&mut self, elapsed_secs: f64) {
        if let Some(mut turn) = self.boat_turn.take() {
            let speed_ms = self.boat_speed * KN_TO_MS;
            let angular_velocity = speed_ms / turn.radius;
            turn.turned += angular_velocity * elapsed_secs;

            if turn.turned >= PI {
                // Turn complete: snap to final position
                let final_bearing = if turn.clockwise {
                    turn.start_bearing + PI
                } else {
                    turn.start_bearing - PI
                };
                self.boat_position = turn
                    .center
                    .position_from_bearing(final_bearing, turn.radius);
                self.boat_heading = (turn.start_heading + 180.0) % 360.0;
                log::info!(
                    "Emulator: turn complete, now heading {:.0}°",
                    self.boat_heading
                );
            } else {
                let current_bearing = if turn.clockwise {
                    turn.start_bearing + turn.turned
                } else {
                    turn.start_bearing - turn.turned
                };
                self.boat_position = turn
                    .center
                    .position_from_bearing(current_bearing, turn.radius);
                let heading_delta_deg = turn.turned.to_degrees();
                self.boat_heading = if turn.clockwise {
                    (turn.start_heading + heading_delta_deg) % 360.0
                } else {
                    (turn.start_heading - heading_delta_deg + 360.0) % 360.0
                };
                self.boat_turn = Some(turn);
            }
        } else if self.boat_speed != 0.0 {
            // Straight-line movement
            let distance = self.boat_speed * KN_TO_MS * elapsed_secs;
            let heading_rad = self.boat_heading * DEG_TO_RAD;
            self.boat_position = self
                .boat_position
                .position_from_bearing(heading_rad, distance);

            self.check_turn_trigger();
        }
    }

    /// Take the boat's position and heading from another navigation source
    /// while one reports them, such as the Signal K server mayara reads its
    /// navigation from. Returns whether it does.
    fn follow_own_ship(&mut self) -> bool {
        let own_ship = crate::navdata::external_own_ship();
        if own_ship.is_some() != self.following {
            self.following = own_ship.is_some();
            self.world.show_test_scenario(!self.following);
            if self.following {
                log::info!("Emulator: following own ship from the navigation source");
                crate::navdata::withdraw_emulator_nav();
            } else {
                log::info!("Emulator: navigation source went quiet, moving the boat itself");
            }
        }
        let Some(own_ship) = own_ship else {
            return false;
        };
        (self.boat_position, self.boat_heading) = place_boat(own_ship, self.boat_heading);
        self.boat_turn = None;
        if out_of_sight(&self.world_origin, &self.boat_position, self.current_range) {
            log::info!("Emulator: moving the test scenario to where the boat now is");
            self.world.rebuild_around(self.boat_position);
            self.world_origin = self.boat_position;
            self.initial_lon = self.boat_position.lon();
        }
        true
    }

    /// Check whether the boat should initiate a half-circle turn to reverse course.
    /// Going west: turn when only 2 targets remain visible within radar range.
    /// Going east: turn when back at the original starting longitude.
    fn check_turn_trigger(&mut self) {
        let heading_west = self.boat_heading > 180.0 && self.boat_heading < 360.0;
        let heading_east = self.boat_heading > 0.0 && self.boat_heading < 180.0;

        if heading_west {
            let visible = self
                .world
                .count_visible_targets(&self.boat_position, self.current_range as f64);
            if visible <= 2 {
                log::info!(
                    "Emulator: only {} targets visible at range {}m, initiating turn",
                    visible,
                    self.current_range
                );
                self.start_boat_turn();
            }
        } else if heading_east && self.boat_position.lon() >= self.initial_lon {
            log::info!("Emulator: back at starting longitude, initiating turn");
            self.start_boat_turn();
        }
    }

    /// Start a starboard (clockwise) half-circle turn for the boat and
    /// port (counterclockwise) turns for all linear targets.
    fn start_boat_turn(&mut self) {
        let heading_rad = self.boat_heading * DEG_TO_RAD;
        // Starboard (right) = heading + π/2
        let center_bearing = heading_rad + PI / 2.0;
        let center = self
            .boat_position
            .position_from_bearing(center_bearing, TURN_RADIUS);
        let start_bearing = center_bearing + PI;

        log::info!(
            "Emulator: starboard turn at ({:.6}, {:.6}), heading {:.0}°",
            self.boat_position.lat(),
            self.boat_position.lon(),
            self.boat_heading
        );

        self.boat_turn = Some(TurnProgress {
            center,
            radius: TURN_RADIUS,
            start_bearing,
            start_heading: self.boat_heading,
            turned: 0.0,
            clockwise: true,
        });

        // All linear targets turn port (counterclockwise)
        self.world.reverse_all_targets(TURN_RADIUS);
    }

    fn update_navdata(&self) {
        if self.following {
            return;
        }
        crate::navdata::set_position(
            Some(self.boat_position.lat()),
            Some(self.boat_position.lon()),
            crate::navdata::EMULATOR_SOURCE,
        );
        crate::navdata::set_heading_true(
            Some(self.boat_heading * DEG_TO_RAD),
            crate::navdata::EMULATOR_SOURCE,
        );
        crate::navdata::set_emulator_course(
            self.boat_heading * DEG_TO_RAD,
            self.boat_speed * KN_TO_MS,
        );
    }

    fn generate_spoke_batch(&mut self) {
        self.common.new_spoke_message();

        // Update the world's local coordinate cache once per batch
        self.world.update_cache(
            &self.boat_position,
            meters_per_pixel(self.current_range) * MIN_ECHO_PIXELS,
        );
        let legend = self.common.info.get_legend();
        let doppler = DopplerColors::new(
            legend.doppler_approaching,
            legend.doppler_receding,
            self.doppler_mode,
            self.doppler_threshold,
        );

        for _ in 0..SPOKES_PER_BATCH {
            let spoke_data = self.generate_spoke(self.current_spoke, &doppler);

            // Heading in `EMULATOR_SPOKES`-units, matching the convention
            // every brand's normalized `add_spoke` heading uses now.
            let heading_spoke = ((self.boat_heading / 360.0) * EMULATOR_SPOKES as f64) as u16;

            self.common.add_spoke(
                spoke_range(self.current_range),
                self.current_spoke,
                Some(heading_spoke),
                spoke_data,
            );

            let new_spoke = next_spoke(self.current_spoke, self.spoke_step);
            if new_spoke < self.current_spoke {
                // Rotation wrapped - broadcast heading to ensure GUI receives it
                self.rotation_count += 1;
                self.refresh_ais();
                if !self.following {
                    crate::navdata::broadcast_heading(crate::navdata::EMULATOR_SOURCE);
                }
            }
            self.current_spoke = new_spoke;
        }

        self.common.send_spoke_message();
    }

    fn generate_spoke(&self, spoke_angle: u16, doppler: &DopplerColors) -> GenericSpoke {
        // Convert spoke angle to world bearing
        // Spoke 0 = boat heading, increases clockwise
        let spoke_angle_deg = (spoke_angle as f64 / EMULATOR_SPOKES as f64) * 360.0;
        let world_bearing_rad = (self.boat_heading + spoke_angle_deg) * DEG_TO_RAD;

        // Precompute sin/cos for this spoke (used for all pixels)
        let sin_b = world_bearing_rad.sin();
        let cos_b = world_bearing_rad.cos();

        let meters_per_pixel = meters_per_pixel(self.current_range);
        let boats = self.world.boats_on_spoke(sin_b, cos_b, HALF_BEAM_WIDTH);

        // Generate spoke data directly as bytes (one byte per pixel, 4-bit intensity)
        let mut data: Vec<u8> = Vec::with_capacity(EMULATOR_SPOKE_LEN);

        for pixel in 0..EMULATOR_SPOKE_LEN {
            let distance = pixel as f64 * meters_per_pixel;
            data.push(doppler.pixel(self.world.echo_fast(sin_b, cos_b, distance, &boats)));
        }

        data
    }

    /// Take the AIS vessels the navigation source reported since the last
    /// rotation, so they echo where they now are.
    fn refresh_ais(&mut self) {
        if let Some(store) = crate::navdata::get_ais_store() {
            self.world.set_ais_vessels(&store.get_all_active());
        }
    }
}

/// Whether the radar at `boat`, set to `range`, can no longer see anything of
/// a test scenario built around `origin`.
fn out_of_sight(origin: &GeoPosition, boat: &GeoPosition, range: u32) -> bool {
    const METERS_PER_DEGREE: f64 = 111_320.0;
    let reach = spoke_range(range) as f64;
    let north = (boat.lat() - origin.lat()) * METERS_PER_DEGREE;
    let east = (boat.lon() - origin.lon()) * METERS_PER_DEGREE * origin.lat().to_radians().cos();
    north.hypot(east) > reach
}

/// Where a followed source puts the boat: its position, and its heading in
/// degrees, or `heading` when the source gives neither heading nor course.
fn place_boat(
    (position, source_heading): (GeoPosition, Option<f64>),
    heading: f64,
) -> (GeoPosition, f64) {
    (position, source_heading.map_or(heading, f64::to_degrees))
}

/// Spokes paint past the range setting by this ratio, like a HALO.
const SPOKE_COVERAGE_NUMERATOR: u64 = 3;
const SPOKE_COVERAGE_DENOMINATOR: u64 = 2;

/// The distance a spoke covers, in meters. Like a HALO, the emulator paints
/// past the range setting, and it reports the distance its spokes actually
/// reach so that every echo is drawn where it is.
fn spoke_range(range: u32) -> u32 {
    u32::try_from(u64::from(range) * SPOKE_COVERAGE_NUMERATOR / SPOKE_COVERAGE_DENOMINATOR)
        .unwrap_or(u32::MAX)
}

/// The distance one pixel of a spoke covers at this range setting, in meters.
fn meters_per_pixel(range: u32) -> f64 {
    spoke_range(range) as f64 / EMULATOR_SPOKE_LEN as f64
}

/// A requested range in meters, held to the ranges the emulator offers. A
/// request with explicit units reaches the emulator unsnapped.
fn clamp_range(meters: f64) -> u32 {
    let shortest = *EMULATOR_RANGES.first().unwrap_or(&0) as f64;
    let longest = *EMULATOR_RANGES.last().unwrap_or(&0) as f64;
    meters.clamp(shortest, longest).round() as u32
}

fn next_spoke(spoke: u16, step: u16) -> u16 {
    (spoke + step) % EMULATOR_SPOKES as u16
}

/// The legend colours a spoke uses for echoes moving faster than the
/// Doppler speed threshold, as the Doppler mode selects them.
struct DopplerColors {
    approaching: Option<u8>,
    receding: Option<u8>,
    threshold: f64,
}

impl DopplerColors {
    /// `approaching` and `receding` are the legend's `(first_index, count)`.
    fn new(
        approaching: Option<(u8, u8)>,
        receding: Option<(u8, u8)>,
        mode: DopplerMode,
        threshold: f64,
    ) -> Self {
        let first = |range: Option<(u8, u8)>| range.map(|(index, _)| index);
        DopplerColors {
            approaching: match mode {
                DopplerMode::None => None,
                DopplerMode::Both | DopplerMode::Approaching => first(approaching),
            },
            receding: match mode {
                DopplerMode::Both => first(receding),
                DopplerMode::None | DopplerMode::Approaching => None,
            },
            threshold,
        }
    }

    fn pixel(&self, echo: Echo) -> u8 {
        let intensity = echo.intensity & INTENSITY_MASK;
        if intensity == 0 {
            return 0;
        }
        match (self.approaching, self.receding) {
            (Some(index), _) if echo.closing_speed > self.threshold => index,
            (_, Some(index)) if echo.closing_speed < -self.threshold => index,
            _ => intensity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spokes reach half as far again as the range, rounded down to whole
    /// meters, and the pixels span exactly that reach.
    #[test]
    fn spoke_range_is_half_as_far_again_as_the_range() {
        assert_eq!(spoke_range(0), 0);
        assert_eq!(spoke_range(75), 112);
        let longest = *EMULATOR_RANGES.last().unwrap() as u32;
        assert_eq!(spoke_range(longest), longest * 3 / 2);
        assert_eq!(spoke_range(u32::MAX), u32::MAX);

        for range in [75, longest] {
            let reach = meters_per_pixel(range) * EMULATOR_SPOKE_LEN as f64;
            assert!((reach - spoke_range(range) as f64).abs() < 1e-6, "{range}");
        }
    }

    #[test]
    fn a_range_request_is_held_to_the_emulators_ranges() {
        let shortest = *EMULATOR_RANGES.first().unwrap() as u32;
        let longest = *EMULATOR_RANGES.last().unwrap() as u32;
        assert_eq!(clamp_range(0.0), shortest);
        assert_eq!(clamp_range(1e12), longest);
        assert_eq!(clamp_range(f64::INFINITY), longest);
        assert_eq!(clamp_range(1500.4), 1500);
    }
    use std::collections::BTreeSet;

    fn one_rotation(step: u16) -> BTreeSet<u16> {
        std::iter::successors(Some(0), |&spoke| {
            Some(next_spoke(spoke, step)).filter(|&s| s != 0)
        })
        .collect()
    }

    /// A sparse emulator reports 2048 spokes but sends a quarter of them,
    /// spread evenly over the rotation, as a Furuno sends its spokes.
    #[test]
    fn a_sparse_emulator_sends_one_spoke_in_four() {
        let sent = one_rotation(4);

        assert_eq!(sent.len(), EMULATOR_SPOKES / 4);
        assert!(sent.iter().all(|spoke| spoke % 4 == 0));
        assert_eq!(one_rotation(1).len(), EMULATOR_SPOKES);
    }

    /// The boat goes where the followed source puts it and points where
    /// that source heads (or, without a heading, where its course goes).
    #[test]
    fn a_followed_source_places_and_points_the_boat() {
        let fix = GeoPosition::new(53.19, 5.33);

        let (position, heading) = place_boat((fix, Some(PI / 2.0)), 270.0);
        assert_eq!(position, fix);
        assert!((heading - 90.0).abs() < 1e-9);

        let (_, heading) = place_boat((fix, None), 270.0);
        assert_eq!(
            heading, 270.0,
            "keeps its heading without one from the source"
        );
    }

    /// A fix within what the radar shows at its range keeps the test scenario
    /// where it is; one beyond it moves the scenario to the boat, even when
    /// a longer range would still reach it.
    #[test]
    fn a_fix_beyond_the_displayed_range_is_out_of_sight() {
        let origin = GeoPosition::new(53.0, 5.6);
        let range = FRAC_NM_2 as u32;
        let reach = spoke_range(range) as f64;

        let near = origin.position_from_bearing(PI / 4.0, reach * 0.9);
        assert!(!out_of_sight(&origin, &near, range));
        let far = origin.position_from_bearing(PI / 4.0, reach * 1.1);
        assert!(out_of_sight(&origin, &far, range));
        let longest = *super::super::EMULATOR_RANGES.last().unwrap() as u32;
        assert!(!out_of_sight(&origin, &far, longest));
    }

    fn echo(closing_speed: f64) -> Echo {
        Echo {
            intensity: 14,
            closing_speed,
        }
    }

    fn colors(mode: DopplerMode) -> DopplerColors {
        DopplerColors::new(Some((40, 1)), Some((41, 1)), mode, 1.0)
    }

    /// An echo moving faster than the threshold takes the Doppler colour
    /// the mode asks for; slower ones, and every echo with Doppler off,
    /// keep their intensity.
    #[test]
    fn an_echo_faster_than_the_threshold_takes_its_doppler_colour() {
        let both = colors(DopplerMode::Both);
        assert_eq!(both.pixel(echo(2.0)), 40, "approaching");
        assert_eq!(both.pixel(echo(-2.0)), 41, "receding");
        assert_eq!(both.pixel(echo(0.5)), 14, "below the threshold");

        let approaching = colors(DopplerMode::Approaching);
        assert_eq!(approaching.pixel(echo(2.0)), 40);
        assert_eq!(approaching.pixel(echo(-2.0)), 14, "receding not shown");

        let off = colors(DopplerMode::None);
        assert_eq!(off.pixel(echo(2.0)), 14);
        assert_eq!(
            off.pixel(Echo {
                intensity: 0,
                closing_speed: 5.0
            }),
            0,
            "no echo"
        );
    }
}
