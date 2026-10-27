use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

use crate::ais::AisVesselApi;
use crate::radar::{GeoPosition, KN_TO_MS};

// Constants for east-moving targets simulation
const TARGET_SPEED_KNOTS: f64 = 5.0;
const TARGET_DISTANCE_SOUTH: f64 = 500.0; // meters
const TARGET_SPACING: f64 = 500.0; // meters between targets
const NUM_TARGETS: usize = 50;

// Crossing targets: boats coming from north, heading south at 10 knots
const CROSSING_SPEED_KNOTS: f64 = 10.0;
const CROSSING_SPACING: f64 = 500.0; // meters between crossing boats
const NUM_CROSSING_TARGETS: usize = 30;

// Land area dimensions - 700m south, 600m west of radar, oriented east-west
const LAND_DISTANCE_SOUTH: f64 = 700.0; // meters south
const LAND_DISTANCE_WEST: f64 = 600.0; // meters west
const LAND_WIDTH: f64 = 200.0; // meters (north-south extent)
const LAND_LENGTH: f64 = 1000.0; // meters (east-west extent)
const LAND_ORIENTATION: f64 = 90.0; // degrees from North (pointing East-West)

// Circling boat: 500m diameter circle, closest point 100m north, 15 knots
const CIRCLING_RADIUS: f64 = 250.0; // meters (500m diameter / 2)
const CIRCLING_CENTER_NORTH: f64 = 350.0; // meters (100m closest + 250m radius)
const CIRCLING_SPEED_KNOTS: f64 = 15.0;

// Fast eastbound targets: moving east at 40 knots, 300m north of boat
const FAST_TARGET_SPEED_KNOTS: f64 = 40.0;
const FAST_TARGET_DISTANCE_NORTH: f64 = 300.0; // meters
const FAST_TARGET_SPACING: f64 = 400.0; // meters between targets
const NUM_FAST_TARGETS: usize = 10;

/// Echo radius of the test scenario's boats, in meters
const TARGET_RADIUS: f64 = 30.0;

/// Echo radius of an AIS vessel whose length is unknown, in meters
const AIS_DEFAULT_RADIUS: f64 = 15.0;
/// AIS vessels echo with half their length, kept within these bounds
const AIS_MIN_RADIUS: f64 = 5.0;
const AIS_MAX_RADIUS: f64 = 200.0;

// Conversion constants
const DEG_TO_RAD: f64 = PI / 180.0;

/// Radius of the half-circle turn when reversing course (meters)
pub(crate) const TURN_RADIUS: f64 = 100.0;

/// Progress state for a half-circle turn maneuver
#[derive(Clone, Debug)]
pub(crate) struct TurnProgress {
    /// Center of the turning circle
    pub center: GeoPosition,
    /// Turn radius in meters
    pub radius: f64,
    /// Bearing from center to object at start of turn (radians)
    pub start_bearing: f64,
    /// Heading at start of turn (degrees)
    pub start_heading: f64,
    /// Radians turned so far (always positive)
    pub turned: f64,
    /// True for starboard (clockwise), false for port (counterclockwise)
    pub clockwise: bool,
}

/// A moving target (boat)
#[derive(Clone, Debug)]
pub(crate) struct Target {
    /// Current position
    pub position: GeoPosition,
    /// Heading in degrees (0 = North, 90 = East)
    pub heading: f64,
    /// Speed in m/s
    pub speed: f64,
    /// Active half-circle turn maneuver, if any
    turn: Option<TurnProgress>,
}

impl Target {
    fn new(position: GeoPosition, heading: f64, speed_knots: f64) -> Self {
        Target {
            position,
            heading,
            speed: speed_knots * KN_TO_MS,
            turn: None,
        }
    }

    fn update(&mut self, elapsed_secs: f64) {
        if let Some(mut turn) = self.turn.take() {
            let angular_velocity = self.speed / turn.radius;
            turn.turned += angular_velocity * elapsed_secs;

            if turn.turned >= PI {
                // Turn complete: snap to final position
                let final_bearing = if turn.clockwise {
                    turn.start_bearing + PI
                } else {
                    turn.start_bearing - PI
                };
                self.position = turn
                    .center
                    .position_from_bearing(final_bearing, turn.radius);
                self.heading = (turn.start_heading + 180.0) % 360.0;
            } else {
                let current_bearing = if turn.clockwise {
                    turn.start_bearing + turn.turned
                } else {
                    turn.start_bearing - turn.turned
                };
                self.position = turn
                    .center
                    .position_from_bearing(current_bearing, turn.radius);
                let heading_delta_deg = turn.turned.to_degrees();
                self.heading = if turn.clockwise {
                    (turn.start_heading + heading_delta_deg) % 360.0
                } else {
                    (turn.start_heading - heading_delta_deg + 360.0) % 360.0
                };
                self.turn = Some(turn);
            }
        } else {
            let distance = self.speed * elapsed_secs;
            let heading_rad = self.heading * DEG_TO_RAD;
            self.position = self.position.position_from_bearing(heading_rad, distance);
        }
    }

