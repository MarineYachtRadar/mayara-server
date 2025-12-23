//! RadarEngine - Unified radar control and feature management
//!
//! This module provides a single entry point for radar operations,
//! eliminating duplication between server and WASM implementations.
//!
//! # Architecture
//!
//! The `RadarEngine` manages all radar instances and their associated feature processors:
//! - ARPA (Automatic Radar Plotting Aid) target tracking
//! - Guard Zones for collision detection
//! - Trails for target history visualization
//! - Dual-Range for secondary display
//!
//! Both `mayara-server` and `mayara-signalk-wasm` use this engine as the single
//! source of truth for radar control logic.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │           RadarEngine                                        │
//! │  ┌────────────────────────────────────────────────────────┐  │
//! │  │  ManagedRadar (per radar)                              │  │
//! │  │  ├─ RadarController (brand-specific)                   │  │
//! │  │  ├─ ArpaProcessor                                      │  │
//! │  │  ├─ GuardZoneProcessor                                 │  │
//! │  │  ├─ TrailStore                                         │  │
//! │  │  └─ DualRangeController (optional)                     │  │
//! │  └────────────────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;

use crate::arpa::{ArpaProcessor, ArpaSettings, ArpaTarget};
use crate::controllers::{
    FurunoController, GarminController, NavicoController, NavicoModel, RaymarineController,
    RaymarineVariant,
};
use crate::dual_range::{DualRangeConfig, DualRangeController, DualRangeState};
use crate::guard_zones::{GuardZone, GuardZoneProcessor, GuardZoneStatus};
use crate::io::IoProvider;
use crate::models::{self, ModelInfo};
use crate::state::RadarState;
use crate::trails::{TrailData, TrailSettings, TrailStore};
use crate::Brand;

/// Unified controller enum for all radar brands.
///
/// This allows treating all radar controllers uniformly while preserving
/// brand-specific behavior through delegation.
pub enum RadarController {
    Furuno(FurunoController),
    Navico(NavicoController),
    Raymarine(RaymarineController),
    Garmin(GarminController),
}

impl RadarController {
    /// Get the brand of this controller
    pub fn brand(&self) -> Brand {
        match self {
            RadarController::Furuno(_) => Brand::Furuno,
            RadarController::Navico(_) => Brand::Navico,
            RadarController::Raymarine(_) => Brand::Raymarine,
            RadarController::Garmin(_) => Brand::Garmin,
        }
    }

    /// Check if controller is connected
    pub fn is_connected(&self) -> bool {
        match self {
            RadarController::Furuno(c) => c.is_connected(),
            RadarController::Navico(c) => c.is_connected(),
            RadarController::Raymarine(c) => c.is_connected(),
            RadarController::Garmin(c) => c.is_connected(),
        }
    }

    /// Get the radar state (Furuno only - others need different approach)
    /// Returns None for brands that don't expose RadarState
    pub fn radar_state(&self) -> Option<&RadarState> {
        match self {
            RadarController::Furuno(c) => Some(c.radar_state()),
            // Other controllers don't have radar_state() yet
            RadarController::Navico(_) => None,
            RadarController::Raymarine(_) => None,
            RadarController::Garmin(_) => None,
        }
    }

    /// Set power/transmit state
    pub fn set_power<I: IoProvider>(&mut self, io: &mut I, transmit: bool) {
        match self {
            RadarController::Furuno(c) => c.set_transmit(io, transmit),
            RadarController::Navico(c) => c.set_power(io, transmit),
            RadarController::Raymarine(c) => c.set_power(io, transmit),
            RadarController::Garmin(c) => c.set_power(io, transmit),
        }
    }

    /// Set range in meters
    pub fn set_range<I: IoProvider>(&mut self, io: &mut I, range_meters: u32) {
        match self {
            RadarController::Furuno(c) => c.set_range(io, range_meters),
            // Navico uses decimeters
            RadarController::Navico(c) => c.set_range(io, (range_meters * 10) as i32),
            // Raymarine uses range index - caller should convert meters to index
            // TODO: Need range table lookup for proper conversion
            RadarController::Raymarine(_) => {}
            RadarController::Garmin(c) => c.set_range(io, range_meters),
        }
    }

