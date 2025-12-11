# SignalK Radar API v4 Proposal

## Overview

This proposal extends the SignalK radar REST API to provide a unified interface for controlling
marine radars from multiple vendors while preserving access to vendor-specific features. The design
is informed by protocol analysis of Furuno, Navico, Raymarine, and Garmin radar implementations.

## Design Goals

1. **Common API surface** - Uniform endpoints for controls shared across all vendors
2. **Vendor extensibility** - Namespaced endpoints for vendor-specific features
3. **Discoverability** - Capabilities endpoint for runtime feature detection
4. **Documentation** - Human-readable explanations for controls and their values
5. **Real-time state** - WebSocket subscription for control state changes
6. **State awareness** - Support for disabled/read-only controls in preset modes

## Base Path

All radar endpoints are under:
```
/signalk/v2/api/vessels/self/radars/{radarId}
```

---

## Common Controls API

These endpoints are available on ALL radars regardless of vendor.

### Minimum Common Set

| Control | Endpoint | Description |
|---------|----------|-------------|
| Power | `PUT /power` | Transmit state: off, standby, transmit |
| Range | `PUT /range` | Detection range in meters |
| Gain | `PUT /gain` | Signal amplification |
| Sea Clutter | `PUT /sea` | Sea return suppression |
| Rain Clutter | `PUT /rain` | Rain/precipitation suppression |

### Extended Common Controls

These are supported by most (but not all) radars:

| Control | Endpoint | Supported By |
|---------|----------|--------------|
| Interference Rejection | `PUT /interferenceRejection` | Furuno, Navico, Raymarine, Garmin |
| Target Expansion | `PUT /targetExpansion` | Navico, Raymarine |
| Bearing Alignment | `PUT /bearingAlignment` | All |
| Antenna Height | `PUT /antennaHeight` | Furuno, Navico |
| No Transmit Zones | `PUT /noTransmitZones` | Furuno, Navico, Garmin |
| Scan Speed | `PUT /scanSpeed` | Furuno, Navico |

**Note**: Features listed are based on protocol analysis. Some may require further verification.

### Common Control Schemas

#### Power State
```json
PUT /radars/{id}/power
{
  "value": "transmit"  // "off" | "standby" | "transmit" | "warming_up" (read-only)
}
```

#### Range
```json
PUT /radars/{id}/range
{
  "value": 5000  // meters (API always uses meters, plugin converts to vendor-specific indices)
}
```

#### Gain Control
```json
PUT /radars/{id}/gain
{
  "mode": "auto",     // "manual" | "auto"
  "value": 50,        // 0-100 (percentage), only used when mode=manual
  "autoLevel": "high" // "low" | "high" (only for auto mode, optional)
}
```

#### Sea Clutter
```json
PUT /radars/{id}/sea
{
  "mode": "auto",     // "manual" | "auto"
  "value": 30,        // 0-100 (percentage)
  "autoLevel": "calm" // vendor-specific: "calm" | "moderate" | "rough"
}
```

#### Rain Clutter
```json
PUT /radars/{id}/rain
{
  "mode": "manual",
  "value": 25
}
```

#### No Transmit Zones
```json
PUT /radars/{id}/noTransmitZones
{
  "zones": [
    { "enabled": true, "start": 90, "end": 180 },
    { "enabled": false, "start": 0, "end": 0 }
  ]
}
```
Note: Number of zones varies by vendor (Furuno: 2, Navico: 2-4, Garmin: 1). The API always uses start/end angles; the plugin handles any vendor-specific conversions internally.

---

## Dual Display / Dual Scan Support

Some radars support displaying two independent range views simultaneously. The API supports this via a `screen` parameter on per-screen controls.

### Furuno Dual Scan

DRS-NXT radars support dual scan mode with two independent displays (max 12nm / 22224m each).

| Control | Behavior |
|---------|----------|
| Range | Per-screen |
| Power/Status | Per-screen |
| RezBoost | Per-screen |
| Gain | Universal (affects both) |
| Sea Clutter | Universal (affects both) |
| Rain Clutter | Universal (affects both) |
| Bird Mode | Universal (affects both) |
| Target Analyzer | Universal (affects both) |
| Int. Rejection | Universal (affects both) |
| TX Channel | Universal (single transmitter) |

