//! Guard Zone Implementation
//!
//! Defines guard zone shapes and the zone processor.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Guard zone shape
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ZoneShape {
    /// Arc-shaped zone (sector of an annulus)
    #[serde(rename_all = "camelCase")]
    Arc {
        /// Start bearing in degrees (0-360)
        start_bearing: f64,
        /// End bearing in degrees (0-360)
        end_bearing: f64,
        /// Inner radius in meters
        inner_radius: f64,
        /// Outer radius in meters
        outer_radius: f64,
    },
    /// Full ring zone (360 degrees)
    #[serde(rename_all = "camelCase")]
    Ring {
        /// Inner radius in meters
        inner_radius: f64,
        /// Outer radius in meters
        outer_radius: f64,
    },
}

impl ZoneShape {
    /// Check if a point (bearing, distance) is inside this shape
    pub fn contains(&self, bearing: f64, distance: f64) -> bool {
        match self {
            ZoneShape::Arc {
                start_bearing,
                end_bearing,
                inner_radius,
                outer_radius,
            } => {
                // Check distance first (cheaper)
                if distance < *inner_radius || distance > *outer_radius {
                    return false;
                }

                // Check bearing (handle wrap-around)
                let bearing = normalize_bearing(bearing);
                let start = normalize_bearing(*start_bearing);
                let end = normalize_bearing(*end_bearing);

                if start <= end {
                    // Normal case: start < end
                    bearing >= start && bearing <= end
                } else {
                    // Wrap-around case: zone crosses 0 degrees
                    bearing >= start || bearing <= end
                }
            }
            ZoneShape::Ring {
                inner_radius,
                outer_radius,
            } => distance >= *inner_radius && distance <= *outer_radius,
        }
    }
}

/// Normalize bearing to 0-360 range
fn normalize_bearing(bearing: f64) -> f64 {
    let mut b = bearing % 360.0;
    if b < 0.0 {
        b += 360.0;
    }
    b
}

/// Guard zone definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardZone {
    /// Zone identifier
    pub id: u32,
    /// Whether the zone is active
    pub enabled: bool,
    /// Zone shape
    pub shape: ZoneShape,
    /// Detection threshold (0-255)
    pub sensitivity: u8,
    /// Optional zone name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl GuardZone {
    /// Create a new arc-shaped guard zone
    pub fn new_arc(
        id: u32,
        start_bearing: f64,
        end_bearing: f64,
        inner_radius: f64,
        outer_radius: f64,
    ) -> Self {
        GuardZone {
            id,
            enabled: true,
            shape: ZoneShape::Arc {
                start_bearing,
                end_bearing,
                inner_radius,
                outer_radius,
            },
            sensitivity: 128,
            name: None,
        }
    }

    /// Create a new ring-shaped guard zone
    pub fn new_ring(id: u32, inner_radius: f64, outer_radius: f64) -> Self {
        GuardZone {
            id,
            enabled: true,
            shape: ZoneShape::Ring {
                inner_radius,
                outer_radius,
            },
            sensitivity: 128,
            name: None,
        }
    }
}

/// Guard zone alert event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneAlert {
    /// Zone ID that triggered the alert
    pub zone_id: u32,
    /// Timestamp of the alert (milliseconds)
    pub timestamp: u64,
    /// Bearing where intrusion was detected
    pub bearing: f64,
    /// Distance where intrusion was detected
    pub distance: f64,
    /// Peak intensity of the detection
    pub intensity: u8,
}

/// Alert state for a zone
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZoneAlertState {
    /// No intrusion detected
    Clear,
    /// Intrusion detected
    Alarm,
}

impl Default for ZoneAlertState {
    fn default() -> Self {
        ZoneAlertState::Clear
    }
}

/// Zone state tracking
#[derive(Debug, Clone, Default)]
struct ZoneState {
    /// Current alert state
    alert_state: ZoneAlertState,
    /// Last alert timestamp
    last_alert: Option<u64>,
    /// Consecutive clear scans (for hysteresis)
    clear_count: u32,
}

