//! Motion model for target tracking.
//!
//! This module provides IMM (Interacting Multiple Model) filtering for
//! target motion estimation, combining constant velocity, constant acceleration,
//! and coordinated turn models.

use std::f64::consts::TAU;

use super::kalman::{KalmanFilter, Matrix4x4, Vector4};
use crate::radar::GeoPosition;

/// Motion estimation result
#[derive(Clone, Copy, Debug)]
pub struct MotionEstimate {
    /// Speed over ground in m/s
    pub sog: f64,
    /// Course over ground in radians (0 = North, clockwise)
    pub cog: f64,
}

/// Trait for motion estimation strategies
pub trait MotionModel: Send {
    /// Initialize the model with a first measurement
    fn init(&mut self, position: GeoPosition, time: u64);

    /// Initialize with custom position uncertainty
    fn init_with_uncertainty(&mut self, position: GeoPosition, time: u64, position_variance: f64);

    /// Update the model with a new measurement
    /// Returns the estimated SOG and COG
    fn update(&mut self, position: GeoPosition, time: u64) -> MotionEstimate;

    /// Predict position at a future time
    fn predict(&self, time: u64) -> GeoPosition;

    /// Get the current motion estimate
    fn get_motion(&self) -> MotionEstimate;

    /// Get position uncertainty in meters
    fn get_uncertainty(&self) -> f64;

    /// Clone the model into a boxed trait object
    fn clone_box(&self) -> Box<dyn MotionModel>;
}

// ============================================================================
// IMM (Interacting Multiple Model) Motion Model
// ============================================================================

/// Process noise for constant velocity model (low - straight line motion)
const CV_PROCESS_NOISE: f64 = 0.01;
/// Process noise for constant acceleration model (medium)
const CA_PROCESS_NOISE: f64 = 0.05;
/// Process noise for coordinated turn model (high - maneuvering)
const CT_PROCESS_NOISE: f64 = 0.15;

/// Model transition probability matrix
/// Rows = from model, Cols = to model
/// [CV, CA, CT]
const TRANSITION_PROB: [[f64; 3]; 3] = [
    [0.90, 0.05, 0.05], // From CV: likely stays CV
    [0.10, 0.80, 0.10], // From CA: moderately stable
    [0.05, 0.15, 0.80], // From CT: turning tends to persist
];

/// IMM filter using three motion models:
/// - Constant Velocity (CV): For straight-line motion
/// - Constant Acceleration (CA): For speeding up/slowing down
/// - Coordinated Turn (CT): For maneuvering targets
pub struct ImmMotionModel {
    /// Kalman filter for constant velocity model
    cv_filter: KalmanFilter,
    /// Kalman filter for constant acceleration model (higher process noise)
    ca_filter: KalmanFilter,
    /// Kalman filter for coordinated turn model (highest process noise)
    ct_filter: KalmanFilter,
    /// Model probabilities [CV, CA, CT]
    model_probs: [f64; 3],
    /// Last known position
    last_position: GeoPosition,
    /// Combined SOG estimate
    sog: f64,
    /// Combined COG estimate
    cog: f64,
    /// Last update time
    last_time: u64,
    /// Number of updates received
    update_count: u32,
    /// Whether the model has been initialized
    initialized: bool,
}

impl ImmMotionModel {
    pub fn new() -> Self {
        let mut cv_filter = KalmanFilter::new();
        cv_filter.set_process_noise(CV_PROCESS_NOISE);

        let mut ca_filter = KalmanFilter::new();
        ca_filter.set_process_noise(CA_PROCESS_NOISE);

        let mut ct_filter = KalmanFilter::new();
        ct_filter.set_process_noise(CT_PROCESS_NOISE);

        ImmMotionModel {
            cv_filter,
            ca_filter,
            ct_filter,
            model_probs: [0.6, 0.2, 0.2], // Initial: favor CV
            last_position: GeoPosition::new(0.0, 0.0),
            sog: 0.0,
            cog: 0.0,
            last_time: 0,
            update_count: 0,
            initialized: false,
        }
    }