#### Per-Screen Control Schema
```json
PUT /radars/{id}/range
{
  "value": 5000,
  "screen": 0  // 0=primary, 1=secondary (optional, defaults to 0)
}
```

### Navico Dual Range

Both 4G and HALO radars support dual range display with two independent views.

| Model | Dual Range | Max Range per Screen |
|-------|------------|---------------------|
| BR24 | No | - |
| 3G | No | - |
| 4G | Yes | Full range |
| HALO | Yes | Full range |

| Control | Behavior |
|---------|----------|
| Range | Per-screen (via separate multicast addresses) |
| Power/Status | Per-screen |
| Gain | Universal (affects both) |
| Sea Clutter | Universal (affects both) |
| Rain Clutter | Universal (affects both) |
| Mode Preset | Universal (affects both) |
| Doppler | Universal (HALO only) |

**Note:** Navico dual range uses separate multicast addresses for each radar channel (A and B), discovered via the beacon packet. Each channel requires its own keep-alive messages.

---

## Vendor-Specific Controls API

Vendor-specific features are namespaced under the vendor name.

### Endpoint Pattern
```
PUT /radars/{id}/{vendor}/{control}
GET /radars/{id}/{vendor}/{control}
```

### Furuno-Specific Controls

| Control | Endpoint | Description |
|---------|----------|-------------|
| RezBoost | `PUT /furuno/rezboost` | Beam sharpening (0=off, 1-3=levels) |
| Bird Mode | `PUT /furuno/birdMode` | Optimizes for bird detection (0=off, 1-3=levels) |
| Target Analyzer | `PUT /furuno/targetAnalyzer` | Doppler-based threat highlighting (target/rain modes) |
| TX Channel | `PUT /furuno/txChannel` | Transmit frequency channel (auto, 1-3) |
| Auto Acquire | `PUT /furuno/autoAcquire` | ARPA auto target acquisition by Doppler |
| Main Bang | `PUT /furuno/mainBang` | Suppresses transmitter pulse (0-100%) |
| Echo Trail | `PUT /furuno/echoTrail` | Historical echo persistence (unverified) |
| Fast Target Tracking | `PUT /furuno/fastTargetTracking` | Quick target acquisition (unverified) |

#### RezBoost Schema
```json
PUT /radars/{id}/furuno/rezboost
{
  "value": 2  // 0=off, 1=low, 2=medium, 3=max
}
```

#### Target Analyzer Schema
```json
PUT /radars/{id}/furuno/targetAnalyzer
{
  "enabled": true,
  "mode": "target"  // "target" | "rain"
}
```

#### Bird Mode Schema
```json
PUT /radars/{id}/furuno/birdMode
{
  "value": 2  // 0=off, 1=low, 2=medium, 3=high
}
```

#### TX Channel Schema
```json
PUT /radars/{id}/furuno/txChannel
{
  "value": 0  // 0=auto, 1=ch1, 2=ch2, 3=ch3
}
```

#### Auto Acquire Schema
```json
PUT /radars/{id}/furuno/autoAcquire
{
  "enabled": true  // ARPA auto acquire by Doppler
}
```

### Navico-Specific Controls (Simrad/Lowrance/B&G)

| Control | Endpoint | Description |
|---------|----------|-------------|
| Doppler Mode | `PUT /navico/doppler` | VelocityTrack target motion |
| Doppler Speed | `PUT /navico/dopplerSpeed` | Minimum speed threshold |
| Mode Preset | `PUT /navico/mode` | Harbor/Offshore/Weather/Bird/Custom |
| Target Boost | `PUT /navico/targetBoost` | Enhances weak targets |
| Target Separation | `PUT /navico/targetSeparation` | Distinguishes close targets |
| Noise Rejection | `PUT /navico/noiseRejection` | Digital noise filtering |
| Sidelobe Suppression | `PUT /navico/sidelobeSuppression` | Reduces antenna side lobes |
| Sea State | `PUT /navico/seaState` | Auto sea clutter calibration |
| Accent Light | `PUT /navico/accentLight` | Scanner status LED (HALO) |
| Local IR | `PUT /navico/localInterferenceRejection` | Nearby radar filtering |
| Antenna Size | `PUT /navico/antennaSize` | Antenna size setting (HALO) |
| Parking Angle | `PUT /navico/parkingAngle` | Standby antenna position (HALO) |

#### Antenna Size Schema (HALO only)
```json
PUT /radars/{id}/navico/antennaSize
{
  "value": 3  // Antenna size index (model-dependent)
}
```

