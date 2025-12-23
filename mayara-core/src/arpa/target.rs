//! ARPA Target State and Tracking Logic
//!
//! Provides target state management and the core refresh algorithm.

use serde::{Deserialize, Serialize};

use super::contour::{Contour, ContourError, MAX_CONTOUR_LENGTH};
use super::doppler::DopplerState;
use super::history::HistoryBuffer;
use super::kalman::KalmanFilter;
use super::polar::{
    meters_per_degree_longitude, LocalPosition, Polar, PolarConverter, METERS_PER_DEGREE_LATITUDE,
    MS_TO_KN,
};

/// Maximum number of sweeps a target can be missed before being marked lost
pub const MAX_LOST_COUNT: i32 = 12;

/// Maximum detection speed in knots (for search radius calculation)
pub const MAX_DETECTION_SPEED_KN: f64 = 40.0;

/// Target tracking status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    /// Under acquisition, first seen, no contour yet
    Acquire0,
    /// Under acquisition, first contour found
    Acquire1,
    /// Under acquisition, speed and course known
    Acquire2,
    /// Under acquisition, speed and course known, next time active
    Acquire3,
    /// Active tracking
    Active,
    /// Target lost
    Lost,
    /// Target scheduled for deletion
    ForDeletion,
}

impl Default for TargetStatus {
    fn default() -> Self {
        TargetStatus::Acquire0
    }
}

/// Refresh state of a target within a scan
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshState {
    /// Target not found in this scan
    NotFound,
    /// Target found and updated
    Found,
    /// Target is out of current range scope
    OutOfScope,
}

impl Default for RefreshState {
    fn default() -> Self {
        RefreshState::NotFound
    }
}

/// Refresh pass number
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    First,
    Second,
    Third,
}

/// Extended position with velocity
#[derive(Debug, Clone, Default)]
pub struct ExtendedPosition {
    /// Latitude in degrees
    pub lat: f64,
    /// Longitude in degrees
    pub lon: f64,
    /// Latitude velocity in m/s
    pub dlat_dt: f64,
    /// Longitude velocity in m/s
    pub dlon_dt: f64,
    /// Timestamp in milliseconds
    pub time: u64,
    /// Speed in knots
    pub speed_kn: f64,
    /// Standard deviation of speed in knots
    pub sd_speed_kn: f64,
}

impl ExtendedPosition {
    pub fn new(
        lat: f64,
        lon: f64,
        dlat_dt: f64,
        dlon_dt: f64,
        time: u64,
        speed_kn: f64,
        sd_speed_kn: f64,
    ) -> Self {
        Self {
            lat,
            lon,
            dlat_dt,
            dlon_dt,
            time,
            speed_kn,
            sd_speed_kn,
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }
}

/// Core ARPA target state
///
/// Contains all state for tracking a single target.
/// This is a pure data structure - no I/O or threading.
#[derive(Debug, Clone)]
pub struct TargetState {
    /// Unique target ID
    pub id: usize,
    /// Current tracking status
    pub status: TargetStatus,
    /// Current position estimate
    pub position: ExtendedPosition,
    /// Expected polar position for next scan
    pub expected: Polar,
    /// Radar position when target was last seen
    pub radar_pos_lat: f64,
    pub radar_pos_lon: f64,
    /// Course in degrees (0-360)
    pub course: f64,
    /// Doppler state of target
    pub doppler: DopplerState,
    /// Current contour
    pub contour: Contour,
    /// Average contour length (for validation)
    pub average_contour_length: i32,
    /// Previous contour length
    pub previous_contour_length: i32,
    /// Number of consecutive misses
    pub lost_count: i32,
    /// Time of last refresh
    pub refresh_time: u64,
    /// Whether target was auto-acquired
    pub automatic: bool,
    /// Stationary counter
    pub stationary: i32,
    /// Refresh state in current scan
    pub refreshed: RefreshState,
    /// Whether target was transferred from another radar
    pub transferred: bool,
    /// Kalman filter
    pub kalman: KalmanFilter,
    /// Total pixels in last contour
    pub total_pix: u32,
    /// Approaching pixels in last contour
    pub approaching_pix: u32,
    /// Receding pixels in last contour
    pub receding_pix: u32,
    /// Whether radar has Doppler capability
    pub have_doppler: bool,
    /// Age in rotations
    pub age_rotations: u32,
    /// Small and fast target flag
    pub small_fast: bool,
}

impl TargetState {
    /// Create a new target at the given position
    pub fn new(
        id: usize,
        position: ExtendedPosition,
        radar_lat: f64,
        radar_lon: f64,
        spokes_per_revolution: usize,
        status: TargetStatus,
        have_doppler: bool,
    ) -> Self {
        Self {
            id,
            status,
            position,
            expected: Polar::default(),
            radar_pos_lat: radar_lat,
            radar_pos_lon: radar_lon,
            course: 0.0,
            doppler: DopplerState::Any,
            contour: Contour::new(),
            average_contour_length: 0,
            previous_contour_length: 0,
            lost_count: 0,
            refresh_time: 0,
            automatic: false,
            stationary: 0,
            refreshed: RefreshState::NotFound,
            transferred: false,
            kalman: KalmanFilter::new(spokes_per_revolution),
            total_pix: 0,
            approaching_pix: 0,
            receding_pix: 0,
            have_doppler,
            age_rotations: 0,
            small_fast: false,
        }
    }

