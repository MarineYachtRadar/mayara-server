//! Trail History Storage
//!
//! Efficient storage for target position history using circular buffers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single point in a target's trail
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrailPoint {
    /// Unix timestamp in milliseconds
    pub timestamp: u64,
    /// Bearing in degrees (0-360)
    pub bearing: f64,
    /// Distance in meters
    pub distance: f64,
    /// Latitude (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// Trail motion mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrailMode {
    /// Trail shows target motion relative to own ship
    Relative,
    /// Trail shows true geographic motion
    True,
}

impl Default for TrailMode {
    fn default() -> Self {
        TrailMode::Relative
    }
}

/// Trail display settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrailSettings {
    /// Whether trails are enabled
    pub enabled: bool,
    /// Trail mode (relative or true motion)
    pub mode: TrailMode,
    /// Maximum trail duration in seconds
    pub duration_seconds: u32,
    /// Maximum points per trail (for memory management)
    pub max_points: usize,
    /// Minimum interval between points in milliseconds
    pub min_interval_ms: u64,
}

impl Default for TrailSettings {
    fn default() -> Self {
        TrailSettings {
            enabled: true,
            mode: TrailMode::Relative,
            duration_seconds: 300, // 5 minutes
            max_points: 100,
            min_interval_ms: 3000, // 3 seconds
        }
    }
}

/// Trail for a single target (circular buffer)
#[derive(Debug, Clone)]
struct TargetTrail {
    /// Points in the trail (newest at end)
    points: Vec<TrailPoint>,
    /// Maximum capacity
    max_points: usize,
}

impl TargetTrail {
    fn new(max_points: usize) -> Self {
        TargetTrail {
            points: Vec::with_capacity(max_points),
            max_points,
        }
    }

    fn add_point(&mut self, point: TrailPoint) {
        if self.points.len() >= self.max_points {
            self.points.remove(0);
        }
        self.points.push(point);
    }

    fn get_points(&self) -> &[TrailPoint] {
        &self.points
    }

    fn clear(&mut self) {
        self.points.clear();
    }

    fn prune_old(&mut self, min_timestamp: u64) {
        self.points.retain(|p| p.timestamp >= min_timestamp);
    }
}

/// Trail storage for all targets
#[derive(Debug)]
pub struct TrailStore {
    /// Settings
    settings: TrailSettings,
    /// Trails indexed by target ID
    trails: HashMap<u32, TargetTrail>,
    /// Last update timestamp per target (for rate limiting)
    last_update: HashMap<u32, u64>,
}

impl TrailStore {
    /// Create a new trail store
    pub fn new(settings: TrailSettings) -> Self {
        TrailStore {
            settings,
            trails: HashMap::new(),
            last_update: HashMap::new(),
        }
    }

    /// Update settings
    pub fn update_settings(&mut self, settings: TrailSettings) {
        // If max_points changed, update existing trails
        if settings.max_points != self.settings.max_points {
            for trail in self.trails.values_mut() {
                trail.max_points = settings.max_points;
                // Truncate if needed
                while trail.points.len() > settings.max_points {
                    trail.points.remove(0);
                }
            }
        }
        self.settings = settings;
    }

    /// Get current settings
    pub fn settings(&self) -> &TrailSettings {
        &self.settings
    }

    /// Add a trail point for a target
    ///
    /// Returns true if the point was added, false if rate-limited
    pub fn add_point(&mut self, target_id: u32, point: TrailPoint) -> bool {
        if !self.settings.enabled {
            return false;
        }

        // Rate limiting
        if let Some(&last) = self.last_update.get(&target_id) {
            if point.timestamp - last < self.settings.min_interval_ms {
                return false;
            }
        }

        // Get or create trail
        let trail = self
            .trails
            .entry(target_id)
            .or_insert_with(|| TargetTrail::new(self.settings.max_points));

        trail.add_point(point);
        self.last_update.insert(target_id, point.timestamp);
        true
    }

    /// Get trail points for a target
    pub fn get_trail(&self, target_id: u32) -> Vec<TrailPoint> {
        self.trails
            .get(&target_id)
            .map(|t| t.get_points().to_vec())
            .unwrap_or_default()
    }

    /// Get all trails
    pub fn get_all_trails(&self) -> HashMap<u32, Vec<TrailPoint>> {
        self.trails
            .iter()
            .map(|(id, trail)| (*id, trail.get_points().to_vec()))
            .collect()
    }

    /// Clear trail for a specific target
    pub fn clear_trail(&mut self, target_id: u32) {
        if let Some(trail) = self.trails.get_mut(&target_id) {
            trail.clear();
        }
        self.last_update.remove(&target_id);
    }

    /// Remove trail for a target (when target is lost)
    pub fn remove_trail(&mut self, target_id: u32) {
        self.trails.remove(&target_id);
        self.last_update.remove(&target_id);
    }

    /// Clear all trails
    pub fn clear_all(&mut self) {
        self.trails.clear();
        self.last_update.clear();
    }

    /// Prune old points based on duration setting
    pub fn prune_old_points(&mut self, current_timestamp: u64) {
        let min_timestamp =
            current_timestamp.saturating_sub((self.settings.duration_seconds as u64) * 1000);

        for trail in self.trails.values_mut() {
            trail.prune_old(min_timestamp);
        }

        // Remove empty trails
        self.trails.retain(|_, trail| !trail.points.is_empty());
    }

    /// Get number of tracked trails
    pub fn trail_count(&self) -> usize {
        self.trails.len()
    }

