//! Radar Capability Types (v5 API)
//!
//! This module defines the types used by the SignalK Radar API v5.
//! The key concept is that providers declare their capabilities,
//! and clients use this schema to build dynamic UIs.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub mod builder;
pub mod controls;

/// Optional features a radar provider may implement.
///
/// These indicate what API features are available (provider capabilities),
/// NOT hardware capabilities (those are in `Characteristics`).
///
/// Example: A radar may have Doppler hardware (`characteristics.has_doppler = true`)
/// but the provider might not implement the trails API endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportedFeature {
    /// ARPA target tracking (GET/POST/DELETE /targets, WS /targets)
    Arpa,
    /// Guard zone alerting (GET/PUT /guardZones)
    GuardZones,
    /// Target history/trail data (GET /trails)
    Trails,
    /// Dual-range simultaneous display
    DualRange,
}

/// Capability manifest returned by GET /radars/{id}/capabilities
///
/// This is the complete schema for a radar, including hardware characteristics
/// and available controls. Clients should cache this and use it to build UIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityManifest {
    /// Radar ID (e.g., "1", "2")
    pub id: String,

    /// Persistent key for this radar (e.g., "Furuno-RD003212")
    /// Used for storing installation settings via Application Data API
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Radar manufacturer (e.g., "Furuno")
    pub make: String,

    /// Radar model (e.g., "DRS4D-NXT")
    pub model: String,

    /// Model family (e.g., "DRS-NXT")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,

    /// Serial number if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,

    /// Firmware version if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,

    /// Hardware characteristics
    pub characteristics: Characteristics,

    /// Available controls (schema only, no values)
    pub controls: Vec<ControlDefinition>,

    /// Control dependencies and constraints
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ControlConstraint>,

    /// Optional features this provider implements
    ///
    /// Indicates which optional API features are available:
    /// - `arpa`: ARPA target tracking (GET /targets, POST /targets, etc.)
    /// - `guardZones`: Guard zone alerting (GET /guardZones, etc.)
    /// - `trails`: Target trails/history (GET /trails)
    /// - `dualRange`: Dual-range simultaneous display
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_features: Vec<SupportedFeature>,
}