#### Parking Angle Schema (HALO only)
```json
PUT /radars/{id}/navico/parkingAngle
{
  "value": 180  // Degrees (0-359), where antenna stops in standby
}
```

#### Doppler Mode Schema
```json
PUT /radars/{id}/navico/doppler
{
  "mode": "approaching"  // "off" | "both" | "approaching"
}
```

#### Mode Preset Schema
```json
PUT /radars/{id}/navico/mode
{
  "value": "harbor"  // "custom" | "harbor" | "offshore" | "weather" | "bird"
}
```

### Raymarine-Specific Controls

| Control | Endpoint | Description |
|---------|----------|-------------|
| Doppler | `PUT /raymarine/doppler` | Motion detection (Q24D, Cyclone) |
| Color Gain | `PUT /raymarine/colorGain` | Echo color intensity (Quantum) |
| Mode Preset | `PUT /raymarine/mode` | Harbor/Coastal/Offshore/Weather |
| Display Timing | `PUT /raymarine/displayTiming` | Radar timing adjustment (RD series) |
| Fast Scan | `PUT /raymarine/fastScan` | High-speed rotation mode |
| FTC | `PUT /raymarine/ftc` | Fast Time Constant (RD series) |
| Tune | `PUT /raymarine/tune` | Magnetron tuning (RD series) |
| Main Bang | `PUT /raymarine/mainBang` | Transmitter pulse suppression (RD series) |

#### Mode Preset Schema (Quantum)
```json
PUT /radars/{id}/raymarine/mode
{
  "value": "harbor"  // "harbor" | "coastal" | "offshore" | "weather"
}
```

#### FTC Schema (RD series only)
```json
PUT /radars/{id}/raymarine/ftc
{
  "enabled": true,
  "value": 50  // 0-100
}
```

#### Tune Schema (RD series magnetron radars)
```json
PUT /radars/{id}/raymarine/tune
{
  "mode": "auto",  // "manual" | "auto"
  "value": 50      // 0-100 (manual mode only)
}
```

### Garmin-Specific Controls

| Control | Endpoint | Description |
|---------|----------|-------------|
| Crosstalk Rejection | `PUT /garmin/crosstalkRejection` | Multi-radar interference |
| Timed Idle | `PUT /garmin/timedIdle` | Power-saving transmit cycling |
| Auto Gain Level | `PUT /garmin/autoGainLevel` | High/Low auto sensitivity |
| FTC | `PUT /garmin/ftc` | Fast Time Constant (HD series only) |

#### FTC Schema (Garmin HD only, not xHD)
```json
PUT /radars/{id}/garmin/ftc
{
  "value": 50  // 0-100
}
```
Note: FTC is only available on legacy Garmin HD radars, not the xHD series.

#### Timed Idle Schema
```json
PUT /radars/{id}/garmin/timedIdle
{
  "enabled": true,
  "idleTime": 300,    // seconds
  "runTime": 60       // seconds
}
```

---

## Capabilities Endpoint

Provides runtime discovery of radar features.

### Request
```
GET /radars/{id}/capabilities
```

### Response Schema
```json
{
  "id": "radar-0",
  "vendor": "furuno",
  "model": "DRS4D-NXT",
  "modelFamily": "DRS-NXT",
  "serialNumber": "6424",
  "firmwareVersion": "01.01:01.01:01.05:01.05",

  "characteristics": {
    "spokesPerRevolution": 2048,
    "maxSpokeLength": 512,
    "pixelDepth": 4,
    "hasDoppler": true,
    "hasDualScan": true,
    "maxRange": 66672,        // meters (36nm)
    "maxDualScanRange": 22224, // meters (12nm)
    "antennaSize": "24inch",
    "power": "4kW",
    "noTransmitZoneCount": 2,  // Furuno: 2, Navico: 4, Garmin: 1
    "hasFTC": false,           // Garmin HD, Raymarine RD have it
    "hasTuning": false,        // Raymarine magnetron radars only
    "hasAntennaParking": false // HALO only
  },

  "connection": {
    "keepAliveRequired": false,     // Furuno uses TCP with its own keep-alive
    "keepAliveIntervalMs": 5000,    // Furuno: 5000ms, Navico HALO: 100ms, Raymarine: 1000ms
    "protocol": "tcp"               // "tcp" | "udp"
  },

  "controls": {
    "common": [
      "power",
      "range",
      "gain",
      "sea",
      "rain",
      "interferenceRejection",
      "bearingAlignment",
      "noTransmitZones",
      "scanSpeed"
    ],
    "vendor": [
      "rezboost",
      "birdMode",
      "targetAnalyzer",
      "txChannel",
      "autoAcquire"
    ]
  },

  "ranges": [
    231, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112,
    14816, 22224, 29632, 44448, 59264, 66672, 88896
  ]
}
```