    /// Calculate likelihood of measurement given model prediction
    fn calculate_likelihood(
        predicted: &GeoPosition,
        measured: &GeoPosition,
        uncertainty: f64,
    ) -> f64 {
        let distance = calculate_distance(predicted, measured);
        // Gaussian likelihood
        let sigma = uncertainty.max(1.0);
        let exponent = -0.5 * (distance / sigma).powi(2);
        exponent.exp() / (sigma * TAU.sqrt())
    }

    /// IMM interaction/mixing step. Produces, for each filter j, a mixed
    /// initial state and covariance that the filter then uses as its prior
    /// for this measurement. Without this step the three filters drift
    /// apart over time and the IMM degenerates to "three parallel Kalmans
    /// with a softmax over likelihoods" — which is much less informative
    /// than the literature suggests by name.
    ///
    /// Bar-Shalom, *Estimation with Applications to Tracking and
    /// Navigation*, eq. 11.6.6:
    ///   x0_j = Σ_i μ_{i|j} · x_i
    ///   P0_j = Σ_i μ_{i|j} · (P_i + (x_i - x0_j)(x_i - x0_j)ᵀ)
    /// Then `model_probs[j] = c_bar[j]` is the predicted weight for j.
    ///
    /// Requires all three filters to be in the same local-metre frame.
    /// That's automatic here because `init_with_uncertainty` sets the
    /// same ref_lat/ref_lon on all three from the same first measurement.
    fn mix_states(&mut self) {
        let mut c_bar = [0.0; 3];
        for (j, c) in c_bar.iter_mut().enumerate() {
            for (i, &p) in self.model_probs.iter().enumerate() {
                *c += TRANSITION_PROB[i][j] * p;
            }
        }

        let mut mixing_probs = [[0.0_f64; 3]; 3];
        for (j, &c_j) in c_bar.iter().enumerate() {
            if c_j > 1e-10 {
                for (i, &p_i) in self.model_probs.iter().enumerate() {
                    mixing_probs[i][j] = TRANSITION_PROB[i][j] * p_i / c_j;
                }
            }
        }

        // Snapshot current (state, P) for each filter once — we read them
        // multiple times to build the mixed priors and must not see our
        // own writes mid-loop.
        let filters = [&self.cv_filter, &self.ca_filter, &self.ct_filter];
        let snapshots: [(Vector4, Matrix4x4); 3] = [
            filters[0].state_and_covariance(),
            filters[1].state_and_covariance(),
            filters[2].state_and_covariance(),
        ];

        // Sanity check: all filters must share the same local frame. The
        // IMM constructor and init paths guarantee this, but a future
        // change could break it silently.
        debug_assert!(
            (self.cv_filter.ref_lat() - self.ca_filter.ref_lat()).abs() < 1e-12
                && (self.cv_filter.ref_lat() - self.ct_filter.ref_lat()).abs() < 1e-12
                && (self.cv_filter.ref_lon() - self.ca_filter.ref_lon()).abs() < 1e-12
                && (self.cv_filter.ref_lon() - self.ct_filter.ref_lon()).abs() < 1e-12,
            "IMM filters must share ref_lat/ref_lon for mixing to be valid"
        );

        let mut mixed: [(Vector4, Matrix4x4); 3] = [(Vector4::zeros(), Matrix4x4::zeros()); 3];
        for (j, slot) in mixed.iter_mut().enumerate() {
            // x0_j = Σ_i μ_{i|j} · x_i
            let mut x0 = Vector4::zeros();
            for i in 0..3 {
                x0 += mixing_probs[i][j] * snapshots[i].0;
            }

            // P0_j = Σ_i μ_{i|j} · (P_i + (x_i - x0_j)(x_i - x0_j)ᵀ)
            let mut p0 = Matrix4x4::zeros();
            for i in 0..3 {
                let dx = snapshots[i].0 - x0;
                p0 += mixing_probs[i][j] * (snapshots[i].1 + dx * dx.transpose());
            }

            *slot = (x0, p0);
        }

        // Write the mixed priors back into each filter.
        self.cv_filter
            .set_state_and_covariance(mixed[0].0, mixed[0].1);
        self.ca_filter
            .set_state_and_covariance(mixed[1].0, mixed[1].1);
        self.ct_filter
            .set_state_and_covariance(mixed[2].0, mixed[2].1);

        // Predicted model probabilities for the upcoming update step.
        self.model_probs = c_bar;
    }

