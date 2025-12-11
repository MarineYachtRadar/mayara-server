# SignalK Radar API v5 Specification

## Design Philosophy

> **The chartplotter developer's experience is paramount.**
>
> A chartplotter sees radars as simple, uniform resources (`/radars/1`, `/radars/2`).
> The plugin handles all vendor complexity internally. Clients never need to know
> if they're talking to Furuno, Navico, or any other brand.

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Control protocol | **REST** | Simple, stateless, ~50ms latency acceptable for user actions |
| Spoke streaming | **WebSocket** | High-frequency binary data, low latency critical |
| Control IDs | **Semantic** | Function-based (`beamSharpening`), not vendor-namespaced (`furuno/rezboost`) |
| Radar IDs | **Simple** | `1`, `2` - vendor abstracted from client |
| Schema delivery | **Provider-declared** | Plugin declares capabilities, not server |

---

## Quick Start for Client Developers

### 1. Discover Available Radars
```http
GET /signalk/v2/api/vessels/self/radars

Response:
[
  {"id": "1", "make": "Furuno", "model": "DRS4D-NXT", "status": "transmit"},
  {"id": "2", "make": "Navico", "model": "HALO24", "status": "standby"}
]
```

### 2. Get Radar Capabilities (Cache This)
```http
GET /signalk/v2/api/vessels/self/radars/1/capabilities

Response:
{
  "id": "1",
  "make": "Furuno",
  "model": "DRS4D-NXT",
  "characteristics": {
    "maxRange": 88896,
    "minRange": 116,
    "supportedRanges": [116, 231, 463, 926, ...],
    "hasDoppler": true,
    "hasDualRange": true
  },
  "controls": [
    {"id": "power", "category": "base", "type": "enum", ...},
    {"id": "range", "category": "base", "type": "number", ...},
    {"id": "gain", "category": "base", "type": "compound", ...},
    {"id": "beamSharpening", "category": "extended", "type": "enum", ...}
  ],
  "constraints": [...]
}
```

### 3. Get Current State
```http
GET /signalk/v2/api/vessels/self/radars/1/state

Response:
{
  "id": "1",
  "timestamp": "2025-01-15T10:30:00Z",
  "status": "transmit",
  "controls": {
    "power": "transmit",
    "range": 5556,
    "gain": {"mode": "auto", "value": 65},
    "beamSharpening": 2
  }
}
```

### 4. Control the Radar
```http
PUT /signalk/v2/api/vessels/self/radars/1/controls/range
Content-Type: application/json

{"value": 11112}

Response: {"success": true}
```

### 5. Connect to Spoke Stream
```javascript
const ws = new WebSocket('ws://host/signalk/v2/api/vessels/self/radars/1/stream');
ws.binaryType = 'arraybuffer';
ws.onmessage = (event) => {
  const spokeData = new Uint8Array(event.data);
  // Render spoke on PPI display
};
```

---

## Client Integration Patterns

### Basic Integration (Minimum Viable)
```
1. GET /radars           → Find radars
2. GET /radars/1/state   → Get current settings
3. PUT /radars/1/controls/power {"value": "transmit"}
4. WS  /radars/1/stream  → Display spokes
```

### Full Integration (Recommended)
```
1. GET /radars                    → Find radars
2. GET /radars/1/capabilities     → Cache schema, build dynamic UI
3. GET /radars/1/state            → Initialize UI with current values
4. PUT /radars/1/controls/{id}    → User changes settings
5. Poll GET /radars/1/state       → Sync state (every 1-5 seconds)
6. WS  /radars/1/stream           → Display spokes
```

### Adaptive UI Generation

Clients SHOULD generate control UI dynamically from capabilities:

```javascript
// Pseudo-code: Generate controls from capabilities
capabilities.controls.forEach(control => {
  if (control.type === 'enum') {
    renderDropdown(control.id, control.name, control.values);
  } else if (control.type === 'number') {
    renderSlider(control.id, control.name, control.range.min, control.range.max);
  } else if (control.type === 'compound') {
    renderCompoundControl(control.id, control.name, control.properties);
  }
});
```

Benefits:
- Automatically supports new controls without client updates
- Handles vendor differences transparently
- Graceful degradation for unknown controls

