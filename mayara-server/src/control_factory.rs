//! Control Factory Module
//!
//! Bridges between mayara-core's ControlDefinition (schema) and
//! mayara-server's Control (runtime state).
//!
//! This module builds server Controls from core ControlDefinitions,
//! making mayara-core the single source of truth for control metadata.

use std::sync::Arc;

use mayara_core::capabilities::{
    controls, ControlDefinition as CoreControlDefinition, ControlType as CoreControlType,
    WireProtocolHint,
};
use mayara_core::Brand;

use crate::settings::{AutomaticValue, Control, HAS_AUTO_NOT_ADJUSTABLE};

/// Build a server Control from a mayara-core ControlDefinition.
/// The control ID is taken from the core definition.
/// The core definition is attached to the Control for enum value lookups.
#[inline(never)]
pub fn build_control(core_def: &CoreControlDefinition) -> Control {
    let core_arc = Arc::new(core_def.clone());
    let control = match core_def.control_type {
        CoreControlType::Number => build_numeric_control(core_def),
        CoreControlType::Enum => build_enum_control(core_def),
        CoreControlType::Boolean => build_boolean_control(core_def),
        CoreControlType::Compound => build_compound_control(core_def),
        CoreControlType::String => build_string_control(core_def),
    };
    control.with_core_def(core_arc)
}

/// Build a numeric control from core definition
#[inline(never)]
fn build_numeric_control(def: &CoreControlDefinition) -> Control {
    let range = def.range.as_ref();
    let min = range.map(|r| r.min as f32).unwrap_or(0.0);
    let max = range.map(|r| r.max as f32).unwrap_or(100.0);

    let mut control = Control::new_numeric(&def.id, min, max);

    // Apply wire hints
    if let Some(hints) = &def.wire_hints {
        control = apply_wire_hints(control, hints);
    }

    // Apply unit
    if let Some(range) = &def.range {
        if let Some(unit) = &range.unit {
            control = control.unit(unit);
        }
    }

    // Apply read-only
    if def.read_only {
        control = control.read_only(true);
    }

    control
}

/// Build an enum/list control from core definition
#[inline(never)]
fn build_enum_control(def: &CoreControlDefinition) -> Control {
    let labels: Vec<&str> = def
        .values
        .as_ref()
        .map(|vs| vs.iter().map(|v| v.label.as_str()).collect())
        .unwrap_or_default();

    let mut control = Control::new_list(&def.id, &labels);

    // Override max_value if enum values aren't sequential 0..n-1
    // For example, Furuno scanSpeed uses values 0 and 2 (not 0 and 1)
    if let Some(values) = &def.values {
        let max_enum_value = values
            .iter()
            .filter_map(|v| v.value.as_i64())
            .max()
            .unwrap_or(0) as f32;
        if max_enum_value > (labels.len() - 1) as f32 {
            control = control.max_value(max_enum_value);
        }
    }

    // Apply wire hints
    if let Some(hints) = &def.wire_hints {
        control = apply_wire_hints(control, hints);
    }

    // Apply read-only
    if def.read_only {
        control = control.read_only(true);
    }

    control
}

/// Build a boolean control from core definition (as 2-value list)
#[inline(never)]
fn build_boolean_control(def: &CoreControlDefinition) -> Control {
    let mut control = Control::new_list(&def.id, &["Off", "On"]);

    // Apply wire hints
    if let Some(hints) = &def.wire_hints {
        control = apply_wire_hints(control, hints);
    }

    // Apply read-only
    if def.read_only {
        control = control.read_only(true);
    }

    control
}

/// Build a compound control (auto/manual with value) from core definition
#[inline(never)]
fn build_compound_control(def: &CoreControlDefinition) -> Control {
    // For compound controls like gain/sea/rain, extract the value property's range
    let (min, max) = if let Some(props) = &def.properties {
        if let Some(value_prop) = props.get("value") {
            if let Some(range) = &value_prop.range {
                (range.min as f32, range.max as f32)
            } else {
                (0.0, 100.0)
            }
        } else {
            (0.0, 100.0)
        }
    } else {
        (0.0, 100.0)
    };

    // Check if this control has auto mode
    let has_auto = def
        .modes
        .as_ref()
        .map_or(false, |m| m.contains(&"auto".to_string()));

    let mut control = if has_auto {
        // Check for adjustable auto from wire hints
        let auto_value = if let Some(hints) = &def.wire_hints {
            if hints.has_auto_adjustable {
                AutomaticValue {
                    has_auto: true,
                    has_auto_adjustable: true,
                    auto_adjust_min_value: hints.auto_adjust_min.unwrap_or(-50.0),
                    auto_adjust_max_value: hints.auto_adjust_max.unwrap_or(50.0),
                }
            } else {
                HAS_AUTO_NOT_ADJUSTABLE
            }
        } else {
            HAS_AUTO_NOT_ADJUSTABLE
        };
        Control::new_auto(&def.id, min, max, auto_value)
    } else {
        Control::new_numeric(&def.id, min, max)
    };

    // Apply wire hints
    if let Some(hints) = &def.wire_hints {
        control = apply_wire_hints(control, hints);
    }

    // Apply unit from value property
    if let Some(props) = &def.properties {
        if let Some(value_prop) = props.get("value") {
            if let Some(range) = &value_prop.range {
                if let Some(unit) = &range.unit {
                    control = control.unit(unit);
                }
            }
        }
    }

    // Apply read-only
    if def.read_only {
        control = control.read_only(true);
    }

    control
}

