//! Dual-Range Display Support
//!
//! Dual-range mode allows the radar to display two different range scales
//! simultaneously, typically one for long-range situational awareness and
//! one zoomed-in view for nearby navigation.
//!
//! ## Supported Radars
//!
//! Dual-range is supported on:
//! - Furuno DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, DRS25A-NXT
//! - Navico HALO 20+, HALO 24, HALO 3, HALO 6
//! - Some Garmin xHD3/xHD2 models
//!
//! ## Usage
//!
//! ```rust,ignore
//! use mayara_core::dual_range::{DualRangeConfig, DualRangeState};
//!
//! // Enable dual-range with a secondary range of 1nm (1852m)
//! let config = DualRangeConfig {
//!     enabled: true,
//!     secondary_range: 1852,
//! };
//! ```

use serde::{Deserialize, Serialize};

/// Dual-range display configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DualRangeConfig {
    /// Whether dual-range mode is enabled
    pub enabled: bool,
    /// Secondary range in meters
    pub secondary_range: u32,
}

/// Dual-range state with current values
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DualRangeState {
    /// Whether dual-range mode is currently active
    pub enabled: bool,
    /// Primary (main) range in meters
    pub primary_range: u32,
    /// Secondary range in meters
    pub secondary_range: u32,
    /// Maximum allowed secondary range in meters (hardware limit)
    /// In dual-range mode, the secondary range is typically limited
    /// For Furuno NXT: 22224m (12nm max)
    pub max_secondary_range: u32,
}

impl Default for DualRangeState {
    fn default() -> Self {
        Self {
            enabled: false,
            primary_range: 1852,        // 1nm default
            secondary_range: 926,       // 0.5nm default
            max_secondary_range: 22224, // 12nm default limit
        }
    }
}

/// Dual-range controller manages the state and validates configurations
pub struct DualRangeController {
    state: DualRangeState,
    /// Available secondary ranges (subset of primary ranges)
    available_ranges: Vec<u32>,
}

impl DualRangeController {
    /// Create a new dual-range controller
    ///
    /// # Arguments
    /// * `max_secondary_range` - Maximum allowed secondary range in meters
    /// * `available_ranges` - List of available range values in meters
    pub fn new(max_secondary_range: u32, available_ranges: Vec<u32>) -> Self {
        // Filter available ranges to only include those <= max_secondary_range
        let secondary_ranges: Vec<u32> = available_ranges
            .iter()
            .filter(|&&r| r <= max_secondary_range)
            .copied()
            .collect();

        Self {
            state: DualRangeState {
                max_secondary_range,
                ..Default::default()
            },
            available_ranges: secondary_ranges,
        }
    }

    /// Get current dual-range state
    pub fn state(&self) -> &DualRangeState {
        &self.state
    }

    /// Get available secondary range values
    pub fn available_ranges(&self) -> &[u32] {
        &self.available_ranges
    }

    /// Enable or disable dual-range mode
    pub fn set_enabled(&mut self, enabled: bool) {
        self.state.enabled = enabled;
    }

    /// Set the primary range
    pub fn set_primary_range(&mut self, range: u32) {
        self.state.primary_range = range;
    }

    /// Set the secondary range
    ///
    /// Returns true if the range was accepted, false if it exceeds the limit
    pub fn set_secondary_range(&mut self, range: u32) -> bool {
        if range > self.state.max_secondary_range {
            return false;
        }
        self.state.secondary_range = range;
        true
    }

    /// Apply a configuration update
    ///
    /// Returns true if all values were accepted
    pub fn apply_config(&mut self, config: &DualRangeConfig) -> bool {
        self.state.enabled = config.enabled;
        if config.secondary_range > 0 {
            if config.secondary_range > self.state.max_secondary_range {
                return false;
            }
            self.state.secondary_range = config.secondary_range;
        }
        true
    }

    /// Find the closest valid secondary range to the requested value
    pub fn find_closest_range(&self, target: u32) -> u32 {
        self.available_ranges
            .iter()
            .min_by_key(|&&r| (r as i64 - target as i64).abs())
            .copied()
            .unwrap_or(self.state.secondary_range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dual_range_controller() {
        // Furuno NXT ranges up to 12nm (22224m) for secondary
        let ranges = vec![231, 463, 926, 1852, 3704, 7408, 14816, 22224, 44448, 88896];
        let mut controller = DualRangeController::new(22224, ranges);

        assert!(!controller.state().enabled);
        assert_eq!(controller.available_ranges().len(), 8); // Only <= 22224m

        // Enable dual-range
        controller.set_enabled(true);
        assert!(controller.state().enabled);

        // Set valid secondary range
        assert!(controller.set_secondary_range(7408));
        assert_eq!(controller.state().secondary_range, 7408);

        // Reject range that exceeds limit
        assert!(!controller.set_secondary_range(44448));
        assert_eq!(controller.state().secondary_range, 7408); // Unchanged
    }

    #[test]
    fn test_find_closest_range() {
        let ranges = vec![231, 463, 926, 1852, 3704, 7408];
        let controller = DualRangeController::new(22224, ranges);

        // Exact match
        assert_eq!(controller.find_closest_range(1852), 1852);

        // Find closest
        assert_eq!(controller.find_closest_range(1000), 926);
        assert_eq!(controller.find_closest_range(5000), 3704);
    }

    #[test]
    fn test_config_serialization() {
        let config = DualRangeConfig {
            enabled: true,
            secondary_range: 1852,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"secondaryRange\":1852"));

        let parsed: DualRangeConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.secondary_range, 1852);
    }
}