    /// Start a port (counterclockwise) half-circle turn
    pub(crate) fn start_port_turn(&mut self, radius: f64) {
        let heading_rad = self.heading * DEG_TO_RAD;
        // Port (left) direction is heading - π/2
        let center_bearing = heading_rad - PI / 2.0;
        let center = self.position.position_from_bearing(center_bearing, radius);
        // Bearing from center back to target
        let start_bearing = center_bearing + PI;

        self.turn = Some(TurnProgress {
            center,
            radius,
            start_bearing,
            start_heading: self.heading,
            turned: 0.0,
            clockwise: false,
        });
    }
}

/// A vessel the navigation source reports over AIS, echoing like a boat of
/// its size
#[derive(Clone, Debug)]
pub(crate) struct AisEcho {
    target: Target,
    /// Radar return radius in meters
    radius: f64,
    mmsi: String,
    /// Latitude, longitude, course (radians) and speed of the report
    /// `target` set out from
    report: [f64; 4],
}

impl AisEcho {
    /// `None` for a vessel without a position. One without course or speed
    /// is taken to be lying still.
    fn new(vessel: &AisVesselApi) -> Option<Self> {
        let position = vessel.position.as_ref()?;
        let course = vessel.cog.or(vessel.heading).unwrap_or(0.0);
        let speed = vessel.sog.unwrap_or(0.0);
        let radius = vessel
            .dimensions
            .as_ref()
            .and_then(|d| d.length)
            .map_or(AIS_DEFAULT_RADIUS, |length| {
                (length / 2.0).clamp(AIS_MIN_RADIUS, AIS_MAX_RADIUS)
            });
        Some(AisEcho {
            target: Target {
                position: GeoPosition::new(position.latitude, position.longitude),
                heading: course.to_degrees(),
                speed,
                turn: None,
            },
            radius,
            mmsi: vessel.mmsi.clone(),
            report: [position.latitude, position.longitude, course, speed],
        })
    }
}

/// A static buoy (small radar target)
#[derive(Clone, Debug)]
pub(crate) struct Buoy {
    /// Position of the buoy
    pub position: GeoPosition,
    /// Radar return radius in meters (typically small, ~5m)
    pub radius: f64,
}

impl Buoy {
    fn new(position: GeoPosition, radius: f64) -> Self {
        Buoy { position, radius }
    }
}

/// A target moving in a circle
#[derive(Clone, Debug)]
pub(crate) struct CirclingTarget {
    /// Center of the circular path
    pub center: GeoPosition,
    /// Radius of the circular path in meters
    pub radius: f64,
    /// Current angle in radians (0 = South of center, increasing clockwise)
    pub angle: f64,
    /// Angular velocity in radians per second
    pub angular_velocity: f64,
    /// Current position (computed from center, radius, angle)
    pub position: GeoPosition,
    /// Current heading in degrees (tangent to circle)
    pub heading: f64,
}

impl CirclingTarget {
    fn new(center: GeoPosition, radius: f64, speed_knots: f64) -> Self {
        let speed_ms = speed_knots * KN_TO_MS;
        // Angular velocity = v / r (radians per second)
        let angular_velocity = speed_ms / radius;

        // Start at the south point of the circle (closest to own ship)
        // Angle 0 = south of center, boat moves clockwise (east first)
        let angle = 0.0;

        // Calculate initial position (south of center)
        let south_bearing = PI; // 180 degrees
        let position = center.position_from_bearing(south_bearing, radius);

        // Initial heading: moving east (90 degrees) when at south point, clockwise
        let heading = 90.0;

        CirclingTarget {
            center,
            radius,
            angle,
            angular_velocity,
            position,
            heading,
        }
    }

    fn update(&mut self, elapsed_secs: f64) {
        // Update angle (clockwise motion)
        self.angle += self.angular_velocity * elapsed_secs;
        self.angle %= TAU;

        // Calculate bearing from center to boat position
        // angle=0 → south (bearing=PI), angle increases clockwise
        // So bearing = PI + angle
        let bearing = PI + self.angle;

        // Update position
        self.position = self.center.position_from_bearing(bearing, self.radius);

        // Heading is tangent to circle, 90 degrees ahead of bearing from center
        // For clockwise motion: heading = bearing + PI/2
        let heading_rad = bearing + PI / 2.0;
        self.heading = (heading_rad / DEG_TO_RAD) % 360.0;
        if self.heading < 0.0 {
            self.heading += 360.0;
        }
    }
}

