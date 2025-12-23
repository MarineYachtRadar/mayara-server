//! Furuno Radar Model Database
//!
//! This module contains specifications for Furuno radar models.

use super::ModelInfo;
use crate::Brand;

/// Range table for DRS-NXT series (in meters)
/// Ranges: 1/16, 1/8, 1/4, 1/2, 3/4, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32, 36, 48 NM
static RANGE_TABLE_NXT: &[u32] = &[
    116,   // 1/16 NM
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
    59264, // 32 NM
    66672, // 36 NM
    88896, // 48 NM
];

/// Range table for standard DRS series (non-NXT, in meters)
static RANGE_TABLE_DRS: &[u32] = &[
    116,   // 1/16 NM
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
    59264, // 32 NM
    66672, // 36 NM
];

/// Range table for FAR series commercial radars (in meters)
static RANGE_TABLE_FAR: &[u32] = &[
    125,    // ~1/16 NM
    250,    // ~1/8 NM
    500,    // ~1/4 NM
    750,    // ~3/8 NM
    1500,   // ~3/4 NM
    3000,   // ~1.5 NM
    6000,   // ~3 NM
    12000,  // ~6 NM
    24000,  // ~12 NM
    48000,  // ~24 NM
    96000,  // ~48 NM
    120000, // ~64 NM
];

/// Extended controls available on NXT series
/// Note: bearingAlignment and antennaHeight are installation config values,
/// not live controls - they're stored in SignalK plugin config
static CONTROLS_NXT: &[&str] = &[
    "beamSharpening", // RezBoost
    "dopplerMode",    // Target Analyzer (enabled + target/rain mode)
    "birdMode",
    "interferenceRejection",
    "noiseReduction",      // Command 0x67 feature 3
    "mainBangSuppression", // Command 0x83
    "scanSpeed",
    "noTransmitZones",
    "autoAcquire",      // Auto target acquisition
    "txChannel",        // TX channel selection
    "bearingAlignment", // Installation config - schema only, not in /state
    "antennaHeight",    // Installation config - schema only, not in /state
];

/// Extended controls available on standard DRS series
/// Note: bearingAlignment and antennaHeight are installation config values
static CONTROLS_DRS: &[&str] = &[
    "interferenceRejection",
    "scanSpeed",
    "noTransmitZones",
    "bearingAlignment", // Installation config - schema only, not in /state
    "antennaHeight",    // Installation config - schema only, not in /state
];

/// Extended controls available on FAR series
/// Note: bearingAlignment and antennaHeight are installation config values
static CONTROLS_FAR: &[&str] = &[
    "interferenceRejection",
    "noTransmitZones",
    "txChannel",
    "bearingAlignment", // Installation config - schema only, not in /state
    "antennaHeight",    // Installation config - schema only, not in /state
];