Note: Ranges in meters. Furuno DRS4D-NXT example: 1/8nm (231m) to 48nm (88896m).

---

## Control Metadata Endpoint

Provides human-readable descriptions and valid values for controls.

### Request
```
GET /radars/{id}/controls
GET /radars/{id}/controls/{controlName}
```

### Response Schema
```json
{
  "controls": {
    "gain": {
      "name": "Gain",
      "description": "Controls the amplification of received radar signals. Higher values increase sensitivity but may also amplify noise.",
      "type": "ranged",
      "modes": ["manual", "auto"],
      "range": { "min": 0, "max": 100, "step": 1, "unit": "percent" },
      "autoLevels": ["low", "high"],
      "default": { "mode": "auto", "autoLevel": "high" }
    },

    "furuno/rezboost": {
      "name": "RezBoost",
      "vendor": "furuno",
      "description": "Furuno's proprietary beam sharpening technology that uses advanced signal processing to narrow the effective beam width. This produces resolution comparable to larger antenna arrays, improving target separation and reducing elongated echoes from distant targets.",
      "marketingNote": "RezBoost™ achieves the equivalent resolution of a larger antenna array in a compact radome.",
      "type": "enum",
      "values": [
        { "value": 0, "label": "Off", "description": "RezBoost disabled" },
        { "value": 1, "label": "Low", "description": "Mild beam sharpening" },
        { "value": 2, "label": "Medium", "description": "Moderate beam sharpening" },
        { "value": 3, "label": "Max", "description": "Maximum beam sharpening, 2° effective beam width" }
      ],
      "default": 2
    },

    "furuno/targetAnalyzer": {
      "name": "Target Analyzer",
      "vendor": "furuno",
      "description": "Uses Doppler processing to analyze radar echoes. In 'target' mode, highlights approaching targets for collision avoidance. In 'rain' mode, identifies precipitation patterns.",
      "marketingNote": "Target Analyzer™ is FURUNO's exclusive Doppler-based threat detection that works independent of vessel speed.",
      "type": "compound",
      "properties": {
        "enabled": { "type": "boolean" },
        "mode": { "type": "enum", "values": ["target", "rain"] }
      },
      "default": { "enabled": true, "mode": "target" },
      "note": "Universal setting - affects both screens in dual scan mode"
    },

    "furuno/birdMode": {
      "name": "Bird Mode",
      "vendor": "furuno",
      "description": "Optimizes radar display for detecting flocks of birds, which often indicate fish schools below. Essential for sportfishing applications.",
      "type": "enum",
      "values": [
        { "value": 0, "label": "Off", "description": "Bird mode disabled" },
        { "value": 1, "label": "Low", "description": "Mild bird detection sensitivity" },
        { "value": 2, "label": "Medium", "description": "Moderate bird detection sensitivity" },
        { "value": 3, "label": "High", "description": "Maximum bird detection sensitivity" }
      ],
      "default": 0,
      "note": "Universal setting - affects both screens in dual scan mode"
    },

    "furuno/txChannel": {
      "name": "TX Channel",
      "vendor": "furuno",
      "description": "Selects the transmission frequency channel to avoid interference with other nearby radars operating on similar frequencies.",
      "type": "enum",
      "values": [
        { "value": 0, "label": "Auto", "description": "Automatic channel selection" },
        { "value": 1, "label": "Channel 1", "description": "Fixed channel 1" },
        { "value": 2, "label": "Channel 2", "description": "Fixed channel 2" },
        { "value": 3, "label": "Channel 3", "description": "Fixed channel 3" }
      ],
      "default": 0
    },

    "furuno/autoAcquire": {
      "name": "Auto Acquire",
      "vendor": "furuno",
      "description": "Enables automatic ARPA target acquisition using Doppler detection. When enabled, the radar automatically begins tracking moving targets without manual selection.",
      "type": "boolean",
      "default": false
    },

    "navico/doppler": {
      "name": "VelocityTrack",
      "vendor": "navico",
      "description": "Simrad/Lowrance/B&G's Doppler motion detection that color-codes all radar targets based on relative motion. Unlike manual ARPA tracking, VelocityTrack automatically analyzes every target on screen without selection limits.",
      "marketingNote": "VelocityTrack™ provides instant visual feedback on whether targets are approaching or receding, with no manual target selection required.",
      "type": "enum",
      "values": [
        { "value": "off", "label": "Off", "description": "Doppler detection disabled" },
        { "value": "both", "label": "Both", "description": "Highlight approaching and receding targets" },
        { "value": "approaching", "label": "Approaching Only", "description": "Only highlight approaching targets" }
      ],
      "default": "approaching"
    },

    "navico/mode": {
      "name": "Radar Mode",
      "vendor": "navico",
      "description": "Preset operating modes that automatically configure gain, sea clutter, rain clutter, and other settings for specific conditions. In preset modes, individual controls become read-only.",
      "type": "enum",
      "values": [
        { "value": "custom", "label": "Custom", "description": "Full manual control of all settings" },
        { "value": "harbor", "label": "Harbor", "description": "Optimized for busy ports with fast scanning and clutter reduction" },
        { "value": "offshore", "label": "Offshore", "description": "Balanced settings for open water navigation" },
        { "value": "weather", "label": "Weather", "description": "Enhanced sensitivity for detecting precipitation cells" },
        { "value": "bird", "label": "Bird", "description": "Optimized for detecting birds indicating fish schools" }
      ],
      "default": "harbor",
      "controlsDisabledInPreset": ["gain", "sea", "rain", "interferenceRejection"]
    },

    "raymarine/doppler": {
      "name": "Doppler Target Detection",
      "vendor": "raymarine",
      "description": "Quantum 2 and Cyclone series feature Doppler-based collision avoidance that color-codes moving contacts to show if they are approaching or moving away.",
      "marketingNote": "Leveraging sophisticated Doppler processing, Quantum 2 clearly highlights moving radar contacts for enhanced situational awareness.",
      "type": "enum",
      "values": [
        { "value": "off", "label": "Off" },
        { "value": "approaching", "label": "Approaching" }
      ]
    },

    "garmin/crosstalkRejection": {
      "name": "Crosstalk Rejection",
      "vendor": "garmin",
      "description": "Filters interference from nearby radars operating on similar frequencies, particularly useful in crowded marina or commercial shipping environments.",
      "type": "ranged",
      "range": { "min": 0, "max": 100, "step": 1, "unit": "percent" }
    }
  }
}
```

