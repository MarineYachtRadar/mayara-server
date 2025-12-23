//! Radar Model Database
//!
//! This module provides a database of known radar models with their capabilities,
//! range tables, and available controls. This information is used to build
//! capability manifests for the v5 API.

use crate::Brand;

pub mod furuno;
pub mod garmin;
pub mod navico;
pub mod raymarine;

/// Information about a specific radar model
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Radar brand
    pub brand: Brand,
    /// Model identifier (e.g., "DRS4D-NXT")
    pub model: &'static str,
    /// Model family (e.g., "DRS-NXT")
    pub family: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,

    // Hardware characteristics
    /// Maximum detection range in meters
    pub max_range: u32,
    /// Minimum detection range in meters
    pub min_range: u32,
    /// Discrete range values supported (in meters)
    pub range_table: &'static [u32],
    /// Number of spokes per antenna revolution
    pub spokes_per_revolution: u16,
    /// Maximum spoke length in samples
    pub max_spoke_length: u16,

    // Feature flags
    /// Whether Doppler processing is available
    pub has_doppler: bool,
    /// Whether dual-range display is supported
    pub has_dual_range: bool,
    /// Maximum range for dual-range mode (0 if not supported)
    pub max_dual_range: u32,
    /// Number of no-transmit zones supported
    pub no_transmit_zone_count: u8,

    // Available extended controls (semantic IDs)
    /// List of extended control IDs available on this model
    pub controls: &'static [&'static str],
}

/// Unknown/generic model used when a radar model isn't in the database
pub static UNKNOWN_MODEL: ModelInfo = ModelInfo {
    brand: Brand::Furuno, // Will be overwritten
    model: "Unknown",
    family: "Unknown",
    display_name: "Unknown Radar",
    max_range: 74080,
    min_range: 50,
    range_table: &[
        50, 75, 100, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000,
        36000, 48000, 64000, 74080,
    ],
    spokes_per_revolution: 2048,
    max_spoke_length: 512,
    has_doppler: false,
    has_dual_range: false,
    max_dual_range: 0,
    no_transmit_zone_count: 0,
    controls: &[],
};

/// Look up a model by brand and model string
///
/// Returns None if the model is not found in the database.
/// Use `UNKNOWN_MODEL` as a fallback for unknown models.
pub fn get_model(brand: Brand, model: &str) -> Option<&'static ModelInfo> {
    match brand {
        Brand::Furuno => furuno::get_model(model),
        Brand::Navico => navico::get_model(model),
        Brand::Raymarine => raymarine::get_model(model),
        Brand::Garmin => garmin::get_model(model),
    }
}

/// Get all known models for a brand
pub fn get_models_for_brand(brand: Brand) -> &'static [ModelInfo] {
    match brand {
        Brand::Furuno => furuno::MODELS,
        Brand::Navico => navico::MODELS,
        Brand::Raymarine => raymarine::MODELS,
        Brand::Garmin => garmin::MODELS,
    }
}

/// Infer model from radar characteristics when model string is unknown
///
/// This is useful for Navico radars where the beacon doesn't always include
/// the explicit model name.
pub fn infer_model(brand: Brand, spokes: u16, max_spoke_len: u16) -> Option<&'static ModelInfo> {
    let models = get_models_for_brand(brand);

    // Find best match based on spokes and spoke length
    models
        .iter()
        .find(|m| m.spokes_per_revolution == spokes && m.max_spoke_length == max_spoke_len)
}

/// Get all unique range values supported by any model of a given brand.
/// This is useful for range detection when the specific model is not yet known.
/// Returns a sorted, deduplicated list of ranges in meters.
pub fn get_all_ranges_for_brand(brand: Brand) -> Vec<u32> {
    let models = get_models_for_brand(brand);
    let mut ranges: Vec<u32> = models
        .iter()
        .flat_map(|m| m.range_table.iter().copied())
        .collect();
    ranges.sort();
    ranges.dedup();
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_furuno_model() {
        let model = get_model(Brand::Furuno, "DRS4D-NXT");
        assert!(model.is_some());
        let m = model.unwrap();
        assert_eq!(m.model, "DRS4D-NXT");
        assert!(m.has_doppler);
    }

    #[test]
    fn test_unknown_model() {
        let model = get_model(Brand::Furuno, "NonExistent");
        assert!(model.is_none());
    }
}