/// Land area (stationary oblong)
#[derive(Clone, Debug)]
pub(crate) struct LandArea {
    /// Center position
    pub center: GeoPosition,
    /// Half-width (perpendicular to orientation)
    pub half_width: f64,
    /// Half-length (along orientation)
    pub half_length: f64,
    /// Orientation in radians (angle of the long axis from North)
    pub orientation_rad: f64,
}

impl LandArea {
    fn new(center: GeoPosition, width: f64, length: f64, orientation_deg: f64) -> Self {
        LandArea {
            center,
            half_width: width / 2.0,
            half_length: length / 2.0,
            orientation_rad: orientation_deg * DEG_TO_RAD,
        }
    }

    /// Check if a point is inside the land area (original geodesic method, unused)
    #[allow(dead_code)]
    fn contains(&self, point: &GeoPosition) -> bool {
        // Calculate distance and bearing from center to point
        let (distance, bearing) = distance_and_bearing(&self.center, point);

        // Rotate the bearing by the negative of orientation to align with local coordinates
        let local_angle = bearing - self.orientation_rad;

        // Calculate local x, y coordinates
        let local_x = distance * local_angle.sin(); // perpendicular to long axis
        let local_y = distance * local_angle.cos(); // along long axis

        // Check if within the oblong bounds
        local_x.abs() <= self.half_width && local_y.abs() <= self.half_length
    }
}

/// A boat in local coords (x east, y north, in meters) with its velocity
/// over ground in m/s and its echo radius in meters.
struct LocalBoat {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    radius: f64,
}

impl LocalBoat {
    /// How fast the boat closes on own ship over ground, in m/s; negative
    /// when it moves away.
    fn closing_speed(&self) -> f64 {
        let distance = self.x.hypot(self.y);
        if distance == 0.0 {
            return 0.0;
        }
        -(self.vx * self.x + self.vy * self.y) / distance
    }
}

/// A boat a spoke passes through, in that spoke's terms
pub(crate) struct SpokeBoat {
    /// Distance along the spoke to the boat's centre, in meters
    along: f64,
    /// How far the boat's centre lies off the spoke, in meters, less the
    /// beam's spread at that distance
    across: f64,
    radius: f64,
    closing_speed: f64,
}

/// What the radar sees at one point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Echo {
    /// 0 = no return, 15 = strongest
    pub intensity: u8,
    /// Speed over ground towards own ship in m/s, as a Doppler radar
    /// measures it; negative when moving away, 0 for anything fixed.
    pub closing_speed: f64,
}

impl Echo {
    const NONE: Echo = Echo {
        intensity: 0,
        closing_speed: 0.0,
    };

    fn fixed(intensity: u8) -> Echo {
        Echo {
            intensity,
            closing_speed: 0.0,
        }
    }
}

/// Cached local coordinates for efficient per-spoke lookups
pub(crate) struct LocalCache {
    /// Land center in local coords, while the test scenario shows
    land_center: Option<(f64, f64)>,
    /// Every moving boat, in local coords
    boats: Vec<LocalBoat>,
    /// Buoy positions in local coords (with radius squared)
    buoys: Vec<(f64, f64, f64)>,
}

/// The simulated world
pub(crate) struct EmulatorWorld {
    /// Land area (fixed position)
    pub land: LandArea,
    /// Moving targets (east-moving boats)
    pub targets: Vec<Target>,
    /// Crossing targets (north-to-south boats)
    pub crossing_targets: Vec<Target>,
    /// Fast eastbound targets (40 knots, 300m north)
    pub fast_targets: Vec<Target>,
    /// Circling target (boat going in circles north of own ship)
    pub circling_target: CirclingTarget,
    /// Static buoys
    pub buoys: Vec<Buoy>,
    /// Whether the boats, land and buoys above show. They are placed around
    /// the emulator's own boat, so they only fit while it sails that boat.
    test_scenario: bool,
    /// Vessels the navigation source reports over AIS
    ais: Vec<AisEcho>,
    /// Cached local coordinates (updated once per batch)
    cache: Option<LocalCache>,
}