    /// Reset target to lost state
    pub fn set_lost(&mut self) {
        self.contour = Contour::new();
        self.previous_contour_length = 0;
        self.lost_count = 0;
        self.kalman.reset();
        self.status = TargetStatus::Lost;
        self.automatic = false;
        self.refresh_time = 0;
        self.course = 0.0;
        self.stationary = 0;
        self.position.dlat_dt = 0.0;
        self.position.dlon_dt = 0.0;
        self.position.speed_kn = 0.0;
    }

    /// Count pixels in the target contour
    pub fn count_pixels(&mut self, history: &HistoryBuffer) {
        use super::history::HistoryPixel;

        self.total_pix = 0;
        self.approaching_pix = 0;
        self.receding_pix = 0;

        let spoke_len = history.spoke_len();

        for point in &self.contour.points {
            for radius in 0..spoke_len {
                let angle_idx = history.mod_spokes(point.angle);
                if let Some(pixel) = history
                    .spokes
                    .get(angle_idx)
                    .and_then(|s| s.sweep.get(radius))
                {
                    let is_target = pixel.contains(HistoryPixel::TARGET);
                    if !is_target {
                        break;
                    }
                    let is_approaching = pixel.contains(HistoryPixel::APPROACHING);
                    let is_receding = pixel.contains(HistoryPixel::RECEDING);

                    self.total_pix += 1;
                    if is_approaching {
                        self.approaching_pix += 1;
                    }
                    if is_receding {
                        self.receding_pix += 1;
                    }
                }
            }
        }
    }

    /// Update Doppler state based on pixel counts
    pub fn update_doppler_state(&mut self) {
        if !self.have_doppler || self.doppler == DopplerState::AnyPlus {
            return;
        }

        let new_state =
            self.doppler
                .transition(self.total_pix, self.approaching_pix, self.receding_pix);

        if new_state != self.doppler {
            self.doppler = new_state;
        }
    }
}

/// Configuration for target refresh
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    pub spokes_per_revolution: i32,
    pub spoke_len: i32,
    pub pixels_per_meter: f64,
    pub rotation_period_ms: u64,
    pub have_doppler: bool,
}

