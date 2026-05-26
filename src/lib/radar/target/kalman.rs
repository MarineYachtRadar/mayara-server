//! Kalman filter for target motion estimation.
//!
//! Implements a 4-state Extended Kalman Filter for tracking target position
//! and velocity in geographic coordinates.
//!
//! Based on radar_pi implementation by Douwe Fokkema.
//! See: "An Introduction to the Kalman Filter" by Greg Welch and Gary Bishop

use nalgebra::{SMatrix, SVector};

use super::{METERS_PER_DEGREE_LATITUDE, meters_per_degree_longitude};
use crate::radar::GeoPosition;

pub(super) type Vector4 = SVector<f64, 4>;
pub(super) type Matrix4x4 = SMatrix<f64, 4, 4>;
type Matrix2x2 = SMatrix<f64, 2, 2>;
type Matrix4x2 = SMatrix<f64, 4, 2>;
type Matrix2x4 = SMatrix<f64, 2, 4>;
type Vector2 = SVector<f64, 2>;

/// Process noise - allowed covariance of target speed change.
/// Critical for performance: lower = straighter tracks, higher = allows curves.
/// Value 0.015 allows for reasonable maneuvering targets.
const PROCESS_NOISE: f64 = 0.015;

/// 4-state Kalman filter: [lat, lon, dlat/dt, dlon/dt]
///
/// State vector (all in meters from radar position):
/// - x[0]: latitude offset (meters)
/// - x[1]: longitude offset (meters)
/// - x[2]: latitude velocity (m/s)
/// - x[3]: longitude velocity (m/s)
pub struct KalmanFilter {
    /// State vector [lat_m, lon_m, vlat_m/s, vlon_m/s]
    state: Vector4,
    /// Error covariance matrix P
    p: Matrix4x4,
    /// Process noise covariance matrix Q (2x2 for velocity noise)
    q: Matrix2x2,
    /// Measurement noise covariance matrix R (2x2 for position noise)
    r: Matrix2x2,
    /// Last update time (millis since epoch)
    last_time: u64,
    /// Whether filter has been initialized
    initialized: bool,
    /// Reference latitude for coordinate conversion
    ref_lat: f64,
    /// Reference longitude for coordinate conversion
    ref_lon: f64,
}

impl KalmanFilter {
    /// Create a new uninitialized Kalman filter
    pub fn new() -> Self {
        // Initial P matrix - position uncertainty ~20m, velocity ~4 m/s
        let mut p = Matrix4x4::zeros();
        p[(0, 0)] = 20.0; // lat position variance (m²)
        p[(1, 1)] = 20.0; // lon position variance (m²)
        p[(2, 2)] = 4.0; // lat velocity variance (m/s)²
        p[(3, 3)] = 4.0; // lon velocity variance (m/s)²

        // Q - process noise (velocity can change)
        let mut q = Matrix2x2::zeros();
        q[(0, 0)] = PROCESS_NOISE; // lat velocity noise (m/s)²
        q[(1, 1)] = PROCESS_NOISE; // lon velocity noise (m/s)²

        // R - measurement noise (radar position accuracy)
        // Higher values trust predictions more, lower values trust measurements more
        let mut r = Matrix2x2::zeros();
        r[(0, 0)] = 25.0; // lat measurement variance (m²) ~5m std dev
        r[(1, 1)] = 25.0; // lon measurement variance (m²) ~5m std dev

        KalmanFilter {
            state: Vector4::zeros(),
            p,
            q,
            r,
            last_time: 0,
            initialized: false,
            ref_lat: 0.0,
            ref_lon: 0.0,
        }
    }

    /// Initialize filter with first measurement
    pub fn init(&mut self, position: GeoPosition, time: u64) {
        self.init_with_uncertainty(position, time, 20.0);
    }