---

## API Endpoints

### Base Path
```
/signalk/v2/api/vessels/self/radars
```

### Endpoint Summary

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/radars` | List all radars |
| GET | `/radars/{id}` | Get radar info |
| GET | `/radars/{id}/capabilities` | Get capability manifest (cacheable) |
| GET | `/radars/{id}/state` | Get current control values |
| GET | `/radars/{id}/controls` | List all controls with values |
| GET | `/radars/{id}/controls/{controlId}` | Get single control value |
| PUT | `/radars/{id}/controls/{controlId}` | Set control value |
| WS | `/radars/{id}/stream` | Binary spoke data stream |

### Backward Compatibility Aliases (v4)

These legacy endpoints map to the generic controls interface:

```
PUT /radars/{id}/power  → PUT /radars/{id}/controls/power
PUT /radars/{id}/range  → PUT /radars/{id}/controls/range
PUT /radars/{id}/gain   → PUT /radars/{id}/controls/gain
PUT /radars/{id}/sea    → PUT /radars/{id}/controls/sea
PUT /radars/{id}/rain   → PUT /radars/{id}/controls/rain
```

---

## Data Types

### RadarListItem (GET /radars response item)

```typescript
interface RadarListItem {
  id: string;              // "1", "2", etc.
  make: string;            // "Furuno" (required)
  model: string;           // "DRS4D-NXT" (required)
  status: RadarStatus;     // Current operational status
}

type RadarStatus = "off" | "standby" | "transmit" | "warming";
```

### CapabilityManifest (GET /radars/{id}/capabilities)

```typescript
interface CapabilityManifest {
  // Required identification
  id: string;
  make: string;
  model: string;

  // Optional identification
  modelFamily?: string;
  serialNumber?: string;
  firmwareVersion?: string;

  // Hardware characteristics
  characteristics: {
    maxRange: number;           // meters
    minRange: number;           // meters
    supportedRanges: number[];  // discrete values in meters
    spokesPerRevolution: number;
    maxSpokeLength: number;
    hasDoppler: boolean;
    hasDualRange: boolean;
    noTransmitZoneCount: number;
  };

  // Available controls (schema only, no values)
  controls: ControlDefinition[];

  // Control dependencies and constraints
  constraints: ControlConstraint[];
}
```

### ControlDefinition

```typescript
interface ControlDefinition {
  id: string;                    // Semantic ID: "gain", "beamSharpening"
  name: string;                  // Human-readable: "Gain"
  description: string;           // Tooltip text
  category: "base" | "extended"; // Base = all radars, Extended = model-specific

  type: "boolean" | "number" | "enum" | "compound";

  // For type: "number"
  range?: {
    min: number;
    max: number;
    step?: number;
    unit?: string;               // "percent", "meters", "degrees"
  };

  // For type: "enum"
  values?: Array<{
    value: string | number;
    label: string;
    description?: string;
  }>;

  // For type: "compound"
  properties?: Record<string, PropertyDefinition>;

  // Auto/manual mode support
  modes?: ("manual" | "auto")[];
  defaultMode?: "manual" | "auto";

  readOnly?: boolean;
  default?: unknown;
}

interface PropertyDefinition {
  type: "boolean" | "number" | "string" | "enum";
  description?: string;
  range?: { min: number; max: number; step?: number };
  values?: Array<{ value: string | number; label: string }>;
}
```

### ControlConstraint

```typescript
interface ControlConstraint {
  controlId: string;

  condition: {
    type: "disabled_when" | "read_only_when" | "restricted_when";
    dependsOn: string;
    operator: "==" | "!=" | ">" | "<" | ">=" | "<=";
    value: string | number | boolean;
  };

  effect: {
    disabled?: boolean;
    readOnly?: boolean;
    allowedValues?: unknown[];
    reason?: string;
  };
}
```

**Example constraint**: Gain is read-only when preset mode is active:
```json
{
  "controlId": "gain",
  "condition": {
    "type": "read_only_when",
    "dependsOn": "presetMode",
    "operator": "!=",
    "value": "custom"
  },
  "effect": {
    "readOnly": true,
    "reason": "Controlled by preset mode"
  }
}
```

### RadarState (GET /radars/{id}/state)

```typescript
interface RadarState {
  id: string;
  timestamp: string;             // ISO 8601
  status: RadarStatus;

