# SignalK Radar API v5 - Implementation Plan

> This document details the implementation steps for the v5 Radar API.
> See [feat_sk_radar_api.md](./feat_sk_radar_api.md) for the full specification.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        mayara-core                                   │
│  (Pure Rust, no I/O, compiles to native + WASM)                     │
├─────────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────┐  ┌──────────────────────────────────────┐ │
│  │  models/             │  │  capabilities/                       │ │
│  │  - ModelDatabase     │  │  - CapabilityManifest                │ │
│  │  - ModelInfo         │  │  - ControlDefinition                 │ │
│  │  - furuno.rs         │  │  - build_capabilities()              │ │
│  │  - navico.rs         │  │  - controls.rs (definitions)         │ │
│  └──────────────────────┘  └──────────────────────────────────────┘ │
│  ┌──────────────────────┐  ┌──────────────────────────────────────┐ │
│  │  protocol/ (existing)│  │  registry/                           │ │
│  │  - Beacon parsing    │  │  - RadarProvider trait               │ │
│  │  - Command formatting│  │  - ControlError enum                 │ │
│  └──────────────────────┘  └──────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────┘
          │                              │
          ▼                              ▼
┌─────────────────────┐      ┌─────────────────────────────┐
│   mayara-lib        │      │   mayara-signalk-wasm       │
│   (async, tokio)    │      │   (sync, poll-based)        │
│   - Async discovery │      │   - FFI exports (v5)        │
│   - Axum web server │      │   - RadarProvider impl      │
└─────────────────────┘      └─────────────────────────────┘
                                        │
                                        ▼
                             ┌─────────────────────────────┐
                             │   signalk-server            │
                             │   - v5 REST endpoints       │
                             │   - WASM bindings           │
                             └─────────────────────────────┘
```

---

## Phase 1: Types in mayara-core

**Goal:** Define v5 types and model database

### 1.1 Create capabilities module

**File:** `mayara-core/src/capabilities/mod.rs`

```rust
//! Radar Capability Types (v5 API)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Capability manifest returned by GET /radars/{id}/capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityManifest {
    pub id: String,
    pub make: String,
    pub model: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,

    pub characteristics: Characteristics,
    pub controls: Vec<ControlDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ControlConstraint>,
}

/// Hardware characteristics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Characteristics {
    pub max_range: u32,
    pub min_range: u32,
    pub supported_ranges: Vec<u32>,
    pub spokes_per_revolution: u16,
    pub max_spoke_length: u16,
    pub has_doppler: bool,
    pub has_dual_range: bool,
    pub no_transmit_zone_count: u8,
}