    /// Initialize filter with custom position uncertainty (for MARPA targets)
    /// position_variance should be in m² (e.g., 625 gives ~50m uncertainty)
    pub fn init_with_uncertainty(
        &mut self,
        position: GeoPosition,
        time: u64,
        position_variance: f64,
    ) {
        self.ref_lat = position.lat();
        self.ref_lon = position.lon();
        // Initial state is at origin (0,0) in local coordinates with zero velocity
        self.state = Vector4::zeros();

        // Reset P to initial uncertainty
        self.p = Matrix4x4::zeros();
        self.p[(0, 0)] = position_variance;
        self.p[(1, 1)] = position_variance;
        self.p[(2, 2)] = 4.0;
        self.p[(3, 3)] = 4.0;

        self.last_time = time;
        self.initialized = true;
    }

    /// Check if filter has been initialized
    #[allow(dead_code)]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Convert geographic position to local meters from reference
    fn geo_to_local(&self, position: &GeoPosition) -> (f64, f64) {
        let dlat = (position.lat() - self.ref_lat) * METERS_PER_DEGREE_LATITUDE;
        let dlon = (position.lon() - self.ref_lon) * meters_per_degree_longitude(&self.ref_lat);
        (dlat, dlon)
    }

    /// Convert local meters to geographic position
    fn local_to_geo(&self, lat_m: f64, lon_m: f64) -> GeoPosition {
        let lat = self.ref_lat + lat_m / METERS_PER_DEGREE_LATITUDE;
        let lon = self.ref_lon + lon_m / meters_per_degree_longitude(&self.ref_lat);
        GeoPosition::new(lat, lon)
    }

    /// Build state transition matrix A for given time delta
    fn state_transition_matrix(delta_t: f64) -> Matrix4x4 {
        // State transition: position += velocity * dt
        // [lat']     [1  0  dt  0 ] [lat ]
        // [lon']  =  [0  1  0   dt] [lon ]
        // [vlat']    [0  0  1   0 ] [vlat]
        // [vlon']    [0  0  0   1 ] [vlon]
        Matrix4x4::new(
            1.0, 0.0, delta_t, 0.0, 0.0, 1.0, 0.0, delta_t, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        )
    }

    /// Build W matrix (maps process noise to state)
    /// Process noise only affects velocity, not position directly
    fn process_noise_mapping() -> Matrix4x2 {
        // W maps velocity noise to state
        // Only velocity states are affected by process noise
        Matrix4x2::new(
            0.0, 0.0, // lat position not directly affected
            0.0, 0.0, // lon position not directly affected
            1.0, 0.0, // lat velocity affected by noise
            0.0, 1.0, // lon velocity affected by noise
        )
    }