/// Build a string control from core definition
#[inline(never)]
fn build_string_control(def: &CoreControlDefinition) -> Control {
    let control = Control::new_string(&def.id);
    if def.read_only {
        control.read_only(true)
    } else {
        control.read_only(false)
    }
}

/// Apply wire protocol hints to a control
fn apply_wire_hints(mut control: Control, hints: &WireProtocolHint) -> Control {
    // Apply scale factor
    if let Some(scale) = hints.scale_factor {
        let with_step = hints.step.is_some();
        control = control.wire_scale_factor(scale, with_step);
    }

    // Apply offset
    if let Some(offset) = hints.offset {
        control = control.wire_offset(offset);
    }

    // Apply has_enabled flag
    if hints.has_enabled {
        control = control.has_enabled();
    }

    // Apply settable indices (for enum controls with read-only values)
    if let Some(ref indices) = hints.settable_indices {
        control.set_valid_values(indices.clone());
    }

    // Apply send_always flag
    if hints.send_always {
        control = control.send_always();
    }

    control
}

// =============================================================================
// Brand-specific control builders
// =============================================================================

/// Build gain control for a specific brand
pub fn gain_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_gain_for_brand(brand);
    build_control(&core_def)
}

/// Build sea clutter control for a specific brand
pub fn sea_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_sea_for_brand(brand);
    build_control(&core_def)
}

/// Build rain clutter control for a specific brand
pub fn rain_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_rain_for_brand(brand);
    build_control(&core_def)
}

/// Build bearing alignment control for a specific brand
pub fn bearing_alignment_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_bearing_alignment_for_brand(brand);
    build_control(&core_def)
}

/// Build antenna height control for a specific brand
pub fn antenna_height_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_antenna_height_for_brand(brand);
    build_control(&core_def)
}

/// Build sidelobe suppression control for a specific brand
pub fn sidelobe_suppression_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_sidelobe_suppression_for_brand(brand);
    build_control(&core_def)
}

/// Build tune control for a specific brand
pub fn tune_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_tune_for_brand(brand);
    build_control(&core_def)
}

/// Build color gain control for a specific brand
pub fn color_gain_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_color_gain_for_brand(brand);
    build_control(&core_def)
}

/// Build FTC control for a specific brand
pub fn ftc_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_ftc_for_brand(brand);
    build_control(&core_def)
}

/// Build doppler speed control for a specific brand
pub fn doppler_speed_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_doppler_speed_for_brand(brand);
    build_control(&core_def)
}

/// Build rotation speed control for a specific brand
pub fn rotation_speed_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_rotation_speed_for_brand(brand);
    build_control(&core_def)
}

/// Build no-transmit zone angle control for a specific brand
pub fn no_transmit_angle_control_for_brand(
    id: &str,
    zone_number: u8,
    is_start: bool,
    brand: Brand,
) -> Control {
    let core_def = controls::control_no_transmit_angle_for_brand(id, zone_number, is_start, brand);
    build_control(&core_def)
}

// =============================================================================
// Generic control builders (no brand-specific wire hints)
// =============================================================================

/// Build power control (off, standby, transmit, warming)
pub fn power_control() -> Control {
    let core_def = controls::control_power();
    build_control(&core_def)
}

/// Build power control with brand-specific settable values
pub fn power_control_for_brand(brand: Brand) -> Control {
    let core_def = controls::control_power_for_brand(brand);
    build_control(&core_def)
}

/// Build operating hours control (read-only)
pub fn operating_hours_control() -> Control {
    let core_def = controls::control_operating_hours();
    build_control(&core_def)
}

/// Build transmit hours control (read-only)
pub fn transmit_hours_control() -> Control {
    let core_def = controls::control_transmit_hours();
    build_control(&core_def)
}

/// Build serial number control (read-only)
pub fn serial_number_control() -> Control {
    let core_def = controls::control_serial_number();
    build_control(&core_def)
}

/// Build firmware version control (read-only)
pub fn firmware_version_control() -> Control {
    let core_def = controls::control_firmware_version();
    build_control(&core_def)
}

/// Build interference rejection control (multi-level enum)
pub fn interference_rejection_control() -> Control {
    let core_def = controls::control_interference_rejection();
    build_control(&core_def)
}

/// Build interference rejection control (Furuno: boolean)
pub fn interference_rejection_control_furuno() -> Control {
    let core_def = controls::control_interference_rejection_furuno();
    build_control(&core_def)
}

/// Build target expansion control
pub fn target_expansion_control() -> Control {
    let core_def = controls::control_target_expansion();
    build_control(&core_def)
}

