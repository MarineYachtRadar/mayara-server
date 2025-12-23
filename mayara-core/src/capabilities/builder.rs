//! Capability Manifest Builder
//!
//! Builds CapabilityManifest from radar discovery information and model database.

use crate::models::{self, ModelInfo};
use crate::radar::RadarDiscovery;

use super::controls::*;
use super::{
    CapabilityManifest, Characteristics, ConstraintCondition, ConstraintEffect, ConstraintType,
    ControlConstraint, ControlDefinition, SupportedFeature,
};

/// Build a capability manifest for a discovered radar
///
/// Uses the model database to look up capabilities, falling back to
/// a generic configuration for unknown models.
///
/// The `supported_features` parameter declares which optional API features
/// the provider implements (e.g., ARPA, guard zones, trails).
#[inline(never)]
pub fn build_capabilities(
    discovery: &RadarDiscovery,
    radar_id: &str,
    supported_features: Vec<SupportedFeature>,
) -> CapabilityManifest {
    // Try to find model in database
    let model_info = discovery
        .model
        .as_deref()
        .and_then(|m| models::get_model(discovery.brand, m))
        .unwrap_or(&models::UNKNOWN_MODEL);

    CapabilityManifest {
        id: radar_id.to_string(),
        key: None, // WASM sets this via state
        make: discovery.brand.as_str().to_string(),
        model: model_info.model.to_string(),
        model_family: Some(model_info.family.to_string()),
        serial_number: discovery.serial_number.clone(),
        firmware_version: None, // Set dynamically via state

        characteristics: Characteristics {
            max_range: model_info.max_range,
            min_range: model_info.min_range,
            supported_ranges: model_info.range_table.to_vec(),
            spokes_per_revolution: model_info.spokes_per_revolution,
            max_spoke_length: model_info.max_spoke_length,
            pixel_values: discovery.pixel_values,
            has_doppler: model_info.has_doppler,
            has_dual_range: model_info.has_dual_range,
            max_dual_range: model_info.max_dual_range,
            no_transmit_zone_count: model_info.no_transmit_zone_count,
        },

        controls: build_controls(model_info, discovery.serial_number.is_some()),
        constraints: build_constraints(model_info),
        supported_features,
    }
}

/// Build a capability manifest directly from model info
///
/// Useful when you don't have a RadarDiscovery but know the model.
///
/// The `supported_features` parameter declares which optional API features
/// the provider implements (e.g., ARPA, guard zones, trails).
#[inline(never)]
pub fn build_capabilities_from_model(
    model_info: &ModelInfo,
    radar_id: &str,
    supported_features: Vec<SupportedFeature>,
) -> CapabilityManifest {
    CapabilityManifest {
        id: radar_id.to_string(),
        key: None, // No persistent key in basic builder
        make: model_info.brand.as_str().to_string(),
        model: model_info.model.to_string(),
        model_family: Some(model_info.family.to_string()),
        serial_number: None,
        firmware_version: None,

        characteristics: Characteristics {
            max_range: model_info.max_range,
            min_range: model_info.min_range,
            supported_ranges: model_info.range_table.to_vec(),
            spokes_per_revolution: model_info.spokes_per_revolution,
            max_spoke_length: model_info.max_spoke_length,
            pixel_values: 64, // Default to 6-bit radar data
            has_doppler: model_info.has_doppler,
            has_dual_range: model_info.has_dual_range,
            max_dual_range: model_info.max_dual_range,
            no_transmit_zone_count: model_info.no_transmit_zone_count,
        },

        controls: build_controls(model_info, false), // No serial number available
        constraints: build_constraints(model_info),
        supported_features,
    }
}

/// Build a capability manifest directly from model info with custom spokes configuration
///
/// Useful when you don't have a RadarDiscovery but know the model and have
/// runtime information about spoke characteristics.
#[inline(never)]
pub fn build_capabilities_from_model_with_spokes(
    model_info: &ModelInfo,
    radar_id: &str,
    supported_features: Vec<SupportedFeature>,
    spokes_per_revolution: u16,
    max_spoke_length: u16,
    pixel_values: u8,
) -> CapabilityManifest {
    build_capabilities_from_model_with_key(
        model_info,
        radar_id,
        None, // No persistent key
        supported_features,
        spokes_per_revolution,
        max_spoke_length,
        pixel_values,
    )
}

/// Build capabilities with a persistent key for installation settings storage.
/// The key is used by GUI to store/retrieve settings via Application Data API.
#[inline(never)]
pub fn build_capabilities_from_model_with_key(
    model_info: &ModelInfo,
    radar_id: &str,
    radar_key: Option<&str>,
    supported_features: Vec<SupportedFeature>,
    spokes_per_revolution: u16,
    max_spoke_length: u16,
    pixel_values: u8,
) -> CapabilityManifest {
    CapabilityManifest {
        id: radar_id.to_string(),
        key: radar_key.map(|k| k.to_string()),
        make: model_info.brand.as_str().to_string(),
        model: model_info.model.to_string(),
        model_family: Some(model_info.family.to_string()),
        serial_number: None,
        firmware_version: None,

        characteristics: Characteristics {
            max_range: model_info.max_range,
            min_range: model_info.min_range,
            supported_ranges: model_info.range_table.to_vec(),
            spokes_per_revolution,
            max_spoke_length,
            pixel_values,
            has_doppler: model_info.has_doppler,
            has_dual_range: model_info.has_dual_range,
            max_dual_range: model_info.max_dual_range,
            no_transmit_zone_count: model_info.no_transmit_zone_count,
        },

        controls: build_controls(model_info, false),
        constraints: build_constraints(model_info),
        supported_features,
    }
}