    /// Set gain (0-100)
    pub fn set_gain<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        match self {
            RadarController::Furuno(c) => c.set_gain(io, value, auto),
            RadarController::Navico(c) => c.set_gain(io, value as u8, auto),
            RadarController::Raymarine(c) => c.set_gain(io, value as u8, auto),
            RadarController::Garmin(c) => c.set_gain(io, value as u32, auto),
        }
    }

    /// Set sea clutter (0-100)
    pub fn set_sea<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        match self {
            RadarController::Furuno(c) => c.set_sea(io, value, auto),
            RadarController::Navico(c) => c.set_sea(io, value as u8, auto),
            RadarController::Raymarine(c) => c.set_sea(io, value as u8, auto),
            RadarController::Garmin(c) => c.set_sea(io, value as u32, auto),
        }
    }

    /// Set rain clutter (0-100)
    pub fn set_rain<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        match self {
            RadarController::Furuno(c) => c.set_rain(io, value, auto),
            // Navico rain doesn't have auto mode
            RadarController::Navico(c) => c.set_rain(io, value as u8),
            // Raymarine rain uses 'enabled' instead of 'auto'
            RadarController::Raymarine(c) => c.set_rain(io, value as u8, !auto),
            RadarController::Garmin(c) => c.set_rain(io, value as u32, auto),
        }
    }

    /// Set bearing alignment in degrees
    pub fn set_bearing_alignment<I: IoProvider>(&mut self, io: &mut I, degrees: f64) {
        match self {
            RadarController::Furuno(c) => c.set_bearing_alignment(io, degrees),
            // Navico uses deci-degrees
            RadarController::Navico(c) => c.set_bearing_alignment(io, (degrees * 10.0) as i16),
            RadarController::Raymarine(c) => c.set_bearing_alignment(io, degrees as f32),
            RadarController::Garmin(c) => c.set_bearing_alignment(io, degrees as f32),
        }
    }

    /// Set interference rejection (level 0-3 or boolean)
    pub fn set_interference_rejection<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        match self {
            RadarController::Furuno(c) => c.set_interference_rejection(io, level > 0),
            RadarController::Navico(c) => c.set_interference_rejection(io, level),
            RadarController::Raymarine(c) => c.set_interference_rejection(io, level),
            // Garmin doesn't have this control
            RadarController::Garmin(_) => {}
        }
    }
}

/// A managed radar instance with its controller and all feature processors.
pub struct ManagedRadar {
    /// The radar ID
    pub id: String,
    /// Brand-specific controller
    pub controller: RadarController,
    /// ARPA target tracking processor
    pub arpa: ArpaProcessor,
    /// Guard zone collision detection
    pub guard_zones: GuardZoneProcessor,
    /// Target trail history
    pub trails: TrailStore,
    /// Dual-range controller (if supported by model)
    pub dual_range: Option<DualRangeController>,
    /// Model information (once detected)
    pub model_info: Option<ModelInfo>,
}

impl ManagedRadar {
    /// Create a new managed radar
    pub fn new(id: String, controller: RadarController) -> Self {
        Self {
            id,
            controller,
            arpa: ArpaProcessor::new(ArpaSettings::default()),
            guard_zones: GuardZoneProcessor::new(),
            trails: TrailStore::new(TrailSettings::default()),
            dual_range: None,
            model_info: None,
        }
    }

    /// Set the model info and initialize dual-range if supported
    pub fn set_model_info(&mut self, model_info: ModelInfo) {
        if model_info.has_dual_range {
            self.dual_range = Some(DualRangeController::new(
                model_info.max_dual_range,
                model_info.range_table.to_vec(),
            ));
        }
        self.model_info = Some(model_info);
    }
}

/// Central engine managing all radars and their features.
///
/// This is the single source of truth for radar control logic, used by both
/// `mayara-server` and `mayara-signalk-wasm`.
pub struct RadarEngine {
    /// Managed radars keyed by radar ID
    radars: HashMap<String, ManagedRadar>,
}