    /// Update model probabilities based on measurement likelihoods
    fn update_probabilities(&mut self, cv_likelihood: f64, ca_likelihood: f64, ct_likelihood: f64) {
        let likelihoods = [cv_likelihood, ca_likelihood, ct_likelihood];

        // Calculate normalization factor
        let c: f64 = likelihoods
            .iter()
            .zip(self.model_probs.iter())
            .map(|(l, p)| l * p)
            .sum();

        // Update probabilities
        if c > 1e-10 {
            for (p, &l) in self.model_probs.iter_mut().zip(likelihoods.iter()) {
                *p = l * *p / c;
            }
        }

        // Ensure probabilities sum to 1
        let sum: f64 = self.model_probs.iter().sum();
        if sum > 1e-10 {
            for p in &mut self.model_probs {
                *p /= sum;
            }
        }
    }

    /// Combine estimates from all models
    fn combine_estimates(&mut self) {
        let cv_motion = self.cv_filter.get_motion();
        let ca_motion = self.ca_filter.get_motion();
        let ct_motion = self.ct_filter.get_motion();

        // Weighted average of SOG
        self.sog = self.model_probs[0] * cv_motion.0
            + self.model_probs[1] * ca_motion.0
            + self.model_probs[2] * ct_motion.0;

        // Weighted average of COG (handle wraparound)
        // Use vector averaging for angles
        let mut sin_sum = 0.0;
        let mut cos_sum = 0.0;
        let cogs = [cv_motion.1, ca_motion.1, ct_motion.1];

        for (&p, &cog) in self.model_probs.iter().zip(cogs.iter()) {
            sin_sum += p * cog.sin();
            cos_sum += p * cog.cos();
        }

        self.cog = sin_sum.atan2(cos_sum);
        if self.cog < 0.0 {
            self.cog += TAU;
        }
    }
}

impl Default for ImmMotionModel {
    fn default() -> Self {
        Self::new()
    }
}

impl MotionModel for ImmMotionModel {
    fn init(&mut self, position: GeoPosition, time: u64) {
        self.init_with_uncertainty(position, time, 20.0);
    }

    fn init_with_uncertainty(&mut self, position: GeoPosition, time: u64, position_variance: f64) {
        self.cv_filter
            .init_with_uncertainty(position, time, position_variance);
        self.ca_filter
            .init_with_uncertainty(position, time, position_variance);
        self.ct_filter
            .init_with_uncertainty(position, time, position_variance);

        self.last_position = position;
        self.last_time = time;
        self.sog = 0.0;
        self.cog = 0.0;
        self.update_count = 1;
        self.model_probs = [0.6, 0.2, 0.2];
        self.initialized = true;
    }

