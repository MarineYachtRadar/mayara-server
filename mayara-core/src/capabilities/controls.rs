//! Standard Control Definitions for SignalK Radar API v5
//!
//! This module provides factory functions for creating ControlDefinition
//! instances for all standard controls (base and extended).

use std::collections::HashMap;

use super::{
    ControlCategory, ControlDefinition, ControlType, EnumValue, PropertyDefinition, RangeSpec,
    WireProtocolHint,
};
use crate::Brand;

// =============================================================================
// Base Controls (Required - All Radars)
// =============================================================================

/// Power control: off, standby, transmit, warming
///
/// All states are readable, but settable states vary by brand:
/// - Furuno: Only standby (1) and transmit (2) are settable
/// - Navico/Raymarine/Garmin: standby and transmit are settable
/// - off (0) and warming (3) are read-only states reported by radar
pub fn control_power() -> ControlDefinition {
    ControlDefinition {
        id: "power".into(),
        name: "Power".into(),
        description: "Radar operational state".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: "off".into(),
                label: "Off".into(),
                description: Some("Radar powered off".into()),
                read_only: true, // Cannot be set by client
            },
            EnumValue {
                value: "standby".into(),
                label: "Standby".into(),
                description: Some("Radar on, not transmitting".into()),
                read_only: false,
            },
            EnumValue {
                value: "transmit".into(),
                label: "Transmit".into(),
                description: Some("Radar transmitting".into()),
                read_only: false,
            },
            EnumValue {
                value: "warming".into(),
                label: "Warming Up".into(),
                description: Some("Magnetron warming (read-only state)".into()),
                read_only: true, // Cannot be set by client
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some("standby".into()),
        wire_hints: None,
    }
}

/// Power control with brand-specific settable values
///
/// Returns a ControlDefinition with wire_hints indicating which values are settable.
/// All brands can only set standby and transmit; off and warming are read-only.
pub fn control_power_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_power();
    def.wire_hints = Some(match brand {
        // All brands: only standby (index 1) and transmit (index 2) are settable
        // off (0) and warming (3) are read-only states reported by radar
        Brand::Furuno => WireProtocolHint {
            settable_indices: Some(vec![1, 2]), // standby, transmit
            send_always: true,                  // Furuno needs power commands sent always
            ..Default::default()
        },
        Brand::Navico | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            settable_indices: Some(vec![1, 2]), // standby, transmit
            ..Default::default()
        },
    });
    def
}

/// Range control: detection range in meters
pub fn control_range(supported_ranges: &[u32]) -> ControlDefinition {
    let min = *supported_ranges.first().unwrap_or(&100) as f64;
    let max = *supported_ranges.last().unwrap_or(&100000) as f64;

    ControlDefinition {
        id: "range".into(),
        name: "Range".into(),
        description:
            "Detection range in meters. Use supportedRanges from capabilities for valid values."
                .into(),
        category: ControlCategory::Base,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min,
            max,
            step: None, // Discrete values, use supportedRanges
            unit: Some("meters".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: None,
        wire_hints: None,
    }
}

/// Gain control: signal amplification with auto/manual mode
#[inline(never)]
pub fn control_gain() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Auto or manual control".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "auto".into(),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: "manual".into(),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Gain level (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "gain".into(),
        name: "Gain".into(),
        description:
            "Signal amplification. Higher values increase sensitivity but may also amplify noise."
                .into(),
        category: ControlCategory::Base,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "auto", "value": 50})),
        wire_hints: None,
    }
}

/// Sea clutter control: suppresses returns from waves
#[inline(never)]
pub fn control_sea() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Auto or manual control".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "auto".into(),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: "manual".into(),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Sea clutter suppression (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "sea".into(),
        name: "Sea Clutter".into(),
        description: "Suppresses radar returns from waves near the vessel.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "auto", "value": 30})),
        wire_hints: None,
    }
}

/// Rain clutter control: suppresses returns from precipitation
#[inline(never)]
pub fn control_rain() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "string".into(),
            description: Some("Auto or manual mode".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: serde_json::json!("auto"),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: serde_json::json!("manual"),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Rain clutter suppression (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "rain".into(),
        name: "Rain Clutter".into(),
        description: "Suppresses radar returns from rain and precipitation.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("manual".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "manual", "value": 0})),
        wire_hints: None,
    }
}

// =============================================================================
// Info Controls (Read-Only)
// =============================================================================

/// Serial number: radar hardware serial number (read-only)
pub fn control_serial_number() -> ControlDefinition {
    ControlDefinition {
        id: "serialNumber".into(),
        name: "Serial Number".into(),
        description: "Radar hardware serial number.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::String,
        range: None,
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: true,
        default: None,
        wire_hints: None,
    }
}

/// Firmware version: radar firmware version (read-only)
pub fn control_firmware_version() -> ControlDefinition {
    ControlDefinition {
        id: "firmwareVersion".into(),
        name: "Firmware Version".into(),
        description: "Radar firmware version.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::String,
        range: None,
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: true,
        default: None,
        wire_hints: None,
    }
}