  controls: Record<string, ControlValue>;

  disabledControls?: Array<{
    controlId: string;
    reason: string;
  }>;
}

type ControlValue =
  | boolean
  | number
  | string
  | { mode: "auto" | "manual"; value?: number; [key: string]: unknown };
```

---

## Semantic Control ID Registry

Providers use these standard control IDs. The plugin maps them to vendor-specific commands internally.

### Base Controls (Required - All Radars)

| ID | Type | Description |
|----|------|-------------|
| `power` | enum | `"off"`, `"standby"`, `"transmit"`, `"warming"` |
| `range` | number | Detection range in meters |
| `gain` | compound | `{mode: "auto"|"manual", value?: 0-100}` |
| `sea` | compound | `{mode: "auto"|"manual", value?: 0-100}` Sea clutter |
| `rain` | compound | `{mode?: "auto"|"manual", value: 0-100}` Rain clutter |

### Extended Controls (Optional - Model-Specific)

| ID | Type | Vendors | Vendor Names |
|----|------|---------|--------------|
| `interferenceRejection` | enum | All | IR |
| `beamSharpening` | enum | Furuno, Navico | RezBoost, Beam Sharpening |
| `dopplerMode` | compound | Furuno, Navico, Raymarine | Target Analyzer, VelocityTrack, Doppler |
| `birdMode` | enum | Furuno, Navico | Bird Mode |
| `targetSeparation` | enum | Navico, Raymarine | Target Separation, ATX |
| `noiseRejection` | enum | Navico | Noise Rejection |
| `scanSpeed` | enum | Furuno, Navico | Scan Speed |
| `presetMode` | enum | Navico, Raymarine | Harbor/Offshore/Weather |
| `noTransmitZones` | compound | Furuno, Navico, Garmin | Sector Blanking |
| `bearingAlignment` | number | All | Heading offset (degrees) |
| `antennaHeight` | number | Furuno, Navico | Height above waterline |
| `txChannel` | enum | Furuno | TX Channel |
| `autoAcquire` | boolean | Furuno | ARPA Auto Acquire |

Providers MAY add additional controls not in this registry. Clients SHOULD handle unknown controls gracefully (display generically or ignore).

---

## Control Schemas

### Power

```json
PUT /radars/1/controls/power
{"value": "transmit"}

// Valid values: "off", "standby", "transmit"
// "warming" is read-only (returned in state, not settable)
```

### Range

```json
PUT /radars/1/controls/range
{"value": 5556}

// Value in meters
// Must be one of supportedRanges from capabilities
```

### Gain (Compound)

```json
PUT /radars/1/controls/gain
{
  "mode": "auto",
  "value": 50
}

// mode: "manual" | "auto"
// value: 0-100 (percentage), used in manual mode
```

### Sea Clutter (Compound)

```json
PUT /radars/1/controls/sea
{
  "mode": "auto",
  "value": 30
}
```

### Rain Clutter (Compound)

```json
PUT /radars/1/controls/rain
{
  "mode": "manual",
  "value": 25
}
```

### Beam Sharpening (Extended)

```json
PUT /radars/1/controls/beamSharpening
{"value": 2}

// Furuno (RezBoost): 0=off, 1=low, 2=medium, 3=max
// Navico: 0=off, 1=low, 2=medium, 3=high
```

### Doppler Mode (Extended)

```json
PUT /radars/1/controls/dopplerMode
{
  "enabled": true,
  "mode": "approaching"
}

// Furuno (Target Analyzer): mode = "target" | "rain"
// Navico (VelocityTrack): mode = "off" | "both" | "approaching"
// Raymarine: mode = "off" | "approaching"
```

### Preset Mode (Extended)

```json
PUT /radars/1/controls/presetMode
{"value": "harbor"}