---

## Control State Endpoint

Returns current state of all controls including disabled status.

### Request
```
GET /radars/{id}/state
```

### Response Schema
```json
{
  "id": "radar-0",
  "timestamp": "2025-01-15T10:30:00Z",

  "power": {
    "value": "transmit",
    "disabled": false
  },

  "range": {
    "value": 6000,
    "disabled": false
  },

  "gain": {
    "mode": "auto",
    "value": 50,
    "autoLevel": "high",
    "disabled": true,
    "disabledReason": "Controlled by Harbor mode preset"
  },

  "sea": {
    "mode": "auto",
    "value": 35,
    "disabled": true,
    "disabledReason": "Controlled by Harbor mode preset"
  },

  "navico/mode": {
    "value": "harbor",
    "disabled": false
  },

  "navico/doppler": {
    "mode": "approaching",
    "disabled": false
  }
}
```

### Disabled State

Controls can be disabled (read-only) when:
1. **Preset Mode Active**: HALO Harbor/Offshore/Weather modes lock dependent controls
2. **Hardware Limitation**: Feature unavailable at current range or configuration
3. **Dependency**: Another control's state disables this one

The `disabledReason` field provides human-readable explanation.

---

## WebSocket API

Real-time control state updates via WebSocket subscription.

### Connection
```
ws://{host}/signalk/v2/api/vessels/self/radars/{radarId}/state
```

### Message Format

#### Subscription Request
```json
{
  "type": "subscribe",
  "controls": ["*"]  // or specific: ["gain", "sea", "navico/mode"]
}
```