/// Operating hours: total hours of radar operation (power-on time, read-only)
pub fn control_operating_hours() -> ControlDefinition {
    ControlDefinition {
        id: "operatingHours".into(),
        name: "Operating Hours".into(),
        description: "Total hours of radar power-on operation.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.0,
            max: 999999.0,
            step: Some(0.1),
            unit: Some("hours".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: true,
        default: None,
        wire_hints: None,
    }
}

/// Transmit hours: total hours the radar has been transmitting (read-only)
pub fn control_transmit_hours() -> ControlDefinition {
    ControlDefinition {
        id: "transmitHours".into(),
        name: "Transmit Hours".into(),
        description: "Total hours the radar has been actively transmitting.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.0,
            max: 999999.0,
            step: Some(0.1),
            unit: Some("hours".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: true,
        default: None,
        wire_hints: None,
    }
}

/// Rotation speed: current antenna rotation speed (read-only)
pub fn control_rotation_speed() -> ControlDefinition {
    ControlDefinition {
        id: "rotationSpeed".into(),
        name: "Rotation Speed".into(),
        description: "Current antenna rotation speed in RPM.".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.0,
            max: 99.0,
            step: Some(0.1),
            unit: Some("RPM".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: true,
        default: None,
        wire_hints: None,
    }
}

/// Rotation speed with brand-specific wire encoding
pub fn control_rotation_speed_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_rotation_speed();
    def.wire_hints = Some(match brand {
        // All brands with rotation speed use the same 0.1 RPM precision wire encoding
        Brand::Furuno | Brand::Navico | Brand::Raymarine => WireProtocolHint {
            scale_factor: Some(990.0), // 99.0 RPM * 10 = 990 (0.1 RPM precision)
            step: Some(0.1),
            ..Default::default()
        },
        Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// No-transmit zone start angle: start bearing of a no-transmit sector
///
/// Used by server for flat control model. The compound noTransmitZones control
/// is used in the v5 API but server internally tracks start/end separately.
/// Value of -1 means zone is disabled.
pub fn control_no_transmit_start(zone_number: u8) -> ControlDefinition {
    ControlDefinition {
        id: format!("noTransmitStart{}", zone_number),
        name: format!("No-Transmit Zone {} Start", zone_number),
        description: format!(
            "Start angle of no-transmit zone {} in degrees. -1 = disabled.",
            zone_number
        ),
        category: ControlCategory::Installation,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: -1.0, // -1 = zone disabled
            max: 359.0,
            step: Some(1.0),
            unit: Some("degrees".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(serde_json::json!(-1)), // Default to disabled
        wire_hints: None,
    }
}

/// No-transmit zone end angle: end bearing of a no-transmit sector
/// Value of -1 means zone is disabled.
pub fn control_no_transmit_end(zone_number: u8) -> ControlDefinition {
    ControlDefinition {
        id: format!("noTransmitEnd{}", zone_number),
        name: format!("No-Transmit Zone {} End", zone_number),
        description: format!(
            "End angle of no-transmit zone {} in degrees. -1 = disabled.",
            zone_number
        ),
        category: ControlCategory::Installation,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: -1.0, // -1 = zone disabled
            max: 359.0,
            step: Some(1.0),
            unit: Some("degrees".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(serde_json::json!(-1)), // Default to disabled
        wire_hints: None,
    }
}

/// No-transmit zone angle control with brand-specific wire encoding
pub fn control_no_transmit_angle_for_brand(
    id: &str,
    zone_number: u8,
    is_start: bool,
    brand: Brand,
) -> ControlDefinition {
    let mut def = if is_start {
        control_no_transmit_start(zone_number)
    } else {
        control_no_transmit_end(zone_number)
    };
    // Override ID to match what was passed
    def.id = id.to_string();

    def.wire_hints = Some(match brand {
        Brand::Furuno => WireProtocolHint {
            // No offset needed - wire protocol uses 0-359 degrees directly
            ..Default::default()
        },
        Brand::Navico => WireProtocolHint {
            scale_factor: Some(3600.0), // 0.1 degree precision for full 360°: 360.0 * 10 = 3600
            step: Some(0.1),
            has_enabled: true,
            ..Default::default()
        },
        Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

// =============================================================================
// Extended Controls (Optional - Model-Specific)
// =============================================================================

/// Beam sharpening: digital beam narrowing for improved resolution
///
/// Furuno: RezBoost
/// Navico: Beam Sharpening
pub fn control_beam_sharpening() -> ControlDefinition {
    ControlDefinition {
        id: "beamSharpening".into(),
        name: "Beam Sharpening".into(),
        description: "Digital beam narrowing for improved target separation. Higher levels provide better resolution but may reduce sensitivity.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: Some("Beam sharpening disabled".into()),
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: Some("Mild beam sharpening".into()),
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: Some("Moderate beam sharpening".into()),
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: Some("Maximum beam sharpening".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(2.into()),
        wire_hints: None,
    }
}

/// Doppler mode: motion-based target highlighting
///
/// Furuno: Target Analyzer
/// Navico: VelocityTrack
/// Raymarine: Doppler
#[inline(never)]
pub fn control_doppler_mode() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "enabled".into(),
        PropertyDefinition {
            prop_type: "boolean".into(),
            description: Some("Enable Doppler processing".into()),
            range: None,
            values: None,
        },
    );

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Doppler display mode".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "approaching".into(),
                    label: "Approaching Only".into(),
                    description: Some("Highlight only approaching targets".into()),
                    read_only: false,
                },
                EnumValue {
                    value: "both".into(),
                    label: "Both Directions".into(),
                    description: Some("Highlight both approaching and receding targets".into()),
                    read_only: false,
                },
                EnumValue {
                    value: "target".into(),
                    label: "Target Mode".into(),
                    description: Some("Furuno: Highlights collision threats".into()),
                    read_only: false,
                },
                EnumValue {
                    value: "rain".into(),
                    label: "Rain Mode".into(),
                    description: Some("Furuno: Identifies precipitation".into()),
                    read_only: false,
                },
            ]),
        },
    );

    ControlDefinition {
        id: "dopplerMode".into(),
        name: "Doppler Mode".into(),
        description:
            "Uses Doppler processing to highlight moving targets based on their relative motion."
                .into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(serde_json::json!({"enabled": true, "mode": "approaching"})),
        wire_hints: Some(WireProtocolHint {
            has_enabled: true,
            ..Default::default()
        }),
    }
}

/// Bird mode: optimizes display for detecting bird flocks
pub fn control_bird_mode() -> ControlDefinition {
    ControlDefinition {
        id: "birdMode".into(),
        name: "Bird Mode".into(),
        description: "Optimizes radar display for detecting flocks of birds, which often indicate fish schools below.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: Some("Bird mode disabled".into()),
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: Some("Mild bird detection sensitivity".into()),
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: Some("Moderate bird detection sensitivity".into()),
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: Some("Maximum bird detection sensitivity".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// TX Channel: transmission frequency selection (Furuno)
pub fn control_tx_channel() -> ControlDefinition {
    ControlDefinition {
        id: "txChannel".into(),
        name: "TX Channel".into(),
        description:
            "Selects the transmission frequency channel to avoid interference with nearby radars."
                .into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Auto".into(),
                description: Some("Automatic channel selection".into()),
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Channel 1".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Channel 2".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "Channel 3".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Interference rejection: filters interference from other radars (multi-level for Navico/Garmin)
pub fn control_interference_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "interferenceRejection".into(),
        name: "Interference Rejection".into(),
        description: "Filters interference from other radars operating on similar frequencies."
            .into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(1.into()),
        wire_hints: None,
    }
}

/// Interference rejection for Furuno: simple on/off toggle
pub fn control_interference_rejection_furuno() -> ControlDefinition {
    ControlDefinition {
        id: "interferenceRejection".into(),
        name: "Int. Rejection".into(),
        description: "Interference Rejection: filters interference from other radars.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Boolean,
        range: None,
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(false.into()),
        wire_hints: None,
    }
}

/// Preset mode: pre-configured operating modes (Navico, Raymarine)
pub fn control_preset_mode() -> ControlDefinition {
    ControlDefinition {
        id: "presetMode".into(),
        name: "Preset Mode".into(),
        description: "Pre-configured operating modes that automatically adjust multiple settings. In preset modes, some controls become read-only.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: "custom".into(),
                label: "Custom".into(),
                description: Some("Full manual control of all settings".into()),
                read_only: false,
            },
            EnumValue {
                value: "harbor".into(),
                label: "Harbor".into(),
                description: Some("Optimized for busy ports with fast scanning".into()),
                read_only: false,
            },
            EnumValue {
                value: "offshore".into(),
                label: "Offshore".into(),
                description: Some("Balanced settings for open water navigation".into()),
                read_only: false,
            },
            EnumValue {
                value: "weather".into(),
                label: "Weather".into(),
                description: Some("Enhanced sensitivity for detecting precipitation".into()),
                read_only: false,
            },
            EnumValue {
                value: "bird".into(),
                label: "Bird".into(),
                description: Some("Optimized for detecting bird flocks".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some("harbor".into()),
        wire_hints: None,
    }
}

/// Target separation: distinguishes closely-spaced targets (Navico, Raymarine)
pub fn control_target_separation() -> ControlDefinition {
    ControlDefinition {
        id: "targetSeparation".into(),
        name: "Target Separation".into(),
        description: "Improves ability to distinguish closely-spaced targets.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(1.into()),
        wire_hints: None,
    }
}

/// Bearing alignment: heading offset correction
pub fn control_bearing_alignment() -> ControlDefinition {
    ControlDefinition {
        id: "bearingAlignment".into(),
        name: "Bearing Alignment".into(),
        description: "Corrects for antenna mounting offset from vessel heading.".into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: -179.0,
            max: 179.0,
            step: Some(1.0),
            unit: Some("degrees".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.0.into()),
        wire_hints: Some(WireProtocolHint {
            write_only: true, // Cannot reliably read from hardware
            ..Default::default()
        }),
    }
}

/// Antenna height: height of radar antenna above waterline in meters
///
/// Affects sea clutter calculations.
pub fn control_antenna_height() -> ControlDefinition {
    ControlDefinition {
        id: "antennaHeight".into(),
        name: "Antenna Height".into(),
        description:
            "Height of radar antenna above waterline in meters. Used for sea clutter calculations."
                .into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.0,
            max: 99.0,
            step: Some(0.01),
            unit: Some("m".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(5.into()),
        wire_hints: Some(WireProtocolHint {
            write_only: true, // Cannot reliably read from hardware
            ..Default::default()
        }),
    }
}

/// No-transmit zones: sectors where radar won't transmit
pub fn control_no_transmit_zones(zone_count: u8) -> ControlDefinition {
    ControlDefinition {
        id: "noTransmitZones".into(),
        name: "No-Transmit Zones".into(),
        description: format!(
            "Configure up to {} sectors where the radar will not transmit.",
            zone_count
        ),
        category: ControlCategory::Installation,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: {
            let mut props = HashMap::new();
            props.insert(
                "zones".into(),
                PropertyDefinition {
                    prop_type: "array".into(),
                    description: Some(
                        "Array of zone objects with enabled, start, and end angles".into(),
                    ),
                    range: None,
                    values: None,
                },
            );
            Some(props)
        },
        modes: None,
        default_mode: None,
        read_only: false,
        default: None,
        wire_hints: None,
    }
}

/// Scan speed: antenna rotation speed (Navico)
/// 4G verified: 0=Off (Normal), 1=Medium, 2=Medium-High
pub fn control_scan_speed() -> ControlDefinition {
    ControlDefinition {
        id: "scanSpeed".into(),
        name: "Scan Speed".into(),
        description: "Antenna rotation speed. Faster speeds update the display more frequently but may reduce sensitivity.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: Some("Normal rotation speed".into()),
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Medium".into(),
                description: Some("Medium rotation speed".into()),
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium-High".into(),
                description: Some("Medium-high rotation speed".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Scan speed: antenna rotation speed (Furuno)
///
/// Furuno uses: 0=24RPM (fixed), 2=Auto (varies by range)
pub fn control_scan_speed_furuno() -> ControlDefinition {
    ControlDefinition {
        id: "scanSpeed".into(),
        name: "Scan Speed".into(),
        description: "Antenna rotation speed. Auto adjusts based on range setting.".into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "24 RPM".into(),
                description: Some("Fixed 24 rotations per minute".into()),
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Auto".into(),
                description: Some("Automatically adjusts based on range".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(2.into()),
        wire_hints: None,
    }
}

/// Auto acquire: automatic ARPA target acquisition (Furuno)
pub fn control_auto_acquire() -> ControlDefinition {
    ControlDefinition {
        id: "autoAcquire".into(),
        name: "Auto Acquire".into(),
        description: "Automatically acquires and tracks moving targets using Doppler detection."
            .into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Boolean,
        range: None,
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(false.into()),
        wire_hints: Some(WireProtocolHint {
            write_only: true, // Cannot read from hardware ($S87 only, no $R87)
            ..Default::default()
        }),
    }
}

/// Noise reduction: reduces snow-like noise in the display
///
/// Furuno: Command 0x67 feature 3
pub fn control_noise_reduction() -> ControlDefinition {
    ControlDefinition {
        id: "noiseReduction".into(),
        name: "Noise Reduction".into(),
        description: "Reduces snow-like noise in the radar display.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Boolean,
        range: None,
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(false.into()),
        wire_hints: None,
    }
}

/// Main bang suppression: reduces center artifact
///
/// Furuno: Command 0x83
/// Raymarine: Main bang suppression enabled
pub fn control_main_bang_suppression() -> ControlDefinition {
    ControlDefinition {
        id: "mainBangSuppression".into(),
        name: "Main Bang Suppression".into(),
        description: "Reduces the main bang artifact at the center of the radar display.".into(),
        category: ControlCategory::Installation,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.0,
            max: 100.0,
            step: Some(1.0),
            unit: Some("percent".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(50.into()),
        wire_hints: None,
    }
}

/// Target expansion: makes small targets more visible
///
/// Navico: Target Expansion (0x09 C1 or 0x12 C1)
/// Raymarine: Target Expansion
pub fn control_target_expansion() -> ControlDefinition {
    ControlDefinition {
        id: "targetExpansion".into(),
        name: "Target Expansion".into(),
        description: "Makes small targets more visible by enlarging their display.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "On".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "High".into(),
                description: Some("HALO only".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Target boost: amplifies weak targets
///
/// Navico: Target Boost (0x0A C1)
pub fn control_target_boost() -> ControlDefinition {
    ControlDefinition {
        id: "targetBoost".into(),
        name: "Target Boost".into(),
        description: "Amplifies weak targets to make them more visible.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Sea state: preset sea clutter configuration
///
/// Navico: Sea State (0x0B C1)
pub fn control_sea_state() -> ControlDefinition {
    ControlDefinition {
        id: "seaState".into(),
        name: "Sea State".into(),
        description: "Preset sea clutter configuration based on current sea conditions.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Calm".into(),
                description: Some("Light seas with minimal wave action".into()),
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Moderate".into(),
                description: Some("Average sea conditions".into()),
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Rough".into(),
                description: Some("Heavy seas with large waves".into()),
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Sidelobe suppression: reduces sidelobe artifacts
///
/// Navico: Sidelobe Suppression (0x06 C1 subtype 0x05)
#[inline(never)]
pub fn control_sidelobe_suppression() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Auto or manual control".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "auto".into(),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: "manual".into(),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Suppression level (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "sidelobeSuppression".into(),
        name: "Sidelobe Suppression".into(),
        description: "Reduces sidelobe artifacts caused by strong nearby targets.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "auto", "value": 50})),
        wire_hints: None,
    }
}

/// Noise rejection: filters radar noise
///
/// Navico: Noise Rejection (0x21 C1)
pub fn control_noise_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "noiseRejection".into(),
        name: "Noise Rejection".into(),
        description: "Filters out radar noise from the display.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Crosstalk rejection: filters interference from nearby radars
///
/// Garmin: Crosstalk Rejection (0x0932)
pub fn control_crosstalk_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "crosstalkRejection".into(),
        name: "Crosstalk Rejection".into(),
        description: "Filters interference from nearby radars operating on similar frequencies."
            .into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "On".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// FTC (Fast Time Constant): reduces rain/snow clutter
///
/// Raymarine: FTC
/// Garmin HD: FTC (0x02B8)
#[inline(never)]
pub fn control_ftc() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "enabled".into(),
        PropertyDefinition {
            prop_type: "boolean".into(),
            description: Some("Enable FTC processing".into()),
            range: None,
            values: None,
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("FTC level (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "ftc".into(),
        name: "FTC".into(),
        description: "Fast Time Constant processing to reduce rain and snow clutter.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(serde_json::json!({"enabled": false, "value": 0})),
        wire_hints: None,
    }
}

/// Tune: receiver tuning control
///
/// Raymarine: Tune (auto/manual)
#[inline(never)]
pub fn control_tune() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Auto or manual tuning".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "auto".into(),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: "manual".into(),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Tune value (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "tune".into(),
        name: "Tune".into(),
        description: "Receiver tuning for optimal signal reception.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "auto", "value": 50})),
        wire_hints: None,
    }
}

/// Color gain: adjusts color intensity
///
/// Raymarine Quantum: Color Gain
#[inline(never)]
pub fn control_color_gain() -> ControlDefinition {
    let mut properties = HashMap::new();

    properties.insert(
        "mode".into(),
        PropertyDefinition {
            prop_type: "enum".into(),
            description: Some("Auto or manual control".into()),
            range: None,
            values: Some(vec![
                EnumValue {
                    value: "auto".into(),
                    label: "Auto".into(),
                    description: None,
                    read_only: false,
                },
                EnumValue {
                    value: "manual".into(),
                    label: "Manual".into(),
                    description: None,
                    read_only: false,
                },
            ]),
        },
    );

    properties.insert(
        "value".into(),
        PropertyDefinition {
            prop_type: "number".into(),
            description: Some("Color gain level (0-100%)".into()),
            range: Some(RangeSpec {
                min: 0.0,
                max: 100.0,
                step: Some(1.0),
                unit: Some("percent".into()),
            }),
            values: None,
        },
    );

    ControlDefinition {
        id: "colorGain".into(),
        name: "Color Gain".into(),
        description: "Adjusts the color intensity of radar returns.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
        default: Some(serde_json::json!({"mode": "auto", "value": 50})),
        wire_hints: None,
    }
}

/// Accent light: pedestal illumination
///
/// Navico HALO: Accent Light (0x31 C1)
pub fn control_accent_light() -> ControlDefinition {
    ControlDefinition {
        id: "accentLight".into(),
        name: "Accent Light".into(),
        description: "Controls the pedestal accent lighting brightness.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

/// Doppler speed threshold: minimum speed for Doppler detection
///
/// Navico HALO: Doppler Speed (0x24 C1)
/// Furuno: DopplerSpeed (command 0xEF related)
pub fn control_doppler_speed() -> ControlDefinition {
    ControlDefinition {
        id: "dopplerSpeed".into(),
        name: "Doppler Speed Threshold".into(),
        description: "Minimum target speed for Doppler detection in knots.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min: 0.5,
            max: 100.0,
            step: Some(0.5),
            unit: Some("knots".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(5.0.into()),
        wire_hints: None,
    }
}

/// Local interference rejection: filters local interference sources
///
/// Navico: Local Interference Rejection (0x0E C1)
pub fn control_local_interference_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "localInterferenceRejection".into(),
        name: "Local Interference Rejection".into(),
        description: "Filters interference from local sources such as ship electronics.".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 1.into(),
                label: "Low".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 2.into(),
                label: "Medium".into(),
                description: None,
                read_only: false,
            },
            EnumValue {
                value: 3.into(),
                label: "High".into(),
                description: None,
                read_only: false,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
        wire_hints: None,
    }
}

// =============================================================================
// Helper to get extended control by ID
// =============================================================================

/// Get an extended control definition by its semantic ID
#[inline(never)]
pub fn get_extended_control(id: &str) -> Option<ControlDefinition> {
    match id {
        // Signal processing
        "beamSharpening" => Some(control_beam_sharpening()),
        "dopplerMode" => Some(control_doppler_mode()),
        "dopplerSpeed" => Some(control_doppler_speed()),
        "birdMode" => Some(control_bird_mode()),
        "noiseReduction" => Some(control_noise_reduction()),
        "noiseRejection" => Some(control_noise_rejection()),
        "mainBangSuppression" => Some(control_main_bang_suppression()),
        // Interference
        "interferenceRejection" => Some(control_interference_rejection()),
        "localInterferenceRejection" => Some(control_local_interference_rejection()),
        "crosstalkRejection" => Some(control_crosstalk_rejection()),
        "sidelobeSuppression" => Some(control_sidelobe_suppression()),
        // Target processing
        "targetSeparation" => Some(control_target_separation()),
        "targetExpansion" => Some(control_target_expansion()),
        "targetBoost" => Some(control_target_boost()),
        // Clutter
        "seaState" => Some(control_sea_state()),
        "ftc" => Some(control_ftc()),
        // Modes
        "presetMode" => Some(control_preset_mode()),
        "txChannel" => Some(control_tx_channel()),
        "scanSpeed" => Some(control_scan_speed()),
        // Receiver
        "tune" => Some(control_tune()),
        "colorGain" => Some(control_color_gain()),
        // Installation
        "bearingAlignment" => Some(control_bearing_alignment()),
        "antennaHeight" => Some(control_antenna_height()),
        // Acquisition
        "autoAcquire" => Some(control_auto_acquire()),
        // Hardware
        "accentLight" => Some(control_accent_light()),
        _ => None,
    }
}

/// Get extended control with customization for no-transmit zones
#[inline(never)]
pub fn get_extended_control_with_zones(id: &str, zone_count: u8) -> Option<ControlDefinition> {
    if id == "noTransmitZones" {
        Some(control_no_transmit_zones(zone_count))
    } else {
        get_extended_control(id)
    }
}

// =============================================================================
// Brand-Aware Factory Functions
// =============================================================================
//
// These functions return ControlDefinitions with WireProtocolHint populated
// based on brand-specific wire encoding requirements.

/// Gain control with brand-specific wire hints
#[inline(never)]
pub fn control_gain_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_gain();
    def.wire_hints = Some(match brand {
        // Furuno uses 0-100 on wire (same as UI), no scaling needed
        Brand::Furuno => WireProtocolHint {
            has_auto: true,
            ..Default::default()
        },
        Brand::Raymarine => WireProtocolHint {
            scale_factor: Some(100.0),
            has_auto: true,
            ..Default::default()
        },
        Brand::Navico | Brand::Garmin => WireProtocolHint {
            scale_factor: Some(255.0),
            has_auto: true,
            ..Default::default()
        },
    });
    def
}

/// Sea clutter control with brand-specific wire hints
#[inline(never)]
pub fn control_sea_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_sea();
    def.wire_hints = Some(match brand {
        // Furuno uses 0-100 on wire (same as UI), no scaling needed
        Brand::Furuno => WireProtocolHint {
            has_auto: true,
            ..Default::default()
        },
        Brand::Navico | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            scale_factor: Some(255.0),
            has_auto: true,
            ..Default::default()
        },
    });
    def
}

/// Rain clutter control with brand-specific wire hints
#[inline(never)]
pub fn control_rain_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_rain();
    def.wire_hints = Some(match brand {
        // Furuno uses 0-100 on wire (same as UI), no scaling needed
        Brand::Furuno => WireProtocolHint {
            ..Default::default()
        },
        Brand::Navico | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            scale_factor: Some(255.0),
            ..Default::default()
        },
    });
    def
}

/// Bearing alignment control with brand-specific range and wire hints
pub fn control_bearing_alignment_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_bearing_alignment();

    // Set brand-specific range limits
    def.range = Some(match brand {
        // Navico: -180 to +180 degrees in 0.1 degree steps (wire uses deci-degrees 0-3599)
        Brand::Navico => RangeSpec {
            min: -180.0,
            max: 180.0,
            step: Some(0.1),
            unit: Some("degrees".into()),
        },
        // Furuno: -179 to +180 degrees in 0.1 degree steps (wire uses tenths)
        Brand::Furuno => RangeSpec {
            min: -179.0,
            max: 180.0,
            step: Some(0.1),
            unit: Some("degrees".into()),
        },
        // Raymarine/Garmin: -180 to +180 in 1 degree steps
        Brand::Raymarine | Brand::Garmin => RangeSpec {
            min: -180.0,
            max: 180.0,
            step: Some(1.0),
            unit: Some("degrees".into()),
        },
    });

    def.wire_hints = Some(match brand {
        // Navico: Wire protocol uses deci-degrees (0-3599), conversion done in report.rs
        Brand::Navico => WireProtocolHint {
            scale_factor: Some(3600.0), // 360 degrees * 10 = 3600 deci-degrees
            write_only: false,
            ..Default::default()
        },
        // Furuno: Wire protocol uses tenths of degrees
        Brand::Furuno => WireProtocolHint {
            scale_factor: Some(1800.0), // ±180 degrees * 10 = 1800 tenths
            write_only: false,
            ..Default::default()
        },
        // Raymarine/Garmin: degrees directly
        Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            write_only: false,
            ..Default::default()
        },
    });
    def
}

/// Antenna height control with brand-specific range and wire hints
pub fn control_antenna_height_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_antenna_height();

    // Set brand-specific range limits
    def.range = Some(match brand {
        // Navico: 0-99m in 0.1m steps (wire uses decimeters 0-990)
        Brand::Navico => RangeSpec {
            min: 0.0,
            max: 99.0,
            step: Some(0.1),
            unit: Some("m".into()),
        },
        // Furuno: 0-30m in 1m steps (integer meters)
        Brand::Furuno => RangeSpec {
            min: 0.0,
            max: 30.0,
            step: Some(1.0),
            unit: Some("m".into()),
        },
        // Raymarine: 0-30m in 0.1m steps
        Brand::Raymarine => RangeSpec {
            min: 0.0,
            max: 30.0,
            step: Some(0.1),
            unit: Some("m".into()),
        },
        // Garmin: 0-30m in 0.1m steps
        Brand::Garmin => RangeSpec {
            min: 0.0,
            max: 30.0,
            step: Some(0.1),
            unit: Some("m".into()),
        },
    });

    def.wire_hints = Some(match brand {
        // Navico: Wire protocol uses decimeters (0-990), control uses meters (0-99).
        // Conversion is done in report.rs when reading Report 04.
        Brand::Navico => WireProtocolHint {
            scale_factor: Some(990.0), // 99m * 10 = 990 decimeters
            write_only: false,
            ..Default::default()
        },
        Brand::Furuno | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            write_only: false,
            ..Default::default()
        },
    });
    def
}

/// Sidelobe suppression control with brand-specific wire hints
pub fn control_sidelobe_suppression_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_sidelobe_suppression();
    def.wire_hints = Some(match brand {
        Brand::Navico => WireProtocolHint {
            scale_factor: Some(255.0),
            has_auto: true,
            ..Default::default()
        },
        Brand::Furuno | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// Tune control with brand-specific wire hints
pub fn control_tune_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_tune();
    def.wire_hints = Some(match brand {
        Brand::Raymarine => WireProtocolHint {
            scale_factor: Some(255.0),
            has_auto: true,
            ..Default::default()
        },
        Brand::Furuno | Brand::Navico | Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// Color gain control with brand-specific wire hints
pub fn control_color_gain_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_color_gain();
    def.wire_hints = Some(match brand {
        Brand::Raymarine => WireProtocolHint {
            scale_factor: Some(100.0),
            has_auto: true,
            ..Default::default()
        },
        Brand::Furuno | Brand::Navico | Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// FTC control with brand-specific wire hints
pub fn control_ftc_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_ftc();
    def.wire_hints = Some(match brand {
        Brand::Raymarine => WireProtocolHint {
            scale_factor: Some(100.0),
            has_enabled: true,
            ..Default::default()
        },
        Brand::Garmin => WireProtocolHint {
            has_enabled: true,
            ..Default::default()
        },
        Brand::Furuno | Brand::Navico => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// Doppler speed control with brand-specific range and wire hints
pub fn control_doppler_speed_for_brand(brand: Brand) -> ControlDefinition {
    let mut def = control_doppler_speed();

    // Set brand-specific range limits
    def.range = Some(match brand {
        // Navico HALO: 0.5-16 knots in 0.5 knot steps
        Brand::Navico => RangeSpec {
            min: 0.5,
            max: 16.0,
            step: Some(0.5),
            unit: Some("knots".into()),
        },
        // Furuno NXT: 0.2-15 knots in 0.1 knot steps
        Brand::Furuno => RangeSpec {
            min: 0.2,
            max: 15.0,
            step: Some(0.1),
            unit: Some("knots".into()),
        },
        // Raymarine/Garmin: similar range
        Brand::Raymarine | Brand::Garmin => RangeSpec {
            min: 0.5,
            max: 20.0,
            step: Some(0.5),
            unit: Some("knots".into()),
        },
    });

    def.wire_hints = Some(match brand {
        Brand::Navico => WireProtocolHint {
            scale_factor: Some(99.0 * 16.0), // cm/s units
            step: Some(0.5),
            ..Default::default()
        },
        Brand::Furuno | Brand::Raymarine | Brand::Garmin => WireProtocolHint {
            ..Default::default()
        },
    });
    def
}

/// Get base control with brand-specific wire hints
#[inline(never)]
pub fn get_base_control_for_brand(id: &str, brand: Brand) -> Option<ControlDefinition> {
    match id {
        "power" => Some(control_power_for_brand(brand)),
        "gain" => Some(control_gain_for_brand(brand)),
        "sea" => Some(control_sea_for_brand(brand)),
        "rain" => Some(control_rain_for_brand(brand)),
        "serialNumber" => Some(control_serial_number()),
        "firmwareVersion" => Some(control_firmware_version()),
        "operatingHours" => Some(control_operating_hours()),
        "transmitHours" => Some(control_transmit_hours()),
        "rotationSpeed" => Some(control_rotation_speed_for_brand(brand)),
        _ => None,
    }
}

/// Get extended control with brand-specific wire hints
#[inline(never)]
pub fn get_extended_control_for_brand(id: &str, brand: Brand) -> Option<ControlDefinition> {
    match id {
        // Controls with brand-specific wire hints
        "bearingAlignment" => Some(control_bearing_alignment_for_brand(brand)),
        "antennaHeight" => Some(control_antenna_height_for_brand(brand)),
        "sidelobeSuppression" => Some(control_sidelobe_suppression_for_brand(brand)),
        "tune" => Some(control_tune_for_brand(brand)),
        "colorGain" => Some(control_color_gain_for_brand(brand)),
        "ftc" => Some(control_ftc_for_brand(brand)),
        "dopplerSpeed" => Some(control_doppler_speed_for_brand(brand)),
        // No-transmit zone angle controls
        "noTransmitStart1" => Some(control_no_transmit_angle_for_brand(id, 1, true, brand)),
        "noTransmitEnd1" => Some(control_no_transmit_angle_for_brand(id, 1, false, brand)),
        "noTransmitStart2" => Some(control_no_transmit_angle_for_brand(id, 2, true, brand)),
        "noTransmitEnd2" => Some(control_no_transmit_angle_for_brand(id, 2, false, brand)),
        "noTransmitStart3" => Some(control_no_transmit_angle_for_brand(id, 3, true, brand)),
        "noTransmitEnd3" => Some(control_no_transmit_angle_for_brand(id, 3, false, brand)),
        "noTransmitStart4" => Some(control_no_transmit_angle_for_brand(id, 4, true, brand)),
        "noTransmitEnd4" => Some(control_no_transmit_angle_for_brand(id, 4, false, brand)),
        // Furuno-specific controls
        "scanSpeed" if brand == Brand::Furuno => Some(control_scan_speed_furuno()),
        "interferenceRejection" if brand == Brand::Furuno => {
            Some(control_interference_rejection_furuno())
        }
        // Controls without brand-specific hints (use generic)
        _ => get_extended_control(id),
    }
}

/// Get any control (base or extended) with brand-specific wire hints
#[inline(never)]
pub fn get_control_for_brand(id: &str, brand: Brand) -> Option<ControlDefinition> {
    get_base_control_for_brand(id, brand).or_else(|| get_extended_control_for_brand(id, brand))
}

/// Base control IDs that all radars of a brand support (before model is known)
const BASE_CONTROL_IDS: &[&str] = &["power", "gain", "sea", "rain"];

/// Get all base controls for a brand as a vector of ControlDefinitions.
/// These are controls that exist before the model is known.
pub fn get_base_controls_for_brand(brand: Brand) -> Vec<ControlDefinition> {
    let mut controls = Vec::new();
    for id in BASE_CONTROL_IDS {
        if let Some(def) = get_base_control_for_brand(id, brand) {
            controls.push(def);
        }
    }
    controls
}

/// Get all controls for a brand and model as a vector of ControlDefinitions.
/// This includes base controls plus model-specific extended controls.
///
/// If model_name is None, only base controls are returned.
pub fn get_all_controls_for_model(
    brand: Brand,
    model_name: Option<&str>,
) -> Vec<ControlDefinition> {
    use crate::models;

    let mut controls = get_base_controls_for_brand(brand);

    // Add model-specific extended controls if model is known
    if let Some(name) = model_name {
        if let Some(model_info) = models::get_model(brand, name) {
            for control_id in model_info.controls {
                // Skip special compound controls
                if *control_id == super::ControlId::NoTransmitZones {
                    continue;
                }
                if let Some(def) = get_extended_control_for_brand(control_id.as_ref(), brand) {
                    controls.push(def);
                }
            }
        }
    }

    controls
}