/// Build the list of controls for a radar model
///
/// NOTE: Controls are pushed one by one to avoid creating large stack frames.
/// The vec![] macro would create all ControlDefinition structs (~328 bytes each)
/// on the stack simultaneously before moving them to the heap.
#[inline(never)]
fn build_controls(model: &ModelInfo, has_serial_number: bool) -> Vec<ControlDefinition> {
    let mut controls = Vec::with_capacity(20);

    // Base controls (all radars) - push individually to avoid stack allocation
    controls.push(control_power());
    controls.push(control_range(model.range_table));
    controls.push(control_gain());
    controls.push(control_sea());
    controls.push(control_rain());

    // Info controls (read-only)
    controls.push(control_firmware_version());
    controls.push(control_operating_hours());
    controls.push(control_transmit_hours());

    // Only include serial number control if we have the data
    if has_serial_number {
        controls.push(control_serial_number());
    }

    // Extended controls based on model capabilities
    // Note: Installation category controls (bearingAlignment, antennaHeight) ARE included
    // in capabilities so clients can see the schema, but they won't appear in /state
    // since they're configuration values stored locally, not queried from the radar.
    for control_id in model.controls {
        if *control_id == "noTransmitZones" {
            if let Some(def) =
                get_extended_control_with_zones(control_id, model.no_transmit_zone_count)
            {
                controls.push(def);
            }
        } else if *control_id == "interferenceRejection" && model.brand == crate::Brand::Furuno {
            // Furuno has simple on/off interference rejection
            controls.push(control_interference_rejection_furuno());
        } else if *control_id == "scanSpeed" && model.brand == crate::Brand::Furuno {
            // Furuno uses 0=24RPM, 2=Auto
            controls.push(control_scan_speed_furuno());
        } else if let Some(def) = get_extended_control(control_id) {
            controls.push(def);
        }
    }

    controls
}

/// Build constraints for a radar model
#[inline(never)]
fn build_constraints(model: &ModelInfo) -> Vec<ControlConstraint> {
    let mut constraints = vec![];

    // If preset mode is available, add constraints for controls it locks
    if model.controls.contains(&"presetMode") {
        let locked_controls = ["gain", "sea", "rain", "interferenceRejection"];

        for control_id in locked_controls {
            // Only add constraint if the control exists on this model
            if control_id == "interferenceRejection"
                && !model.controls.contains(&"interferenceRejection")
            {
                continue;
            }

            constraints.push(ControlConstraint {
                control_id: control_id.to_string(),
                condition: ConstraintCondition {
                    condition_type: ConstraintType::ReadOnlyWhen,
                    depends_on: "presetMode".into(),
                    operator: "!=".into(),
                    value: "custom".into(),
                },
                effect: ConstraintEffect {
                    disabled: None,
                    read_only: Some(true),
                    allowed_values: None,
                    reason: Some("Controlled by preset mode".into()),
                },
            });
        }
    }

    // Doppler mode constraint: only available when radar has Doppler
    if model.has_doppler && model.controls.contains(&"dopplerMode") {
        // No additional constraint needed - presence in controls indicates availability
    }

    // Dual range constraint: range limited in dual-range mode for Furuno
    if model.has_dual_range && model.max_dual_range > 0 {
        // Could add constraint that secondary screen range is limited
        // This would be a "restricted_when" constraint
    }

    constraints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Brand;

    #[test]
    fn test_build_capabilities_furuno() {
        let discovery = RadarDiscovery {
            brand: Brand::Furuno,
            model: Some("DRS4D-NXT".into()),
            name: "Test Radar".into(),
            address: "192.168.1.100:10010".into(),
            data_port: 10024,
            command_port: 10025,
            spokes_per_revolution: 2048,
            max_spoke_len: 512,
            pixel_values: 64,
            serial_number: Some("12345".into()),
            nic_address: None,
            suffix: None,
            data_address: None,
            report_address: None,
            send_address: None,
        };

        let caps = build_capabilities(&discovery, "1", vec![]);

        assert_eq!(caps.id, "1");
        assert_eq!(caps.make, "Furuno");
        assert_eq!(caps.model, "DRS4D-NXT");
        assert!(caps.characteristics.has_doppler);
        assert!(caps.characteristics.has_dual_range);
        assert!(caps.controls.len() >= 5); // At least base controls
        assert!(caps.supported_features.is_empty());
    }

    #[test]
    fn test_build_capabilities_with_features() {
        let discovery = RadarDiscovery {
            brand: Brand::Furuno,
            model: Some("DRS4D-NXT".into()),
            name: "Test Radar".into(),
            address: "192.168.1.100:10010".into(),
            data_port: 10024,
            command_port: 10025,
            spokes_per_revolution: 2048,
            max_spoke_len: 512,
            pixel_values: 64,
            serial_number: None,
            nic_address: None,
            suffix: None,
            data_address: None,
            report_address: None,
            send_address: None,
        };

        let caps = build_capabilities(
            &discovery,
            "1",
            vec![SupportedFeature::Arpa, SupportedFeature::GuardZones],
        );

        assert_eq!(caps.supported_features.len(), 2);
        assert!(caps.supported_features.contains(&SupportedFeature::Arpa));
        assert!(caps
            .supported_features
            .contains(&SupportedFeature::GuardZones));
    }
}