#### State Update Message
```json
{
  "type": "state",
  "timestamp": "2025-01-15T10:30:01Z",
  "changes": [
    {
      "control": "navico/mode",
      "value": "weather",
      "disabled": false
    },
    {
      "control": "gain",
      "mode": "auto",
      "value": 75,
      "autoLevel": "high",
      "disabled": true,
      "disabledReason": "Controlled by Weather mode preset"
    },
    {
      "control": "sea",
      "mode": "auto",
      "value": 20,
      "disabled": true,
      "disabledReason": "Controlled by Weather mode preset"
    },
    {
      "control": "rain",
      "mode": "auto",
      "value": 60,
      "disabled": true,
      "disabledReason": "Controlled by Weather mode preset"
    }
  ]
}
```

### Use Cases

1. **Multi-controller sync**: When a hardware MFD changes a setting, all connected clients receive the update
2. **Mode cascade**: Changing from Custom to Harbor mode triggers updates for all affected controls
3. **Auto-adjustment**: Auto gain/sea values change based on conditions - clients see real-time values

---

## Implementation Notes

### WASM Plugin Interface

The `mayara-signalk-wasm` plugin should export:

```rust
// Existing exports
#[wasm_bindgen]
pub fn radar_get_radars() -> String;
#[wasm_bindgen]
pub fn radar_get_info(radar_id: &str) -> String;
#[wasm_bindgen]
pub fn radar_set_power(radar_id: &str, state: &str) -> bool;
#[wasm_bindgen]
pub fn radar_set_range(radar_id: &str, range_m: u32) -> bool;
#[wasm_bindgen]
pub fn radar_set_gain(radar_id: &str, mode: &str, value: u32, auto_level: &str) -> bool;
#[wasm_bindgen]
pub fn radar_set_sea(radar_id: &str, mode: &str, value: u32) -> bool;
#[wasm_bindgen]
pub fn radar_set_rain(radar_id: &str, mode: &str, value: u32) -> bool;
#[wasm_bindgen]
pub fn radar_set_controls(radar_id: &str, controls_json: &str) -> bool;

// New exports for v2 API
#[wasm_bindgen]
pub fn radar_get_capabilities(radar_id: &str) -> String;
#[wasm_bindgen]
pub fn radar_get_controls_metadata(radar_id: &str) -> String;
#[wasm_bindgen]
pub fn radar_get_state(radar_id: &str) -> String;
#[wasm_bindgen]
pub fn radar_set_vendor_control(radar_id: &str, vendor: &str, control: &str, value_json: &str) -> bool;
#[wasm_bindgen]
pub fn radar_subscribe_state(radar_id: &str, callback: &js_sys::Function) -> u32;
#[wasm_bindgen]
pub fn radar_unsubscribe_state(subscription_id: u32) -> bool;
```

### SignalK Server Route Mapping

```javascript
// Common controls
router.put('/radars/:id/power', (req, res) => wasmPlugin.radar_set_power(req.params.id, req.body.value));
router.put('/radars/:id/range', (req, res) => wasmPlugin.radar_set_range(req.params.id, req.body.value));
router.put('/radars/:id/gain', (req, res) => wasmPlugin.radar_set_gain(...));
router.put('/radars/:id/sea', (req, res) => wasmPlugin.radar_set_sea(...));
router.put('/radars/:id/rain', (req, res) => wasmPlugin.radar_set_rain(...));

// Vendor-specific controls
router.put('/radars/:id/:vendor/:control', (req, res) => {
  wasmPlugin.radar_set_vendor_control(req.params.id, req.params.vendor, req.params.control, JSON.stringify(req.body));
});

// Discovery
router.get('/radars/:id/capabilities', ...);
router.get('/radars/:id/controls', ...);
router.get('/radars/:id/state', ...);

// WebSocket
router.ws('/radars/:id/state', ...);
```

---

## Model-Specific Capabilities

Radar capabilities vary significantly by model, even within the same vendor. The API uses the **model name** from device discovery to determine available features.

### How Model Detection Works

During discovery, radars broadcast identifying information. The quality and explicitness of model identification varies by vendor:

#### Furuno (UDP beacon on port 10010) ✅ Explicit Model
- **Model name**: Explicit in 170-byte response (e.g., `DRS4D-NXT`, `DRS6A-NXT`)
- Firmware version: `01.01:01.01:01.05:01.05`
- Serial number: `6424`
- MAC address (Furuno OUI: `00:d0:1d:xx:xx:xx`)

