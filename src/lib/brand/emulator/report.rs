use std::f64::consts::PI;
use std::time::Duration;

use tokio::time::{Instant, interval};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::world::{EmulatorWorld, TurnProgress, TURN_RADIUS};
use super::{EMULATOR_SPOKE_LEN, EMULATOR_SPOKES, get_initial_position};
use crate::Cli;
use crate::radar::settings::{ControlId, ControlUpdate};
use crate::radar::spoke::GenericSpoke;
use crate::radar::{
    CommonRadar, GeoPosition, NAUTICAL_MILE, Power, RadarError, RadarInfo, SharedRadars,
};

// Rotation speed: ~24 RPM = 2.5 seconds per rotation
const ROTATION_TIME_MS: u64 = 2500;
const SPOKES_PER_BATCH: usize = 32; // Like Navico

// Conversion constants
const KNOTS_TO_MS: f64 = 1852.0 / 3600.0;
const DEG_TO_RAD: f64 = PI / 180.0;

pub(crate) struct EmulatorReportReceiver {
    common: CommonRadar,
    command_sender: Option<Command>,

    // Emulator state
    world: EmulatorWorld,
    boat_position: GeoPosition,
    boat_heading: f64,  // degrees
    boat_speed: f64,    // knots
    current_range: u32, // meters
    transmitting: bool,

    // Turn state for course reversal loop
    boat_turn: Option<TurnProgress>,
    initial_lon: f64,

    // Spoke generation state
    current_spoke: u16,
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
        let blob_tx = radars.get_blob_tx();
        let common = CommonRadar::new(args, key, info, radars, control_update_rx, false, blob_tx);

        // Create the world simulation
        let world = EmulatorWorld::new(initial_pos);

        EmulatorReportReceiver {
            common,
            command_sender,
            world,
            boat_position: initial_pos,
            boat_heading: heading,
            boat_speed: speed,
            current_range: (NAUTICAL_MILE / 2) as u32, // Default 1/2 nm
            transmitting: false,
            boat_turn: None,
            initial_lon: initial_pos.lon(),
            current_spoke: 0,
            last_update: Instant::now(),
            rotation_count: 0,
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        // Set initial status to Transmit in emulator mode
        self.common
            .set_value(&ControlId::Power, Power::Transmit as i32 as f64);
        self.transmitting = true;

        // Set initial range value in controls
        self.common
            .set_value(&ControlId::Range, self.current_range as f64);

        // Calculate interval for spoke batches
        // 2048 spokes / 32 per batch = 64 batches per rotation
        // 2500ms / 64 = ~39ms per batch
        let batch_interval_ms =
            ROTATION_TIME_MS / (EMULATOR_SPOKES as u64 / SPOKES_PER_BATCH as u64);
        let mut spoke_interval = interval(Duration::from_millis(batch_interval_ms));

        // Update navdata with initial position
        self.update_navdata();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Ok(());
                }

                _ = spoke_interval.tick() => {
                    if self.transmitting {
                        self.generate_spoke_batch();
                    }
                    self.update_boat_position();
                    self.update_navdata();
                }

