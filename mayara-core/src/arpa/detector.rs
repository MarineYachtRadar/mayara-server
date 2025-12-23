//! Target Detection
//!
//! Detects potential targets from radar spoke data for automatic acquisition.

use super::types::ArpaSettings;

/// A detected target candidate from radar data
#[derive(Debug, Clone)]
pub struct DetectedTarget {
    /// Bearing in degrees (0-360)
    pub bearing: f64,
    /// Distance in meters
    pub distance: f64,
    /// Peak intensity (0-255)
    pub intensity: u8,
    /// Size in pixels (radial extent)
    pub size: u32,
}

/// Target detector for automatic ARPA acquisition
#[derive(Debug)]
pub struct TargetDetector {
    /// Detection settings
    settings: ArpaSettings,
    /// Range scale in meters (max range of current spoke data)
    range_scale: f64,
    /// Recent detections for correlation
    recent_detections: Vec<(u64, Vec<DetectedTarget>)>,
    /// How many scans to correlate
    correlation_scans: usize,
}

impl TargetDetector {
    /// Create a new target detector
    pub fn new(settings: ArpaSettings) -> Self {
        TargetDetector {
            settings,
            range_scale: 1852.0, // Default 1nm
            recent_detections: Vec::new(),
            correlation_scans: 3,
        }
    }

    /// Update detection settings
    pub fn update_settings(&mut self, settings: ArpaSettings) {
        self.settings = settings;
    }

    /// Set the current range scale
    pub fn set_range_scale(&mut self, range_meters: f64) {
        self.range_scale = range_meters;
    }

    /// Detect targets in a single spoke
    ///
    /// # Arguments
    ///
    /// * `spoke_data` - Raw pixel data for the spoke
    /// * `bearing` - Bearing of this spoke in degrees
    /// * `timestamp` - Current timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// Vector of detected target candidates in this spoke
    pub fn detect_in_spoke(
        &mut self,
        spoke_data: &[u8],
        bearing: f64,
        _timestamp: u64,
    ) -> Vec<DetectedTarget> {
        if !self.settings.auto_acquisition {
            return Vec::new();
        }

        let threshold = self.settings.detection_threshold;
        let min_size = self.settings.min_target_size as usize;
        let samples = spoke_data.len();

        if samples == 0 {
            return Vec::new();
        }

        let mut detections = Vec::new();
        let mut in_target = false;
        let mut target_start = 0;
        let mut peak_intensity: u8 = 0;
        let mut peak_index = 0;

        for (i, &pixel) in spoke_data.iter().enumerate() {
            if pixel >= threshold {
                if !in_target {
                    // Start of new target
                    in_target = true;
                    target_start = i;
                    peak_intensity = pixel;
                    peak_index = i;
                } else if pixel > peak_intensity {
                    // Update peak
                    peak_intensity = pixel;
                    peak_index = i;
                }
            } else if in_target {
                // End of target
                let size = i - target_start;
                if size >= min_size {
                    // Calculate distance from sample index
                    let distance = (peak_index as f64 / samples as f64) * self.range_scale;

                    detections.push(DetectedTarget {
                        bearing,
                        distance,
                        intensity: peak_intensity,
                        size: size as u32,
                    });
                }
                in_target = false;
            }
        }

        // Handle target at end of spoke
        if in_target {
            let size = samples - target_start;
            if size >= min_size {
                let distance = (peak_index as f64 / samples as f64) * self.range_scale;
                detections.push(DetectedTarget {
                    bearing,
                    distance,
                    intensity: peak_intensity,
                    size: size as u32,
                });
            }
        }

        detections
    }

    /// Process a complete radar revolution and correlate detections
    ///
    /// # Arguments
    ///
    /// * `detections` - All detections from this revolution
    /// * `timestamp` - Timestamp of this revolution
    ///
    /// # Returns
    ///
    /// Correlated targets that appear consistently across multiple scans
    pub fn correlate_revolution(
        &mut self,
        detections: Vec<DetectedTarget>,
        timestamp: u64,
    ) -> Vec<DetectedTarget> {
        // Store this revolution's detections
        self.recent_detections.push((timestamp, detections));

        // Keep only recent scans
        while self.recent_detections.len() > self.correlation_scans {
            self.recent_detections.remove(0);
        }

        // Need at least 2 scans to correlate
        if self.recent_detections.len() < 2 {
            return Vec::new();
        }

        // Get latest detections
        let (_, latest) = self.recent_detections.last().unwrap();
        let mut correlated = Vec::new();

        // For each detection in latest scan, check if similar detection exists in previous scans
        for det in latest {
            let mut match_count = 0;
            for (_, prev_dets) in self.recent_detections.iter().rev().skip(1) {
                if Self::has_matching_detection(det, prev_dets) {
                    match_count += 1;
                }
            }

            // Require match in at least half of previous scans
            let required_matches = (self.correlation_scans - 1) / 2;
            if match_count >= required_matches {
                correlated.push(det.clone());
            }
        }

        correlated
    }