// Navico: "custom" | "harbor" | "offshore" | "weather" | "bird"
// Raymarine: "harbor" | "coastal" | "offshore" | "weather"
```

### No Transmit Zones (Extended)

```json
PUT /radars/1/controls/noTransmitZones
{
  "zones": [
    {"enabled": true, "start": 90, "end": 180},
    {"enabled": false, "start": 0, "end": 0}
  ]
}

// Number of zones varies: Furuno=2, Navico=2-4, Garmin=1
// Angles in degrees (0-360)
```

---

## Error Responses

```typescript
// Unknown radar
404 { "error": "Radar not found", "id": "99" }

// Unknown control
404 { "error": "Control not found", "controlId": "unknownControl" }

// Invalid value
400 { "error": "Invalid value", "controlId": "range", "message": "Value 999999 exceeds max 88896" }

// Control disabled
409 { "error": "Control disabled", "controlId": "gain", "reason": "Controlled by Harbor preset" }
```

---

## Provider Interface

Radar plugins implement this interface to register with SignalK:

```typescript
interface RadarProvider {
  name: string;

  methods: {
    // Discovery
    getRadars(): Promise<string[]>;
    getRadarInfo(radarId: string): Promise<RadarInfo>;

    // Capabilities (v5)
    getCapabilities(radarId: string): Promise<CapabilityManifest>;

    // State (v5)
    getState(radarId: string): Promise<RadarState>;

    // Generic control interface (v5)
    getControl(radarId: string, controlId: string): Promise<ControlValue>;
    setControl(radarId: string, controlId: string, value: unknown): Promise<boolean>;

    // Streaming
    handleStreamConnection(radarId: string, ws: WebSocket): void;
  };
}
```

### WASM Plugin Exports

```rust
// Discovery
#[no_mangle]
pub extern "C" fn radar_get_radars(out_ptr: *mut u8, out_max_len: usize) -> i32;

#[no_mangle]
pub extern "C" fn radar_get_info(request_ptr: *const u8, request_len: usize,
                                  out_ptr: *mut u8, out_max_len: usize) -> i32;

// Capabilities (v5)
#[no_mangle]
pub extern "C" fn radar_get_capabilities(request_ptr: *const u8, request_len: usize,
                                          out_ptr: *mut u8, out_max_len: usize) -> i32;

// State (v5)
#[no_mangle]
pub extern "C" fn radar_get_state(request_ptr: *const u8, request_len: usize,
                                   out_ptr: *mut u8, out_max_len: usize) -> i32;

// Generic control (v5)
#[no_mangle]
pub extern "C" fn radar_get_control(request_ptr: *const u8, request_len: usize,
                                     out_ptr: *mut u8, out_max_len: usize) -> i32;

#[no_mangle]
pub extern "C" fn radar_set_control(request_ptr: *const u8, request_len: usize,
                                     out_ptr: *mut u8, out_max_len: usize) -> i32;

// Legacy (backward compat)
#[no_mangle]
pub extern "C" fn radar_set_power(request_ptr: *const u8, request_len: usize,
                                   out_ptr: *mut u8, out_max_len: usize) -> i32;
