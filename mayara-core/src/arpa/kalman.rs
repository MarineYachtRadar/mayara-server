//! Extended Kalman Filter for ARPA Target Tracking
//!
//! Implements a 4-state Extended Kalman Filter for tracking radar targets.
//! State vector: [lat_m, lon_m, dlat_dt, dlon_dt] (position and velocity in meters)
//!
//! The filter uses a constant velocity model with process noise to account
//! for target maneuvering.

use nalgebra::{SMatrix, SVector};
use std::f64::consts::PI;

use super::polar::{LocalPosition, Polar};

/// Process noise - controls how quickly the filter adapts to heading changes
/// Higher values = faster adaptation but more noise sensitivity
/// Lower values = smoother tracking but slower to adapt to maneuvers
const PROCESS_NOISE: f64 = 1.0;

// Matrix type aliases
type Matrix2x2 = SMatrix<f64, 2, 2>;
type Matrix4x4 = SMatrix<f64, 4, 4>;
type Matrix4x2 = SMatrix<f64, 4, 2>;
type Matrix2x4 = SMatrix<f64, 2, 4>;

/// Extended Kalman Filter for target tracking
///
/// Uses polar coordinate measurements (angle, range) to track targets
/// in local Cartesian coordinates (meters from own ship).
#[derive(Debug, Clone)]
pub struct KalmanFilter {
    /// State transition matrix
    a: Matrix4x4,
    /// Transpose of state transition matrix
    at: Matrix4x4,
    /// Process noise coupling matrix
    w: Matrix4x2,
    /// Transpose of process noise coupling
    wt: Matrix2x4,
    /// Observation matrix (Jacobian of measurement function)
    h: Matrix2x4,
    /// Transpose of observation matrix
    ht: Matrix4x2,
    /// Estimate error covariance
    p: Matrix4x4,
    /// Process noise covariance
    q: Matrix2x2,
    /// Measurement noise covariance
    r: Matrix2x2,
    /// Kalman gain
    k: Matrix4x2,
    /// Identity matrix
    i: Matrix4x4,
    /// Spokes per revolution (for angle conversion)
    pub spokes_per_revolution: f64,
}

impl KalmanFilter {
    /// Create a new Kalman filter
    ///
    /// # Arguments
    /// * `spokes_per_revolution` - Number of spokes in one radar revolution
    pub fn new(spokes_per_revolution: usize) -> Self {
        let mut filter = KalmanFilter {
            a: Matrix4x4::identity(),
            at: Matrix4x4::identity(),
            w: Matrix4x2::zeros(),
            wt: Matrix2x4::zeros(),
            h: Matrix2x4::zeros(),
            ht: Matrix4x2::zeros(),
            p: Matrix4x4::zeros(),
            q: Matrix2x2::zeros(),
            r: Matrix2x2::zeros(),
            k: Matrix4x2::zeros(),
            i: Matrix4x4::identity(),
            spokes_per_revolution: spokes_per_revolution as f64,
        };
        filter.reset();
        filter
    }

    /// Reset the filter to initial state
    pub fn reset(&mut self) {
        // State transition matrix (identity + time coupling set in predict)
        self.a = Matrix4x4::identity();
        self.at = Matrix4x4::identity();

        // Process noise coupling: W maps velocity noise to state
        // State: [lat, lon, dlat_dt, dlon_dt]
        // Noise affects velocity directly
        self.w = Matrix4x2::zeros();
        self.w[(2, 0)] = 1.0; // dlat_dt affected by noise[0]
        self.w[(3, 1)] = 1.0; // dlon_dt affected by noise[1]
        self.wt = self.w.transpose();

        // Observation matrix (set dynamically in set_measurement)
        self.h = Matrix2x4::zeros();
        self.ht = Matrix4x2::zeros();

        // Initial estimate error covariance
        // Higher values = more uncertainty, filter adapts faster initially
        self.p = Matrix4x4::zeros();
        self.p[(0, 0)] = 20.0; // Position variance (meters²)
        self.p[(1, 1)] = 20.0;
        self.p[(2, 2)] = 4.0; // Velocity variance (m/s)²
        self.p[(3, 3)] = 4.0;

        // Process noise covariance
        // Controls how much the filter expects the target to maneuver
        self.q[(0, 0)] = PROCESS_NOISE; // Variance in lat velocity
        self.q[(1, 1)] = PROCESS_NOISE; // Variance in lon velocity

        // Measurement noise covariance
        self.r[(0, 0)] = 100.0; // Variance in angle measurement
        self.r[(1, 1)] = 25.0; // Variance in range measurement
    }

    /// Predict step: project state and covariance forward in time
    ///
    /// # Arguments
    /// * `x` - Current local position (will be updated)
    /// * `delta_time` - Time step in seconds
    pub fn predict(&mut self, x: &mut LocalPosition, delta_time: f64) {
        // Build state vector
        let mut state = SMatrix::<f64, 4, 1>::new(x.lat, x.lon, x.dlat_dt, x.dlon_dt);

        // Set time-dependent elements in transition matrix
        // x_new = x + v * dt
        self.a[(0, 2)] = delta_time;
        self.a[(1, 3)] = delta_time;

        self.at[(2, 0)] = delta_time;
        self.at[(3, 1)] = delta_time;

        // Predict state: x = A * x
        state = self.a * state;

        // Update output
        x.lat = state[(0, 0)];
        x.lon = state[(1, 0)];
        x.dlat_dt = state[(2, 0)];
        x.dlon_dt = state[(3, 0)];

        // Estimate speed uncertainty
        x.sd_speed = ((self.p[(2, 2)] + self.p[(3, 3)]) / 2.0).sqrt();
    }