    /// Check if a detection matches any in a list (within tolerance)
    fn has_matching_detection(target: &DetectedTarget, candidates: &[DetectedTarget]) -> bool {
        const BEARING_TOLERANCE: f64 = 5.0; // degrees
        const DISTANCE_TOLERANCE: f64 = 0.1; // 10% of distance

        for candidate in candidates {
            let bearing_diff = (target.bearing - candidate.bearing).abs();
            let bearing_diff = if bearing_diff > 180.0 {
                360.0 - bearing_diff
            } else {
                bearing_diff
            };

            let distance_diff = (target.distance - candidate.distance).abs();
            let distance_tolerance = target.distance * DISTANCE_TOLERANCE;

            if bearing_diff <= BEARING_TOLERANCE && distance_diff <= distance_tolerance {
                return true;
            }
        }
        false
    }

    /// Clear detection history (e.g., on range change)
    pub fn clear_history(&mut self) {
        self.recent_detections.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> ArpaSettings {
        ArpaSettings {
            enabled: true,
            auto_acquisition: true,
            detection_threshold: 128,
            min_target_size: 3,
            ..Default::default()
        }
    }

    #[test]
    fn test_detect_single_target() {
        let mut detector = TargetDetector::new(test_settings());
        detector.set_range_scale(1852.0); // 1nm

        // Create spoke with a target blob
        let mut spoke = vec![0u8; 512];
        // Target at sample 256 (0.5nm = 926m)
        for i in 254..260 {
            spoke[i] = 200;
        }

        let detections = detector.detect_in_spoke(&spoke, 45.0, 0);

        assert_eq!(detections.len(), 1);
        let det = &detections[0];
        assert_eq!(det.bearing, 45.0);
        assert!((det.distance - 926.0).abs() < 50.0); // ~926m at 0.5nm
        assert!(det.intensity >= 200);
        assert!(det.size >= 3);
    }

    #[test]
    fn test_detect_multiple_targets() {
        let mut detector = TargetDetector::new(test_settings());
        detector.set_range_scale(1852.0);

        let mut spoke = vec![0u8; 512];
        // Target 1 at 0.25nm
        for i in 126..132 {
            spoke[i] = 180;
        }
        // Target 2 at 0.75nm
        for i in 382..390 {
            spoke[i] = 220;
        }

        let detections = detector.detect_in_spoke(&spoke, 90.0, 0);

        assert_eq!(detections.len(), 2);
    }

    #[test]
    fn test_threshold_filtering() {
        let mut detector = TargetDetector::new(test_settings());

        let mut spoke = vec![0u8; 512];
        // Weak return below threshold
        for i in 250..260 {
            spoke[i] = 100; // Below 128 threshold
        }

        let detections = detector.detect_in_spoke(&spoke, 0.0, 0);
        assert!(detections.is_empty());
    }

    #[test]
    fn test_size_filtering() {
        let mut detector = TargetDetector::new(test_settings());

        let mut spoke = vec![0u8; 512];
        // Very small return (< min_target_size)
        spoke[256] = 200;
        spoke[257] = 200;
        // Only 2 pixels, min is 3

        let detections = detector.detect_in_spoke(&spoke, 0.0, 0);
        assert!(detections.is_empty());
    }

    #[test]
    fn test_auto_acquisition_disabled() {
        let mut settings = test_settings();
        settings.auto_acquisition = false;
        let mut detector = TargetDetector::new(settings);

        let mut spoke = vec![0u8; 512];
        for i in 250..260 {
            spoke[i] = 200;
        }

        let detections = detector.detect_in_spoke(&spoke, 0.0, 0);
        assert!(detections.is_empty());
    }
}
