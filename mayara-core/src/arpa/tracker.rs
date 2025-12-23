//! ARPA Target Tracker
//!
//! Main processor that manages target tracking using Kalman filtering.

use std::collections::HashMap;

use super::cpa::calculate_danger;
use super::detector::{DetectedTarget, TargetDetector};
use super::types::*;

/// Main ARPA processor
#[derive(Debug)]
pub struct ArpaProcessor {
    /// Settings
    settings: ArpaSettings,
    /// Currently tracked targets
    tracks: HashMap<u32, TrackingState>,
    /// Target detector for auto-acquisition
    detector: TargetDetector,
    /// Own ship state
    own_ship: Option<OwnShip>,
    /// Next target ID to assign
    next_id: u32,
    /// Process noise for Kalman filter
    process_noise: f64,
    /// Measurement noise for Kalman filter
    measurement_noise: f64,
}

impl ArpaProcessor {
    /// Create a new ARPA processor
    pub fn new(settings: ArpaSettings) -> Self {
        ArpaProcessor {
            detector: TargetDetector::new(settings.clone()),
            settings,
            tracks: HashMap::new(),
            own_ship: None,
            next_id: 1,
            process_noise: 0.1,      // m²/s⁴ - acceleration variance
            measurement_noise: 25.0, // m² - position measurement variance
        }
    }

    /// Update settings
    pub fn update_settings(&mut self, settings: ArpaSettings) {
        self.detector.update_settings(settings.clone());
        self.settings = settings;
    }

    /// Get current settings
    pub fn settings(&self) -> &ArpaSettings {
        &self.settings
    }

    /// Update own ship state (required for CPA/TCPA)
    pub fn update_own_ship(&mut self, own_ship: OwnShip) {
        self.own_ship = Some(own_ship);
    }

    /// Get own ship state
    pub fn own_ship(&self) -> Option<&OwnShip> {
        self.own_ship.as_ref()
    }

    /// Set range scale (affects detection)
    pub fn set_range_scale(&mut self, range_meters: f64) {
        self.detector.set_range_scale(range_meters);
    }

    /// Manually acquire a target at the specified position
    ///
    /// # Returns
    ///
    /// The new target ID, or None if max targets reached
    pub fn acquire_target(&mut self, bearing: f64, distance: f64, timestamp: u64) -> Option<u32> {
        if !self.settings.enabled {
            return None;
        }

        if self.tracks.len() >= self.settings.max_targets as usize {
            return None;
        }

        let id = self.next_id;
        self.next_id += 1;
        if self.next_id > 99 {
            self.next_id = 1; // Wrap around
        }

        let track = TrackingState::new(id, bearing, distance, timestamp, AcquisitionMethod::Manual);
        self.tracks.insert(id, track);
        Some(id)
    }

    /// Cancel tracking of a target
    pub fn cancel_target(&mut self, target_id: u32) -> bool {
        self.tracks.remove(&target_id).is_some()
    }

    /// Get all tracked targets
    pub fn get_targets(&self) -> Vec<ArpaTarget> {
        self.tracks
            .values()
            .map(|track| {
                let status = self.get_target_status(track);
                let danger = self.calculate_target_danger(track);
                track.to_arpa_target(status, danger, self.own_ship.as_ref())
            })
            .collect()
    }

    /// Get a specific target by ID
    pub fn get_target(&self, id: u32) -> Option<ArpaTarget> {
        self.tracks.get(&id).map(|track| {
            let status = self.get_target_status(track);
            let danger = self.calculate_target_danger(track);
            track.to_arpa_target(status, danger, self.own_ship.as_ref())
        })
    }

    /// Process a radar spoke and update tracking
    ///
    /// # Arguments
    ///
    /// * `spoke_data` - Raw pixel data for the spoke
    /// * `bearing` - Bearing of this spoke in degrees
    /// * `timestamp` - Current timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// Vector of events (target updates, acquisitions, losses, warnings)
    pub fn process_spoke(
        &mut self,
        spoke_data: &[u8],
        bearing: f64,
        timestamp: u64,
    ) -> Vec<ArpaEvent> {
        if !self.settings.enabled {
            return Vec::new();
        }

        let mut events = Vec::new();

        // Detect potential targets in this spoke
        let detections = self
            .detector
            .detect_in_spoke(spoke_data, bearing, timestamp);

        // Update existing tracks that align with this bearing
        events.extend(self.update_tracks_for_bearing(bearing, &detections, timestamp));

        // Check for lost targets
        events.extend(self.check_lost_targets(timestamp));

        events
    }