impl EmulatorWorld {
    pub(crate) fn new(initial_boat_pos: GeoPosition) -> Self {
        // Create land area 700m south, directly south of initial position
        let south_bearing = 180.0 * DEG_TO_RAD;
        let west_bearing = 270.0 * DEG_TO_RAD;
        let land_south = initial_boat_pos.position_from_bearing(south_bearing, LAND_DISTANCE_SOUTH);
        let land_center = if LAND_DISTANCE_WEST > 0.0 {
            land_south.position_from_bearing(west_bearing, LAND_DISTANCE_WEST)
        } else {
            land_south
        };
        // Orientation: long axis points East-West (90 degrees from North)
        let land = LandArea::new(land_center, LAND_WIDTH, LAND_LENGTH, LAND_ORIENTATION);

        // Create targets 500m south of initial boat position, moving East
        // They start at various positions West of the boat
        let mut targets = Vec::with_capacity(NUM_TARGETS);
        let south_bearing = 180.0 * DEG_TO_RAD; // South
        let west_bearing = 270.0 * DEG_TO_RAD; // West

        // Base position: 500m south of boat
        let base_pos = initial_boat_pos.position_from_bearing(south_bearing, TARGET_DISTANCE_SOUTH);

        for i in 0..NUM_TARGETS {
            // Offset West by i * TARGET_SPACING
            let offset = i as f64 * TARGET_SPACING;
            let target_pos = base_pos.position_from_bearing(west_bearing, offset);
            // Moving East at 5 knots
            targets.push(Target::new(target_pos, 90.0, TARGET_SPEED_KNOTS));
        }

        // Create crossing targets: boats coming from North, heading South at 10 knots
        // The first one passes 20m ahead (east of) the front east-moving boat,
        // creating a close-pass scenario for CPA/TCPA testing
        let mut crossing_targets = Vec::with_capacity(NUM_CROSSING_TARGETS);

        // Calculate crossing point: where the first crossing boat will pass
        // Place crossing boats 600m north of the east-moving boats' track
        let crossing_start_distance_north = 600.0; // meters north of east-boat track
        let ahead_distance = 20.0; // meters ahead (east of) front boat - creates close pass

        // Position where crossing will happen: 20m east of front east-boat, at same latitude
        let east_bearing_for_crossing = 90.0 * DEG_TO_RAD;
        let crossing_point =
            base_pos.position_from_bearing(east_bearing_for_crossing, ahead_distance);

        // Starting position for first crossing boat: 400m north of crossing point
        let north_bearing = 0.0 * DEG_TO_RAD; // North
        let first_crossing_start =
            crossing_point.position_from_bearing(north_bearing, crossing_start_distance_north);

        for i in 0..NUM_CROSSING_TARGETS {
            // Offset east by i * CROSSING_SPACING so they form a line
            let offset = i as f64 * CROSSING_SPACING;
            let east_bearing = 90.0 * DEG_TO_RAD;
            let crossing_pos = first_crossing_start.position_from_bearing(east_bearing, offset);
            // Moving South at 10 knots
            crossing_targets.push(Target::new(crossing_pos, 180.0, CROSSING_SPEED_KNOTS));
        }

        // Create a static buoy near the east-moving boats' path
        // Place it 10m south of their track, 200m east of the front boat
        let buoy_east_offset = 200.0; // meters east of front boat
        let buoy_south_offset = 10.0; // meters south of track
        let east_bearing = 90.0 * DEG_TO_RAD;
        let buoy_base = base_pos.position_from_bearing(east_bearing, buoy_east_offset);
        let buoy_pos = buoy_base.position_from_bearing(south_bearing, buoy_south_offset);
        let buoys = vec![Buoy::new(buoy_pos, 5.0)]; // 5m radius buoy

        // Create circling target: center is 350m north (100m closest + 250m radius)
        let north_bearing = 0.0; // North
        let circling_center =
            initial_boat_pos.position_from_bearing(north_bearing, CIRCLING_CENTER_NORTH);
        let circling_target =
            CirclingTarget::new(circling_center, CIRCLING_RADIUS, CIRCLING_SPEED_KNOTS);

        // Create fast eastbound targets: 300m north of boat, moving east at 40 knots
        // They start west of the boat and will appear from the northwest
        let mut fast_targets = Vec::with_capacity(NUM_FAST_TARGETS);
        let fast_base_pos =
            initial_boat_pos.position_from_bearing(north_bearing, FAST_TARGET_DISTANCE_NORTH);

        for i in 0..NUM_FAST_TARGETS {
            // Offset West by i * FAST_TARGET_SPACING
            let offset = i as f64 * FAST_TARGET_SPACING;
            let target_pos = fast_base_pos.position_from_bearing(west_bearing, offset);
            // Moving East at 40 knots
            fast_targets.push(Target::new(target_pos, 90.0, FAST_TARGET_SPEED_KNOTS));
        }

        EmulatorWorld {
            land,
            targets,
            crossing_targets,
            fast_targets,
            circling_target,
            buoys,
            test_scenario: true,
            ais: Vec::new(),
            cache: None,
        }
    }

    /// Update all moving objects based on elapsed time
    pub(crate) fn update(&mut self, elapsed_secs: f64) {
        for target in &mut self.targets {
            target.update(elapsed_secs);
        }
        for target in &mut self.crossing_targets {
            target.update(elapsed_secs);
        }
        for target in &mut self.fast_targets {
            target.update(elapsed_secs);
        }
        self.circling_target.update(elapsed_secs);
        for echo in &mut self.ais {
            echo.target.update(elapsed_secs);
        }
        // Invalidate cache when positions change
        self.cache = None;
    }