/// Guard zone processor
#[derive(Debug)]
pub struct GuardZoneProcessor {
    /// Configured zones
    zones: HashMap<u32, GuardZone>,
    /// Zone states
    states: HashMap<u32, ZoneState>,
    /// Current range scale in meters
    range_scale: f64,
    /// Number of clear scans required to clear alarm
    hysteresis_count: u32,
}

impl GuardZoneProcessor {
    /// Create a new guard zone processor
    pub fn new() -> Self {
        GuardZoneProcessor {
            zones: HashMap::new(),
            states: HashMap::new(),
            range_scale: 1852.0,
            hysteresis_count: 3,
        }
    }

    /// Set the current range scale
    pub fn set_range_scale(&mut self, range_meters: f64) {
        self.range_scale = range_meters;
    }

    /// Add or update a guard zone
    pub fn add_zone(&mut self, zone: GuardZone) {
        let id = zone.id;
        self.zones.insert(id, zone);
        self.states.entry(id).or_default();
    }

    /// Remove a guard zone
    pub fn remove_zone(&mut self, zone_id: u32) -> bool {
        self.states.remove(&zone_id);
        self.zones.remove(&zone_id).is_some()
    }

    /// Get a guard zone by ID
    pub fn get_zone(&self, zone_id: u32) -> Option<&GuardZone> {
        self.zones.get(&zone_id)
    }

    /// Get all guard zones
    pub fn get_zones(&self) -> Vec<&GuardZone> {
        self.zones.values().collect()
    }

    /// Enable/disable a zone
    pub fn set_zone_enabled(&mut self, zone_id: u32, enabled: bool) -> bool {
        if let Some(zone) = self.zones.get_mut(&zone_id) {
            zone.enabled = enabled;
            if !enabled {
                // Reset state when disabled
                if let Some(state) = self.states.get_mut(&zone_id) {
                    state.alert_state = ZoneAlertState::Clear;
                    state.last_alert = None;
                    state.clear_count = 0;
                }
            }
            true
        } else {
            false
        }
    }

    /// Get current alert state for a zone
    pub fn get_alert_state(&self, zone_id: u32) -> ZoneAlertState {
        self.states
            .get(&zone_id)
            .map(|s| s.alert_state)
            .unwrap_or_default()
    }

    /// Check a radar spoke for zone intrusions
    ///
    /// # Arguments
    ///
    /// * `spoke_data` - Raw pixel data for the spoke
    /// * `bearing` - Bearing of this spoke in degrees
    /// * `timestamp` - Current timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// Vector of alert events for zones that detected intrusions
    pub fn check_spoke(
        &mut self,
        spoke_data: &[u8],
        bearing: f64,
        timestamp: u64,
    ) -> Vec<ZoneAlert> {
        let mut alerts = Vec::new();
        let samples = spoke_data.len();

        if samples == 0 {
            return alerts;
        }

        // Check each enabled zone
        for (&zone_id, zone) in &self.zones {
            if !zone.enabled {
                continue;
            }

            // Check if this bearing could intersect the zone
            let zone_matches_bearing = match &zone.shape {
                ZoneShape::Arc {
                    start_bearing,
                    end_bearing,
                    ..
                } => {
                    let bearing = normalize_bearing(bearing);
                    let start = normalize_bearing(*start_bearing);
                    let end = normalize_bearing(*end_bearing);
                    if start <= end {
                        bearing >= start && bearing <= end
                    } else {
                        bearing >= start || bearing <= end
                    }
                }
                ZoneShape::Ring { .. } => true,
            };

            if !zone_matches_bearing {
                continue;
            }

            // Scan spoke for intrusions within zone distance range
            let (inner, outer) = match &zone.shape {
                ZoneShape::Arc {
                    inner_radius,
                    outer_radius,
                    ..
                } => (*inner_radius, *outer_radius),
                ZoneShape::Ring {
                    inner_radius,
                    outer_radius,
                } => (*inner_radius, *outer_radius),
            };

            // Convert distance to sample indices
            let inner_idx = ((inner / self.range_scale) * samples as f64) as usize;
            let outer_idx =
                ((outer / self.range_scale) * samples as f64).min(samples as f64) as usize;

            // Find peak intensity in the zone range
            let mut peak_intensity: u8 = 0;
            let mut peak_idx = 0;

            for i in inner_idx..outer_idx.min(samples) {
                if spoke_data[i] > peak_intensity {
                    peak_intensity = spoke_data[i];
                    peak_idx = i;
                }
            }

            // Check against threshold
            let state = self.states.entry(zone_id).or_default();

            if peak_intensity >= zone.sensitivity {
                // Intrusion detected
                let distance = (peak_idx as f64 / samples as f64) * self.range_scale;

                // Only emit alert on state change to Alarm
                if state.alert_state != ZoneAlertState::Alarm {
                    state.alert_state = ZoneAlertState::Alarm;
                    state.last_alert = Some(timestamp);
                    alerts.push(ZoneAlert {
                        zone_id,
                        timestamp,
                        bearing,
                        distance,
                        intensity: peak_intensity,
                    });
                }
                state.clear_count = 0;
            } else {
                // No intrusion on this sweep
                if state.alert_state == ZoneAlertState::Alarm {
                    state.clear_count += 1;
                    if state.clear_count >= self.hysteresis_count {
                        state.alert_state = ZoneAlertState::Clear;
                    }
                }
            }
        }

        alerts
    }