    /// Build observation matrix H (observes only position, not velocity)
    fn observation_matrix() -> Matrix2x4 {
        Matrix2x4::new(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }

    /// Predict position at given time without updating filter state
    pub fn predict(&self, time: u64) -> GeoPosition {
        if !self.initialized || time <= self.last_time {
            return self.local_to_geo(self.state[0], self.state[1]);
        }

        let delta_t = (time - self.last_time) as f64 / 1000.0;
        let a = Self::state_transition_matrix(delta_t);
        let predicted = a * self.state;

        self.local_to_geo(predicted[0], predicted[1])
    }

    /// Update P covariance matrix after prediction (separate from predict for timing)
    fn update_p(&mut self, delta_t: f64) {
        let a = Self::state_transition_matrix(delta_t);
        let at = a.transpose();
        let w = Self::process_noise_mapping();
        let wt = w.transpose();

        // P = A * P * AT + W * Q * WT
        self.p = a * self.p * at + w * self.q * wt;
    }

    /// Update filter with new measurement, returns (sog_ms, cog_rad)
    pub fn update(&mut self, position: GeoPosition, time: u64) -> (f64, f64) {
        if !self.initialized {
            self.init(position, time);
            return (0.0, 0.0);
        }

        if time <= self.last_time {
            return self.get_motion();
        }

        let delta_t = (time - self.last_time) as f64 / 1000.0;

        // Predict step - advance state by dt
        let a = Self::state_transition_matrix(delta_t);
        let predicted_state = a * self.state;

        // Update P with process noise
        self.update_p(delta_t);

        // Measurement in local coordinates
        let (z_lat, z_lon) = self.geo_to_local(&position);
        let z = Vector2::new(z_lat, z_lon);

        // Observation matrix
        let h = Self::observation_matrix();
        let ht = h.transpose();

        // Innovation (measurement residual)
        let y = z - h * predicted_state;

        // Kalman gain: K = P * HT * (H * P * HT + R)^-1
        let s = h * self.p * ht + self.r;
        let Some(s_inv) = s.try_inverse() else {
            // S is the innovation covariance H·P·Hᵀ + R. With positive-
            // definite P and R this should never be singular. If it is,
            // something has gone wrong (NaN propagation, numerical drift)
            // and substituting identity for s_inv silently corrupts the
            // filter. Skip this update — keep state at the prediction and
            // hope the next measurement gives a well-conditioned S.
            log::error!(
                "Kalman: singular innovation covariance, skipping update at t={}",
                time
            );
            self.state = predicted_state;
            self.last_time = time;
            return self.get_motion();
        };
        let k: Matrix4x2 = self.p * ht * s_inv;

        // Updated state: X = X + K * y
        self.state = predicted_state + k * y;

        // Updated covariance: P = (I - K * H) * P
        let i_kh = Matrix4x4::identity() - k * h;
        self.p = i_kh * self.p;

        self.last_time = time;

        self.get_motion()
    }

    /// Get current SOG (m/s) and COG (radians, 0 = North) from velocity state
    pub fn get_motion(&self) -> (f64, f64) {
        // State is already in meters and m/s
        let lat_vel_ms = self.state[2];
        let lon_vel_ms = self.state[3];

        let sog = (lat_vel_ms * lat_vel_ms + lon_vel_ms * lon_vel_ms).sqrt();

        // COG: atan2(east_velocity, north_velocity) gives bearing from north
        let cog = lon_vel_ms.atan2(lat_vel_ms);
        // Normalize to [0, 2π)
        let cog = if cog < 0.0 {
            cog + std::f64::consts::TAU
        } else {
            cog
        };

        (sog, cog)
    }

    /// Get current position estimate
    #[allow(dead_code)]
    pub fn get_position(&self) -> GeoPosition {
        self.local_to_geo(self.state[0], self.state[1])
    }

    /// Get position uncertainty in meters (approximate)
    pub fn get_uncertainty(&self) -> f64 {
        // P matrix is already in meters, so variance is in m²
        let lat_var = self.p[(0, 0)];
        let lon_var = self.p[(1, 1)];

        // Return 2-sigma uncertainty (95% confidence)
        2.0 * (lat_var + lon_var).sqrt()
    }

    /// Get speed uncertainty in m/s
    #[allow(dead_code)]
    pub fn get_speed_uncertainty(&self) -> f64 {
        // Rough approximation of standard deviation of speed
        ((self.p[(2, 2)] + self.p[(3, 3)]) / 2.0).sqrt()
    }

    /// Return the (state, covariance) pair for IMM mixing.
    /// Callers must respect the filter's `ref_lat` / `ref_lon` — the
    /// state vector is in local metres relative to that reference.
    pub(super) fn state_and_covariance(&self) -> (Vector4, Matrix4x4) {
        (self.state, self.p)
    }

    /// Replace `(state, P)` for IMM mixing. Time and reference position
    /// are unchanged. Used by `ImmMotionModel` to seed each filter with
    /// its mixed initial state at the start of each measurement step.
    pub(super) fn set_state_and_covariance(&mut self, state: Vector4, p: Matrix4x4) {
        self.state = state;
        self.p = p;
    }

    /// Reference latitude used to convert geographic positions to the
    /// filter's local-metres coordinate frame. Exposed for IMM mixing,
    /// which must convert all filters to a common frame before averaging
    /// their states.
    pub(super) fn ref_lat(&self) -> f64 {
        self.ref_lat
    }

    /// Reference longitude — see `ref_lat`.
    pub(super) fn ref_lon(&self) -> f64 {
        self.ref_lon
    }

    /// Set process noise level (higher = more maneuverable targets)
    #[allow(dead_code)]
    pub fn set_process_noise(&mut self, noise: f64) {
        self.q[(0, 0)] = noise;
        self.q[(1, 1)] = noise;
    }
}

impl Default for KalmanFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_and_predict() {
        let mut kf = KalmanFilter::new();
        let pos = GeoPosition::new(52.0, 4.0);
        kf.init(pos, 0);