    fn update(&mut self, position: GeoPosition, time: u64) -> MotionEstimate {
        if !self.initialized {
            self.init(position, time);
            return MotionEstimate { sog: 0.0, cog: 0.0 };
        }

        // Reject stale or duplicate measurements before doing any work.
        // KalmanFilter::update bails on `time <= last_time`, but the
        // mixing step that runs first would still rewrite all three
        // filter states for an out-of-order sample — corrupting future
        // predictions and model probabilities even though no new
        // measurement was actually accepted. Match the per-filter
        // contract by returning the current motion estimate unchanged.
        if time <= self.last_time {
            return self.get_motion();
        }

        // Step 1: Interaction/Mixing
        self.mix_states();

        // Step 2: Mode-matched filtering
        // Get predictions from each model
        let cv_pred = self.cv_filter.predict(time);
        let ca_pred = self.ca_filter.predict(time);
        let ct_pred = self.ct_filter.predict(time);

        // Update each filter
        self.cv_filter.update(position, time);
        self.ca_filter.update(position, time);
        self.ct_filter.update(position, time);

        // Step 3: Mode probability update
        let cv_unc = self.cv_filter.get_uncertainty();
        let ca_unc = self.ca_filter.get_uncertainty();
        let ct_unc = self.ct_filter.get_uncertainty();

        let cv_likelihood = Self::calculate_likelihood(&cv_pred, &position, cv_unc);
        let ca_likelihood = Self::calculate_likelihood(&ca_pred, &position, ca_unc);
        let ct_likelihood = Self::calculate_likelihood(&ct_pred, &position, ct_unc);

        self.update_probabilities(cv_likelihood, ca_likelihood, ct_likelihood);

        // Step 4: Estimate combination
        self.combine_estimates();

        self.last_position = position;
        self.last_time = time;
        self.update_count += 1;

        log::trace!(
            "IMM update: probs=[CV:{:.2}, CA:{:.2}, CT:{:.2}], sog={:.1}m/s, cog={:.1}°",
            self.model_probs[0],
            self.model_probs[1],
            self.model_probs[2],
            self.sog,
            self.cog.to_degrees()
        );

        MotionEstimate {
            sog: self.sog,
            cog: self.cog,
        }
    }

    fn predict(&self, time: u64) -> GeoPosition {
        // Use weighted combination of predictions
        let cv_pred = self.cv_filter.predict(time);
        let ca_pred = self.ca_filter.predict(time);
        let ct_pred = self.ct_filter.predict(time);

        // Weight by model probabilities
        let lat = self.model_probs[0] * cv_pred.lat()
            + self.model_probs[1] * ca_pred.lat()
            + self.model_probs[2] * ct_pred.lat();
        let lon = self.model_probs[0] * cv_pred.lon()
            + self.model_probs[1] * ca_pred.lon()
            + self.model_probs[2] * ct_pred.lon();

        GeoPosition::new(lat, lon)
    }

    fn get_motion(&self) -> MotionEstimate {
        MotionEstimate {
            sog: self.sog,
            cog: self.cog,
        }
    }

    fn get_uncertainty(&self) -> f64 {
        // Weighted combination of uncertainties
        self.model_probs[0] * self.cv_filter.get_uncertainty()
            + self.model_probs[1] * self.ca_filter.get_uncertainty()
            + self.model_probs[2] * self.ct_filter.get_uncertainty()
    }

    fn clone_box(&self) -> Box<dyn MotionModel> {
        Box::new(ImmMotionModel {
            cv_filter: KalmanFilter::new(),
            ca_filter: KalmanFilter::new(),
            ct_filter: KalmanFilter::new(),
            model_probs: self.model_probs,
            last_position: self.last_position,
            sog: self.sog,
            cog: self.cog,
            last_time: self.last_time,
            update_count: self.update_count,
            initialized: self.initialized,
        })
    }
}

// ============================================================================
// Utilities
// ============================================================================