    /// Process end of revolution (for zones that track full scans)
    pub fn end_revolution(&mut self, _timestamp: u64) {
        // Could be used for zones that need full-scan analysis
        // Currently a no-op placeholder
    }

    /// Clear all alert states
    pub fn clear_alerts(&mut self) {
        for state in self.states.values_mut() {
            state.alert_state = ZoneAlertState::Clear;
            state.clear_count = 0;
        }
    }

    /// Get number of zones
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }
}

impl Default for GuardZoneProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard zone status for API response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardZoneStatus {
    /// Zone definition
    pub zone: GuardZone,
    /// Current alert state
    pub state: ZoneAlertState,
}

impl GuardZoneProcessor {
    /// Get zone status for API response
    pub fn get_zone_status(&self, zone_id: u32) -> Option<GuardZoneStatus> {
        self.zones.get(&zone_id).map(|zone| GuardZoneStatus {
            zone: zone.clone(),
            state: self.get_alert_state(zone_id),
        })
    }

    /// Get all zone statuses
    pub fn get_all_zone_status(&self) -> Vec<GuardZoneStatus> {
        self.zones
            .keys()
            .filter_map(|&id| self.get_zone_status(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arc_zone_contains() {
        let shape = ZoneShape::Arc {
            start_bearing: 45.0,
            end_bearing: 135.0,
            inner_radius: 500.0,
            outer_radius: 1000.0,
        };

        // Inside
        assert!(shape.contains(90.0, 750.0));
        assert!(shape.contains(45.0, 500.0));
        assert!(shape.contains(135.0, 1000.0));

        // Outside - bearing
        assert!(!shape.contains(0.0, 750.0));
        assert!(!shape.contains(180.0, 750.0));

        // Outside - distance
        assert!(!shape.contains(90.0, 400.0));
        assert!(!shape.contains(90.0, 1100.0));
    }

    #[test]
    fn test_arc_zone_wrap_around() {
        let shape = ZoneShape::Arc {
            start_bearing: 315.0,
            end_bearing: 45.0,
            inner_radius: 500.0,
            outer_radius: 1000.0,
        };

        // Inside (crosses 0)
        assert!(shape.contains(0.0, 750.0));
        assert!(shape.contains(315.0, 750.0));
        assert!(shape.contains(45.0, 750.0));
        assert!(shape.contains(350.0, 750.0));
        assert!(shape.contains(10.0, 750.0));

        // Outside
        assert!(!shape.contains(90.0, 750.0));
        assert!(!shape.contains(180.0, 750.0));
        assert!(!shape.contains(270.0, 750.0));
    }

    #[test]
    fn test_ring_zone_contains() {
        let shape = ZoneShape::Ring {
            inner_radius: 500.0,
            outer_radius: 1000.0,
        };

        // Inside - any bearing
        assert!(shape.contains(0.0, 750.0));
        assert!(shape.contains(90.0, 750.0));
        assert!(shape.contains(180.0, 750.0));
        assert!(shape.contains(270.0, 750.0));

        // Outside - distance
        assert!(!shape.contains(0.0, 400.0));
        assert!(!shape.contains(0.0, 1100.0));
    }

    #[test]
    fn test_add_remove_zone() {
        let mut processor = GuardZoneProcessor::new();

        let zone = GuardZone::new_arc(1, 0.0, 90.0, 500.0, 1000.0);
        processor.add_zone(zone);

        assert_eq!(processor.zone_count(), 1);
        assert!(processor.get_zone(1).is_some());

        assert!(processor.remove_zone(1));
        assert_eq!(processor.zone_count(), 0);
        assert!(processor.get_zone(1).is_none());
    }

    #[test]
    fn test_zone_alert() {
        let mut processor = GuardZoneProcessor::new();
        processor.set_range_scale(1852.0); // 1nm

        let zone = GuardZone::new_arc(1, 40.0, 50.0, 450.0, 950.0);
        processor.add_zone(zone);

        // Create spoke with target in zone
        let mut spoke = vec![0u8; 512];
        // Target at ~700m (sample ~194 for 1852m range)
        spoke[194] = 200;
        spoke[195] = 220;
        spoke[196] = 180;

        let alerts = processor.check_spoke(&spoke, 45.0, 1000);

        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].zone_id, 1);
        assert!(alerts[0].distance > 450.0 && alerts[0].distance < 950.0);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Alarm);
    }

