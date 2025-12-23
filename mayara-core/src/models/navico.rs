//! Navico Radar Model Database
//!
//! This module contains specifications for Navico radar models
//! (Simrad, Lowrance, B&G branded radars).

use super::ModelInfo;
use crate::Brand;

/// Range table for HALO series (in meters)
/// Matches B&G chart plotter range steps for consistent UX
/// 1 NM = 1852 meters
static RANGE_TABLE_HALO: &[u32] = &[
    50,    // 50m
    75,    // 75m
    100,   // 100m
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    66672, // 36 NM
    88896, // 48 NM
];

/// Range table for 4G/3G series (in meters)
/// Matches B&G chart plotter range steps for consistent UX
/// 1 NM = 1852 meters
static RANGE_TABLE_4G: &[u32] = &[
    50,    // 50m
    75,    // 75m
    100,   // 100m
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    66672, // 36 NM
];

/// Extended controls for HALO series
static CONTROLS_HALO: &[&str] = &[
    "presetMode",   // Harbor/Offshore/Weather/Custom
    "dopplerMode",  // VelocityTrack
    "dopplerSpeed", // VelocityTrack speed threshold
    "targetSeparation",
    "targetExpansion",
    "targetBoost",
    "seaState",
    "noiseRejection",
    "interferenceRejection",
    "localInterferenceRejection",
    "sidelobeSuppression",
    "birdMode",
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
    "scanSpeed",
    "accentLight", // Pedestal lighting
];

/// Extended controls for 4G/3G series
static CONTROLS_4G: &[&str] = &[
    "presetMode",
    "targetSeparation",
    "targetExpansion",
    "targetBoost",
    "seaState",
    "noiseRejection",
    "interferenceRejection",
    "sidelobeSuppression",
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
];

/// All known Navico radar models
pub static MODELS: &[ModelInfo] = &[
    // HALO Series (Doppler capable)
    // Generic HALO entry for radars that don't report specific variant
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO",
        family: "HALO",
        display_name: "Navico HALO",
        max_range: 74080, // Conservative max range
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 1024,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 4,
        controls: CONTROLS_HALO,
    },
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO20+",
        family: "HALO",
        display_name: "Navico HALO20+",
        max_range: 72000,
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_HALO,
    },
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO24",
        family: "HALO",
        display_name: "Navico HALO24",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_HALO,
    },
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO3",
        family: "HALO",
        display_name: "Navico HALO3",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_HALO,
    },
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO4",
        family: "HALO",
        display_name: "Navico HALO4",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_HALO,
    },
    ModelInfo {
        brand: Brand::Navico,
        model: "HALO6",
        family: "HALO",
        display_name: "Navico HALO6",
        max_range: 133344,
        min_range: 50,
        range_table: RANGE_TABLE_HALO,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_HALO,
    },
    // 4G Series
    ModelInfo {
        brand: Brand::Navico,
        model: "4G",
        family: "4G",
        display_name: "Navico 4G",
        max_range: 64000,
        min_range: 50,
        range_table: RANGE_TABLE_4G,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_4G,
    },
    // 3G Series
    ModelInfo {
        brand: Brand::Navico,
        model: "3G",
        family: "3G",
        display_name: "Navico 3G",
        max_range: 48000,
        min_range: 50,
        range_table: RANGE_TABLE_4G,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_4G,
    },
    // BR24
    ModelInfo {
        brand: Brand::Navico,
        model: "BR24",
        family: "BR24",
        display_name: "Navico BR24",
        max_range: 44448,
        min_range: 50,
        range_table: RANGE_TABLE_4G,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: &["interferenceRejection", "bearingAlignment"],
    },
];

/// Look up a Navico model by name
pub fn get_model(model: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.model == model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_halo24() {
        let model = get_model("HALO24").unwrap();
        assert_eq!(model.family, "HALO");
        assert!(model.has_doppler);
        assert!(model.controls.contains(&"dopplerMode"));
    }

    #[test]
    fn test_4g() {
        let model = get_model("4G").unwrap();
        assert!(!model.has_doppler);
        assert!(model.controls.contains(&"presetMode"));
    }
}
