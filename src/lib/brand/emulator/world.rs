use std::f64::consts::{PI, TAU};

use crate::radar::GeoPosition;

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

// Conversion constants
const KNOTS_TO_MS: f64 = 1852.0 / 3600.0; // 1 knot = 1852m/h = 0.5144 m/s
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
            speed: speed_knots * KNOTS_TO_MS,
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
                self.position = turn.center.position_from_bearing(final_bearing, turn.radius);
                self.heading = (turn.start_heading + 180.0) % 360.0;
            } else {
                let current_bearing = if turn.clockwise {
                    turn.start_bearing + turn.turned
                } else {
                    turn.start_bearing - turn.turned
                };
                self.position = turn.center.position_from_bearing(current_bearing, turn.radius);
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
        let speed_ms = speed_knots * KNOTS_TO_MS;
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

/// Cached local coordinates for efficient per-spoke lookups
pub(crate) struct LocalCache {
    /// Land center in local coords
    land_center: (f64, f64),
    /// Target positions in local coords
    targets: Vec<(f64, f64)>,
    /// Crossing target positions in local coords
    crossing_targets: Vec<(f64, f64)>,
    /// Fast target positions in local coords
    fast_targets: Vec<(f64, f64)>,
    /// Circling target position in local coords
    circling_target: (f64, f64),
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
        // Invalidate cache when positions change
        self.cache = None;
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
    /// Call once per spoke batch before generating spokes
    pub(crate) fn update_cache(&mut self, boat_pos: &GeoPosition) {
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

        self.cache = Some(LocalCache {
            land_center: to_local(&self.land.center),
            targets: self.targets.iter().map(|t| to_local(&t.position)).collect(),
            crossing_targets: self
                .crossing_targets
                .iter()
                .map(|t| to_local(&t.position))
                .collect(),
            fast_targets: self
                .fast_targets
                .iter()
                .map(|t| to_local(&t.position))
                .collect(),
            circling_target: to_local(&self.circling_target.position),
            buoys: self
                .buoys
                .iter()
                .map(|b| {
                    let (x, y) = to_local(&b.position);
                    (x, y, b.radius * b.radius)
                })
                .collect(),
        });
    }

    /// Get the radar return intensity at a given position
    /// Uses cached local coordinates for efficiency
    /// sin_b, cos_b are precomputed sin/cos of bearing
    /// Returns 0-15 intensity value (0 = no return, 15 = strongest)
    #[inline]
    pub(crate) fn get_intensity_fast(&self, sin_b: f64, cos_b: f64, distance: f64) -> u8 {
        let cache = match &self.cache {
            Some(c) => c,
            None => return 0,
        };

        // Calculate point in local Cartesian coords (x=east, y=north)
        let point_x = distance * sin_b;
        let point_y = distance * cos_b;

        // Check land using cached local coords
        {
            let dx = point_x - cache.land_center.0;
            let dy = point_y - cache.land_center.1;
            let cos_o = self.land.orientation_rad.cos();
            let sin_o = self.land.orientation_rad.sin();
            let local_x = dx * cos_o - dy * sin_o;
            let local_y = dx * sin_o + dy * cos_o;
            if local_x.abs() <= self.land.half_width && local_y.abs() <= self.land.half_length {
                return 14;
            }
        }

        // Check targets - they appear as point targets with some spread
        const TARGET_RADIUS: f64 = 30.0;
        const TARGET_RADIUS_SQ: f64 = TARGET_RADIUS * TARGET_RADIUS;

        for &(tx, ty) in &cache.targets {
            let dx = point_x - tx;
            let dy = point_y - ty;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < TARGET_RADIUS_SQ {
                let intensity = 15.0 - (dist_sq.sqrt() / TARGET_RADIUS) * 5.0;
                return intensity.max(13.0) as u8;
            }
        }

        for &(tx, ty) in &cache.crossing_targets {
            let dx = point_x - tx;
            let dy = point_y - ty;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < TARGET_RADIUS_SQ {
                let intensity = 15.0 - (dist_sq.sqrt() / TARGET_RADIUS) * 5.0;
                return intensity.max(13.0) as u8;
            }
        }

        for &(tx, ty) in &cache.fast_targets {
            let dx = point_x - tx;
            let dy = point_y - ty;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < TARGET_RADIUS_SQ {
                let intensity = 15.0 - (dist_sq.sqrt() / TARGET_RADIUS) * 5.0;
                return intensity.max(13.0) as u8;
            }
        }

        {
            let dx = point_x - cache.circling_target.0;
            let dy = point_y - cache.circling_target.1;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < TARGET_RADIUS_SQ {
                let intensity = 15.0 - (dist_sq.sqrt() / TARGET_RADIUS) * 5.0;
                return intensity.max(13.0) as u8;
            }
        }

        for &(bx, by, radius_sq) in &cache.buoys {
            let dx = point_x - bx;
            let dy = point_y - by;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq < radius_sq {
                return 10;
            }
        }

        0
    }

    /// Get the radar return intensity at a given position (legacy method)
    #[allow(dead_code)]
    pub(crate) fn get_intensity(&self, _boat_pos: &GeoPosition, bearing_rad: f64, distance: f64) -> u8 {
        self.get_intensity_fast(bearing_rad.sin(), bearing_rad.cos(), distance)
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

    #[test]
    fn test_distance_between() {
        let p1 = GeoPosition::new(53.0, 5.0);
        let p2 = GeoPosition::new(53.0, 5.01); // Small offset East

        let dist = distance_between(&p1, &p2);
        // At 53N, 0.01 degrees longitude is about 670m
        assert!(dist > 600.0 && dist < 700.0);
    }
}
