//! Polar Coordinate System for ARPA
//!
//! Provides polar coordinate types and conversions for radar target tracking.

use std::f64::consts::PI;
use std::ops::Add;

/// Polar coordinates relative to radar center
#[derive(Debug, Clone, Copy, Default)]
pub struct Polar {
    /// Angle in spoke units (0 to spokes_per_revolution - 1)
    pub angle: i32,
    /// Radius in pixels from radar center
    pub r: i32,
    /// Timestamp in milliseconds when this position was observed
    pub time: u64,
}

impl Polar {
    /// Create a new polar coordinate
    pub fn new(angle: i32, r: i32, time: u64) -> Self {
        Polar { angle, r, time }
    }

    /// Convert angle to radians
    pub fn angle_in_rad(&self, spokes_per_revolution: f64) -> f64 {
        self.angle as f64 * 2.0 * PI / spokes_per_revolution
    }

    /// Check if this polar angle is between start and end angles
    /// Handles wraparound correctly.
    pub fn angle_is_between(&self, start: i32, end: i32) -> bool {
        if self.angle >= start && self.angle < end {
            return true;
        }
        if end < start && (self.angle >= start || self.angle < end) {
            return true;
        }
        false
    }
}

impl Add for Polar {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Polar {
            angle: self.angle + other.angle,
            r: self.r + other.r,
            time: self.time + other.time,
        }
    }
}

/// The four cardinal directions for contour following
pub const FOUR_DIRECTIONS: [Polar; 4] = [
    Polar {
        angle: 0,
        r: 1,
        time: 0,
    }, // Up (radially outward)
    Polar {
        angle: 1,
        r: 0,
        time: 0,
    }, // Right (clockwise)
    Polar {
        angle: 0,
        r: -1,
        time: 0,
    }, // Down (radially inward)
    Polar {
        angle: -1,
        r: 0,
        time: 0,
    }, // Left (counter-clockwise)
];

/// Local position in meters relative to own ship
#[derive(Debug, Clone, Default)]
pub struct LocalPosition {
    /// Latitude offset in meters (north positive)
    pub lat: f64,
    /// Longitude offset in meters (east positive)
    pub lon: f64,
    /// Velocity north in m/s
    pub dlat_dt: f64,
    /// Velocity east in m/s
    pub dlon_dt: f64,
    /// Standard deviation of speed in m/s
    pub sd_speed: f64,
}

impl LocalPosition {
    pub fn new(lat: f64, lon: f64, dlat_dt: f64, dlon_dt: f64) -> Self {
        Self {
            lat,
            lon,
            dlat_dt,
            dlon_dt,
            sd_speed: 0.0,
        }
    }

    /// Get speed in m/s
    pub fn speed_ms(&self) -> f64 {
        (self.dlat_dt * self.dlat_dt + self.dlon_dt * self.dlon_dt).sqrt()
    }

    /// Get course in degrees (0-360, north = 0)
    pub fn course_deg(&self) -> f64 {
        let mut course = self.dlon_dt.atan2(self.dlat_dt).to_degrees();
        if course < 0.0 {
            course += 360.0;
        }
        course
    }
}

/// Conversion constants
pub const METERS_PER_DEGREE_LATITUDE: f64 = 60.0 * 1852.0; // 60 nautical miles
pub const NAUTICAL_MILE: f64 = 1852.0;
pub const KN_TO_MS: f64 = NAUTICAL_MILE / 3600.0;
pub const MS_TO_KN: f64 = 3600.0 / NAUTICAL_MILE;

/// Calculate meters per degree longitude at a given latitude
#[inline]
pub fn meters_per_degree_longitude(lat_deg: f64) -> f64 {
    METERS_PER_DEGREE_LATITUDE * lat_deg.to_radians().cos()
}

/// Polar coordinate converter with radar setup parameters
#[derive(Debug, Clone)]
pub struct PolarConverter {
    pub spokes_per_revolution: i32,
    pub spokes_per_revolution_f64: f64,
    pub pixels_per_meter: f64,
}

impl PolarConverter {
    pub fn new(spokes_per_revolution: i32, pixels_per_meter: f64) -> Self {
        Self {
            spokes_per_revolution,
            spokes_per_revolution_f64: spokes_per_revolution as f64,
            pixels_per_meter,
        }
    }

    /// Normalize angle to [0, spokes_per_revolution)
    #[inline]
    pub fn mod_spokes(&self, angle: i32) -> i32 {
        ((angle % self.spokes_per_revolution) + self.spokes_per_revolution)
            % self.spokes_per_revolution
    }