    #[test]
    fn test_zone_hysteresis() {
        let mut processor = GuardZoneProcessor::new();
        processor.set_range_scale(1852.0);

        let zone = GuardZone::new_ring(1, 400.0, 1000.0);
        processor.add_zone(zone);

        // Trigger alarm
        let mut spoke = vec![0u8; 512];
        spoke[200] = 200;
        processor.check_spoke(&spoke, 45.0, 1000);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Alarm);

        // Clear spoke - should not clear immediately (hysteresis)
        let clear_spoke = vec![0u8; 512];
        processor.check_spoke(&clear_spoke, 45.0, 2000);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Alarm);

        processor.check_spoke(&clear_spoke, 45.0, 3000);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Alarm);

        // Third clear scan should clear the alarm
        processor.check_spoke(&clear_spoke, 45.0, 4000);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Clear);
    }

    #[test]
    fn test_zone_disabled() {
        let mut processor = GuardZoneProcessor::new();
        processor.set_range_scale(1852.0);

        let mut zone = GuardZone::new_ring(1, 400.0, 1000.0);
        zone.enabled = false;
        processor.add_zone(zone);

        let mut spoke = vec![0u8; 512];
        spoke[200] = 200;

        let alerts = processor.check_spoke(&spoke, 45.0, 1000);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_zone_below_threshold() {
        let mut processor = GuardZoneProcessor::new();
        processor.set_range_scale(1852.0);

        let mut zone = GuardZone::new_ring(1, 400.0, 1000.0);
        zone.sensitivity = 150; // High threshold
        processor.add_zone(zone);

        let mut spoke = vec![0u8; 512];
        spoke[200] = 100; // Below threshold

        let alerts = processor.check_spoke(&spoke, 45.0, 1000);
        assert!(alerts.is_empty());
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Clear);
    }

    #[test]
    fn test_multiple_zones() {
        let mut processor = GuardZoneProcessor::new();
        processor.set_range_scale(1852.0);

        processor.add_zone(GuardZone::new_arc(1, 0.0, 90.0, 400.0, 600.0));
        processor.add_zone(GuardZone::new_arc(2, 0.0, 90.0, 800.0, 1000.0));

        let mut spoke = vec![0u8; 512];
        // Target in zone 1
        spoke[140] = 200; // ~500m
                          // Target in zone 2
        spoke[240] = 180; // ~900m

        let alerts = processor.check_spoke(&spoke, 45.0, 1000);

        assert_eq!(alerts.len(), 2);
        assert_eq!(processor.get_alert_state(1), ZoneAlertState::Alarm);
        assert_eq!(processor.get_alert_state(2), ZoneAlertState::Alarm);
    }
}