    pub(crate) fn show_test_scenario(&mut self, show: bool) {
        self.test_scenario = show;
    }

    /// Lay the test scenario out afresh around `origin`. The AIS vessels and
    /// whether the scenario shows stay as they are.
    pub(crate) fn rebuild_around(&mut self, origin: GeoPosition) {
        let rebuilt = EmulatorWorld::new(origin);
        *self = EmulatorWorld {
            test_scenario: self.test_scenario,
            ais: std::mem::take(&mut self.ais),
            ..rebuilt
        };
    }

    /// Replace the AIS vessels with the latest the navigation source
    /// reported. Between reports they move on along their course, so a
    /// vessel whose report has not changed keeps where it has got to.
    pub(crate) fn set_ais_vessels(&mut self, vessels: &[AisVesselApi]) {
        let mut previous: HashMap<String, AisEcho> = std::mem::take(&mut self.ais)
            .into_iter()
            .map(|echo| (echo.mmsi.clone(), echo))
            .collect();
        self.ais = vessels
            .iter()
            .filter_map(AisEcho::new)
            .map(|fresh| match previous.remove(&fresh.mmsi) {
                Some(moved_on) if moved_on.report == fresh.report => AisEcho {
                    radius: fresh.radius,
                    ..moved_on
                },
                _ => fresh,
            })
            .collect();
    }

    /// Initiate port (counterclockwise) half-circle turns for all linear targets
    pub(crate) fn reverse_all_targets(&mut self, radius: f64) {
        for target in &mut self.targets {
            target.start_port_turn(radius);
        }
        for target in &mut self.crossing_targets {
            target.start_port_turn(radius);
        }
        for target in &mut self.fast_targets {
            target.start_port_turn(radius);
        }
        // Circling target keeps circling, buoys are static
    }

    /// Count how many targets are within the given range of the boat
    pub(crate) fn count_visible_targets(&self, boat_pos: &GeoPosition, max_range: f64) -> usize {
        let max_range_sq = max_range * max_range;
        let meters_per_degree_lat: f64 = 111_320.0;
        let lat_rad = boat_pos.lat().to_radians();
        let meters_per_degree_lon = meters_per_degree_lat * lat_rad.cos();

        let in_range = |pos: &GeoPosition| -> bool {
            let dx = (pos.lon() - boat_pos.lon()) * meters_per_degree_lon;
            let dy = (pos.lat() - boat_pos.lat()) * meters_per_degree_lat;
            dx * dx + dy * dy <= max_range_sq
        };

        let mut count = 0;
        for t in &self.targets {
            if in_range(&t.position) {
                count += 1;
            }
        }
        for t in &self.crossing_targets {
            if in_range(&t.position) {
                count += 1;
            }
        }
        for t in &self.fast_targets {
            if in_range(&t.position) {
                count += 1;
            }
        }
        if in_range(&self.circling_target.position) {
            count += 1;
        }
        count
    }

    /// Update the local coordinate cache for the current boat position
    /// Call once per spoke batch before generating spokes. No echo is
    /// smaller than `min_radius`, so a small boat still shows when a pixel
    /// spans more than its size.
    pub(crate) fn update_cache(&mut self, boat_pos: &GeoPosition, min_radius: f64) {
        const METERS_PER_DEGREE_LAT: f64 = 111_320.0;

        let lat_rad = boat_pos.lat().to_radians();
        let meters_per_degree_lon = METERS_PER_DEGREE_LAT * lat_rad.cos();

        let to_local = |pos: &GeoPosition| -> (f64, f64) {
            let delta_lat = pos.lat() - boat_pos.lat();
            let delta_lon = pos.lon() - boat_pos.lon();
            (
                delta_lon * meters_per_degree_lon,
                delta_lat * METERS_PER_DEGREE_LAT,
            )
        };

        let boat = |position: &GeoPosition, heading: f64, speed: f64, radius: f64| {
            let (x, y) = to_local(position);
            let heading = heading * DEG_TO_RAD;
            LocalBoat {
                x,
                y,
                vx: speed * heading.sin(),
                vy: speed * heading.cos(),
                radius: radius.max(min_radius),
            }
        };
        let ais = self.ais.iter().map(|echo| {
            let t = &echo.target;
            boat(&t.position, t.heading, t.speed, echo.radius)
        });

        self.cache = Some(if self.test_scenario {
            let circling = &self.circling_target;
            LocalCache {
                land_center: Some(to_local(&self.land.center)),
                boats: self
                    .targets
                    .iter()
                    .chain(&self.crossing_targets)
                    .chain(&self.fast_targets)
                    .map(|t| boat(&t.position, t.heading, t.speed, TARGET_RADIUS))
                    .chain(std::iter::once(boat(
                        &circling.position,
                        circling.heading,
                        circling.angular_velocity * circling.radius,
                        TARGET_RADIUS,
                    )))
                    .chain(ais)
                    .collect(),
                buoys: self
                    .buoys
                    .iter()
                    .map(|b| {
                        let (x, y) = to_local(&b.position);
                        (x, y, b.radius * b.radius)
                    })
                    .collect(),
            }
        } else {
            LocalCache {
                land_center: None,
                boats: ais.collect(),
                buoys: Vec::new(),
            }
        });
    }

