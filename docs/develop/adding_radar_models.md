# Adding New Radar Models

This guide explains how to add support for new radar models in Mayara.

## Overview

Adding a new radar model involves three steps:

1. **Document the protocol** in `docs/radar-protocols/{brand}/protocol.md`
2. **Add the model** to the database in `mayara-core/src/models/{brand}.rs`
3. **Add any new controls** to `mayara-core/src/capabilities/controls.rs`

## Step 1: Document the Protocol

Before adding code, document the radar's network protocol in the appropriate protocol file.

### Location

```
mayara/docs/radar-protocols/
├── furuno/protocol.md
├── navico/protocol.md
├── raymarine/protocol.md
└── garmin/protocol.md
```

### What to Document

For each new model, document:

1. **Network ports** - UDP/TCP ports used for discovery, control, and data
2. **Discovery protocol** - Beacon packets, model identification
3. **Command format** - How to send commands (ASCII, binary, etc.)
4. **Command reference** - All supported commands with parameters
5. **Response format** - How the radar responds
6. **Model-specific differences** - How this model differs from others in the family

### Example Entry

```markdown
## Model: DRS4D-NXT

### Identification

The DRS4D-NXT is identified via the TCP $N96 response:
- Part code: 0359360

### Model-Specific Features

- Has Doppler (Target Analyzer)
- Supports dual-range mode up to 12nm
- 2 no-transmit zones
- RezBoost (beam sharpening)
- Bird Mode

### Commands Supported

| Command | Supported | Notes |
|---------|-----------|-------|
| RezBoost (0xEE) | Yes | 0-3 levels |
| Target Analyzer (0xEF) | Yes | enabled + mode |
| Bird Mode (0xED) | Yes | 0-3 levels |
```

## Step 2: Add the Model to the Database

### File Location

```
mayara-core/src/models/{brand}.rs
```

### Model Definition Structure

Each model is defined as a `ModelInfo` struct:

```rust
ModelInfo {
    brand: Brand::Furuno,
    model: "DRS4D-NXT",           // Exact model name
    family: "DRS-NXT",            // Model family for grouping
    display_name: "Furuno DRS4D-NXT",
    max_range: 88896,             // Maximum range in meters
    min_range: 116,               // Minimum range in meters
    range_table: RANGE_TABLE_NXT, // Reference to range table
    spokes_per_revolution: 2048,  // Spokes per antenna rotation
    max_spoke_length: 1024,       // Maximum samples per spoke
    has_doppler: true,            // Doppler/motion detection
    has_dual_range: true,         // Dual-range display support
    max_dual_range: 22224,        // Max range in dual mode (0 if unsupported)
    no_transmit_zone_count: 2,    // Number of sector blanking zones
    controls: CONTROLS_NXT,       // Reference to controls list
}
```

### Adding a New Model

1. **Add range table** (if new):

```rust
/// Range table for MY-NEW-SERIES (in meters)
static RANGE_TABLE_MYNEW: &[u32] = &[
    116,    // 1/16 NM
    231,    // 1/8 NM
    // ... add all supported ranges
];
```

2. **Add controls list** (if new control set):

```rust
/// Extended controls available on MY-NEW-SERIES
static CONTROLS_MYNEW: &[&str] = &[
    "beamSharpening",
    "dopplerMode",
    "interferenceRejection",
    // ... list all extended control IDs
];
```

3. **Add the model entry** to the `MODELS` array:

```rust
pub static MODELS: &[ModelInfo] = &[
    // ... existing models ...

    // MY-NEW-SERIES
    ModelInfo {
        brand: Brand::Furuno,
        model: "MY-NEW-4D",
        family: "MY-NEW",
        display_name: "Furuno MY-NEW-4D",
        max_range: 88896,
        min_range: 116,
        range_table: RANGE_TABLE_MYNEW,
        spokes_per_revolution: 2048,
        max_spoke_length: 1024,
        has_doppler: true,
        has_dual_range: false,
        max_dual_range: 0,
        no_transmit_zone_count: 2,
        controls: CONTROLS_MYNEW,
    },
];
```

4. **Add a test**:

```rust
#[test]
fn test_my_new_4d() {
    let model = get_model("MY-NEW-4D").unwrap();
    assert_eq!(model.family, "MY-NEW");
    assert!(model.has_doppler);
}
```

## Step 3: Add New Controls (If Needed)

If the radar has controls not already defined in the system, add them.

### Check Existing Controls First