/// Hardware characteristics of the radar
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Characteristics {
    /// Maximum detection range in meters
    pub max_range: u32,

    /// Minimum detection range in meters
    pub min_range: u32,

    /// Discrete range values supported (in meters)
    pub supported_ranges: Vec<u32>,

    /// Number of spokes per antenna revolution
    pub spokes_per_revolution: u16,

    /// Maximum spoke length in samples
    pub max_spoke_length: u16,

    /// Whether Doppler processing is available
    pub has_doppler: bool,

    /// Whether dual-range display is supported
    pub has_dual_range: bool,

    /// Maximum range in dual-range mode (meters), 0 if not supported
    #[serde(skip_serializing_if = "is_zero")]
    pub max_dual_range: u32,

    /// Number of no-transmit zones supported
    pub no_transmit_zone_count: u8,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Control definition (schema, not value)
///
/// Describes a single control that can be read/written via the API.
/// Clients use this to generate appropriate UI controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    /// Semantic control ID (e.g., "gain", "beamSharpening")
    pub id: String,

    /// Human-readable name (e.g., "Gain")
    pub name: String,

    /// Description for tooltips
    pub description: String,

    /// Category: "base" (all radars) or "extended" (model-specific)
    pub category: ControlCategory,

    /// Control type determines UI widget
    #[serde(rename = "type")]
    pub control_type: ControlType,

    /// For number types: min, max, step, unit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeSpec>,

    /// For enum types: list of valid values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<EnumValue>>,

    /// For compound types: nested property definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, PropertyDefinition>>,

    /// Supported modes (e.g., ["auto", "manual"])
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<String>>,

    /// Default mode if modes are supported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,

    /// Whether this control is read-only
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,

    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,

    /// Wire protocol hints for server implementation (not serialized to API)
    #[serde(skip)]
    pub wire_hints: Option<WireProtocolHint>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Control category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlCategory {
    /// Base controls available on all radars
    Base,
    /// Extended controls specific to certain models
    Extended,
    /// Installation/setup controls (antenna height, bearing alignment, etc.)
    Installation,
}

/// Control type determines what UI widget to render
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlType {
    /// On/off toggle
    Boolean,
    /// Numeric value with range
    Number,
    /// Selection from fixed values
    Enum,
    /// Complex object with multiple properties
    Compound,
    /// Text value (typically read-only for info fields)
    String,
}

/// Range specification for number controls
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSpec {
    /// Minimum value
    pub min: f64,

    /// Maximum value
    pub max: f64,

    /// Step increment (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,

    /// Unit label (e.g., "percent", "meters")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// Enum value with label and optional description
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    /// The actual value (string or number)
    pub value: serde_json::Value,

    /// Human-readable label
    pub label: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Property definition for compound controls
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDefinition {
    /// Property type
    #[serde(rename = "type")]
    pub prop_type: String,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Range for number properties
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeSpec>,

    /// Values for enum properties
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<EnumValue>>,
}

/// Control constraint describing dependencies between controls
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlConstraint {
    /// The control being constrained
    pub control_id: String,

    /// Condition that triggers the constraint
    pub condition: ConstraintCondition,

    /// Effect when condition is met
    pub effect: ConstraintEffect,
}

/// Condition for a constraint
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintCondition {
    /// Type of condition
    #[serde(rename = "type")]
    pub condition_type: ConstraintType,

    /// Control that this depends on
    pub depends_on: String,

    /// Comparison operator
    pub operator: String,

    /// Value to compare against
    pub value: serde_json::Value,
}

/// Type of constraint condition
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintType {
    /// Control is disabled when condition is true
    DisabledWhen,
    /// Control is read-only when condition is true
    ReadOnlyWhen,
    /// Control values are restricted when condition is true
    RestrictedWhen,
}

/// Effect of a constraint when triggered
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintEffect {
    /// Whether control is disabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,

    /// Whether control is read-only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,

    /// Restricted set of allowed values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<serde_json::Value>>,

    /// Human-readable reason for the constraint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Wire protocol hints for server implementation
///
/// This is NOT part of the SignalK API - it's internal metadata used by
/// mayara-server to correctly encode/decode control values for the wire protocol.
/// Each radar brand may use different wire encodings for the same control.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProtocolHint {
    /// Scale factor: wire_value * (max / scale_factor) = user_value
    /// e.g., 255.0 for 8-bit values, 100.0 for percentage, 1800.0 for 0.1° precision
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f32>,

    /// Offset applied before scaling (for signed values like bearing alignment)
    /// e.g., -1.0 maps values > max to negative range
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<f32>,

    /// Step for rounding (0.1 for bearings, 1.0 for most controls)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f32>,

    /// Whether this control supports auto mode
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_auto: bool,

    /// Whether auto mode value can be adjusted (e.g., HALO sea clutter)
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_auto_adjustable: bool,

    /// Min adjustment for auto mode (e.g., -50)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_adjust_min: Option<f32>,

    /// Max adjustment for auto mode (e.g., +50)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_adjust_max: Option<f32>,

    /// Whether this control has an enabled/disabled toggle
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_enabled: bool,

    /// For enum controls: indices of values that can be set (others are read-only)
    /// e.g., [1, 2] for power control means only standby (1) and transmit (2) are settable
    /// If None, all values are settable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settable_indices: Option<Vec<i32>>,

    /// Whether commands should always be sent, even if value hasn't changed
    #[serde(default, skip_serializing_if = "is_false")]
    pub send_always: bool,

    /// Whether this control is write-only (cannot be read from hardware)
    /// Installation settings like bearingAlignment, antennaHeight, autoAcquire
    /// must be persisted by the client and restored on startup.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write_only: bool,
}

/// Radar state returned by GET /radars/{id}/state
///
/// Contains current values for all controls, plus metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarStateV5 {
    /// Radar ID
    pub id: String,

    /// ISO 8601 timestamp
    pub timestamp: String,

    /// Operational status
    pub status: String,

    /// Current control values (keyed by control ID)
    /// Uses BTreeMap for stable JSON key ordering
    pub controls: BTreeMap<String, serde_json::Value>,

    /// Controls currently disabled and why
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_controls: Vec<DisabledControl>,
}

/// Information about a disabled control
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisabledControl {
    /// Control ID
    pub control_id: String,

    /// Reason for being disabled
    pub reason: String,
}

/// Error type for control operations
#[derive(Debug, Clone)]
pub enum ControlError {
    /// Radar not found
    RadarNotFound,
    /// Control not found on this radar
    ControlNotFound(String),
    /// Invalid value for control
    InvalidValue(String),
    /// Controller not available (e.g., TCP not connected)
    ControllerNotAvailable,
    /// Control is disabled
    ControlDisabled(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::RadarNotFound => write!(f, "Radar not found"),
            ControlError::ControlNotFound(id) => write!(f, "Control not found: {}", id),
            ControlError::InvalidValue(msg) => write!(f, "Invalid value: {}", msg),
            ControlError::ControllerNotAvailable => write!(f, "Controller not available"),
            ControlError::ControlDisabled(reason) => write!(f, "Control disabled: {}", reason),
        }
    }
}

impl std::error::Error for ControlError {}