/// All known Furuno radar models
pub static MODELS: &[ModelInfo] = &[
    // DRS-NXT Series (Doppler capable)
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS4D-NXT",
        family: "DRS-NXT",
        display_name: "Furuno DRS4D-NXT",
        max_range: 88896, // 48 NM
        min_range: 116,   // 1/16 NM
        range_table: RANGE_TABLE_NXT,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024, // Actual spokes can be up to ~900 samples
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 22224, // 12 NM max in dual-range
        no_transmit_zone_count: 2,
        controls: CONTROLS_NXT,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS6A-NXT",
        family: "DRS-NXT",
        display_name: "Furuno DRS6A-NXT",
        max_range: 88896,
        min_range: 116,
        range_table: RANGE_TABLE_NXT,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024, // Actual spokes can be up to ~900 samples
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 22224,
        no_transmit_zone_count: 2,
        controls: CONTROLS_NXT,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS12A-NXT",
        family: "DRS-NXT",
        display_name: "Furuno DRS12A-NXT",
        max_range: 133344, // 72 NM
        min_range: 116,
        range_table: RANGE_TABLE_NXT,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024, // Actual spokes can be up to ~900 samples
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 22224,
        no_transmit_zone_count: 2,
        controls: CONTROLS_NXT,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS25A-NXT",
        family: "DRS-NXT",
        display_name: "Furuno DRS25A-NXT",
        max_range: 177792, // 96 NM
        min_range: 116,
        range_table: RANGE_TABLE_NXT,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024, // Actual spokes can be up to ~900 samples
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 22224,
        no_transmit_zone_count: 2,
        controls: CONTROLS_NXT,
    },
    // Standard DRS Series (non-Doppler)
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS4D",
        family: "DRS",
        display_name: "Furuno DRS4D",
        max_range: 66672, // 36 NM
        min_range: 116,
        range_table: RANGE_TABLE_DRS,
        spokes_per_revolution: 8192,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_DRS,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS2D",
        family: "DRS",
        display_name: "Furuno DRS2D",
        max_range: 44448, // 24 NM
        min_range: 116,
        range_table: RANGE_TABLE_DRS,
        spokes_per_revolution: 8192,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_DRS,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS6A",
        family: "DRS",
        display_name: "Furuno DRS6A",
        max_range: 66672,
        min_range: 116,
        range_table: RANGE_TABLE_DRS,
        spokes_per_revolution: 8192,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_DRS,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS12A",
        family: "DRS",
        display_name: "Furuno DRS12A",
        max_range: 133344,
        min_range: 116,
        range_table: RANGE_TABLE_DRS,
        spokes_per_revolution: 8192,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_DRS,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "DRS25A",
        family: "DRS",
        display_name: "Furuno DRS25A",
        max_range: 177792,
        min_range: 116,
        range_table: RANGE_TABLE_DRS,
        spokes_per_revolution: 8192,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_DRS,
    },
    // FAR Series (Commercial)
    ModelInfo {
        brand: Brand::Furuno,
        model: "FAR-1513",
        family: "FAR",
        display_name: "Furuno FAR-1513",
        max_range: 120000,
        min_range: 125,
        range_table: RANGE_TABLE_FAR,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 4,
        controls: CONTROLS_FAR,
    },
    ModelInfo {
        brand: Brand::Furuno,
        model: "FAR-1518",
        family: "FAR",
        display_name: "Furuno FAR-1518",
        max_range: 120000,
        min_range: 125,
        range_table: RANGE_TABLE_FAR,
        spokes_per_revolution: 8192,
        max_spoke_length: 1024,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 4,
        controls: CONTROLS_FAR,
    },
];

/// Look up a Furuno model by name
pub fn get_model(model: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.model == model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drs4d_nxt() {
        let model = get_model("DRS4D-NXT").unwrap();
        assert_eq!(model.family, "DRS-NXT");
        assert!(model.has_doppler);
        assert!(model.has_dual_range);
        assert_eq!(model.no_transmit_zone_count, 2);
        assert!(model.controls.contains(&"dopplerMode"));
        assert!(model.controls.contains(&"beamSharpening"));
    }

    #[test]
    fn test_beam_sharpening_control_lookup() {
        use crate::capabilities::controls::get_control_for_brand;
        use crate::Brand;

        // Verify beamSharpening can be looked up for Furuno
        let def = get_control_for_brand("beamSharpening", Brand::Furuno);
        assert!(def.is_some(), "beamSharpening should be found for Furuno");
        let def = def.unwrap();
        assert_eq!(def.id, "beamSharpening");
    }

    #[test]
    fn test_drs4d() {
        let model = get_model("DRS4D").unwrap();
        assert_eq!(model.family, "DRS");
        assert!(!model.has_doppler);
        assert!(!model.has_dual_range);
    }

    #[test]
    fn test_range_table_nxt() {
        assert_eq!(RANGE_TABLE_NXT.len(), 18);
        assert_eq!(RANGE_TABLE_NXT[0], 116); // 1/16 NM
        assert_eq!(RANGE_TABLE_NXT[17], 88896); // 48 NM
    }
}