impl Default for RadarEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RadarEngine {
    /// Create a new empty radar engine
    pub fn new() -> Self {
        Self {
            radars: HashMap::new(),
        }
    }

    /// Add a Furuno radar
    pub fn add_furuno(&mut self, id: &str, addr: &str) {
        let controller = FurunoController::new(id, addr);
        let managed = ManagedRadar::new(id.to_string(), RadarController::Furuno(controller));
        self.radars.insert(id.to_string(), managed);
    }

    /// Add a Navico radar with full connection parameters
    pub fn add_navico(
        &mut self,
        id: &str,
        command_addr: &str,
        command_port: u16,
        report_addr: &str,
        report_port: u16,
        nic_addr: &str,
        model: NavicoModel,
    ) {
        let controller = NavicoController::new(
            id,
            command_addr,
            command_port,
            report_addr,
            report_port,
            nic_addr,
            model,
        );
        let managed = ManagedRadar::new(id.to_string(), RadarController::Navico(controller));
        self.radars.insert(id.to_string(), managed);
    }

    /// Add a Raymarine radar with full connection parameters
    pub fn add_raymarine(
        &mut self,
        id: &str,
        command_addr: &str,
        command_port: u16,
        report_addr: &str,
        report_port: u16,
        variant: RaymarineVariant,
        has_doppler: bool,
    ) {
        let controller = RaymarineController::new(
            id,
            command_addr,
            command_port,
            report_addr,
            report_port,
            variant,
            has_doppler,
        );
        let managed = ManagedRadar::new(id.to_string(), RadarController::Raymarine(controller));
        self.radars.insert(id.to_string(), managed);
    }

    /// Add a Garmin radar
    pub fn add_garmin(&mut self, id: &str, addr: &str) {
        let controller = GarminController::new(id, addr);
        let managed = ManagedRadar::new(id.to_string(), RadarController::Garmin(controller));
        self.radars.insert(id.to_string(), managed);
    }

    /// Add a pre-configured managed radar directly
    pub fn add_managed(&mut self, radar: ManagedRadar) {
        self.radars.insert(radar.id.clone(), radar);
    }

    /// Remove a radar by ID
    pub fn remove_radar(&mut self, id: &str) -> Option<ManagedRadar> {
        self.radars.remove(id)
    }

    /// Get a radar by ID
    pub fn get(&self, id: &str) -> Option<&ManagedRadar> {
        self.radars.get(id)
    }

    /// Get a mutable radar by ID
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ManagedRadar> {
        self.radars.get_mut(id)
    }