    /// Process a complete revolution and handle auto-acquisition
    ///
    /// # Arguments
    ///
    /// * `timestamp` - Timestamp of revolution completion
    ///
    /// # Returns
    ///
    /// Vector of events from auto-acquisition
    pub fn process_revolution(&mut self, _timestamp: u64) -> Vec<ArpaEvent> {
        if !self.settings.enabled || !self.settings.auto_acquisition {
            return Vec::new();
        }

        // Correlate detections across multiple revolutions
        // This is called after all spokes have been processed
        // The detector accumulates detections internally

        Vec::new()
    }

    /// Update tracks for a specific bearing
    fn update_tracks_for_bearing(
        &mut self,
        bearing: f64,
        detections: &[DetectedTarget],
        timestamp: u64,
    ) -> Vec<ArpaEvent> {
        let mut events = Vec::new();
        const BEARING_TOLERANCE: f64 = 3.0; // degrees

        // Collect track IDs that match this bearing
        let matching_ids: Vec<u32> = self
            .tracks
            .iter()
            .filter_map(|(id, track)| {
                let track_bearing = track.bearing();
                let bearing_diff = (track_bearing - bearing).abs();
                let bearing_diff = if bearing_diff > 180.0 {
                    360.0 - bearing_diff
                } else {
                    bearing_diff
                };
                if bearing_diff <= BEARING_TOLERANCE {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();

        // Process each matching track
        for id in matching_ids {
            if let Some(track) = self.tracks.get_mut(&id) {
                // Find best matching detection
                let expected_distance = track.distance();
                let best_detection = detections.iter().min_by(|a, b| {
                    let dist_a = (a.distance - expected_distance).abs();
                    let dist_b = (b.distance - expected_distance).abs();
                    dist_a.partial_cmp(&dist_b).unwrap()
                });

                if let Some(det) = best_detection {
                    // Check if detection is close enough
                    let distance_tolerance = expected_distance * 0.2; // 20% tolerance
                    if (det.distance - expected_distance).abs() < distance_tolerance {
                        // Update track with measurement
                        let dt = (timestamp - track.last_seen) as f64 / 1000.0;
                        Self::kalman_update_track(
                            track,
                            det.bearing,
                            det.distance,
                            dt,
                            self.process_noise,
                            self.measurement_noise,
                        );
                        track.last_seen = timestamp;
                        track.update_count += 1;

                        // Calculate danger and emit event
                        let status = Self::get_status_for_track(track);
                        let danger =
                            Self::calculate_danger_for_track(track, self.own_ship.as_ref());
                        let target = track.to_arpa_target(status, danger, self.own_ship.as_ref());

                        // Check for collision warning state change
                        let alert_state = target.alert_state(&self.settings);
                        if alert_state != track.prev_alert_state {
                            track.prev_alert_state = alert_state;
                            if alert_state != AlertState::Normal {
                                events.push(ArpaEvent::CollisionWarning {
                                    target_id: track.id,
                                    state: alert_state,
                                    cpa: danger.cpa,
                                    tcpa: danger.tcpa,
                                });
                            }
                        }

                        events.push(ArpaEvent::TargetUpdate { target });
                    }
                }
            }
        }

        events
    }

    /// Kalman filter prediction step (static version)
    fn kalman_predict_track(track: &mut TrackingState, dt: f64, process_noise: f64) {
        // State transition: predict new position based on velocity
        track.x += track.vx * dt;
        track.y += track.vy * dt;

        // Update covariance: P = F*P*F' + Q
        // F = [[1, 0, dt, 0],
        //      [0, 1, 0, dt],
        //      [0, 0, 1, 0],
        //      [0, 0, 0, 1]]
        let q = process_noise * dt * dt; // Simplified process noise

        // Extract current covariance values
        let p = &track.covariance;
        let p00 = p[0] + 2.0 * dt * p[2] + dt * dt * p[10];
        let p01 = p[1] + dt * p[3] + dt * p[9] + dt * dt * p[11];
        let p02 = p[2] + dt * p[10];
        let p03 = p[3] + dt * p[11];
        let p11 = p[5] + 2.0 * dt * p[7] + dt * dt * p[15];
        let p12 = p[6] + dt * p[14];
        let p13 = p[7] + dt * p[15];
        let p22 = p[10];
        let p23 = p[11];
        let p33 = p[15];

        // Add process noise
        track.covariance = [
            p00 + q,
            p01,
            p02,
            p03,
            p01,
            p11 + q,
            p12,
            p13,
            p02,
            p12,
            p22 + q,
            p23,
            p03,
            p13,
            p23,
            p33 + q,
        ];
    }

    /// Kalman filter update step (static version)
    fn kalman_update_track(
        track: &mut TrackingState,
        bearing_deg: f64,
        distance_m: f64,
        dt: f64,
        process_noise: f64,
        measurement_noise: f64,
    ) {
        // First, predict
        if dt > 0.0 {
            Self::kalman_predict_track(track, dt, process_noise);
        }

        // Convert measurement to Cartesian
        let bearing_rad = bearing_deg.to_radians();
        let z_x = distance_m * bearing_rad.sin();
        let z_y = distance_m * bearing_rad.cos();

        // Innovation (measurement residual)
        let y_x = z_x - track.x;
        let y_y = z_y - track.y;

        // Innovation covariance: S = H*P*H' + R
        // H = [[1, 0, 0, 0], [0, 1, 0, 0]]
        let r = measurement_noise;
        let s00 = track.covariance[0] + r;
        let s01 = track.covariance[1];
        let s11 = track.covariance[5] + r;

        // Kalman gain: K = P*H'*S^-1
        let det = s00 * s11 - s01 * s01;
        if det.abs() < 1e-10 {
            return; // Singular matrix, skip update
        }

        let s_inv_00 = s11 / det;
        let s_inv_01 = -s01 / det;
        let s_inv_11 = s00 / det;

        // K = P * H' * S^-1 (simplified for H = [I, 0])
        let p = &track.covariance;
        let k00 = p[0] * s_inv_00 + p[1] * s_inv_01;
        let k01 = p[0] * s_inv_01 + p[1] * s_inv_11;
        let k10 = p[1] * s_inv_00 + p[5] * s_inv_01;
        let k11 = p[1] * s_inv_01 + p[5] * s_inv_11;
        let k20 = p[2] * s_inv_00 + p[6] * s_inv_01;
        let k21 = p[2] * s_inv_01 + p[6] * s_inv_11;
        let k30 = p[3] * s_inv_00 + p[7] * s_inv_01;
        let k31 = p[3] * s_inv_01 + p[7] * s_inv_11;

        // State update: x = x + K*y
        track.x += k00 * y_x + k01 * y_y;
        track.y += k10 * y_x + k11 * y_y;
        track.vx += k20 * y_x + k21 * y_y;
        track.vy += k30 * y_x + k31 * y_y;

        // Covariance update: P = (I - K*H)*P
        // Simplified Joseph form for numerical stability
        let i_kh_00 = 1.0 - k00;
        let i_kh_01 = -k01;
        let i_kh_10 = -k10;
        let i_kh_11 = 1.0 - k11;

        let new_p00 = i_kh_00 * p[0] + i_kh_01 * p[1];
        let new_p01 = i_kh_00 * p[1] + i_kh_01 * p[5];
        let new_p02 = i_kh_00 * p[2] + i_kh_01 * p[6];
        let new_p03 = i_kh_00 * p[3] + i_kh_01 * p[7];
        let new_p11 = i_kh_10 * p[1] + i_kh_11 * p[5];
        let new_p12 = i_kh_10 * p[2] + i_kh_11 * p[6];
        let new_p13 = i_kh_10 * p[3] + i_kh_11 * p[7];

        track.covariance = [
            new_p00,
            new_p01,
            new_p02,
            new_p03,
            new_p01,
            new_p11,
            new_p12,
            new_p13,
            new_p02,
            new_p12,
            p[10] - k20 * p[2] - k21 * p[6],
            p[11] - k20 * p[3] - k21 * p[7],
            new_p03,
            new_p13,
            p[11] - k30 * p[2] - k31 * p[6],
            p[15] - k30 * p[3] - k31 * p[7],
        ];
    }

    /// Check for targets that should be marked as lost
    fn check_lost_targets(&mut self, timestamp: u64) -> Vec<ArpaEvent> {
        let mut events = Vec::new();
        let timeout_ms = (self.settings.lost_target_timeout * 1000.0) as u64;

        let lost_ids: Vec<u32> = self
            .tracks
            .iter()
            .filter(|(_, track)| timestamp - track.last_seen > timeout_ms)
            .map(|(id, _)| *id)
            .collect();

        for id in lost_ids {
            if let Some(track) = self.tracks.remove(&id) {
                events.push(ArpaEvent::TargetLost {
                    target_id: id,
                    last_position: TargetPosition {
                        bearing: track.bearing(),
                        distance: track.distance(),
                        latitude: None,
                        longitude: None,
                    },
                });
            }
        }

        events
    }

    /// Get target status based on update count
    fn get_target_status(&self, track: &TrackingState) -> TargetStatus {
        Self::get_status_for_track(track)
    }

    /// Get target status (static version)
    fn get_status_for_track(track: &TrackingState) -> TargetStatus {
        if track.update_count < 3 {
            TargetStatus::Acquiring
        } else {
            TargetStatus::Tracking
        }
    }

    /// Calculate danger metrics for a track
    fn calculate_target_danger(&self, track: &TrackingState) -> TargetDanger {
        Self::calculate_danger_for_track(track, self.own_ship.as_ref())
    }

    /// Calculate danger metrics (static version)
    fn calculate_danger_for_track(
        track: &TrackingState,
        own_ship: Option<&OwnShip>,
    ) -> TargetDanger {
        if let Some(own_ship) = own_ship {
            calculate_danger(track, own_ship)
        } else {
            // Without own ship data, assume stationary
            let result = super::cpa::calculate_cpa_tcpa_stationary(track);
            TargetDanger {
                cpa: result.cpa,
                tcpa: result.tcpa,
            }
        }
    }

    /// Get number of tracked targets
    pub fn target_count(&self) -> usize {
        self.tracks.len()
    }

    /// Clear all tracks
    pub fn clear_all(&mut self) {
        self.tracks.clear();
        self.detector.clear_history();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> ArpaSettings {
        ArpaSettings {
            enabled: true,
            max_targets: 40,
            cpa_threshold: 500.0,
            tcpa_threshold: 600.0,
            lost_target_timeout: 30.0,
            auto_acquisition: false,
            ..Default::default()
        }
    }

    #[test]
    fn test_manual_acquire() {
        let mut processor = ArpaProcessor::new(test_settings());

        let id = processor.acquire_target(45.0, 1000.0, 0);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), 1);

        let targets = processor.get_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].position.bearing, 45.0);
        assert!((targets[0].position.distance - 1000.0).abs() < 1.0);
    }

