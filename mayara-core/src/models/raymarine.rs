//! Raymarine Radar Model Database
//!
//! This module contains specifications for Raymarine radar models.

use super::ModelInfo;
use crate::Brand;

/// Range table for Quantum series (in meters)
static RANGE_TABLE_QUANTUM: &[u32] = &[
    50, 75, 100, 125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000,
    36000, 48000,
];

/// Range table for analog/RD series (in meters)
static RANGE_TABLE_RD: &[u32] = &[
    125, 250, 500, 750, 1500, 3000, 6000, 12000, 24000, 48000, 72000,
];

/// Extended controls for Quantum 2 (Doppler capable)
static CONTROLS_QUANTUM2: &[&str] = &[
    "presetMode",       // Harbor/Coastal/Offshore
    "dopplerMode",      // True Echo Trail
    "targetSeparation", // ATX
    "targetExpansion",
    "mainBangSuppression",
    "colorGain", // Quantum-specific
    "interferenceRejection",
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
];

/// Extended controls for Quantum (non-Doppler)
static CONTROLS_QUANTUM: &[&str] = &[
    "presetMode",
    "targetSeparation",
    "targetExpansion",
    "mainBangSuppression",
    "colorGain", // Quantum-specific
    "interferenceRejection",
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
];

/// Extended controls for RD series
static CONTROLS_RD: &[&str] = &[
    "interferenceRejection",
    "targetExpansion",
    "mainBangSuppression",
    "ftc",  // Fast Time Constant
    "tune", // Receiver tuning
    "bearingAlignment",
];

/// All known Raymarine radar models
pub static MODELS: &[ModelInfo] = &[
    // Quantum 2 Series (Doppler capable)
    ModelInfo {
        brand: Brand::Raymarine,
        model: "Quantum 2",
        family: "Quantum",
        display_name: "Raymarine Quantum 2",
        max_range: 48000,
        min_range: 50,
        range_table: RANGE_TABLE_QUANTUM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_QUANTUM2,
    },
    ModelInfo {
        brand: Brand::Raymarine,
        model: "Quantum 2 Q24D",
        family: "Quantum",
        display_name: "Raymarine Quantum 2 Q24D",
        max_range: 48000,
        min_range: 50,
        range_table: RANGE_TABLE_QUANTUM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_QUANTUM2,
    },
    // Quantum Series (non-Doppler)
    ModelInfo {
        brand: Brand::Raymarine,
        model: "Quantum",
        family: "Quantum",
        display_name: "Raymarine Quantum",
        max_range: 48000,
        min_range: 50,
        range_table: RANGE_TABLE_QUANTUM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_QUANTUM,
    },
    ModelInfo {
        brand: Brand::Raymarine,
        model: "Quantum Q24C",
        family: "Quantum",
        display_name: "Raymarine Quantum Q24C",
        max_range: 48000,
        min_range: 50,
        range_table: RANGE_TABLE_QUANTUM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_QUANTUM,
    },
    // RD/Digital Series
    ModelInfo {
        brand: Brand::Raymarine,
        model: "RD418D",
        family: "RD",
        display_name: "Raymarine RD418D",
        max_range: 72000,
        min_range: 125,
        range_table: RANGE_TABLE_RD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 0,
        controls: CONTROLS_RD,
    },
    ModelInfo {
        brand: Brand::Raymarine,
        model: "RD424D",
        family: "RD",
        display_name: "Raymarine RD424D",
        max_range: 96000,
        min_range: 125,
        range_table: RANGE_TABLE_RD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 0,
        controls: CONTROLS_RD,
    },
];

/// Look up a Raymarine model by name
pub fn get_model(model: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.model == model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantum2() {
        let model = get_model("Quantum 2").unwrap();
        assert!(model.has_doppler);
        assert!(model.controls.contains(&"dopplerMode"));
    }

    #[test]
    fn test_quantum() {
        let model = get_model("Quantum").unwrap();
        assert!(!model.has_doppler);
    }
}