```
Detection: beacon.model_name → "DRS4D-NXT" → lookup capabilities
```

#### Navico (UDP multicast 236.6.7.x) ⚠️ Inferred Model
- **Serial number**: 16 bytes ASCII in beacon
- **Beacon structure**: Differs by generation (BR24 unique, 3G/4G/HALO different)
- **Dual-range capability**: Indicates 4G or HALO vs older models
- Model must be **inferred** from beacon type + capabilities

```
Detection: beacon_type + dual_range + serial_prefix → infer "HALO24" → lookup
```

| Beacon Type | Dual Range | Doppler | Inferred Model |
|-------------|------------|---------|----------------|
| BR24 format | No | No | BR24 |
| Single-range | No | No | 3G |
| Single-range | No | Yes | HALO20 |
| Dual-range | Yes | No | 4G |
| Dual-range | Yes | Yes | HALO20+, HALO24, HALO3/4/6 |

**Note**: Doppler capability can be detected from report packets, not beacon. Full model identification may require initial communication.

#### Raymarine (UDP multicast 224.0.0.1:5800) ✅ Part Number Detection
- **56-byte beacon**: Contains subtype and model name field
- **Subtype**: Indicates model family (0x01=RD/HD, 0x66=Quantum, 0x4D=Quantum WiFi)
- **Info Report**: Contains **part number** for precise model identification
- Part number is more reliable than inferring from capabilities

```
Detection: info_report.part_number → "E70498" → lookup "Quantum Q24D"
```

**Part Number to Model Mapping:**

| Part Number | Model | Family | Doppler |
|-------------|-------|--------|---------|
| E70210 | Quantum Q24 | Quantum | No |
| E70344 | Quantum Q24C | Quantum | No |
| E70498 | Quantum Q24D | Quantum | Yes |
| E70620 | Cyclone | Quantum | Yes |
| E70621 | Cyclone Pro | Quantum | Yes |
| E70484 | Magnum 4kW | RD | No |
| E70487 | Magnum 12kW | RD | No |
| E52069 | Open Array HD 4kW | RD | No |
| E92160 | Open Array HD 12kW | RD | No |
| E52081 | Open Array SHD 4kW | RD | No |
| E52082 | Open Array SHD 12kW | RD | No |
| E92142 | RD418HD | RD | No |
| E92143 | RD424HD | RD | No |
| E92130 | RD418D | RD | No |
| E92132 | RD424D | RD | No |

**Fallback Detection (if part number unavailable):**

| Subtype | Model Name | Inferred Model |
|---------|------------|----------------|
| 0x01 | (empty) | RD series (check HD vs non-HD from spoke format) |
| 0x66 | QuantumRadar | Quantum (check Doppler for Q24D vs Q24C) |
| 0x4D | QuantumRadar | Quantum WiFi |

#### Garmin (UDP multicast 239.254.2.0) ✅ Explicit Model
- **ScannerMessage (0x099b)**: Contains 64-byte model info string
- Model name is explicit (e.g., `GMR 24 xHD`, `GMR Fantom 24`)

```
Detection: scanner_msg.model_info → "GMR 24 xHD" → lookup capabilities
```

### Model Capability Database

The plugin maintains a model database mapping detected models to their capabilities:

```json
{
  "furuno": {
    "DRS4D-NXT": {
      "family": "DRS-NXT",
      "power": "4kW",
      "antenna": "24inch",
      "maxRange": 66672,
      "hasDoppler": true,
      "hasDualScan": true,
      "maxDualScanRange": 22224,
      "controls": ["rezboost", "birdMode", "targetAnalyzer", "txChannel", "autoAcquire"]
    },
    "DRS6A-NXT": {
      "family": "DRS-NXT",
      "power": "6kW",
      "antenna": "open-array",
      "maxRange": 133344,
      "hasDoppler": true,
      "hasDualScan": true,
      "maxDualScanRange": 22224,
      "controls": ["rezboost", "birdMode", "targetAnalyzer", "txChannel", "autoAcquire"]
    }
  },
  "navico": {
    "HALO24": {
      "family": "HALO",
      "hasDoppler": true,
      "hasDualRange": true,
      "controls": ["doppler", "dopplerSpeed", "mode", "targetBoost", "targetSeparation"]
    }
  }
}
```

### Unknown Models

