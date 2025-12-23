//! Garmin Radar Model Database
//!
//! This module contains specifications for Garmin radar models.

use super::ModelInfo;
use crate::Brand;

/// Range table for xHD series (in meters)
static RANGE_TABLE_XHD: &[u32] = &[
    50, 75, 100, 125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000,
    36000, 48000, 72000,
];

/// Range table for Fantom series (in meters)
static RANGE_TABLE_FANTOM: &[u32] = &[
    50, 75, 100, 125, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000,
    36000, 48000, 72000, 96000,
];

/// Extended controls for Fantom series (Doppler capable)
static CONTROLS_FANTOM: &[&str] = &[
    "dopplerMode", // MotionScope
    "targetSeparation",
    "interferenceRejection",
    "crosstalkRejection", // Garmin-specific
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
    "scanSpeed",
];

/// Extended controls for xHD series
static CONTROLS_XHD: &[&str] = &[
    "targetSeparation",
    "interferenceRejection",
    "crosstalkRejection", // Garmin-specific
    "noTransmitZones",
    "bearingAlignment",
    "antennaHeight",
];

/// All known Garmin radar models
pub static MODELS: &[ModelInfo] = &[
    // Fantom Series (Doppler capable)
    ModelInfo {
        brand: Brand::Garmin,
        model: "Fantom 18",
        family: "Fantom",
        display_name: "Garmin Fantom 18",
        max_range: 72000,
        min_range: 50,
        range_table: RANGE_TABLE_FANTOM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_FANTOM,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "Fantom 24",
        family: "Fantom",
        display_name: "Garmin Fantom 24",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_FANTOM,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_FANTOM,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "Fantom 54",
        family: "Fantom",
        display_name: "Garmin Fantom 54",
        max_range: 133344,
        min_range: 50,
        range_table: RANGE_TABLE_FANTOM,
        spokes_per_revolution: 2048,
        max_spoke_length: 1024,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_FANTOM,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "Fantom 56",
        family: "Fantom",
        display_name: "Garmin Fantom 56",
        max_range: 133344,
        min_range: 50,
        range_table: RANGE_TABLE_FANTOM,
        spokes_per_revolution: 2048,
        max_spoke_length: 1024,
        has_doppler: true,
        has_dual_range: true,
        max_dual_range: 24000,
        no_transmit_zone_count: 2,
        controls: CONTROLS_FANTOM,
    },
    // xHD Series
    ModelInfo {
        brand: Brand::Garmin,
        model: "GMR 18 xHD",
        family: "xHD",
        display_name: "Garmin GMR 18 xHD",
        max_range: 72000,
        min_range: 50,
        range_table: RANGE_TABLE_XHD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_XHD,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "GMR 24 xHD",
        family: "xHD",
        display_name: "Garmin GMR 24 xHD",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_XHD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_XHD,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "GMR 18 HD+",
        family: "HD+",
        display_name: "Garmin GMR 18 HD+",
        max_range: 72000,
        min_range: 50,
        range_table: RANGE_TABLE_XHD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_XHD,
    },
    ModelInfo {
        brand: Brand::Garmin,
        model: "GMR 24 HD+",
        family: "HD+",
        display_name: "Garmin GMR 24 HD+",
        max_range: 96000,
        min_range: 50,
        range_table: RANGE_TABLE_XHD,
        spokes_per_revolution: 2048,
        max_spoke_length: 512,
        has_doppler: false,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_XHD,
    },
];

/// Look up a Garmin model by name
pub fn get_model(model: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.model == model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fantom_24() {
        let model = get_model("Fantom 24").unwrap();
        assert!(model.has_doppler);
        assert!(model.controls.contains(&"dopplerMode"));
    }

    #[test]
    fn test_xhd() {
        let model = get_model("GMR 18 xHD").unwrap();
        assert!(!model.has_doppler);
    }
}