/// Refresh a target - the core ARPA algorithm
///
/// This is the main target tracking function. It:
/// 1. Predicts where the target should be
/// 2. Searches for a matching contour
/// 3. Updates the Kalman filter with the measurement
///
/// # Arguments
/// * `target` - Target to refresh
/// * `history` - History buffer with radar data
/// * `own_lat` - Own ship latitude
/// * `own_lon` - Own ship longitude
/// * `config` - Refresh configuration
/// * `search_radius` - Maximum search distance in pixels
/// * `pass` - Which refresh pass (First, Second, Third)
///
/// # Returns
/// Ok(()) if target was found, Err with reason otherwise
pub fn refresh_target(
    target: &mut TargetState,
    history: &mut HistoryBuffer,
    own_lat: f64,
    own_lon: f64,
    config: &RefreshConfig,
    search_radius: i32,
    pass: Pass,
) -> Result<(), ContourError> {
    // Check preconditions
    if target.status == TargetStatus::Lost || target.refreshed == RefreshState::OutOfScope {
        return Err(ContourError::Lost);
    }
    if target.refreshed == RefreshState::Found {
        return Err(ContourError::AlreadyFound);
    }

    let converter = PolarConverter::new(config.spokes_per_revolution, config.pixels_per_meter);

    // Calculate expected polar position
    let mut pol = converter.geo_to_polar(
        target.position.lat,
        target.position.lon,
        own_lat,
        own_lon,
        target.position.time,
    );

    let initial_angle = pol.angle;
    let initial_r = pol.r;

    // Check if enough time has passed for refresh
    let scan_margin = converter.scan_margin();
    let angle_time =
        history.get_time_at_angle(converter.mod_spokes(pol.angle + scan_margin) as i32);

    let rotation_period = if config.rotation_period_ms > 0 {
        config.rotation_period_ms
    } else {
        2500 // Default
    };

    if angle_time < target.refresh_time + rotation_period - 100 {
        return Err(ContourError::WaitForRefresh);
    }

    // Update refresh time
    target.refresh_time = history.get_time_at_angle(pol.angle);
    let prev_position = target.position.clone();

    // PREDICTION CYCLE

    // Calculate time delta
    let delta_t =
        if target.refresh_time >= prev_position.time && target.status != TargetStatus::Acquire0 {
            (target.refresh_time - prev_position.time) as f64 / 1000.0
        } else {
            0.0
        };

    // Bounds check
    if target.position.lat > 90.0 || target.position.lat < -90.0 {
        return Err(ContourError::Lost);
    }

    // Convert to local coordinates and predict
    let mut x_local = LocalPosition::new(
        (target.position.lat - own_lat) * METERS_PER_DEGREE_LATITUDE,
        (target.position.lon - own_lon) * meters_per_degree_longitude(own_lat),
        target.position.dlat_dt,
        target.position.dlon_dt,
    );

    target.kalman.predict(&mut x_local, delta_t);

    // Convert predicted position back to polar
    pol = converter.local_to_polar(x_local.lat, x_local.lon, target.refresh_time);

    // Bounds check
    if pol.r >= config.spoke_len || pol.r <= 0 {
        return Err(ContourError::Lost);
    }

    target.expected = pol;

    // MEASUREMENT CYCLE

    let mut dist = search_radius;
    if pass == Pass::Third {
        if target.status == TargetStatus::Acquire0 || target.status == TargetStatus::Acquire1 {
            dist *= 2;
        } else if target.position.speed_kn > 15.0 {
            dist *= 2;
        }
    }

    // Search for target
    let mut doppler = target.doppler;
    if pass == Pass::Third {
        doppler = DopplerState::Any;
    }

    let found = history.get_target(&doppler, pol, dist);

    match found {
        Ok((contour, pos)) => {
            // Target found!
            target.contour = contour.clone();

            // Count pixels and update Doppler state
            if target.doppler != DopplerState::Any {
                let backup = target.doppler;
                target.doppler = DopplerState::Any;
                let _ = history.get_target(&target.doppler, pol, dist);
                target.count_pixels(history);
                target.doppler = backup;
                let _ = history.get_target(&target.doppler, pol, dist);
                target.update_doppler_state();
            } else {
                target.count_pixels(history);
                target.update_doppler_state();
            }

            // Validate contour length
            if target.average_contour_length != 0
                && (target.contour.length < target.average_contour_length / 2
                    || target.contour.length > target.average_contour_length * 2)
                && pass != Pass::Third
            {
                return Err(ContourError::WeightedContourLengthTooHigh);
            }

            // Reset pixels so blob isn't found again
            history.reset_pixels(&contour, &pos, config.pixels_per_meter);

            // Check for oversized contour (interference)
            if target.contour.length >= MAX_CONTOUR_LENGTH as i32 - 2 {
                return Err(ContourError::ContourTooLong);
            }

            // Update target state
            target.lost_count = 0;
            target.age_rotations += 1;

            // Status progression
            target.status = match target.status {
                TargetStatus::Acquire0 => TargetStatus::Acquire1,
                TargetStatus::Acquire1 => TargetStatus::Acquire2,
                TargetStatus::Acquire2 => TargetStatus::Acquire3,
                TargetStatus::Acquire3 | TargetStatus::Active => TargetStatus::Active,
                _ => TargetStatus::Acquire0,
            };

            // Get own ship position at measurement time
            let (spoke_lat, spoke_lon) = history.get_position_at_angle(pos.angle);

            if target.status == TargetStatus::Acquire1 {
                // First measurement - set position directly
                let (delta_lat, delta_lon) = converter.polar_to_geo_offset(&pos, spoke_lat);
                target.position.lat = spoke_lat + delta_lat;
                target.position.lon = spoke_lon + delta_lon;
                target.position.dlat_dt = 0.0;
                target.position.dlon_dt = 0.0;
                target.position.sd_speed_kn = 0.0;
                target.expected = pos;
                target.age_rotations = 0;
            }

            // Kalman update for status >= Acquire2
            if target.status == TargetStatus::Acquire2 || target.status == TargetStatus::Acquire3 {
                target.kalman.update_covariance();
                let mut measured = pos;
                target.kalman.update(
                    &mut measured,
                    &mut x_local,
                    &target.expected,
                    config.pixels_per_meter,
                );
            }

            // Update timestamp
            target.position.time = pos.time;

            // Update position from Kalman (except first measurement)
            if target.status != TargetStatus::Acquire1 {
                target.position.lat = own_lat + x_local.lat / METERS_PER_DEGREE_LATITUDE;
                target.position.lon = own_lon + x_local.lon / meters_per_degree_longitude(own_lat);
                target.position.dlat_dt = x_local.dlat_dt;
                target.position.dlon_dt = x_local.dlon_dt;
                target.position.sd_speed_kn = x_local.sd_speed * MS_TO_KN;
            }

            // Small-fast target handling
            if target.status == TargetStatus::Acquire2 {
                let dist_angle = pol.angle - initial_angle;
                let dist_r = pol.r - initial_r;
                let size_angle = history
                    .mod_spokes(target.contour.max_angle - target.contour.min_angle)
                    .max(1);
                let size_r = (target.contour.max_r - target.contour.min_r).max(1);
                let test = (dist_r as f64 / size_r as f64).abs()
                    + (dist_angle as f64 / size_angle as f64).abs();
                target.small_fast = test > 2.0;
            }

            // Linear extrapolation for small-fast targets
            const FORCED_POSITION_STATUS: u32 = 8;
            const FORCED_POSITION_AGE_FAST: u32 = 5;

            if target.small_fast
                && target.age_rotations >= 2
                && target.age_rotations < FORCED_POSITION_STATUS
                && (target.age_rotations < FORCED_POSITION_AGE_FAST
                    || target.position.speed_kn > 10.0)
            {
                let (delta_lat, delta_lon) = converter.polar_to_geo_offset(&pos, spoke_lat);
                let new_lat = spoke_lat + delta_lat;
                let new_lon = spoke_lon + delta_lon;

                let delta_lat_deg = new_lat - prev_position.lat;
                let delta_lon_deg = new_lon - prev_position.lon;
                let delta_t = pos.time.saturating_sub(prev_position.time);

                if delta_t > 1000 {
                    let d_lat_dt =
                        (delta_lat_deg / delta_t as f64) * METERS_PER_DEGREE_LATITUDE * 1000.0;
                    let d_lon_dt = (delta_lon_deg / delta_t as f64)
                        * meters_per_degree_longitude(new_lat)
                        * 1000.0;

                    let factor = 0.8_f64.powf((target.age_rotations - 1) as f64);
                    target.position.lat += factor * (new_lat - target.position.lat);
                    target.position.lon += factor * (new_lon - target.position.lon);
                    target.position.dlat_dt += factor * (d_lat_dt - target.position.dlat_dt);
                    target.position.dlon_dt += factor * (d_lon_dt - target.position.dlon_dt);
                }
            }

            // Update refresh time
            target.refresh_time = target.position.time;

            // Calculate speed and course
            if target.age_rotations >= 1 {
                let s1 = target.position.dlat_dt;
                let s2 = target.position.dlon_dt;
                target.position.speed_kn = (s1 * s1 + s2 * s2).sqrt() * MS_TO_KN;
                target.course = s2.atan2(s1).to_degrees();
                if target.course < 0.0 {
                    target.course += 360.0;
                }

                // Update average contour length
                const WEIGHT_FACTOR: f64 = 0.1;
                if target.contour.length != 0 {
                    if target.average_contour_length == 0 {
                        target.average_contour_length = target.contour.length;
                    } else {
                        target.average_contour_length +=
                            ((target.contour.length - target.average_contour_length) as f64
                                * WEIGHT_FACTOR) as i32;
                    }
                }

                target.previous_contour_length = target.contour.length;
                target.refreshed = RefreshState::Found;
                target.transferred = false;
            }

            Ok(())
        }
        Err(_) => {
            // Target not found
            handle_target_not_found(target, pol, pass)
        }
    }
}