        // Predict at same time should return same position
        let pred = kf.predict(0);
        assert!((pred.lat() - 52.0).abs() < 1e-6);
        assert!((pred.lon() - 4.0).abs() < 1e-6);
    }

    #[test]
    fn test_update_computes_velocity() {
        let mut kf = KalmanFilter::new();

        // First position
        let pos1 = GeoPosition::new(52.0, 4.0);
        kf.init(pos1, 0);

        // Move north by ~111 meters (0.001 degrees lat) in 1 second
        // True speed would be ~111 m/s
        // With high measurement noise (R=25m²), filter will be conservative
        let pos2 = GeoPosition::new(52.001, 4.0);
        let (sog1, _) = kf.update(pos2, 1000);

        // First update: Kalman filter is conservative due to high measurement noise (R=25m²)
        // With conservative settings, speed builds up gradually over multiple updates
        assert!(
            sog1 > 5.0 && sog1 < 150.0,
            "SOG after 1st update was {}",
            sog1
        );

        // Continue moving north at same rate
        let pos3 = GeoPosition::new(52.002, 4.0);
        let (sog2, _) = kf.update(pos3, 2000);

        // Speed should increase as filter gains confidence
        assert!(
            sog2 > sog1 && sog2 < 150.0,
            "SOG after 2nd update was {}",
            sog2
        );

        // More updates to let filter converge
        let pos4 = GeoPosition::new(52.003, 4.0);
        let (sog3, cog) = kf.update(pos4, 3000);

        // Speed continues to increase toward true value
        assert!(
            sog3 > sog2 && sog3 < 150.0,
            "SOG after 3rd update was {}",
            sog3
        );

        // COG should be approximately 0 (north)
        assert!(
            cog.abs() < 0.2 || (cog - std::f64::consts::TAU).abs() < 0.2,
            "COG should be north: {}",
            cog
        );
    }

    #[test]
    fn test_slow_target() {
        let mut kf = KalmanFilter::new();

        // A target moving at 5 m/s (~10 knots) north
        // In 3 seconds, moves ~15 meters = ~0.000135 degrees
        let pos1 = GeoPosition::new(52.0, 4.0);
        kf.init(pos1, 0);

        // Move at 5 m/s for 3 seconds
        let delta_deg = 15.0 / METERS_PER_DEGREE_LATITUDE;
        let pos2 = GeoPosition::new(52.0 + delta_deg, 4.0);
        let (sog1, _) = kf.update(pos2, 3000);

        // Filter should show some speed
        assert!(
            sog1 > 1.0 && sog1 < 20.0,
            "SOG for slow target was {}",
            sog1
        );
    }

    #[test]
    fn test_uncertainty_decreases() {
        let mut kf = KalmanFilter::new();

        let pos = GeoPosition::new(52.0, 4.0);
        kf.init(pos, 0);

        let initial_uncertainty = kf.get_uncertainty();

        // Multiple consistent updates should decrease uncertainty
        for i in 1..5 {
            let t = i * 3000;
            let delta = (i as f64) * 0.0001;
            let pos = GeoPosition::new(52.0 + delta, 4.0);
            kf.update(pos, t);
        }

        let final_uncertainty = kf.get_uncertainty();

        // Uncertainty should decrease with consistent measurements
        assert!(
            final_uncertainty < initial_uncertainty,
            "Uncertainty should decrease: {} -> {}",
            initial_uncertainty,
            final_uncertainty
        );
    }
}