Before creating a new control, check if an equivalent semantic ID exists in:
- `mayara-core/src/capabilities/controls.rs`
- `docs/radar-API/signalk_radar_api_naming.md`

### Naming Convention

Use **semantic naming** - name the control by what it **does**, not the vendor's marketing name:

| Vendor Name | Semantic ID |
|-------------|-------------|
| Furuno "RezBoost" | `beamSharpening` |
| Navico "VelocityTrack" | `dopplerMode` |
| Furuno "Target Analyzer" | `dopplerMode` |

See `docs/radar-API/signalk_radar_api_naming.md` for the full naming guide.

### Adding a New Control

1. **Add factory function** in `controls.rs`:

```rust
/// New feature description
///
/// Furuno: Vendor Name
/// Navico: Other Vendor Name
pub fn control_my_new_feature() -> ControlDefinition {
    ControlDefinition {
        id: "myNewFeature".into(),           // camelCase
        name: "My New Feature".into(),        // Title Case
        description: "What this control does.".into(),
        category: ControlCategory::Extended,  // or Base, Installation
        control_type: ControlType::Enum,      // Boolean, Number, Enum, Compound
        range: None,
        values: Some(vec![
            EnumValue {
                value: 0.into(),
                label: "Off".into(),
                description: None,
            },
            EnumValue {
                value: 1.into(),
                label: "On".into(),
                description: None,
            },
        ]),
        properties: None,
        modes: None,
        default_mode: None,
        read_only: false,
        default: Some(0.into()),
    }
}
```

2. **Register in `get_extended_control()`** (or `get_extended_control_for_brand()` if the control needs brand-specific wire hints):

```rust
// In get_extended_control() for brand-independent controls:
pub fn get_extended_control(id: &str) -> Option<ControlDefinition> {
    match id {
        // ... existing controls ...
        "myNewFeature" => Some(control_my_new_feature()),
        _ => None,
    }
}

// OR in get_extended_control_for_brand() for brand-specific wire hints:
pub fn get_extended_control_for_brand(id: &str, brand: Brand) -> Option<ControlDefinition> {
    match id {
        "myNewFeature" => Some(control_my_new_feature_for_brand(brand)),
        // Falls back to get_extended_control() for other controls
        _ => get_extended_control(id),
    }
}
```

3. **Update naming documentation** in `docs/radar-API/signalk_radar_api_naming.md`

## Control Types Reference

| Type | Use When | Example |
|------|----------|---------|
| `Boolean` | Simple on/off toggle | `noiseReduction` |
| `Number` | Continuous value with range | `mainBangSuppression` (0-100%) |
| `Enum` | Discrete choices | `beamSharpening` (Off/Low/Med/High) |
| `Compound` | Multiple properties | `gain` (mode + value) |

## Control Categories

| Category | Description | UI Placement |
|----------|-------------|--------------|
| `Base` | All radars have these | Main controls |
| `Extended` | Model-specific features | Additional controls |
| `Installation` | Configuration settings | Setup/config panel |

## Testing Your Changes

1. **Run unit tests**:
```bash
cd mayara-core
cargo test
```

2. **Run the server** and test with GUI:
```bash
cd mayara-server
cargo run
# Open http://localhost:6502 in browser
```

3. **Test with actual hardware** - verify:
   - Model is correctly identified
   - Capabilities manifest includes new controls
   - Controls work as expected
   - State queries return correct values

## Checklist

Before submitting:

- [ ] Protocol documented in `docs/radar-protocols/{brand}/protocol.md`
- [ ] Model added to `mayara-core/src/models/{brand}.rs`
- [ ] Range table defined (if new ranges)
- [ ] Controls list defined with correct semantic IDs
- [ ] New controls added to `controls.rs` (if any)
- [ ] New controls registered in `get_extended_control()`
- [ ] Naming follows `signalk_radar_api_naming.md` conventions
- [ ] Unit tests added
- [ ] Tests pass

## File Reference

| File | Purpose |
|------|---------|
| `docs/radar-protocols/{brand}/protocol.md` | Protocol documentation |
| `mayara-core/src/models/mod.rs` | Model database entry point |
| `mayara-core/src/models/{brand}.rs` | Brand-specific model definitions |
| `mayara-core/src/capabilities/mod.rs` | CapabilityManifest and SupportedFeature types |
| `mayara-core/src/capabilities/controls.rs` | Control definitions |
| `mayara-core/src/capabilities/builder.rs` | Capability manifest builder |
| `docs/radar-API/signalk_radar_api_naming.md` | Naming conventions |