    /// Update covariance matrix (prediction step part 2)
    ///
    /// Called separately to prevent redundant updates when doing multiple passes.
    pub fn update_covariance(&mut self) {
        // P = A * P * A^T + W * Q * W^T
        self.p = self.a * self.p * self.at + self.w * self.q * self.wt;
    }

    /// Update step: incorporate measurement
    ///
    /// # Arguments
    /// * `measured` - Measured polar position
    /// * `x` - Current local position estimate (will be updated)
    /// * `expected` - Expected polar position (from prediction)
    /// * `pixels_per_meter` - Scale factor for range measurement
    pub fn update(
        &mut self,
        measured: &Polar,
        x: &mut LocalPosition,
        expected: &Polar,
        pixels_per_meter: f64,
    ) {
        // Compute Jacobian of measurement function h(x)
        // h(x) = [atan2(lon, lat) * spokes_per_revolution / (2*PI), sqrt(lat² + lon²) * scale]
        let q_sum_sq = x.lon * x.lon + x.lat * x.lat;
        if q_sum_sq < 1e-10 {
            return; // Target too close to origin, skip update
        }

        let c = self.spokes_per_revolution / (2.0 * PI);

        // Jacobian for angle: d/d[lat,lon] of atan2(lon, lat) * c
        self.h[(0, 0)] = -c * x.lon / q_sum_sq;
        self.h[(0, 1)] = c * x.lat / q_sum_sq;

        // Jacobian for range: d/d[lat,lon] of sqrt(lat² + lon²) * scale
        let q_sum = q_sum_sq.sqrt();
        self.h[(1, 0)] = x.lat / q_sum * pixels_per_meter;
        self.h[(1, 1)] = x.lon / q_sum * pixels_per_meter;

        self.ht = self.h.transpose();

        // Compute innovation (measurement residual)
        // z = measured - expected (in polar coordinates)
        let mut angle_diff = (measured.angle - expected.angle) as f64;
        // Handle angle wraparound
        if angle_diff > self.spokes_per_revolution / 2.0 {
            angle_diff -= self.spokes_per_revolution;
        }
        if angle_diff < -self.spokes_per_revolution / 2.0 {
            angle_diff += self.spokes_per_revolution;
        }
        let range_diff = (measured.r - expected.r) as f64;

        let z = SMatrix::<f64, 2, 1>::new(angle_diff, range_diff);

        // Current state vector
        let mut state = SVector::<f64, 4>::new(x.lat, x.lon, x.dlat_dt, x.dlon_dt);

        // Compute Kalman gain: K = P * H^T * (H * P * H^T + R)^-1
        let s = self.h * self.p * self.ht + self.r;
        match s.try_inverse() {
            Some(s_inv) => {
                self.k = self.p * self.ht * s_inv;
            }
            None => {
                // Matrix singular, skip update
                return;
            }
        }

        // Update state: x = x + K * z
        state = state + self.k * z;

        x.lat = state[(0, 0)];
        x.lon = state[(1, 0)];
        x.dlat_dt = state[(2, 0)];
        x.dlon_dt = state[(3, 0)];

        // Update covariance: P = (I - K * H) * P
        self.p = (self.i - self.k * self.h) * self.p;

        // Update speed uncertainty
        x.sd_speed = ((self.p[(2, 2)] + self.p[(3, 3)]) / 2.0).sqrt();
    }

    /// Get current position variance (for confidence estimation)
    pub fn position_variance(&self) -> f64 {
        (self.p[(0, 0)] + self.p[(1, 1)]) / 2.0
    }

    /// Get current velocity variance (for confidence estimation)
    pub fn velocity_variance(&self) -> f64 {
        (self.p[(2, 2)] + self.p[(3, 3)]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kalman_new() {
        let kf = KalmanFilter::new(2048);
        assert_eq!(kf.spokes_per_revolution, 2048.0);
    }

    #[test]
    fn test_kalman_predict() {
        let mut kf = KalmanFilter::new(2048);
        let mut pos = LocalPosition::new(100.0, 0.0, 5.0, 0.0);

        kf.predict(&mut pos, 1.0);

        // After 1 second at 5 m/s north, should be at 105m
        assert!((pos.lat - 105.0).abs() < 0.01);
        assert!((pos.lon - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_kalman_reset() {
        let mut kf = KalmanFilter::new(2048);

        // Modify state
        kf.p[(0, 0)] = 999.0;

        // Reset
        kf.reset();

        // Should be back to initial
        assert!((kf.p[(0, 0)] - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_kalman_update() {
        let mut kf = KalmanFilter::new(2048);
        let mut pos = LocalPosition::new(100.0, 0.0, 5.0, 0.0);

        // Predict first
        kf.predict(&mut pos, 1.0);
        kf.update_covariance();

        // Create measurement that matches prediction
        let measured = Polar::new(0, 105, 1000);
        let expected = Polar::new(0, 105, 1000);

        kf.update(&measured, &mut pos, &expected, 1.0);

        // Position should be close to measured
        assert!((pos.lat - 105.0).abs() < 1.0);
    }
}