    /// The boats a spoke at this bearing passes through. With hundreds of
    /// AIS vessels about, checking each one for every pixel of every spoke
    /// would cost far more than a rotation lasts.
    ///
    /// `half_beam` is half the beam width, in radians: a boat that far off
    /// the spoke still shows on it in full.
    pub(crate) fn boats_on_spoke(&self, sin_b: f64, cos_b: f64, half_beam: f64) -> Vec<SpokeBoat> {
        let Some(cache) = &self.cache else {
            return Vec::new();
        };
        cache
            .boats
            .iter()
            .filter_map(|boat| {
                let along = boat.x * sin_b + boat.y * cos_b;
                let across = boat.x * cos_b - boat.y * sin_b;
                let across = (across.abs() - along.max(0.0) * half_beam).max(0.0);
                (along > -boat.radius && across < boat.radius).then(|| SpokeBoat {
                    along,
                    across,
                    radius: boat.radius,
                    closing_speed: boat.closing_speed(),
                })
            })
            .collect()
    }

    /// Get the radar return at a given position
    /// Uses cached local coordinates for efficiency
    /// sin_b, cos_b are precomputed sin/cos of bearing; `boats` are the
    /// boats on that spoke, from [`Self::boats_on_spoke`]
    #[inline]
    pub(crate) fn echo_fast(
        &self,
        sin_b: f64,
        cos_b: f64,
        distance: f64,
        boats: &[SpokeBoat],
    ) -> Echo {
        let cache = match &self.cache {
            Some(c) => c,
            None => return Echo::NONE,
        };

        // Calculate point in local Cartesian coords (x=east, y=north)
        let point_x = distance * sin_b;
        let point_y = distance * cos_b;

        // Check land using cached local coords
        if let Some(land_center) = cache.land_center {
            let dx = point_x - land_center.0;
            let dy = point_y - land_center.1;
            let cos_o = self.land.orientation_rad.cos();
            let sin_o = self.land.orientation_rad.sin();
            let local_x = dx * cos_o - dy * sin_o;
            let local_y = dx * sin_o + dy * cos_o;
            if local_x.abs() <= self.land.half_width && local_y.abs() <= self.land.half_length {
                return Echo::fixed(14);
            }
        }

        // Check targets - they appear as point targets with some spread
        for boat in boats {
            let off = (distance - boat.along).hypot(boat.across);
            if off < boat.radius {
                let intensity = 15.0 - (off / boat.radius) * 5.0;
                return Echo {
                    intensity: intensity.max(13.0) as u8,
                    closing_speed: boat.closing_speed,
                };
            }
        }

        for &(bx, by, radius_sq) in &cache.buoys {
            let dx = point_x - bx;
            let dy = point_y - by;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < radius_sq {
                return Echo::fixed(10);
            }
        }

        Echo::NONE
    }

    /// Get the radar return intensity at a given position (legacy method)
    #[allow(dead_code)]
    pub(crate) fn get_intensity(
        &self,
        _boat_pos: &GeoPosition,
        bearing_rad: f64,
        distance: f64,
    ) -> u8 {
        let (sin_b, cos_b) = bearing_rad.sin_cos();
        let boats = self.boats_on_spoke(sin_b, cos_b, 0.0);
        self.echo_fast(sin_b, cos_b, distance, &boats).intensity
    }
}

