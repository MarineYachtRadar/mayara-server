//! Range Formatting Utilities
//!
//! Provides formatting of range values (stored in meters) for display
//! in various unit systems: metric, nautical miles, or mixed.

use serde::{Deserialize, Serialize};

/// Preference for how to display range values
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RangeUnitPreference {
    /// Always display in meters (e.g., "1852m")
    Metric,
    /// Always display in nautical miles (e.g., "1 NM")
    Nautical,
    /// Mixed: meters for short ranges, NM for longer ranges
    /// (threshold at 1/4 NM = 463m)
    #[default]
    Mixed,
}

/// A formatted range value with both raw meters and display string
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormattedRange {
    /// Range in meters (canonical value for API)
    pub meters: u32,
    /// Human-readable label based on unit preference
    pub label: String,
    /// Unit used in the label ("m" or "NM")
    pub unit: String,
}

/// 1 nautical mile in meters
pub const NM_IN_METERS: f64 = 1852.0;

/// Threshold for switching from meters to NM in mixed mode (1/4 NM)
const MIXED_THRESHOLD_METERS: u32 = 463;

/// Format a single range value
pub fn format_range(meters: u32, preference: RangeUnitPreference) -> FormattedRange {
    match preference {
        RangeUnitPreference::Metric => FormattedRange {
            meters,
            label: format_meters(meters),
            unit: "m".into(),
        },
        RangeUnitPreference::Nautical => FormattedRange {
            meters,
            label: format_nautical(meters),
            unit: "NM".into(),
        },
        RangeUnitPreference::Mixed => {
            if meters < MIXED_THRESHOLD_METERS {
                FormattedRange {
                    meters,
                    label: format_meters(meters),
                    unit: "m".into(),
                }
            } else {
                FormattedRange {
                    meters,
                    label: format_nautical(meters),
                    unit: "NM".into(),
                }
            }
        }
    }
}

/// Format all ranges in a range table
pub fn format_range_table(
    ranges: &[u32],
    preference: RangeUnitPreference,
) -> Vec<FormattedRange> {
    ranges
        .iter()
        .map(|&m| format_range(m, preference))
        .collect()
}

/// Format meters as a display string
fn format_meters(meters: u32) -> String {
    if meters >= 1000 {
        // Show as km for large values
        let km = meters as f64 / 1000.0;
        if km.fract() == 0.0 {
            format!("{}km", km as u32)
        } else {
            format!("{:.1}km", km)
        }
    } else {
        format!("{}m", meters)
    }
}

/// Format as nautical miles
fn format_nautical(meters: u32) -> String {
    let nm = meters as f64 / NM_IN_METERS;

    // Common fractional NM values
    if (nm - 0.0625).abs() < 0.01 {
        return "1/16 NM".into();
    }
    if (nm - 0.125).abs() < 0.01 {
        return "1/8 NM".into();
    }
    if (nm - 0.25).abs() < 0.01 {
        return "1/4 NM".into();
    }
    if (nm - 0.5).abs() < 0.01 {
        return "1/2 NM".into();
    }
    if (nm - 0.75).abs() < 0.01 {
        return "3/4 NM".into();
    }
    if (nm - 1.5).abs() < 0.01 {
        return "1.5 NM".into();
    }

    // Integer or decimal NM
    if nm.fract().abs() < 0.01 {
        format!("{} NM", nm as u32)
    } else {
        format!("{:.1} NM", nm)
    }
}

/// Convert meters to nautical miles
pub fn meters_to_nm(meters: u32) -> f64 {
    meters as f64 / NM_IN_METERS
}

/// Convert nautical miles to meters
pub fn nm_to_meters(nm: f64) -> u32 {
    (nm * NM_IN_METERS).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_meters() {
        assert_eq!(format_meters(50), "50m");
        assert_eq!(format_meters(100), "100m");
        assert_eq!(format_meters(1000), "1km");
        assert_eq!(format_meters(1500), "1.5km");
        assert_eq!(format_meters(24000), "24km");
    }

    #[test]
    fn test_format_nautical() {
        assert_eq!(format_nautical(116), "1/16 NM");
        assert_eq!(format_nautical(231), "1/8 NM");
        assert_eq!(format_nautical(463), "1/4 NM");
        assert_eq!(format_nautical(926), "1/2 NM");
        assert_eq!(format_nautical(1389), "3/4 NM");
        assert_eq!(format_nautical(1852), "1 NM");
        assert_eq!(format_nautical(2778), "1.5 NM");
        assert_eq!(format_nautical(3704), "2 NM");
        assert_eq!(format_nautical(44448), "24 NM");
    }

    #[test]
    fn test_mixed_mode() {
        // Short ranges in meters
        let short = format_range(100, RangeUnitPreference::Mixed);
        assert_eq!(short.unit, "m");
        assert_eq!(short.label, "100m");

        // Long ranges in NM
        let long = format_range(1852, RangeUnitPreference::Mixed);
        assert_eq!(long.unit, "NM");
        assert_eq!(long.label, "1 NM");
    }

    #[test]
    fn test_conversion() {
        assert_eq!(nm_to_meters(1.0), 1852);
        assert!((meters_to_nm(1852) - 1.0).abs() < 0.001);
    }
}