/// Handle the case when target is not found
fn handle_target_not_found(
    target: &mut TargetState,
    _pol: Polar,
    pass: Pass,
) -> Result<(), ContourError> {
    // Small-fast targets must be found quickly
    if target.small_fast && pass == Pass::Second && target.status == TargetStatus::Acquire2 {
        return Err(ContourError::Lost);
    }

    // Delete low-status targets immediately when not found
    if ((target.status == TargetStatus::Acquire1 || target.status == TargetStatus::Acquire2)
        && pass == Pass::Third)
        || target.status == TargetStatus::Acquire0
    {
        return Err(ContourError::Lost);
    }

    if pass == Pass::Third {
        target.lost_count += 1;
    }

    // Delete if not found too often
    if target.lost_count > MAX_LOST_COUNT {
        return Err(ContourError::Lost);
    }

    target.refreshed = RefreshState::NotFound;
    target.transferred = false;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_state_new() {
        let pos = ExtendedPosition::new(51.5, -0.1, 0.0, 0.0, 1000, 0.0, 0.0);
        let target = TargetState::new(1, pos, 51.5, -0.1, 2048, TargetStatus::Acquire0, false);

        assert_eq!(target.id, 1);
        assert_eq!(target.status, TargetStatus::Acquire0);
        assert!(!target.have_doppler);
    }

    #[test]
    fn test_target_set_lost() {
        let pos = ExtendedPosition::new(51.5, -0.1, 5.0, 3.0, 1000, 10.0, 1.0);
        let mut target = TargetState::new(1, pos, 51.5, -0.1, 2048, TargetStatus::Active, false);

        target.set_lost();

        assert_eq!(target.status, TargetStatus::Lost);
        assert_eq!(target.position.speed_kn, 0.0);
        assert_eq!(target.course, 0.0);
    }

    #[test]
    fn test_status_default() {
        let status: TargetStatus = Default::default();
        assert_eq!(status, TargetStatus::Acquire0);
    }

    #[test]
    fn test_extended_position() {
        let pos = ExtendedPosition::new(51.5, -0.1, 5.0, 3.0, 1000, 10.0, 1.0);
        assert!((pos.lat - 51.5).abs() < 1e-10);
        assert!((pos.speed_kn - 10.0).abs() < 1e-10);
    }
}