    /// Convert polar coordinates to local position (meters from own ship)
    ///
    /// # Arguments
    /// * `pol` - Polar coordinates
    /// * `own_lat` - Own ship latitude in degrees
    ///
    /// # Returns
    /// Local position offset in meters (lat_m, lon_m)
    pub fn polar_to_local(&self, pol: &Polar) -> (f64, f64) {
        let angle_rad = pol.angle_in_rad(self.spokes_per_revolution_f64);
        let distance_m = pol.r as f64 / self.pixels_per_meter;

        let lat_m = distance_m * angle_rad.cos();
        let lon_m = distance_m * angle_rad.sin();

        (lat_m, lon_m)
    }

    /// Convert local position (meters) to polar coordinates
    ///
    /// # Arguments
    /// * `lat_m` - Meters north from own ship
    /// * `lon_m` - Meters east from own ship
    /// * `time` - Timestamp
    ///
    /// # Returns
    /// Polar coordinates
    pub fn local_to_polar(&self, lat_m: f64, lon_m: f64, time: u64) -> Polar {
        let r = ((lat_m * lat_m + lon_m * lon_m).sqrt() * self.pixels_per_meter + 1.0) as i32;
        let mut angle = lon_m.atan2(lat_m) * self.spokes_per_revolution_f64 / (2.0 * PI) + 1.0;
        if angle < 0.0 {
            angle += self.spokes_per_revolution_f64;
        }
        Polar::new(angle as i32, r, time)
    }

    /// Convert polar to geographic position offset
    ///
    /// Returns (delta_lat_deg, delta_lon_deg) to add to own ship position
    pub fn polar_to_geo_offset(&self, pol: &Polar, own_lat_deg: f64) -> (f64, f64) {
        let (lat_m, lon_m) = self.polar_to_local(pol);
        let delta_lat = lat_m / METERS_PER_DEGREE_LATITUDE;
        let delta_lon = lon_m / meters_per_degree_longitude(own_lat_deg);
        (delta_lat, delta_lon)
    }

    /// Convert geographic position to polar coordinates
    ///
    /// # Arguments
    /// * `target_lat` - Target latitude in degrees
    /// * `target_lon` - Target longitude in degrees
    /// * `own_lat` - Own ship latitude in degrees
    /// * `own_lon` - Own ship longitude in degrees
    /// * `time` - Timestamp
    pub fn geo_to_polar(
        &self,
        target_lat: f64,
        target_lon: f64,
        own_lat: f64,
        own_lon: f64,
        time: u64,
    ) -> Polar {
        let dif_lat = (target_lat - own_lat) * METERS_PER_DEGREE_LATITUDE;
        let dif_lon = (target_lon - own_lon) * meters_per_degree_longitude(own_lat);
        self.local_to_polar(dif_lat, dif_lon, time)
    }

    /// Number of spokes for a margin (1/10th of revolution)
    pub fn scan_margin(&self) -> i32 {
        self.spokes_per_revolution / 10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_polar_angle_rad() {
        let pol = Polar::new(0, 100, 0);
        assert!((pol.angle_in_rad(360.0) - 0.0).abs() < 1e-10);

        let pol = Polar::new(90, 100, 0);
        assert!((pol.angle_in_rad(360.0) - PI / 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_mod_spokes() {
        let conv = PolarConverter::new(2048, 1.0);
        assert_eq!(conv.mod_spokes(0), 0);
        assert_eq!(conv.mod_spokes(2048), 0);
        assert_eq!(conv.mod_spokes(-1), 2047);
        assert_eq!(conv.mod_spokes(2049), 1);
    }

    #[test]
    fn test_polar_local_roundtrip() {
        let conv = PolarConverter::new(2048, 0.5); // 0.5 pixels per meter

        let pol = Polar::new(512, 100, 1000); // 90 degrees, 200m
        let (lat_m, lon_m) = conv.polar_to_local(&pol);
        let pol2 = conv.local_to_polar(lat_m, lon_m, 1000);

        assert!((pol.r - pol2.r).abs() <= 1);
        assert!((conv.mod_spokes(pol.angle) - conv.mod_spokes(pol2.angle)).abs() <= 1);
    }

    #[test]
    fn test_angle_is_between() {
        let pol = Polar::new(100, 50, 0);
        assert!(pol.angle_is_between(50, 150));
        assert!(!pol.angle_is_between(150, 200));

        // Wraparound case
        let pol2 = Polar::new(10, 50, 0);
        assert!(pol2.angle_is_between(2000, 50)); // 2000..360..50
    }

    #[test]
    fn test_local_position_speed() {
        let pos = LocalPosition::new(0.0, 0.0, 3.0, 4.0);
        assert!((pos.speed_ms() - 5.0).abs() < 1e-10);
    }
}