    #[test]
    fn test_cancel_target() {
        let mut processor = ArpaProcessor::new(test_settings());

        let id = processor.acquire_target(45.0, 1000.0, 0).unwrap();
        assert_eq!(processor.target_count(), 1);

        let cancelled = processor.cancel_target(id);
        assert!(cancelled);
        assert_eq!(processor.target_count(), 0);
    }

    #[test]
    fn test_max_targets() {
        let mut settings = test_settings();
        settings.max_targets = 2;
        let mut processor = ArpaProcessor::new(settings);

        assert!(processor.acquire_target(0.0, 1000.0, 0).is_some());
        assert!(processor.acquire_target(90.0, 1000.0, 0).is_some());
        assert!(processor.acquire_target(180.0, 1000.0, 0).is_none());
    }

    #[test]
    fn test_target_lost() {
        let mut processor = ArpaProcessor::new(test_settings());
        processor.acquire_target(45.0, 1000.0, 0);

        // Process with timestamp beyond timeout
        let timeout_ms = 35_000; // 35 seconds > 30 second timeout
        let events = processor.check_lost_targets(timeout_ms);

        assert_eq!(events.len(), 1);
        match &events[0] {
            ArpaEvent::TargetLost { target_id, .. } => {
                assert_eq!(*target_id, 1);
            }
            _ => panic!("Expected TargetLost event"),
        }

        assert_eq!(processor.target_count(), 0);
    }

    #[test]
    fn test_own_ship_update() {
        let mut processor = ArpaProcessor::new(test_settings());

        let own_ship = OwnShip {
            latitude: 51.5,
            longitude: -0.1,
            heading: 90.0,
            course: 90.0,
            speed: 10.0,
        };

        processor.update_own_ship(own_ship);
        assert!(processor.own_ship().is_some());
        assert_eq!(processor.own_ship().unwrap().speed, 10.0);
    }

    #[test]
    fn test_disabled_processor() {
        let mut settings = test_settings();
        settings.enabled = false;
        let mut processor = ArpaProcessor::new(settings);

        let result = processor.acquire_target(45.0, 1000.0, 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_target_status_transition() {
        let mut processor = ArpaProcessor::new(test_settings());
        processor.acquire_target(45.0, 1000.0, 0);

        let targets = processor.get_targets();
        assert_eq!(targets[0].status, TargetStatus::Acquiring);
        // After 3 updates it would transition to Tracking
    }
}