    /// Get all radar IDs
    pub fn radar_ids(&self) -> Vec<&str> {
        self.radars.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a radar exists
    pub fn contains(&self, id: &str) -> bool {
        self.radars.contains_key(id)
    }

    /// Iterate over all radars
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ManagedRadar)> {
        self.radars.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Iterate mutably over all radars
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut ManagedRadar)> {
        self.radars.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    // =========================================================================
    // ARPA Target Tracking
    // =========================================================================

    /// Get all ARPA targets for a radar
    pub fn get_targets(&self, radar_id: &str) -> Vec<ArpaTarget> {
        self.radars
            .get(radar_id)
            .map(|r| r.arpa.get_targets())
            .unwrap_or_default()
    }

    /// Acquire a new ARPA target at the given position
    pub fn acquire_target(
        &mut self,
        radar_id: &str,
        bearing: f64,
        distance: f64,
        timestamp_ms: u64,
    ) -> Option<u32> {
        self.radars
            .get_mut(radar_id)
            .and_then(|r| r.arpa.acquire_target(bearing, distance, timestamp_ms))
    }

    /// Cancel tracking of a target
    pub fn cancel_target(&mut self, radar_id: &str, target_id: u32) -> bool {
        self.radars
            .get_mut(radar_id)
            .map(|r| r.arpa.cancel_target(target_id))
            .unwrap_or(false)
    }

    /// Get ARPA settings for a radar
    pub fn get_arpa_settings(&self, radar_id: &str) -> Option<ArpaSettings> {
        self.radars.get(radar_id).map(|r| r.arpa.settings().clone())
    }

    /// Update ARPA settings for a radar
    pub fn set_arpa_settings(&mut self, radar_id: &str, settings: ArpaSettings) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.arpa.update_settings(settings);
        }
    }

    // =========================================================================
    // Guard Zones
    // =========================================================================

    /// Get all guard zones for a radar
    pub fn get_guard_zones(&self, radar_id: &str) -> Vec<GuardZoneStatus> {
        self.radars
            .get(radar_id)
            .map(|r| r.guard_zones.get_all_zone_status())
            .unwrap_or_default()
    }

    /// Get a specific guard zone
    pub fn get_guard_zone(&self, radar_id: &str, zone_id: u32) -> Option<GuardZoneStatus> {
        self.radars
            .get(radar_id)
            .and_then(|r| r.guard_zones.get_zone_status(zone_id))
    }

    /// Add or update a guard zone
    pub fn set_guard_zone(&mut self, radar_id: &str, zone: GuardZone) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.guard_zones.add_zone(zone);
        }
    }

    /// Remove a guard zone
    pub fn remove_guard_zone(&mut self, radar_id: &str, zone_id: u32) -> bool {
        self.radars
            .get_mut(radar_id)
            .map(|r| r.guard_zones.remove_zone(zone_id))
            .unwrap_or(false)
    }

    // =========================================================================
    // Trails
    // =========================================================================

    /// Get all trail data for a radar
    pub fn get_all_trails(&self, radar_id: &str) -> Vec<TrailData> {
        self.radars
            .get(radar_id)
            .map(|r| r.trails.get_all_trail_data())
            .unwrap_or_default()
    }

    /// Get trail for a specific target
    pub fn get_trail(&self, radar_id: &str, target_id: u32) -> Option<TrailData> {
        self.radars
            .get(radar_id)
            .and_then(|r| r.trails.get_trail_data(target_id))
    }

    /// Clear all trails for a radar
    pub fn clear_all_trails(&mut self, radar_id: &str) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.trails.clear_all();
        }
    }

    /// Clear trail for a specific target
    pub fn clear_trail(&mut self, radar_id: &str, target_id: u32) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.trails.clear_trail(target_id);
        }
    }

    /// Get trail settings for a radar
    pub fn get_trail_settings(&self, radar_id: &str) -> Option<TrailSettings> {
        self.radars
            .get(radar_id)
            .map(|r| r.trails.settings().clone())
    }

    /// Update trail settings for a radar
    pub fn set_trail_settings(&mut self, radar_id: &str, settings: TrailSettings) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.trails.update_settings(settings);
        }
    }

    // =========================================================================
    // Dual-Range
    // =========================================================================

    /// Get dual-range state for a radar
    pub fn get_dual_range(&self, radar_id: &str) -> Option<&DualRangeState> {
        self.radars
            .get(radar_id)
            .and_then(|r| r.dual_range.as_ref())
            .map(|dr| dr.state())
    }

    /// Check if dual-range is supported for a radar
    pub fn has_dual_range(&self, radar_id: &str) -> bool {
        self.radars
            .get(radar_id)
            .and_then(|r| r.model_info.as_ref())
            .map(|m| m.has_dual_range)
            .unwrap_or(false)
    }

    /// Apply dual-range configuration
    pub fn set_dual_range(&mut self, radar_id: &str, config: &DualRangeConfig) -> bool {
        self.radars
            .get_mut(radar_id)
            .and_then(|r| r.dual_range.as_mut())
            .map(|dr| dr.apply_config(config))
            .unwrap_or(false)
    }

    /// Get available secondary ranges for dual-range
    pub fn get_dual_range_available_ranges(&self, radar_id: &str) -> Vec<u32> {
        self.radars
            .get(radar_id)
            .and_then(|r| r.model_info.as_ref())
            .map(|m| {
                m.range_table
                    .iter()
                    .filter(|&&r| r <= m.max_dual_range)
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    // =========================================================================
    // Radar Controls (delegating to RadarController)
    // =========================================================================

    /// Set power/transmit state for a radar
    pub fn set_power<I: IoProvider>(&mut self, io: &mut I, radar_id: &str, transmit: bool) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_power(io, transmit);
        }
    }

    /// Set range for a radar (in meters)
    pub fn set_range<I: IoProvider>(&mut self, io: &mut I, radar_id: &str, range_meters: u32) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_range(io, range_meters);
        }
    }

    /// Set gain for a radar (0-100)
    pub fn set_gain<I: IoProvider>(&mut self, io: &mut I, radar_id: &str, value: i32, auto: bool) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_gain(io, value, auto);
        }
    }

    /// Set sea clutter for a radar (0-100)
    pub fn set_sea<I: IoProvider>(&mut self, io: &mut I, radar_id: &str, value: i32, auto: bool) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_sea(io, value, auto);
        }
    }

    /// Set rain clutter for a radar (0-100)
    pub fn set_rain<I: IoProvider>(&mut self, io: &mut I, radar_id: &str, value: i32, auto: bool) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_rain(io, value, auto);
        }
    }

    /// Set bearing alignment for a radar (degrees)
    pub fn set_bearing_alignment<I: IoProvider>(
        &mut self,
        io: &mut I,
        radar_id: &str,
        degrees: f64,
    ) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_bearing_alignment(io, degrees);
        }
    }

    /// Set interference rejection for a radar (0-3)
    pub fn set_interference_rejection<I: IoProvider>(
        &mut self,
        io: &mut I,
        radar_id: &str,
        level: u8,
    ) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            radar.controller.set_interference_rejection(io, level);
        }
    }

    /// Get model info for a radar
    pub fn get_model_info(&self, radar_id: &str) -> Option<&ModelInfo> {
        self.radars
            .get(radar_id)
            .and_then(|r| r.model_info.as_ref())
    }

    /// Set model info for a radar (after detection)
    pub fn set_model_info(&mut self, radar_id: &str, model_name: &str) {
        if let Some(radar) = self.radars.get_mut(radar_id) {
            let brand = radar.controller.brand();
            if let Some(model_info) = models::get_model(brand, model_name) {
                radar.set_model_info(model_info.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_creation() {
        let engine = RadarEngine::new();
        assert!(engine.radar_ids().is_empty());
    }

    #[test]
    fn test_arpa_methods() {
        let mut engine = RadarEngine::new();
        engine.add_furuno("test-radar", "192.168.1.1");

        // Should return empty targets for new radar
        let targets = engine.get_targets("test-radar");
        assert!(targets.is_empty());

        // Should return None for non-existent radar
        let targets = engine.get_targets("nonexistent");
        assert!(targets.is_empty());
    }

    #[test]
    fn test_guard_zone_methods() {
        let mut engine = RadarEngine::new();
        engine.add_furuno("test-radar", "192.168.1.1");

        // Should return empty zones
        let zones = engine.get_guard_zones("test-radar");
        assert!(zones.is_empty());

        // Add a zone using the constructor
        let zone = GuardZone::new_arc(1, 0.0, 90.0, 100.0, 200.0);
        engine.set_guard_zone("test-radar", zone);

        // Should now have one zone
        let zones = engine.get_guard_zones("test-radar");
        assert_eq!(zones.len(), 1);

        // Remove the zone
        assert!(engine.remove_guard_zone("test-radar", 1));
        let zones = engine.get_guard_zones("test-radar");
        assert!(zones.is_empty());
    }

    #[test]
    fn test_trail_methods() {
        let mut engine = RadarEngine::new();
        engine.add_furuno("test-radar", "192.168.1.1");

        // Should return empty trails
        let trails = engine.get_all_trails("test-radar");
        assert!(trails.is_empty());

        // Get/set settings should work
        let settings = engine.get_trail_settings("test-radar");
        assert!(settings.is_some());
    }
}