// ... etc
```

---

## Semantic ID to Vendor Command Mapping

The plugin maps semantic control IDs to vendor-specific commands internally:

| Semantic ID | Furuno | Navico | Raymarine |
|-------------|--------|--------|-----------|
| `beamSharpening` | RezBoost (0-3) | Beam Sharpening (0-3) | - |
| `dopplerMode` | Target Analyzer | VelocityTrack | Doppler |
| `birdMode` | Bird Mode (0-3) | Bird Mode preset | - |
| `presetMode` | - | Mode (custom/harbor/offshore/weather/bird) | Mode |
| `targetSeparation` | - | Target Separation | ATX |
| `txChannel` | TX Channel (auto/1/2/3) | - | - |

Clients use the semantic ID; the plugin translates to the appropriate vendor command.

---

## Dual Range Support

Some radars support displaying two independent range views simultaneously.

### Furuno Dual Scan

DRS-NXT radars support dual scan with independent displays (max 12nm each).

| Control | Behavior |
|---------|----------|
| Range | Per-screen (use `screen` parameter) |
| Power/Status | Per-screen |
| Beam Sharpening | Per-screen |
| Gain, Sea, Rain | Universal (affects both) |
| Doppler Mode | Universal |

```json
PUT /radars/1/controls/range
{
  "value": 5000,
  "screen": 0  // 0=primary, 1=secondary (optional, defaults to 0)
}
```

### Navico Dual Range

4G and HALO radars support dual range with separate multicast channels.

| Control | Behavior |
|---------|----------|
| Range | Per-screen |
| Gain, Sea, Rain | Universal |
| Preset Mode, Doppler | Universal |

---

## Model Capability Database

The plugin maintains a database mapping models to their capabilities:

### Furuno Models

| Model | Family | Doppler | Dual Scan | Max Range | Controls |
|-------|--------|---------|-----------|-----------|----------|
| DRS4D-NXT | DRS-NXT | Yes | Yes | 88896m | beamSharpening, dopplerMode, birdMode, txChannel |
| DRS6A-NXT | DRS-NXT | Yes | Yes | 133344m | beamSharpening, dopplerMode, birdMode, txChannel |
| DRS4D | DRS | No | No | 66672m | - |

### Navico Models

| Model | Family | Doppler | Dual Range | Controls |
|-------|--------|---------|------------|----------|
| HALO24 | HALO | Yes | Yes | dopplerMode, presetMode, targetSeparation |
| HALO20+ | HALO | Yes | Yes | dopplerMode, presetMode, targetSeparation |
| 4G | 4G | No | Yes | presetMode |
| 3G | 3G | No | No | - |
| BR24 | BR24 | No | No | - |

### Raymarine Models

| Model | Family | Doppler | Controls |
|-------|--------|---------|----------|
| Quantum Q24D | Quantum | Yes | dopplerMode, presetMode |
| Quantum Q24C | Quantum | No | presetMode |
| Cyclone | Quantum | Yes | dopplerMode, presetMode |
| RD424HD | RD | No | tune, ftc |

---

## Migration from v4

### Changed in v5

| v4 | v5 | Notes |
|----|-----|-------|
| `PUT /radars/{id}/furuno/rezboost` | `PUT /radars/{id}/controls/beamSharpening` | Semantic ID |
| `PUT /radars/{id}/navico/doppler` | `PUT /radars/{id}/controls/dopplerMode` | Semantic ID |
| `PUT /radars/{id}/navico/mode` | `PUT /radars/{id}/controls/presetMode` | Semantic ID |
| Vendor in path | Vendor abstracted | Plugin handles mapping |
| Server defines controls | Provider declares controls | Provider-centric |

### Backward Compatibility

v4 endpoints remain functional as aliases:
- `PUT /radars/{id}/power` → `PUT /radars/{id}/controls/power`
- `PUT /radars/{id}/range` → `PUT /radars/{id}/controls/range`
- etc.

Vendor-specific v4 paths (`/furuno/rezboost`) are deprecated. Use semantic IDs.

---

## Implementation Phases

### Phase 1: Types in mayara-core
- `CapabilityManifest`, `ControlDefinition`, `ControlConstraint`
- Model database with per-model capabilities
- `build_capabilities(discovery)` function

### Phase 2: WASM Exports
- `radar_get_capabilities()`
- `radar_get_state()` (v5 format)
- `radar_get_control()`, `radar_set_control()`

### Phase 3: Server Endpoints
- Add v5 routes to SignalK server
- Route through provider interface
- Maintain v4 backward compatibility

### Phase 4: Client Updates
- Update web UI to use capability-driven rendering
- Remove hardcoded vendor logic

---

## Open Questions

1. **Radar ID format**: Simple numbers (`"1"`, `"2"`) vs current format (`"radar-0"`)?
2. **Constraint evaluation**: Client-side only, or server validates constraints on PUT?
3. **State polling interval**: Recommended default? (Suggest 2 seconds)

---

## References

- [SignalK Specification](https://signalk.org/specification/)
- [mayara-lib Protocol Documentation](../../../mayara/docs/)
- [Furuno Radar Technology](https://www.furuno.com/en/technology/radar/)
- [Simrad HALO Radar](https://www.simrad-yachting.com/simrad/series/halo-radar/)
- [Raymarine Quantum Radar](https://www.raymarine.com/en-us/our-products/marine-radar/quantum)