/// Control definition (schema, not value)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: ControlCategory,

    #[serde(rename = "type")]
    pub control_type: ControlType,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeSpec>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<EnumValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, PropertyDefinition>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlCategory {
    Base,
    Extended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlType {
    Boolean,
    Number,
    Enum,
    Compound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSpec {
    pub min: f64,
    pub max: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub value: serde_json::Value,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDefinition {
    #[serde(rename = "type")]
    pub prop_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<EnumValue>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlConstraint {
    pub control_id: String,
    pub condition: ConstraintCondition,
    pub effect: ConstraintEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintCondition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub depends_on: String,
    pub operator: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintEffect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// Re-export
pub mod builder;
pub mod controls;
```

### 1.2 Create controls definitions

**File:** `mayara-core/src/capabilities/controls.rs`

```rust
//! Standard control definitions for v5 API

use super::*;

/// Base control: power
pub fn control_power() -> ControlDefinition {
    ControlDefinition {
        id: "power".into(),
        name: "Power".into(),
        description: "Radar operational state".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: "off".into(), label: "Off".into(), description: Some("Radar powered off".into()) },
            EnumValue { value: "standby".into(), label: "Standby".into(), description: Some("Radar on, not transmitting".into()) },
            EnumValue { value: "transmit".into(), label: "Transmit".into(), description: Some("Radar transmitting".into()) },
            EnumValue { value: "warming".into(), label: "Warming Up".into(), description: Some("Magnetron warming (read-only)".into()) },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Base control: range
pub fn control_range(supported_ranges: &[u32]) -> ControlDefinition {
    let min = *supported_ranges.first().unwrap_or(&0) as f64;
    let max = *supported_ranges.last().unwrap_or(&100000) as f64;

    ControlDefinition {
        id: "range".into(),
        name: "Range".into(),
        description: "Detection range in meters".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Number,
        range: Some(RangeSpec {
            min,
            max,
            step: None,
            unit: Some("meters".into()),
        }),
        values: None,
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Base control: gain
pub fn control_gain() -> ControlDefinition {
    let mut properties = HashMap::new();
    properties.insert("mode".into(), PropertyDefinition {
        prop_type: "enum".into(),
        description: Some("Auto or manual control".into()),
        range: None,
        values: Some(vec![
            EnumValue { value: "auto".into(), label: "Auto".into(), description: None },
            EnumValue { value: "manual".into(), label: "Manual".into(), description: None },
        ]),
    });
    properties.insert("value".into(), PropertyDefinition {
        prop_type: "number".into(),
        description: Some("Gain level (0-100%)".into()),
        range: Some(RangeSpec { min: 0.0, max: 100.0, step: Some(1.0), unit: Some("percent".into()) }),
        values: None,
    });

    ControlDefinition {
        id: "gain".into(),
        name: "Gain".into(),
        description: "Signal amplification level".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
    }
}

/// Base control: sea clutter
pub fn control_sea() -> ControlDefinition {
    let mut properties = HashMap::new();
    properties.insert("mode".into(), PropertyDefinition {
        prop_type: "enum".into(),
        description: None,
        range: None,
        values: Some(vec![
            EnumValue { value: "auto".into(), label: "Auto".into(), description: None },
            EnumValue { value: "manual".into(), label: "Manual".into(), description: None },
        ]),
    });
    properties.insert("value".into(), PropertyDefinition {
        prop_type: "number".into(),
        description: Some("Sea clutter suppression (0-100%)".into()),
        range: Some(RangeSpec { min: 0.0, max: 100.0, step: Some(1.0), unit: Some("percent".into()) }),
        values: None,
    });

    ControlDefinition {
        id: "sea".into(),
        name: "Sea Clutter".into(),
        description: "Suppresses returns from waves".into(),
        category: ControlCategory::Base,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: Some(vec!["auto".into(), "manual".into()]),
        default_mode: Some("auto".into()),
        read_only: false,
    }
}

/// Base control: rain clutter
pub fn control_rain() -> ControlDefinition {
    ControlDefinition {
        id: "rain".into(),
        name: "Rain Clutter".into(),
        description: "Suppresses returns from precipitation".into(),
        category: ControlCategory::Base,
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
    }
}

// Extended controls

/// Beam sharpening (Furuno RezBoost, Navico Beam Sharpening)
pub fn control_beam_sharpening() -> ControlDefinition {
    ControlDefinition {
        id: "beamSharpening".into(),
        name: "Beam Sharpening".into(),
        description: "Digital beam narrowing for improved target separation".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: 0.into(), label: "Off".into(), description: None },
            EnumValue { value: 1.into(), label: "Low".into(), description: None },
            EnumValue { value: 2.into(), label: "Medium".into(), description: None },
            EnumValue { value: 3.into(), label: "High".into(), description: None },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Doppler mode (Target Analyzer, VelocityTrack)
pub fn control_doppler_mode() -> ControlDefinition {
    let mut properties = HashMap::new();
    properties.insert("enabled".into(), PropertyDefinition {
        prop_type: "boolean".into(),
        description: Some("Enable Doppler processing".into()),
        range: None,
        values: None,
    });
    properties.insert("mode".into(), PropertyDefinition {
        prop_type: "enum".into(),
        description: Some("Doppler display mode".into()),
        range: None,
        values: Some(vec![
            EnumValue { value: "approaching".into(), label: "Approaching Only".into(), description: None },
            EnumValue { value: "both".into(), label: "Both Directions".into(), description: None },
            EnumValue { value: "target".into(), label: "Target Mode".into(), description: Some("Furuno only".into()) },
            EnumValue { value: "rain".into(), label: "Rain Mode".into(), description: Some("Furuno only".into()) },
        ]),
    });

    ControlDefinition {
        id: "dopplerMode".into(),
        name: "Doppler Mode".into(),
        description: "Motion-based target highlighting".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Compound,
        range: None,
        values: None,
        properties: Some(properties),
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Bird mode
pub fn control_bird_mode() -> ControlDefinition {
    ControlDefinition {
        id: "birdMode".into(),
        name: "Bird Mode".into(),
        description: "Optimizes display for detecting bird flocks".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: 0.into(), label: "Off".into(), description: None },
            EnumValue { value: 1.into(), label: "Low".into(), description: None },
            EnumValue { value: 2.into(), label: "Medium".into(), description: None },
            EnumValue { value: 3.into(), label: "High".into(), description: None },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// TX Channel (Furuno)
pub fn control_tx_channel() -> ControlDefinition {
    ControlDefinition {
        id: "txChannel".into(),
        name: "TX Channel".into(),
        description: "Transmission frequency channel".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: 0.into(), label: "Auto".into(), description: None },
            EnumValue { value: 1.into(), label: "Channel 1".into(), description: None },
            EnumValue { value: 2.into(), label: "Channel 2".into(), description: None },
            EnumValue { value: 3.into(), label: "Channel 3".into(), description: None },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Interference rejection
pub fn control_interference_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "interferenceRejection".into(),
        name: "Interference Rejection".into(),
        description: "Filters interference from other radars".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: 0.into(), label: "Off".into(), description: None },
            EnumValue { value: 1.into(), label: "Low".into(), description: None },
            EnumValue { value: 2.into(), label: "Medium".into(), description: None },
            EnumValue { value: 3.into(), label: "High".into(), description: None },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}

/// Preset mode (Navico, Raymarine)
pub fn control_preset_mode() -> ControlDefinition {
    ControlDefinition {
        id: "presetMode".into(),
        name: "Preset Mode".into(),
        description: "Pre-configured operating modes".into(),
        category: ControlCategory::Extended,
        control_type: ControlType::Enum,
        range: None,
        values: Some(vec![
            EnumValue { value: "custom".into(), label: "Custom".into(), description: Some("Manual control".into()) },
            EnumValue { value: "harbor".into(), label: "Harbor".into(), description: Some("Busy port settings".into()) },
            EnumValue { value: "offshore".into(), label: "Offshore".into(), description: Some("Open water".into()) },
            EnumValue { value: "weather".into(), label: "Weather".into(), description: Some("Precipitation detection".into()) },
            EnumValue { value: "bird".into(), label: "Bird".into(), description: Some("Bird detection".into()) },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
    }
}
```

### 1.3 Create model database

**File:** `mayara-core/src/models/mod.rs`

```rust
//! Radar Model Database

use crate::Brand;

pub mod furuno;
pub mod navico;
pub mod raymarine;
pub mod garmin;

/// Static model information
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub brand: Brand,
    pub model: &'static str,
    pub family: &'static str,
    pub display_name: &'static str,

    // Hardware
    pub max_range: u32,
    pub min_range: u32,
    pub range_table: &'static [u32],
    pub spokes_per_revolution: u16,
    pub max_spoke_length: u16,

    // Features
    pub has_doppler: bool,
    pub has_dual_range: bool,
    pub no_transmit_zone_count: u8,

    // Available extended controls
    pub controls: &'static [&'static str],
}

/// Lookup model by brand and model string
pub fn get_model(brand: Brand, model: &str) -> Option<&'static ModelInfo> {
    match brand {
        Brand::Furuno => furuno::get_model(model),
        Brand::Navico => navico::get_model(model),
        Brand::Raymarine => raymarine::get_model(model),
        Brand::Garmin => garmin::get_model(model),
    }
}

/// Get all models for a brand
pub fn get_models_for_brand(brand: Brand) -> &'static [ModelInfo] {
    match brand {
        Brand::Furuno => furuno::MODELS,
        Brand::Navico => navico::MODELS,
        Brand::Raymarine => raymarine::MODELS,
        Brand::Garmin => garmin::MODELS,
    }
}

/// Unknown model fallback
pub static UNKNOWN_MODEL: ModelInfo = ModelInfo {
    brand: Brand::Furuno,
    model: "Unknown",
    family: "Unknown",
    display_name: "Unknown Radar",
    max_range: 74080,
    min_range: 100,
    range_table: &[463, 926, 1852, 3704, 7408, 14816, 29632, 59264],
    spokes_per_revolution: 2048,
    max_spoke_length: 512,
    has_doppler: false,
    has_dual_range: false,
    no_transmit_zone_count: 0,
    controls: &[],
};
```

**File:** `mayara-core/src/models/furuno.rs`

```rust
//! Furuno Radar Models

use super::ModelInfo;
use crate::Brand;

pub static MODELS: &[ModelInfo] = &[
    DRS4D_NXT,
    DRS6A_NXT,
    DRS12A_NXT,
    DRS25A_NXT,
];

pub static DRS4D_NXT: ModelInfo = ModelInfo {
    brand: Brand::Furuno,
    model: "DRS4D-NXT",
    family: "DRS-NXT",
    display_name: "Furuno DRS4D-NXT",
    max_range: 88896,  // 48nm
    min_range: 116,    // 1/16nm
    range_table: &[
        116,    // 1/16 nm
        231,    // 1/8 nm
        463,    // 1/4 nm
        926,    // 1/2 nm
        1389,   // 3/4 nm
        1852,   // 1 nm
        2778,   // 1.5 nm
        3704,   // 2 nm
        5556,   // 3 nm
        7408,   // 4 nm
        11112,  // 6 nm
        14816,  // 8 nm
        22224,  // 12 nm
        29632,  // 16 nm
        44448,  // 24 nm
        59264,  // 32 nm
        66672,  // 36 nm
        88896,  // 48 nm
    ],
    spokes_per_revolution: 2048,  // Reduced from 8192 for WebSocket
    max_spoke_length: 512,
    has_doppler: true,
    has_dual_range: true,
    no_transmit_zone_count: 2,
    controls: &["beamSharpening", "dopplerMode", "birdMode", "txChannel", "interferenceRejection"],
};

pub static DRS6A_NXT: ModelInfo = ModelInfo {
    brand: Brand::Furuno,
    model: "DRS6A-NXT",
    family: "DRS-NXT",
    display_name: "Furuno DRS6A-NXT",
    max_range: 133344,  // 72nm
    min_range: 116,
    range_table: &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408,
        11112, 14816, 22224, 29632, 44448, 59264, 66672, 88896, 133344,
    ],
    spokes_per_revolution: 2048,
    max_spoke_length: 512,
    has_doppler: true,
    has_dual_range: true,
    no_transmit_zone_count: 2,
    controls: &["beamSharpening", "dopplerMode", "birdMode", "txChannel", "interferenceRejection"],
};

pub static DRS12A_NXT: ModelInfo = ModelInfo {
    brand: Brand::Furuno,
    model: "DRS12A-NXT",
    family: "DRS-NXT",
    display_name: "Furuno DRS12A-NXT",
    max_range: 177792,  // 96nm
    min_range: 116,
    range_table: &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408,
        11112, 14816, 22224, 29632, 44448, 59264, 66672, 88896, 133344, 177792,
    ],
    spokes_per_revolution: 2048,
    max_spoke_length: 1024,
    has_doppler: true,
    has_dual_range: true,
    no_transmit_zone_count: 2,
    controls: &["beamSharpening", "dopplerMode", "birdMode", "txChannel", "interferenceRejection"],
};

pub static DRS25A_NXT: ModelInfo = ModelInfo {
    brand: Brand::Furuno,
    model: "DRS25A-NXT",
    family: "DRS-NXT",
    display_name: "Furuno DRS25A-NXT",
    max_range: 177792,  // 96nm
    min_range: 116,
    range_table: &[
        116, 231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408,
        11112, 14816, 22224, 29632, 44448, 59264, 66672, 88896, 133344, 177792,
    ],
    spokes_per_revolution: 2048,
    max_spoke_length: 1024,
    has_doppler: true,
    has_dual_range: true,
    no_transmit_zone_count: 2,
    controls: &["beamSharpening", "dopplerMode", "birdMode", "txChannel", "interferenceRejection"],
};

pub fn get_model(model: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.model == model)
}
```

### 1.4 Create capability builder

**File:** `mayara-core/src/capabilities/builder.rs`

```rust
//! Build CapabilityManifest from RadarDiscovery

use crate::radar::RadarDiscovery;
use crate::models::{self, ModelInfo};
use super::*;
use super::controls::*;

/// Build capability manifest for a discovered radar
pub fn build_capabilities(discovery: &RadarDiscovery, radar_id: &str) -> CapabilityManifest {
    let model_info = models::get_model(discovery.brand, discovery.model.as_deref().unwrap_or(""))
        .unwrap_or(&models::UNKNOWN_MODEL);

    CapabilityManifest {
        id: radar_id.to_string(),
        make: discovery.brand.as_str().to_string(),
        model: model_info.model.to_string(),
        model_family: Some(model_info.family.to_string()),
        serial_number: None,  // Could extract from discovery
        firmware_version: None,

        characteristics: Characteristics {
            max_range: model_info.max_range,
            min_range: model_info.min_range,
            supported_ranges: model_info.range_table.to_vec(),
            spokes_per_revolution: model_info.spokes_per_revolution,
            max_spoke_length: model_info.max_spoke_length,
            has_doppler: model_info.has_doppler,
            has_dual_range: model_info.has_dual_range,
            no_transmit_zone_count: model_info.no_transmit_zone_count,
        },

        controls: build_controls(model_info),
        constraints: build_constraints(model_info),
    }
}

fn build_controls(model: &ModelInfo) -> Vec<ControlDefinition> {
    let mut controls = vec![
        control_power(),
        control_range(model.range_table),
        control_gain(),
        control_sea(),
        control_rain(),
    ];

    // Add extended controls based on model
    for control_id in model.controls {
        if let Some(def) = get_extended_control(control_id) {
            controls.push(def);
        }
    }

    controls
}

fn get_extended_control(id: &str) -> Option<ControlDefinition> {
    match id {
        "beamSharpening" => Some(control_beam_sharpening()),
        "dopplerMode" => Some(control_doppler_mode()),
        "birdMode" => Some(control_bird_mode()),
        "txChannel" => Some(control_tx_channel()),
        "interferenceRejection" => Some(control_interference_rejection()),
        "presetMode" => Some(control_preset_mode()),
        _ => None,
    }
}

fn build_constraints(model: &ModelInfo) -> Vec<ControlConstraint> {
    let mut constraints = vec![];

    // If preset mode is available, add constraints for controls it locks
    if model.controls.contains(&"presetMode") {
        for locked in &["gain", "sea", "rain", "interferenceRejection"] {
            constraints.push(ControlConstraint {
                control_id: locked.to_string(),
                condition: ConstraintCondition {
                    condition_type: "read_only_when".into(),
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

    constraints
}
```

### 1.5 Update lib.rs exports

**File:** `mayara-core/src/lib.rs` (additions)

```rust
pub mod capabilities;
pub mod models;

// Re-exports
pub use capabilities::{CapabilityManifest, ControlDefinition, Characteristics};
pub use capabilities::builder::build_capabilities;
```

---

## Phase 2: WASM FFI Exports

**Goal:** Add v5 exports to mayara-signalk-wasm

### 2.1 Add new exports to lib.rs

**File:** `mayara-signalk-wasm/src/lib.rs` (additions)

```rust
// =============================================================================
// Radar Provider API v5
// =============================================================================

/// Return CapabilityManifest JSON for a radar
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn radar_get_capabilities(
    request_ptr: *const u8,
    request_len: usize,
    out_ptr: *mut u8,
    out_max_len: usize,
) -> i32 {
    let request_str = match parse_request(request_ptr, request_len) {
        Ok(s) => s,
        Err(_) => return write_string(r#"{"error":"invalid utf8"}"#, out_ptr, out_max_len),
    };

    #[derive(serde::Deserialize)]
    struct Request {
        #[serde(rename = "radarId")]
        radar_id: String,
    }

    let req: Request = match serde_json::from_str(&request_str) {
        Ok(r) => r,
        Err(_) => return write_string(r#"{"error":"invalid request"}"#, out_ptr, out_max_len),
    };

    unsafe {
        if let Some(ref provider) = PROVIDER {
            if let Some(caps) = provider.get_capabilities(&req.radar_id) {
                match serde_json::to_string(&caps) {
                    Ok(json) => write_string(&json, out_ptr, out_max_len),
                    Err(_) => write_string(r#"{"error":"serialize failed"}"#, out_ptr, out_max_len),
                }
            } else {
                write_string(r#"{"error":"radar not found"}"#, out_ptr, out_max_len)
            }
        } else {
            write_string(r#"{"error":"provider not initialized"}"#, out_ptr, out_max_len)
        }
    }
}

/// Return RadarState JSON (v5 format)
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn radar_get_state(
    request_ptr: *const u8,
    request_len: usize,
    out_ptr: *mut u8,
    out_max_len: usize,
) -> i32 {
    let request_str = match parse_request(request_ptr, request_len) {
        Ok(s) => s,
        Err(_) => return write_string(r#"{"error":"invalid utf8"}"#, out_ptr, out_max_len),
    };

    #[derive(serde::Deserialize)]
    struct Request {
        #[serde(rename = "radarId")]
        radar_id: String,
    }

    let req: Request = match serde_json::from_str(&request_str) {
        Ok(r) => r,
        Err(_) => return write_string(r#"{"error":"invalid request"}"#, out_ptr, out_max_len),
    };

    unsafe {
        if let Some(ref provider) = PROVIDER {
            if let Some(state) = provider.get_state_v5(&req.radar_id) {
                match serde_json::to_string(&state) {
                    Ok(json) => write_string(&json, out_ptr, out_max_len),
                    Err(_) => write_string(r#"{"error":"serialize failed"}"#, out_ptr, out_max_len),
                }
            } else {
                write_string(r#"{"error":"radar not found"}"#, out_ptr, out_max_len)
            }
        } else {
            write_string(r#"{"error":"provider not initialized"}"#, out_ptr, out_max_len)
        }
    }
}

/// Get single control value
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn radar_get_control(
    request_ptr: *const u8,
    request_len: usize,
    out_ptr: *mut u8,
    out_max_len: usize,
) -> i32 {
    let request_str = match parse_request(request_ptr, request_len) {
        Ok(s) => s,
        Err(_) => return write_string(r#"{"error":"invalid utf8"}"#, out_ptr, out_max_len),
    };

    #[derive(serde::Deserialize)]
    struct Request {
        #[serde(rename = "radarId")]
        radar_id: String,
        #[serde(rename = "controlId")]
        control_id: String,
    }

    let req: Request = match serde_json::from_str(&request_str) {
        Ok(r) => r,
        Err(_) => return write_string(r#"{"error":"invalid request"}"#, out_ptr, out_max_len),
    };

    unsafe {
        if let Some(ref provider) = PROVIDER {
            if let Some(value) = provider.get_control(&req.radar_id, &req.control_id) {
                match serde_json::to_string(&value) {
                    Ok(json) => write_string(&json, out_ptr, out_max_len),
                    Err(_) => write_string(r#"{"error":"serialize failed"}"#, out_ptr, out_max_len),
                }
            } else {
                write_string(r#"{"error":"control not found"}"#, out_ptr, out_max_len)
            }
        } else {
            write_string(r#"{"error":"provider not initialized"}"#, out_ptr, out_max_len)
        }
    }
}

/// Set single control value (v5 generic interface)
#[no_mangle]
#[allow(static_mut_refs)]
pub extern "C" fn radar_set_control(
    request_ptr: *const u8,
    request_len: usize,
    out_ptr: *mut u8,
    out_max_len: usize,
) -> i32 {
    let request_str = match parse_request(request_ptr, request_len) {
        Ok(s) => s,
        Err(_) => return write_string(r#"{"success":false,"error":"invalid utf8"}"#, out_ptr, out_max_len),
    };

    #[derive(serde::Deserialize)]
    struct Request {
        #[serde(rename = "radarId")]
        radar_id: String,
        #[serde(rename = "controlId")]
        control_id: String,
        value: serde_json::Value,
    }

    let req: Request = match serde_json::from_str(&request_str) {
        Ok(r) => r,
        Err(_) => return write_string(r#"{"success":false,"error":"invalid request"}"#, out_ptr, out_max_len),
    };

    debug(&format!("radar_set_control: {} {} {:?}", req.radar_id, req.control_id, req.value));

    unsafe {
        if let Some(ref mut provider) = PROVIDER {
            match provider.set_control(&req.radar_id, &req.control_id, &req.value) {
                Ok(()) => write_string(r#"{"success":true}"#, out_ptr, out_max_len),
                Err(e) => {
                    let error = format!(r#"{{"success":false,"error":"{}"}}"#, e);
                    write_string(&error, out_ptr, out_max_len)
                }
            }
        } else {
            write_string(r#"{"success":false,"error":"provider not initialized"}"#, out_ptr, out_max_len)
        }
    }
}
```

---

## Phase 3: Extend RadarProvider

**Goal:** Implement v5 methods in radar_provider.rs

### 3.1 Add v5 types

**File:** `mayara-signalk-wasm/src/radar_provider.rs` (additions)

```rust
use mayara_core::capabilities::{CapabilityManifest, build_capabilities};
use std::collections::BTreeMap;

/// v5 State format
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarStateV5 {
    pub id: String,
    pub timestamp: String,
    pub status: String,
    pub controls: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_controls: Vec<DisabledControl>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisabledControl {
    pub control_id: String,
    pub reason: String,
}

/// Control error type
#[derive(Debug)]
pub enum ControlError {
    RadarNotFound,
    ControlNotFound(String),
    InvalidValue(String),
    ControllerNotAvailable,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::RadarNotFound => write!(f, "Radar not found"),
            ControlError::ControlNotFound(id) => write!(f, "Control not found: {}", id),
            ControlError::InvalidValue(msg) => write!(f, "Invalid value: {}", msg),
            ControlError::ControllerNotAvailable => write!(f, "Controller not available"),
        }
    }
}
```

### 3.2 Implement v5 methods

**File:** `mayara-signalk-wasm/src/radar_provider.rs` (additions to impl RadarProvider)

```rust
impl RadarProvider {
    // ... existing methods ...

    /// V5: Get capability manifest
    pub fn get_capabilities(&self, radar_id: &str) -> Option<CapabilityManifest> {
        let radar = self.find_radar(radar_id)?;
        Some(build_capabilities(&radar.discovery, radar_id))
    }

    /// V5: Get current state
    pub fn get_state_v5(&self, radar_id: &str) -> Option<RadarStateV5> {
        let radar = self.find_radar(radar_id)?;
        let state = RadarState::from(&radar.discovery);

        let mut controls = BTreeMap::new();

        // Power
        controls.insert("power".into(), serde_json::json!(state.status));

        // Range (from controller if available)
        if let Some(controller) = self.furuno_controllers.get(radar_id) {
            controls.insert("range".into(), serde_json::json!(controller.get_range()));
            controls.insert("gain".into(), serde_json::json!({
                "mode": if controller.is_gain_auto() { "auto" } else { "manual" },
                "value": controller.get_gain()
            }));
            controls.insert("sea".into(), serde_json::json!({
                "mode": if controller.is_sea_auto() { "auto" } else { "manual" },
                "value": controller.get_sea()
            }));
            controls.insert("rain".into(), serde_json::json!(controller.get_rain()));
        }

        Some(RadarStateV5 {
            id: state.id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            status: state.status,
            controls,
            disabled_controls: vec![],
        })
    }

    /// V5: Get single control value
    pub fn get_control(&self, radar_id: &str, control_id: &str) -> Option<serde_json::Value> {
        let state = self.get_state_v5(radar_id)?;
        state.controls.get(control_id).cloned()
    }

    /// V5: Set single control value
    pub fn set_control(&mut self, radar_id: &str, control_id: &str, value: &serde_json::Value) -> Result<(), ControlError> {
        debug(&format!("set_control({}, {}, {:?})", radar_id, control_id, value));

        // Base controls
        match control_id {
            "power" => {
                let state = value.as_str()
                    .or_else(|| value.get("value").and_then(|v| v.as_str()))
                    .ok_or_else(|| ControlError::InvalidValue("power requires string value".into()))?;
                if self.set_power(radar_id, state) {
                    return Ok(());
                }
                return Err(ControlError::ControllerNotAvailable);
            }
            "range" => {
                let range = value.as_u64()
                    .or_else(|| value.get("value").and_then(|v| v.as_u64()))
                    .ok_or_else(|| ControlError::InvalidValue("range requires number".into()))? as u32;
                if self.set_range(radar_id, range) {
                    return Ok(());
                }
                return Err(ControlError::ControllerNotAvailable);
            }
            "gain" => {
                let auto = value.get("mode").and_then(|m| m.as_str()) == Some("auto");
                let val = value.get("value").and_then(|v| v.as_u64()).map(|v| v as u8);
                if self.set_gain(radar_id, auto, val) {
                    return Ok(());
                }
                return Err(ControlError::ControllerNotAvailable);
            }
            "sea" => {
                let auto = value.get("mode").and_then(|m| m.as_str()) == Some("auto");
                let val = value.get("value").and_then(|v| v.as_u64()).map(|v| v as u8);
                if self.set_sea(radar_id, auto, val) {
                    return Ok(());
                }
                return Err(ControlError::ControllerNotAvailable);
            }
            "rain" => {
                let auto = value.get("mode").and_then(|m| m.as_str()) == Some("auto");
                let val = value.get("value").and_then(|v| v.as_u64()).map(|v| v as u8);
                if self.set_rain(radar_id, auto, val) {
                    return Ok(());
                }
                return Err(ControlError::ControllerNotAvailable);
            }
            _ => {}
        }

        // Extended controls - dispatch by brand
        let radar = self.find_radar(radar_id).ok_or(ControlError::RadarNotFound)?;
        match radar.discovery.brand {
            mayara_core::Brand::Furuno => self.furuno_set_extended_control(radar_id, control_id, value),
            _ => Err(ControlError::ControlNotFound(control_id.to_string())),
        }
    }

    /// Furuno extended control dispatch
    fn furuno_set_extended_control(&mut self, radar_id: &str, control_id: &str, value: &serde_json::Value) -> Result<(), ControlError> {
        let controller = self.furuno_controllers.get_mut(radar_id)
            .ok_or(ControlError::ControllerNotAvailable)?;

        // Send announce before control
        self.locator.send_furuno_announce();

        match control_id {
            "beamSharpening" => {
                let val = value.as_u64()
                    .or_else(|| value.get("value").and_then(|v| v.as_u64()))
                    .ok_or_else(|| ControlError::InvalidValue("beamSharpening requires number".into()))? as u8;
                controller.set_rezboost(val);
                Ok(())
            }
            "birdMode" => {
                let val = value.as_u64()
                    .or_else(|| value.get("value").and_then(|v| v.as_u64()))
                    .ok_or_else(|| ControlError::InvalidValue("birdMode requires number".into()))? as u8;
                controller.set_bird_mode(val);
                Ok(())
            }
            "dopplerMode" => {
                let enabled = value.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
                let mode = value.get("mode").and_then(|m| m.as_str()).unwrap_or("target");
                controller.set_target_analyzer(enabled, mode);
                Ok(())
            }
            "txChannel" => {
                let val = value.as_u64()
                    .or_else(|| value.get("value").and_then(|v| v.as_u64()))
                    .ok_or_else(|| ControlError::InvalidValue("txChannel requires number".into()))? as u8;
                controller.set_tx_channel(val);
                Ok(())
            }
            "interferenceRejection" => {
                let val = value.as_u64()
                    .or_else(|| value.get("value").and_then(|v| v.as_u64()))
                    .ok_or_else(|| ControlError::InvalidValue("interferenceRejection requires number".into()))? as u8;
                controller.set_ir(val);
                Ok(())
            }
            _ => Err(ControlError::ControlNotFound(control_id.to_string())),
        }
    }
}
```

---

## Phase 4: SignalK Server Endpoints

**Goal:** Add v5 REST routes

### 4.1 Server route updates

**File:** `signalk-server/src/api/radar/index.ts` (additions)

```typescript
// V5 Endpoints

// GET /radars/{id}/capabilities
router.get('/radars/:id/capabilities', async (req, res) => {
  const { id } = req.params;
  try {
    const result = await wasmPlugin.call('radar_get_capabilities', { radarId: id });
    res.json(JSON.parse(result));
  } catch (e) {
    res.status(404).json({ error: 'Radar not found', id });
  }
});

// GET /radars/{id}/state
router.get('/radars/:id/state', async (req, res) => {
  const { id } = req.params;
  try {
    const result = await wasmPlugin.call('radar_get_state', { radarId: id });
    res.json(JSON.parse(result));
  } catch (e) {
    res.status(404).json({ error: 'Radar not found', id });
  }
});

// GET /radars/{id}/controls/{controlId}
router.get('/radars/:id/controls/:controlId', async (req, res) => {
  const { id, controlId } = req.params;
  try {
    const result = await wasmPlugin.call('radar_get_control', { radarId: id, controlId });
    res.json(JSON.parse(result));
  } catch (e) {
    res.status(404).json({ error: 'Control not found', controlId });
  }
});

// PUT /radars/{id}/controls/{controlId}
router.put('/radars/:id/controls/:controlId', async (req, res) => {
  const { id, controlId } = req.params;
  try {
    const result = await wasmPlugin.call('radar_set_control', {
      radarId: id,
      controlId,
      value: req.body.value ?? req.body
    });
    const parsed = JSON.parse(result);
    if (parsed.success) {
      res.json(parsed);
    } else {
      res.status(400).json(parsed);
    }
  } catch (e) {
    res.status(500).json({ success: false, error: String(e) });
  }
});

// Backward compatibility aliases
router.put('/radars/:id/power', (req, res) => {
  req.params.controlId = 'power';
  // Forward to generic handler
});
// ... etc for range, gain, sea, rain
```

---

## File Summary

| File | Action | Description |
|------|--------|-------------|
| `mayara-core/src/lib.rs` | Modify | Add `pub mod capabilities; pub mod models;` |
| `mayara-core/src/capabilities/mod.rs` | Create | v5 types (CapabilityManifest, ControlDefinition, etc.) |
| `mayara-core/src/capabilities/controls.rs` | Create | Base & extended control definitions |
| `mayara-core/src/capabilities/builder.rs` | Create | `build_capabilities()` function |
| `mayara-core/src/models/mod.rs` | Create | ModelDatabase, ModelInfo struct |
| `mayara-core/src/models/furuno.rs` | Create | Furuno model definitions |
| `mayara-core/src/models/navico.rs` | Create | Navico model definitions (stub) |
| `mayara-core/src/models/raymarine.rs` | Create | Raymarine model definitions (stub) |
| `mayara-core/src/models/garmin.rs` | Create | Garmin model definitions (stub) |
| `mayara-signalk-wasm/src/lib.rs` | Modify | Add v5 FFI exports |
| `mayara-signalk-wasm/src/radar_provider.rs` | Modify | Add v5 methods, ControlError |
| `signalk-server/src/api/radar/index.ts` | Modify | Add v5 endpoints |

---

## Testing Checklist

- [ ] `cargo build` passes for mayara-core
- [ ] `cargo build --target wasm32-unknown-unknown` passes for mayara-signalk-wasm
- [ ] `GET /radars` returns list with make/model
- [ ] `GET /radars/1/capabilities` returns valid CapabilityManifest
- [ ] `GET /radars/1/state` returns current control values
- [ ] `PUT /radars/1/controls/power` with `{"value": "transmit"}` works
- [ ] `PUT /radars/1/controls/range` with `{"value": 5556}` works
- [ ] `PUT /radars/1/controls/beamSharpening` with `{"value": 2}` works (Furuno)
- [ ] Legacy endpoints still work (`PUT /radars/1/power`)

---

## Next Steps After Implementation

1. Add Navico, Raymarine, Garmin model databases
2. Implement state change notifications (WebSocket)
3. Add constraint validation on server side
4. Update web UI to use capability-driven rendering
5. Document API for SignalK maintainers