/// Calculate distance in meters between two positions
#[allow(dead_code)]
fn distance_between(from: &GeoPosition, to: &GeoPosition) -> f64 {
    const EARTH_RADIUS: f64 = 6_371_000.0; // meters

    let lat1 = from.lat().to_radians();
    let lat2 = to.lat().to_radians();
    let delta_lat = (to.lat() - from.lat()).to_radians();
    let delta_lon = (to.lon() - from.lon()).to_radians();

    let a =
        (delta_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    EARTH_RADIUS * c
}

/// Calculate distance and bearing from one position to another
/// Returns (distance in meters, bearing in radians)
fn distance_and_bearing(from: &GeoPosition, to: &GeoPosition) -> (f64, f64) {
    let distance = distance_between(from, to);

    let lat1 = from.lat().to_radians();
    let lat2 = to.lat().to_radians();
    let delta_lon = (to.lon() - from.lon()).to_radians();

    let y = delta_lon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * delta_lon.cos();
    let bearing = y.atan2(x);

    // Normalize bearing to [0, 2*PI)
    let bearing = (bearing + TAU) % TAU;

    (distance, bearing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_land_area_contains() {
        let center = GeoPosition::new(53.2, 5.3);
        let land = LandArea::new(center, 200.0, 1000.0, 45.0);

        // Center should be inside
        assert!(land.contains(&center));

        // Point far away should be outside
        let far = GeoPosition::new(54.0, 6.0);
        assert!(!land.contains(&far));
    }

    #[test]
    fn test_target_movement() {
        let pos = GeoPosition::new(53.0, 5.0);
        let mut target = Target::new(pos, 90.0, 5.0); // Moving East at 5 knots

        // After 1 hour, should move ~9.26 km East
        target.update(3600.0);

        // Longitude should increase (moving East)
        assert!(target.position.lon() > 5.0);
    }

    /// A Doppler radar sees a boat's speed along the line of sight: a boat
    /// heading for own ship closes at its full speed, one moving away
    /// recedes, and one crossing does neither.
    #[test]
    fn a_boat_closes_at_its_speed_along_the_line_of_sight() {
        let speed = 5.0;
        let north_heading_south = LocalBoat {
            x: 0.0,
            y: 500.0,
            vx: 0.0,
            vy: -speed,
            radius: TARGET_RADIUS,
        };
        let north_heading_north = LocalBoat {
            x: 0.0,
            y: 500.0,
            vx: 0.0,
            vy: speed,
            radius: TARGET_RADIUS,
        };
        let north_heading_east = LocalBoat {
            x: 0.0,
            y: 500.0,
            vx: speed,
            vy: 0.0,
            radius: TARGET_RADIUS,
        };

        assert!((north_heading_south.closing_speed() - speed).abs() < 1e-9);
        assert!((north_heading_north.closing_speed() + speed).abs() < 1e-9);
        assert!(north_heading_east.closing_speed().abs() < 1e-9);
    }

    const OWN_SHIP: (f64, f64) = (53.2, 5.3);
    /// Half a 2° beam
    const HALF_BEAM: f64 = DEG_TO_RAD;

    fn vessel(bearing_deg: f64, distance: f64, length: Option<f64>) -> AisVesselApi {
        let own = GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1);
        let at = own.position_from_bearing(bearing_deg * DEG_TO_RAD, distance);
        AisVesselApi {
            mmsi: "244000001".into(),
            name: None,
            position: Some(crate::ais::Position {
                latitude: at.lat(),
                longitude: at.lon(),
            }),
            dimensions: length.map(|length| crate::ais::Dimensions {
                length: Some(length),
                ..Default::default()
            }),
            heading: None,
            cog: Some(0.0),
            sog: Some(5.0),
        }
    }

    /// A world showing only the given AIS vessels around own ship
    fn ais_world(vessels: &[AisVesselApi]) -> EmulatorWorld {
        let own = GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1);
        let mut world = EmulatorWorld::new(own);
        world.show_test_scenario(false);
        world.set_ais_vessels(vessels);
        world.update_cache(&own, 1.0);
        world
    }

    fn echo_at(world: &EmulatorWorld, bearing_deg: f64, distance: f64) -> Echo {
        let (sin_b, cos_b) = (bearing_deg * DEG_TO_RAD).sin_cos();
        let boats = world.boats_on_spoke(sin_b, cos_b, HALF_BEAM);
        world.echo_fast(sin_b, cos_b, distance, &boats)
    }

    /// An AIS vessel echoes where it is, as long as half its length, and
    /// carries its speed towards own ship for Doppler.
    #[test]
    fn an_ais_vessel_echoes_at_its_position_with_its_size() {
        let world = ais_world(&[vessel(180.0, 1000.0, Some(100.0))]);

        let centre = echo_at(&world, 180.0, 1000.0);
        assert!(centre.intensity >= 14, "strongest at the centre");
        assert!(
            (centre.closing_speed - 5.0).abs() < 0.1,
            "heads north at 5 m/s"
        );
        assert_eq!(echo_at(&world, 180.0, 1040.0).intensity, 13);
        assert_eq!(echo_at(&world, 180.0, 1060.0), Echo::NONE);
        assert_eq!(echo_at(&world, 90.0, 1000.0), Echo::NONE);
    }

    /// A vessel without a reported length still shows, and one without a
    /// position cannot be placed.
    #[test]
    fn ais_vessels_without_length_or_position() {
        let mut unplaced = vessel(0.0, 500.0, None);
        unplaced.position = None;
        let world = ais_world(&[vessel(0.0, 1000.0, None), unplaced]);

        assert_eq!(world.ais.len(), 1);
        assert!(echo_at(&world, 0.0, 1000.0).intensity > 0);
        assert_eq!(
            echo_at(&world, 0.0, 1000.0 + AIS_DEFAULT_RADIUS + 5.0),
            Echo::NONE
        );
    }

    /// A small boat far out spreads over the beam width, so it shows on
    /// the spokes beside it too, as on a real radar.
    #[test]
    fn the_beam_spreads_a_small_vessel_over_neighbouring_spokes() {
        let distance = 5_000.0;
        let world = ais_world(&[vessel(0.8, distance, Some(10.0))]);
        let (sin_b, cos_b) = (0.0, 1.0);
        let shows = |half_beam| {
            let boats = world.boats_on_spoke(sin_b, cos_b, half_beam);
            (4950..5050).any(|d| world.echo_fast(sin_b, cos_b, d as f64, &boats).intensity > 0)
        };

        assert!(shows(HALF_BEAM));
        assert!(
            !shows(0.0),
            "too far off the spoke without the beam's spread"
        );
    }

    /// Between AIS reports a vessel moves on along its course.
    #[test]
    fn an_ais_vessel_moves_on_between_reports() {
        let mut world = ais_world(&[vessel(180.0, 1000.0, Some(20.0))]);

        world.update(20.0);
        world.update_cache(&GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1), 1.0);

        assert!(echo_at(&world, 180.0, 900.0).intensity > 0);
        assert_eq!(echo_at(&world, 180.0, 1000.0), Echo::NONE);
    }

    /// The store is read once per rotation, more often than most vessels
    /// report, so a repeated report must not pull the echo back.
    #[test]
    fn a_repeated_ais_report_keeps_the_vessel_moving_on() {
        let report = vessel(180.0, 1000.0, Some(20.0));
        let mut world = ais_world(std::slice::from_ref(&report));

        world.update(20.0);
        world.set_ais_vessels(std::slice::from_ref(&report));
        world.update_cache(&GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1), 1.0);

        assert!(echo_at(&world, 180.0, 900.0).intensity > 0);
        assert_eq!(echo_at(&world, 180.0, 1000.0), Echo::NONE);
    }

    /// Following a boat far from the scenario rebuilds it there; the AIS
    /// vessels must not vanish until the store is next read.
    #[test]
    fn rebuilding_the_world_keeps_the_ais_vessels() {
        let mut world = ais_world(&[vessel(180.0, 1000.0, Some(20.0))]);

        world.rebuild_around(GeoPosition::new(OWN_SHIP.0 + 1.0, OWN_SHIP.1));
        world.update_cache(&GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1), 1.0);

        assert!(echo_at(&world, 180.0, 1000.0).intensity > 0);
    }

    /// A vessel's length often arrives in a later message than its
    /// position, so the echo takes it up without waiting for a new position.
    #[test]
    fn an_ais_vessel_takes_up_a_newly_reported_length() {
        let mut world = ais_world(&[vessel(180.0, 1000.0, None)]);
        world.set_ais_vessels(&[vessel(180.0, 1000.0, Some(200.0))]);
        world.update_cache(&GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1), 1.0);

        assert!(echo_at(&world, 180.0, 1080.0).intensity > 0);
    }

    #[test]
    fn a_new_ais_report_moves_the_vessel_to_it() {
        let mut world = ais_world(&[vessel(180.0, 1000.0, Some(20.0))]);

        world.update(20.0);
        world.set_ais_vessels(&[vessel(180.0, 1500.0, Some(20.0))]);
        world.update_cache(&GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1), 1.0);

        assert!(echo_at(&world, 180.0, 1500.0).intensity > 0);
        assert_eq!(echo_at(&world, 180.0, 900.0), Echo::NONE);
    }

    /// The test scenario is laid out around the emulator's own boat, so it
    /// disappears while the emulator follows a boat sailing elsewhere.
    #[test]
    fn the_test_scenario_can_be_hidden() {
        let own = GeoPosition::new(OWN_SHIP.0, OWN_SHIP.1);
        let mut world = EmulatorWorld::new(own);
        let land_bearing = 180.0 + LAND_DISTANCE_WEST.atan2(LAND_DISTANCE_SOUTH).to_degrees();
        let land_distance = LAND_DISTANCE_WEST.hypot(LAND_DISTANCE_SOUTH);
        world.update_cache(&own, 1.0);
        assert!(echo_at(&world, land_bearing, land_distance).intensity > 0);

        world.show_test_scenario(false);
        world.update_cache(&own, 1.0);
        assert_eq!(echo_at(&world, land_bearing, land_distance), Echo::NONE);
    }

    #[test]
    fn test_distance_between() {
        let p1 = GeoPosition::new(53.0, 5.0);
        let p2 = GeoPosition::new(53.0, 5.01); // Small offset East

        let dist = distance_between(&p1, &p2);
        // At 53N, 0.01 degrees longitude is about 670m
        assert!(dist > 600.0 && dist < 700.0);
    }
}