## Understanding CapabilityManifest and SupportedFeatures

When clients connect to a radar, they request its **CapabilityManifest** - a complete schema describing what the radar can do. This is built automatically from the model database, but there's an important distinction between **hardware capabilities** and **provider features**.

### Hardware Capabilities vs Provider Features

The `CapabilityManifest` contains two different types of capability information:

1. **`characteristics`** - What the **hardware** can do (from ModelInfo)
2. **`supportedFeatures`** - What the **provider** implements (passed at runtime)

```rust
// Hardware capabilities (from model database)
characteristics: Characteristics {
    has_doppler: true,      // Hardware supports Doppler
    has_dual_range: true,   // Hardware supports dual-range
    // ...
}

// Provider features (what API endpoints are implemented)
supported_features: vec![
    SupportedFeature::Arpa,        // Provider implements /targets endpoint
    SupportedFeature::GuardZones,  // Provider implements /guardZones endpoint
]
```

**Why the distinction?**

A radar may have Doppler hardware (`has_doppler: true`), but the provider might not implement the `/trails` endpoint. Clients need to know both:
- Can the hardware do it? → Check `characteristics`
- Did the provider implement it? → Check `supportedFeatures`

### Available SupportedFeature Values

```rust
pub enum SupportedFeature {
    Arpa,        // ARPA target tracking (GET/POST/DELETE /targets)
    GuardZones,  // Guard zone alerting (GET/PUT /guardZones)
    Trails,      // Target history/trails (GET /trails)
    DualRange,   // Dual-range simultaneous display
}
```

### How Capabilities Are Built

The capability manifest is built by the `build_capabilities()` or `build_capabilities_from_model()` functions:

```rust
use mayara_core::capabilities::{builder::build_capabilities, SupportedFeature};

// Build capabilities with supported features
let caps = build_capabilities(
    &discovery,           // RadarDiscovery from network
    radar_id,             // Radar identifier
    vec![                 // Features this provider implements
        SupportedFeature::Arpa,
    ],
);
```

The builder:
1. Looks up the radar model in the database
2. Copies hardware characteristics from `ModelInfo`
3. Builds control definitions based on `ModelInfo.controls`
4. Adds the `supported_features` you pass in

### When Adding a New Model

When you add a new model, you only need to:

1. **Add the ModelInfo** with correct hardware capabilities (`has_doppler`, `has_dual_range`, etc.)
2. **List the controls** the model supports

You do **not** need to specify `supportedFeatures` in the model - that's determined by the provider at runtime based on what API endpoints it implements.

### For Provider Developers

If you're building a new provider (not just adding a model), you decide which features to announce:

```rust
// mayara-server standalone - announces ARPA support
let supported_features = vec![SupportedFeature::Arpa];

// A minimal provider might not implement any optional features
let supported_features = vec![];

// A full-featured provider might implement everything
let supported_features = vec![
    SupportedFeature::Arpa,
    SupportedFeature::GuardZones,
    SupportedFeature::Trails,
];
```

See `mayara-server/src/web.rs` for an example.

## Examples

### Adding a Simple Model (Same Family)

If adding a model to an existing family with the same features:

```rust
// Just add the entry with different ranges
ModelInfo {
    brand: Brand::Furuno,
    model: "DRS6A-NXT",        // Different model name
    family: "DRS-NXT",         // Same family
    display_name: "Furuno DRS6A-NXT",
    max_range: 88896,          // Same or different
    // ... same controls reference
    controls: CONTROLS_NXT,    // Reuse existing controls
}
```

### Adding a Model with Different Controls

If the model has a different feature set:

```rust
// 1. Define new controls list
static CONTROLS_MY_VARIANT: &[&str] = &[
    "interferenceRejection",
    // Fewer features than full NXT
];

// 2. Add model with new controls reference
ModelInfo {
    brand: Brand::Furuno,
    model: "DRS4D-BASIC",
    family: "DRS",
    controls: CONTROLS_MY_VARIANT,
    has_doppler: false,        // Different capabilities
    // ...
}
```

### Adding a Completely New Brand

1. Create `mayara-core/src/models/newbrand.rs`
2. Add `pub mod newbrand;` to `mayara-core/src/models/mod.rs`
3. Add `Brand::NewBrand` to `mayara-core/src/brand.rs`
4. Update `get_model()` and `get_models_for_brand()` in `mod.rs`
5. Create protocol documentation in `docs/radar-protocols/newbrand/`