    /// Get total number of points across all trails
    pub fn total_points(&self) -> usize {
        self.trails.values().map(|t| t.points.len()).sum()
    }
}

/// Trail data for serialization (API response)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrailData {
    /// Target ID
    pub target_id: u32,
    /// Trail points (oldest first)
    pub points: Vec<TrailPoint>,
}

impl TrailStore {
    /// Get trail data for API response
    pub fn get_trail_data(&self, target_id: u32) -> Option<TrailData> {
        self.trails.get(&target_id).map(|trail| TrailData {
            target_id,
            points: trail.get_points().to_vec(),
        })
    }

    /// Get all trails for API response
    pub fn get_all_trail_data(&self) -> Vec<TrailData> {
        self.trails
            .iter()
            .map(|(id, trail)| TrailData {
                target_id: *id,
                points: trail.get_points().to_vec(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> TrailSettings {
        TrailSettings {
            enabled: true,
            mode: TrailMode::Relative,
            duration_seconds: 60,
            max_points: 10,
            min_interval_ms: 1000,
        }
    }

    fn make_point(timestamp: u64, bearing: f64, distance: f64) -> TrailPoint {
        TrailPoint {
            timestamp,
            bearing,
            distance,
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn test_add_point() {
        let mut store = TrailStore::new(test_settings());

        let added = store.add_point(1, make_point(1000, 45.0, 1000.0));
        assert!(added);

        let trail = store.get_trail(1);
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].bearing, 45.0);
    }

    #[test]
    fn test_rate_limiting() {
        let mut store = TrailStore::new(test_settings());

        // First point
        assert!(store.add_point(1, make_point(1000, 45.0, 1000.0)));

        // Too soon (only 500ms later)
        assert!(!store.add_point(1, make_point(1500, 46.0, 1010.0)));

        // After interval (1100ms later)
        assert!(store.add_point(1, make_point(2100, 47.0, 1020.0)));

        let trail = store.get_trail(1);
        assert_eq!(trail.len(), 2);
    }

    #[test]
    fn test_max_points() {
        let mut settings = test_settings();
        settings.max_points = 3;
        settings.min_interval_ms = 0; // Disable rate limiting
        let mut store = TrailStore::new(settings);

        for i in 0..5 {
            store.add_point(1, make_point(i * 1000, i as f64 * 10.0, 1000.0));
        }

        let trail = store.get_trail(1);
        assert_eq!(trail.len(), 3);
        // Should have the last 3 points
        assert_eq!(trail[0].bearing, 20.0);
        assert_eq!(trail[1].bearing, 30.0);
        assert_eq!(trail[2].bearing, 40.0);
    }

    #[test]
    fn test_prune_old() {
        let mut settings = test_settings();
        settings.duration_seconds = 30;
        settings.min_interval_ms = 0;
        let mut store = TrailStore::new(settings);

        // Add points spanning 50 seconds
        store.add_point(1, make_point(0, 10.0, 1000.0));
        store.add_point(1, make_point(20_000, 20.0, 1000.0));
        store.add_point(1, make_point(40_000, 30.0, 1000.0));
        store.add_point(1, make_point(50_000, 40.0, 1000.0));

        // Prune at t=50s with 30s duration (keep >= 20s)
        store.prune_old_points(50_000);

        let trail = store.get_trail(1);
        assert_eq!(trail.len(), 3); // Points at 20s, 40s, 50s
        assert_eq!(trail[0].bearing, 20.0);
    }

    #[test]
    fn test_clear_trail() {
        let mut store = TrailStore::new(test_settings());

        store.add_point(1, make_point(1000, 45.0, 1000.0));
        store.add_point(2, make_point(1000, 90.0, 2000.0));

        store.clear_trail(1);

        assert!(store.get_trail(1).is_empty());
        assert_eq!(store.get_trail(2).len(), 1);
    }

    #[test]
    fn test_remove_trail() {
        let mut store = TrailStore::new(test_settings());

        store.add_point(1, make_point(1000, 45.0, 1000.0));
        store.remove_trail(1);

        assert_eq!(store.trail_count(), 0);
    }

    #[test]
    fn test_disabled() {
        let mut settings = test_settings();
        settings.enabled = false;
        let mut store = TrailStore::new(settings);

        let added = store.add_point(1, make_point(1000, 45.0, 1000.0));
        assert!(!added);
        assert!(store.get_trail(1).is_empty());
    }

    #[test]
    fn test_multiple_targets() {
        let mut settings = test_settings();
        settings.min_interval_ms = 0;
        let mut store = TrailStore::new(settings);

        store.add_point(1, make_point(1000, 45.0, 1000.0));
        store.add_point(1, make_point(2000, 46.0, 1010.0));
        store.add_point(2, make_point(1000, 90.0, 2000.0));

        assert_eq!(store.trail_count(), 2);
        assert_eq!(store.total_points(), 3);
        assert_eq!(store.get_trail(1).len(), 2);
        assert_eq!(store.get_trail(2).len(), 1);
    }

    #[test]
    fn test_get_all_trails() {
        let mut settings = test_settings();
        settings.min_interval_ms = 0;
        let mut store = TrailStore::new(settings);

        store.add_point(1, make_point(1000, 45.0, 1000.0));
        store.add_point(2, make_point(1000, 90.0, 2000.0));

        let all_trails = store.get_all_trails();
        assert_eq!(all_trails.len(), 2);
        assert!(all_trails.contains_key(&1));
        assert!(all_trails.contains_key(&2));
    }
}