/// Build target boost control
pub fn target_boost_control() -> Control {
    let core_def = controls::control_target_boost();
    build_control(&core_def)
}

/// Build target separation control
pub fn target_separation_control() -> Control {
    let core_def = controls::control_target_separation();
    build_control(&core_def)
}

/// Build noise rejection control
pub fn noise_rejection_control() -> Control {
    let core_def = controls::control_noise_rejection();
    build_control(&core_def)
}

/// Build sea state control
pub fn sea_state_control() -> Control {
    let core_def = controls::control_sea_state();
    build_control(&core_def)
}

/// Build accent light control
pub fn accent_light_control() -> Control {
    let core_def = controls::control_accent_light();
    build_control(&core_def)
}

/// Build local interference rejection control
pub fn local_interference_rejection_control() -> Control {
    let core_def = controls::control_local_interference_rejection();
    build_control(&core_def)
}

/// Build main bang suppression control
pub fn main_bang_suppression_control() -> Control {
    let core_def = controls::control_main_bang_suppression();
    build_control(&core_def)
}

// =============================================================================
// Batch control builders - create all controls for a brand/model from core
// =============================================================================

use std::collections::HashMap;

/// Build all base controls for a brand from mayara-core definitions.
/// Returns a HashMap suitable for inserting into SharedControls.
///
/// This uses the same control definitions that WASM uses, ensuring
/// both platforms have identical control schemas.
pub fn build_base_controls_for_brand(brand: Brand) -> HashMap<String, Control> {
    let core_defs = controls::get_base_controls_for_brand(brand);
    let mut result = HashMap::new();
    for def in core_defs {
        let id = def.id.clone();
        result.insert(id, build_control(&def));
    }
    result
}

/// Build all controls for a brand and model from mayara-core definitions.
/// Includes base controls plus model-specific extended controls.
///
/// If model_name is None, only base controls are returned.
pub fn build_all_controls_for_model(
    brand: Brand,
    model_name: Option<&str>,
) -> HashMap<String, Control> {
    let core_defs = controls::get_all_controls_for_model(brand, model_name);
    let mut result = HashMap::new();
    for def in core_defs {
        let id = def.id.clone();
        result.insert(id, build_control(&def));
    }
    result
}

/// Build model-specific extended controls for a brand and model.
/// Does NOT include base controls (use build_base_controls_for_brand for those).
pub fn build_extended_controls_for_model(
    brand: Brand,
    model_name: &str,
) -> HashMap<String, Control> {
    use mayara_core::models;

    let mut result = HashMap::new();
    if let Some(model_info) = models::get_model(brand, model_name) {
        for control_id in model_info.controls {
            // Skip special compound controls that map to multiple controls
            if *control_id == "noTransmitZones" {
                continue;
            }
            if let Some(def) = controls::get_extended_control_for_brand(control_id, brand) {
                result.insert(control_id.to_string(), build_control(&def));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gain_control_furuno() {
        let control = gain_control_for_brand(Brand::Furuno);
        assert_eq!(control.id(), "gain");
    }

    #[test]
    fn test_gain_control_raymarine() {
        let control = gain_control_for_brand(Brand::Raymarine);
        assert_eq!(control.id(), "gain");
    }

    #[test]
    fn test_operating_hours() {
        let control = operating_hours_control();
        assert_eq!(control.id(), "operatingHours");
    }

    #[test]
    fn test_sea_control_navico() {
        let control = sea_control_for_brand(Brand::Navico);
        assert_eq!(control.id(), "sea");
    }

    #[test]
    fn test_rain_control_raymarine() {
        let control = rain_control_for_brand(Brand::Raymarine);
        assert_eq!(control.id(), "rain");
    }

    #[test]
    fn test_power_control_enum_lookup() {
        let control = power_control();
        assert_eq!(control.id(), "power");

        // Test case-insensitive lookup by value
        assert_eq!(control.enum_value_to_index("off"), Some(0));
        assert_eq!(control.enum_value_to_index("standby"), Some(1));
        assert_eq!(control.enum_value_to_index("transmit"), Some(2));
        assert_eq!(control.enum_value_to_index("warming"), Some(3));

        // Test case-insensitive lookup (mixed case)
        assert_eq!(control.enum_value_to_index("Transmit"), Some(2));
        assert_eq!(control.enum_value_to_index("TRANSMIT"), Some(2));
        assert_eq!(control.enum_value_to_index("TrAnSmIt"), Some(2));

        // Test lookup by label
        assert_eq!(control.enum_value_to_index("Off"), Some(0));
        assert_eq!(control.enum_value_to_index("Standby"), Some(1));
        assert_eq!(control.enum_value_to_index("Warming Up"), Some(3));

        // Test reverse lookup
        assert_eq!(control.index_to_enum_value(0), Some("off".to_string()));
        assert_eq!(control.index_to_enum_value(1), Some("standby".to_string()));
        assert_eq!(control.index_to_enum_value(2), Some("transmit".to_string()));
        assert_eq!(control.index_to_enum_value(3), Some("warming".to_string()));
    }
}