                r = self.common.control_update_rx.recv() => {
                    match r {
                        Ok(cu) => {
                            self.handle_control_update(cu).await;
                        }
                        Err(_) => {}
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
                    if let Some(power_val) = value.as_f64() {
                        let power = power_val as i32;
                        match power {
                            2 => {
                                // Transmit
                                log::info!("{}: Starting transmission", self.common.key);
                                self.transmitting = true;
                                self.common
                                    .set_value(&ControlId::Power, Power::Transmit as i32 as f64);
                            }
                            1 => {
                                // Standby
                                log::info!("{}: Stopping transmission (standby)", self.common.key);
                                self.transmitting = false;
                                self.common
                                    .set_value(&ControlId::Power, Power::Standby as i32 as f64);
                            }
                            0 => {
                                // Off
                                log::info!("{}: Power off", self.common.key);
                                self.transmitting = false;
                                self.common
                                    .set_value(&ControlId::Power, Power::Off as i32 as f64);
                            }
                            _ => {}
                        }
                    }
                }
            }
            ControlId::Range => {
                if let Some(ref value) = cv.value {
                    if let Some(range_val) = value.as_f64() {
                        let range = range_val as u32;
                        log::debug!("{}: Range changed to {} m", self.common.key, range);
                        self.current_range = range;
                        self.common.set_value(&ControlId::Range, range as f64);
                    }
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

        if let Some(mut turn) = self.boat_turn.take() {
            let speed_ms = self.boat_speed * KNOTS_TO_MS;
            let angular_velocity = speed_ms / turn.radius;
            turn.turned += angular_velocity * elapsed_secs;

            if turn.turned >= PI {
                // Turn complete: snap to final position
                let final_bearing = if turn.clockwise {
                    turn.start_bearing + PI
                } else {
                    turn.start_bearing - PI
                };
                self.boat_position =
                    turn.center.position_from_bearing(final_bearing, turn.radius);
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
                self.boat_position =
                    turn.center.position_from_bearing(current_bearing, turn.radius);
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
            let distance = self.boat_speed * KNOTS_TO_MS * elapsed_secs;
            let heading_rad = self.boat_heading * DEG_TO_RAD;
            self.boat_position = self
                .boat_position
                .position_from_bearing(heading_rad, distance);

            self.check_turn_trigger();
        }

        // Always update world (moving targets) even when boat is stationary
        self.world.update(elapsed_secs);
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
        // Update the global navigation data
        crate::navdata::set_position(
            Some(self.boat_position.lat()),
            Some(self.boat_position.lon()),
        );
        crate::navdata::set_heading_true(Some(self.boat_heading * DEG_TO_RAD), "emulator");
        crate::navdata::set_sog(Some(self.boat_speed * KNOTS_TO_MS));
        crate::navdata::set_cog(Some(self.boat_heading * DEG_TO_RAD));
    }

    fn generate_spoke_batch(&mut self) {
        self.common.new_spoke_message();

        // Update the world's local coordinate cache once per batch
        self.world.update_cache(&self.boat_position);

        for _ in 0..SPOKES_PER_BATCH {
            let spoke_data = self.generate_spoke(self.current_spoke);

            // Convert heading to raw spoke units (0-4096 like Navico hardware)
            // to_protobuf_spoke divides by 2 to convert to 0-2048 display space
            // Note: Do NOT include the 0x4000 flag here - that flag is used in the raw
            // Navico protocol but is stripped by extract_heading_value() before add_spoke.
            const NAVICO_SPOKES_RAW: f64 = 4096.0;
            let heading_spoke = ((self.boat_heading / 360.0) * NAVICO_SPOKES_RAW) as u16;

            self.common.add_spoke(
                self.current_range,
                self.current_spoke,
                Some(heading_spoke),
                spoke_data,
            );

            let new_spoke = (self.current_spoke + 1) % EMULATOR_SPOKES as u16;
            if new_spoke < self.current_spoke {
                // Rotation wrapped - broadcast heading to ensure GUI receives it
                self.rotation_count += 1;
                crate::navdata::broadcast_heading("emulator");
            }
            self.current_spoke = new_spoke;
        }

        self.common.send_spoke_message();
    }

    fn generate_spoke(&self, spoke_angle: u16) -> GenericSpoke {
        // Convert spoke angle to world bearing
        // Spoke 0 = boat heading, increases clockwise
        let spoke_angle_deg = (spoke_angle as f64 / EMULATOR_SPOKES as f64) * 360.0;
        let world_bearing_rad = (self.boat_heading + spoke_angle_deg) * DEG_TO_RAD;

        // Precompute sin/cos for this spoke (used for all pixels)
        let sin_b = world_bearing_rad.sin();
        let cos_b = world_bearing_rad.cos();

        // Spokes are 1.5x the range setting
        let spoke_range = self.current_range as f64 * 1.5;
        let meters_per_pixel = spoke_range / EMULATOR_SPOKE_LEN as f64;

        // Generate spoke data directly as bytes (one byte per pixel, 4-bit intensity)
        let mut data: Vec<u8> = Vec::with_capacity(EMULATOR_SPOKE_LEN);

        for pixel in 0..EMULATOR_SPOKE_LEN {
            let distance = pixel as f64 * meters_per_pixel;
            let intensity = self.world.get_intensity_fast(sin_b, cos_b, distance);
            data.push(intensity & 0x0f);
        }

        data
    }
}