/// Calculate distance between two positions in meters
fn calculate_distance(from: &GeoPosition, to: &GeoPosition) -> f64 {
    use super::METERS_PER_DEGREE_LATITUDE;
    use super::meters_per_degree_longitude;

    let dlat = (to.lat() - from.lat()) * METERS_PER_DEGREE_LATITUDE;
    let dlon = (to.lon() - from.lon()) * meters_per_degree_longitude(&from.lat());

    (dlat * dlat + dlon * dlon).sqrt()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;
    use crate::radar::KN_TO_MS;

    #[test]
    fn test_imm_model_straight_line() {
        let mut model = ImmMotionModel::new();

        // Initialize at origin
        let pos0 = GeoPosition::new(52.0, 4.0);
        model.init(pos0, 0);

        // Move north at ~10 m/s - do multiple updates to let IMM converge
        let delta_lat = 30.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        let _ = model.update(GeoPosition::new(52.0 + delta_lat, 4.0), 3000);
        let _ = model.update(GeoPosition::new(52.0 + 2.0 * delta_lat, 4.0), 6000);
        let estimate = model.update(GeoPosition::new(52.0 + 3.0 * delta_lat, 4.0), 9000);

        // Should have reasonable speed estimate after convergence
        assert!(
            estimate.sog > 5.0 && estimate.sog < 15.0,
            "SOG should be ~10 m/s, got {}",
            estimate.sog
        );

        // CV model should dominate for straight line motion
        assert!(
            model.model_probs[0] > 0.4,
            "CV model should have high probability for straight line, got {:?}",
            model.model_probs
        );
    }

    #[test]
    fn test_imm_model_turn() {
        let mut model = ImmMotionModel::new();

        // Initialize
        let pos0 = GeoPosition::new(52.0, 4.0);
        model.init(pos0, 0);

        // Move east for first update
        let delta_lon = 30.0 / super::super::meters_per_degree_longitude(&52.0);
        let pos1 = GeoPosition::new(52.0, 4.0 + delta_lon);
        model.update(pos1, 3000);

        // Now turn north
        let delta_lat = 30.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        let pos2 = GeoPosition::new(52.0 + delta_lat, 4.0 + delta_lon);
        model.update(pos2, 6000);

        // CT model should have increased probability after turn
        // (may not dominate immediately, but should increase)
        assert!(
            model.model_probs[2] > 0.1,
            "CT model should have increased, got {:?}",
            model.model_probs
        );
    }

    #[test]
    fn test_imm_model_continuous_circling() {
        // Tests IMM model tracking a target circling at 15 knots in a 250m radius circle
        // for 2 full revolutions. This tests the model's ability to adapt to continuous
        // turning motion.

        let mut model = ImmMotionModel::new();

        // Circle parameters (matching emulator world.rs)
        let radius_m = 250.0;
        let speed_knots = 15.0;
        let speed_ms = speed_knots * KN_TO_MS; // ~7.72 m/s
        let angular_velocity = speed_ms / radius_m; // ~0.031 rad/s

        // Time for one full circle = 2π / angular_velocity ≈ 203 seconds
        let circle_time_s = TAU / angular_velocity;

        // Center of circle
        let center_lat = 52.0 + 350.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        let center_lon = 4.0;

        // Helper to calculate position on circle at given angle
        let position_at_angle = |angle: f64| -> GeoPosition {
            let bearing = PI + angle;
            let lat =
                center_lat + radius_m * bearing.cos() / super::super::METERS_PER_DEGREE_LATITUDE;
            let lon = center_lon
                + radius_m * bearing.sin() / super::super::meters_per_degree_longitude(&center_lat);
            GeoPosition::new(lat, lon)
        };

        // Radar revolution time ~3 seconds
        let revolution_ms = 3000u64;

        // Number of radar revolutions for 2 full circles
        let num_revolutions = (2.0 * circle_time_s / 3.0).ceil() as u64 + 2;

        // Initialize at angle=0 (south of center)
        let pos0 = position_at_angle(0.0);
        model.init(pos0, 0);

        // Track prediction errors
        let mut max_prediction_error = 0.0f64;
        let mut total_prediction_error = 0.0f64;
        let mut prediction_count = 0;

        // Update through 2 full circles
        for rev in 1..num_revolutions {
            let time = rev * revolution_ms;
            let angle = angular_velocity * (time as f64 / 1000.0);
            let actual_pos = position_at_angle(angle);

            // Get prediction before update
            let predicted_pos = model.predict(time);
            let prediction_error = calculate_distance(&predicted_pos, &actual_pos);

            max_prediction_error = max_prediction_error.max(prediction_error);
            total_prediction_error += prediction_error;
            prediction_count += 1;

            // Update with actual position
            model.update(actual_pos, time);
        }

        let avg_prediction_error = total_prediction_error / prediction_count as f64;
        let total_circles = (angular_velocity * num_revolutions as f64 * 3.0) / TAU;

        println!(
            "IMM continuous circling: {:.1} circles, avg error={:.1}m, max error={:.1}m, CT prob={:.2}",
            total_circles, avg_prediction_error, max_prediction_error, model.model_probs[2]
        );

        // After continuous turning, CT model should have significant probability
        assert!(
            model.model_probs[2] > 0.2,
            "CT model should have increased for continuous turning, got {:?}",
            model.model_probs
        );

        // Average prediction error should be reasonable (< 100m for 3s predictions at 7.7 m/s)
        // With circling, the model predicts ahead based on velocity, but target curves
        assert!(
            avg_prediction_error < 100.0,
            "Average prediction error {:.1}m should be < 100m",
            avg_prediction_error
        );

        // Final SOG should be close to actual speed
        let final_motion = model.get_motion();
        assert!(
            (final_motion.sog - speed_ms).abs() < 3.0,
            "Final SOG {:.1} m/s should be close to actual {:.1} m/s",
            final_motion.sog,
            speed_ms
        );
    }

    /// IMM mixing must pull the three filters' state estimates together.
    /// Without mixing, each filter drifts independently with its own Q,
    /// so after many updates the high-Q CT filter is materially offset
    /// from the low-Q CV one. With proper mixing the filters stay in the
    /// same neighbourhood.
    #[test]
    fn imm_mixing_keeps_filter_states_consistent() {
        let mut model = ImmMotionModel::new();
        let start = GeoPosition::new(52.0, 4.0);
        model.init(start, 0);

        // 20 clean updates moving north at ~10 m/s.
        let delta_lat = 30.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        for i in 1..=20u64 {
            model.update(GeoPosition::new(52.0 + delta_lat * i as f64, 4.0), i * 3000);
        }

        // All three filters should predict the same future position to
        // within a few metres. The blended prediction relies on this; if
        // the filters drift apart the blended position is no longer at
        // any of them.
        let future_time = 21 * 3000;
        let cv_pred = model.cv_filter.predict(future_time);
        let ca_pred = model.ca_filter.predict(future_time);
        let ct_pred = model.ct_filter.predict(future_time);

        let max_spread = calculate_distance(&cv_pred, &ca_pred)
            .max(calculate_distance(&cv_pred, &ct_pred))
            .max(calculate_distance(&ca_pred, &ct_pred));

        assert!(
            max_spread < 5.0,
            "IMM mixing should keep filter predictions within 5m of each \
             other after 20 clean steady-state updates; got max spread {:.2}m",
            max_spread
        );
    }

    /// A duplicate or out-of-order timestamp must be a no-op from the
    /// caller's perspective: same SOG/COG returned, no mutation of the
    /// internal filters or model probabilities. Without the stale-time
    /// guard, mix_states would rewrite all three filter states even
    /// though no new measurement was accepted, drifting future
    /// predictions for a replayed sample.
    #[test]
    fn imm_update_is_idempotent_on_stale_or_duplicate_timestamps() {
        let mut model = ImmMotionModel::new();
        model.init(GeoPosition::new(52.0, 4.0), 0);

        let lat_step = 30.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        for i in 1..=5u64 {
            model.update(GeoPosition::new(52.0 + lat_step * i as f64, 4.0), i * 3000);
        }

        let baseline_motion = model.get_motion();
        let baseline_probs = model.model_probs;
        let baseline_cv = model.cv_filter.state_and_covariance();
        let baseline_ca = model.ca_filter.state_and_covariance();
        let baseline_ct = model.ct_filter.state_and_covariance();

        // Same timestamp as last update — must be a no-op.
        let returned = model.update(GeoPosition::new(53.0, 5.0), 5 * 3000);
        assert!((returned.sog - baseline_motion.sog).abs() < 1e-12);
        assert!((returned.cog - baseline_motion.cog).abs() < 1e-12);
        assert_eq!(model.model_probs, baseline_probs);
        assert_eq!(model.cv_filter.state_and_covariance().0, baseline_cv.0);
        assert_eq!(model.ca_filter.state_and_covariance().0, baseline_ca.0);
        assert_eq!(model.ct_filter.state_and_covariance().0, baseline_ct.0);

        // Strictly older timestamp — also a no-op.
        let returned = model.update(GeoPosition::new(54.0, 6.0), 2 * 3000);
        assert!((returned.sog - baseline_motion.sog).abs() < 1e-12);
        assert_eq!(model.model_probs, baseline_probs);
        assert_eq!(model.cv_filter.state_and_covariance().0, baseline_cv.0);
    }
}