When an unknown model is detected:
1. Return `"modelFamily": "unknown"` in capabilities
2. Enable only common controls (power, range, gain, sea, rain)
3. Allow vendor-specific controls to be attempted (may fail gracefully)
4. Log the unknown model for future database updates

### Firmware-Dependent Features

Some features may depend on firmware version, not just model:
- Check `firmwareVersion` in capabilities response
- Plugin can gate features based on minimum firmware versions if known
- Firmware requirements need to be documented as they are discovered

---

## Vendor Feature Glossary

### Furuno

| Feature | Marketing Name | Technical Description | Status |
|---------|---------------|----------------------|--------|
| RezBoost™ | Beam Sharpening | Signal processing that narrows effective beam width for improved target separation. Per-screen setting in dual scan. | ✅ Verified |
| Target Analyzer™ | Doppler Threat Detection | Dual-mode analysis: Target mode highlights approaching threats, Rain mode identifies precipitation. Universal setting. | ✅ Verified |
| Bird Mode | Fish Finder Aid | 4-level sensitivity (Off/Low/Med/High) for detecting bird flocks indicating fish schools. Universal setting. | ✅ Verified |
| TX Channel | Frequency Selection | Select transmission channel (Auto/1/2/3) to avoid interference with nearby radars | ✅ Verified |
| Auto Acquire | Doppler ARPA | Automatic ARPA target acquisition using Doppler motion detection | ✅ Verified |
| Dual Scan | Dual Range Display | Two independent radar displays with different ranges (up to 12nm each), sharing antenna rotation | ✅ Verified |
| Sector Blanking | No-Transmit Zones | Up to 2 configurable sectors where radar won't transmit (start angle + width) | ✅ Verified |
| Main Bang | Pulse Suppression | Suppresses transmitter pulse reflection (0-100%) | ⚠️ Protocol known |
| Fast Target Tracking™ | Rapid ARPA | Generates target vectors in seconds vs traditional ARPA delay | ❓ Unverified |
| Echo Trail | Target History | Displays historical echo positions showing target movement | ❓ Unverified |

### Navico (Simrad/Lowrance/B&G)

| Feature | Marketing Name | Technical Description |
|---------|---------------|----------------------|
| VelocityTrack™ | Doppler Motion Detection | Automatic motion analysis of all targets without selection limits |
| Beam Sharpening | Resolution Enhancement | Digital processing to improve angular resolution |
| Target Separation | Close Target Distinction | Improves ability to distinguish closely-spaced targets |
| MARPA | Target Tracking | Manual Automatic Radar Plotting Aid for collision avoidance |
| Dual Range | Split Display | Simultaneous display of two range scales |

### Raymarine

| Feature | Marketing Name | Technical Description |
|---------|---------------|----------------------|
| CHIRP | Pulse Compression | Frequency-modulated pulse for improved range resolution |
| ATX™ | Advanced Target Separation | Enhanced target separation using multiple compressed pulses |
| Doppler | Collision Avoidance | Motion-based target highlighting (Quantum 2, Cyclone series) |

### Garmin

| Feature | Marketing Name | Technical Description |
|---------|---------------|----------------------|
| Timed Idle | Power Management | Cycles between transmit and standby to conserve power |
| Crosstalk Rejection | Multi-Radar Filtering | Reduces interference from other nearby radars |

---

## Migration Path

### Phase 1: Capabilities Discovery
- Add `/capabilities` endpoint
- Add `/controls` metadata endpoint
- Maintain backward compatibility with existing API

### Phase 2: Common Controls
- Implement standardized common control endpoints
- Add control state with disabled flag support
- Deprecate (but maintain) legacy endpoints

### Phase 3: Vendor Extensions
- Add vendor-namespaced endpoints
- Implement full control metadata
- Add WebSocket state subscription

### Phase 4: Full Migration
- Remove deprecated endpoints
- Complete vendor-specific control coverage
- Add comprehensive control documentation

---

## References

- [Furuno Radar Technology](https://www.furuno.com/en/technology/radar/)
- [Simrad HALO Radar](https://www.simrad-yachting.com/simrad/series/halo-radar/)
- [Raymarine Quantum Radar](https://www.raymarine.com/en-us/our-products/marine-radar/quantum)
- [mayara-lib Protocol Documentation](../../../mayara/docs/)
- [SignalK Radar API v1](../../../signalk-server/docs/develop/rest-api/radar_api.md)
