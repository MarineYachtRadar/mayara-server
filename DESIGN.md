# Mayara Architecture Design

## Goals

1. **Dual-target support**: Run as standalone server AND as SignalK WASM plugin
2. **Code sharing**: Single source of truth for radar protocol parsing
3. **Minimal changes**: Preserve existing mayara-lib/mayara-server functionality
4. **SignalK integration**: Proper Radar API following SignalK patterns

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         APPLICATIONS                                │
├───────────────────┬───────────────────┬─────────────────────────────┤
│   mayara-server   │    mayara-wasm    │      mayara-webapp          │
│  (Standalone)     │  (SignalK Plugin) │   (SignalK WebApp)          │
│                   │                   │                             │
│  - Axum server    │  - SignalK FFI    │  - PPI Radar Display        │
│  - WebSocket      │  - Poll-based I/O │  - Control Panel            │
│  - REST API       │  - Radar Provider │  - WebGL/2D Renderer        │
│  - Built-in UI    │                   │  - Connects to Radar API    │
└─────────┬─────────┴─────────┬─────────┴──────────────┬──────────────┘
          │                   │                        │
          ▼                   ▼                        ▼
┌─────────────────────────────────┐  ┌───────────────────────────────┐
│         mayara-lib              │  │   SignalK Socket FFI          │
│     (Native Runtime)            │  │                               │
│                                 │  │   - sk_udp_create/bind/send   │
│   - Tokio async runtime         │  │   - sk_udp_recv (non-block)   │
│   - tokio::net::UdpSocket       │  │   - sk_tcp_create/connect     │
│   - Platform networking         │  │   - sk_tcp_send/recv_line     │
│   - Session management          │  │   - Provided by SignalK       │
└────────────┬────────────────────┘  └───────────────────────────────┘
             │
             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                        mayara-core                                  │
│                  (Protocol Library - No I/O)                        │
│                                                                     │
│   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌────────────┐ │
│   │   Furuno    │  │   Navico    │  │  Raymarine  │  │   Garmin   │ │
│   │  Protocol   │  │  Protocol   │  │  Protocol   │  │  Protocol  │ │
│   └─────────────┘  └─────────────┘  └─────────────┘  └────────────┘ │
│                                                                     │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │  Data Structures: RadarInfo, Legend, Controls, Spoke        │   │
│   └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │  Protobuf: RadarMessage encoding                            │   │
│   └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Crate Responsibilities

### mayara-core

**Purpose**: Platform-independent radar protocol parsing

**Contains**:
- Protocol constants (ports, headers, addresses)
- Packet parsing functions (`&[u8]` → `Result<T>`)
- Command formatting functions (pure string generation)
- Data structures (RadarInfo, Legend, Controls)
- Spoke data structures and encoding
- Protobuf RadarMessage generation
- Brand detection logic

**Does NOT contain**:
- Any I/O operations
- Async/await or tokio
- Platform-specific code
- Network sockets

**Compiles to**: Native (all platforms) + WASM

```rust
// Example: Pure parsing function
pub fn parse_furuno_beacon(data: &[u8]) -> Result<FurunoReport, ParseError> {
    if data.len() < 32 {
        return Err(ParseError::TooShort);
    }
    if &data[0..11] != FURUNO_HEADER {
        return Err(ParseError::InvalidHeader);
    }
    Ok(FurunoReport {
        serial: String::from_utf8_lossy(&data[16..24]).to_string(),
        model: parse_model(&data[20..]),
        // ... pure data extraction
    })
}
```

### mayara-lib

**Purpose**: Native async runtime with real networking

**Contains**:
- Tokio async runtime integration
- UDP/TCP socket wrappers (tokio::net)
- Platform-specific code (Windows/Linux/macOS)
- Network interface enumeration
- Session and subsystem management
- Locator implementation using real sockets

**Depends on**: mayara-core

**Compiles to**: Native only (not WASM)

```rust
// Example: Uses mayara-core for parsing
use mayara_core::protocol::furuno;

async fn receive_beacon(&mut self) -> Result<()> {
    let (len, from) = self.socket.recv_from(&mut buf).await?;

    // Delegate parsing to mayara-core
    match furuno::parse_beacon_response(&buf[..len]) {
        Ok(report) => self.radar_found(report, from),
        Err(e) => log::warn!("Parse error: {}", e),
    }
    Ok(())
}
```

### mayara-wasm

**Purpose**: SignalK WASM plugin

**Contains**:
- SignalK FFI bindings (socket, delta emission)
- Poll-based I/O (no async)
- Radar Provider registration
- SignalK-specific configuration

**Depends on**: mayara-core (NOT mayara-lib)

**Compiles to**: WASM only (wasm32-wasip1)

```rust
// Example: Uses mayara-core with SignalK sockets
use mayara_core::protocol::furuno;
use crate::signalk_ffi::UdpSocket;

pub fn poll(&mut self) -> i32 {
    while let Some((len, addr, _)) = self.socket.recv_from(&mut self.buf) {
        // Same parsing logic as native
        match furuno::parse_beacon_response(&self.buf[..len]) {
            Ok(report) => self.emit_radar_found(report, addr),
            Err(e) => sk_debug(&format!("Parse error: {}", e)),
        }
    }
    0
}
```

### mayara-server

**Purpose**: Standalone HTTP/WebSocket server

**Contains**:
- Axum web server
- REST API endpoints
- WebSocket spoke streaming
- CLI interface

**Depends on**: mayara-lib

**Unchanged**: This crate requires no modifications

---

## SignalK Integration

### Radar API Design

Following the Autopilot API pattern (functional API, not resource storage):

```
/signalk/v2/api/vessels/self/radars
├── GET                     → List available radars
├── /{id}
│   ├── GET                 → Radar state (status, range, gain, legend, controls)
│   ├── PUT                 → Update controls
│   ├── /power
│   │   └── PUT             → standby/transmit
│   ├── /range
│   │   └── PUT             → Set range (meters)
│   ├── /gain
│   │   └── PUT             → Set gain (auto/manual + value)
│   └── /stream             → WebSocket for spoke data (optional)
```

### Stream URL Flexibility

Radar metadata includes optional `streamUrl`:

```json
{
  "id": "radar-0",
  "name": "Furuno DRS4D-NXT",
  "status": "transmit",
  "range": 2000,
  "streamUrl": "ws://192.168.1.100:3001/v1/api/spokes/radar-0"
}
```

- **If `streamUrl` present**: Client connects directly to external server (mayara-server)
- **If `streamUrl` absent**: Client connects to `/radars/{id}/stream` on SignalK

This allows:
1. WASM-only mode (discovery + control, external streaming)
2. Full WASM mode (future: streaming through SignalK)
3. Hybrid deployments

---

## Data Flow

### Native (mayara-server)

```
Radar Hardware
     │
     │ UDP (multicast/broadcast)
     ▼
┌─────────────────┐
│  mayara-lib     │
│  (Locator)      │ ──► mayara-core::parse_beacon()
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  mayara-lib     │
│  (DataReceiver) │ ──► mayara-core::parse_spoke()
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  mayara-server  │
│  (WebSocket)    │ ──► Protobuf RadarMessage to clients
└─────────────────┘
```

### WASM (SignalK Plugin)

```
Radar Hardware
     │
     │ UDP (multicast/broadcast)
     ▼
┌─────────────────┐
│  SignalK Server │
│  (socket-mgr)   │ ──► Buffers packets for WASM
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  mayara-wasm    │
│  (poll loop)    │ ──► mayara-core::parse_beacon()
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  SignalK Server │
│  (Radar API)    │ ──► REST metadata + optional stream proxy
└─────────────────┘
         │
         ▼
┌─────────────────┐
│  Freeboard-SK   │ ──► Renders radar on map
└─────────────────┘
```

---

## Migration Plan

### Phase 1: Create mayara-core
- [ ] New crate with minimal dependencies
- [ ] Move protocol constants
- [ ] Move parsing functions (Furuno, Navico, Raymarine, Garmin)
- [ ] Move data structures
- [ ] Add comprehensive tests

### Phase 2: Refactor mayara-lib
- [ ] Add dependency on mayara-core
- [ ] Replace inline parsing with mayara-core calls
- [ ] Re-export types for API compatibility
- [ ] Verify mayara-server still works

### Phase 3: Create mayara-wasm
- [x] New crate targeting wasm32-wasip1 (wasm32-unknown-unknown)
- [x] Implement SignalK socket FFI wrappers (UDP + TCP)
- [x] Implement radar locator using mayara-core
- [x] Register as SignalK Radar Provider
- [x] FurunoController for direct TCP radar control
- [x] Test with real hardware (DRS4D-NXT: transmit/standby verified)

### Phase 4: SignalK Radar API (separate PR)
- [ ] Add RadarProvider interface to SignalK
- [ ] Add `/vessels/self/radars` endpoints
- [ ] WebSocket streaming support
- [ ] Documentation

---

## File Structure

```
mayara/
├── Cargo.toml                    # Workspace definition
├── CHANGELOG.md                  # This refactoring changelog
├── DESIGN.md                     # This document
├── README.md                     # Project overview
│
├── mayara-core/                  # NEW: Protocol library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── brand.rs              # Brand enum
│       ├── error.rs              # ParseError types
│       ├── radar.rs              # RadarInfo, Legend, Controls
│       ├── spoke.rs              # Spoke data structures
│       ├── protocol/
│       │   ├── mod.rs
│       │   ├── furuno/
│       │   │   ├── mod.rs        # Furuno parsing
│       │   │   └── command.rs    # Furuno command formatting
│       │   ├── navico.rs         # Navico parsing
│       │   ├── raymarine.rs      # Raymarine parsing
│       │   └── garmin.rs         # Garmin parsing
│       └── protos/
│           └── RadarMessage.proto
│
├── mayara-lib/                   # MODIFIED: Now uses mayara-core
│   ├── Cargo.toml                # + mayara-core dependency
│   └── src/
│       ├── lib.rs                # Re-exports mayara-core types
│       ├── network/              # Unchanged
│       ├── locator.rs            # Calls mayara-core::parse_*
│       └── ...
│
├── mayara-server/                # UNCHANGED
│   ├── Cargo.toml
│   ├── src/
│   │   └── main.rs
│   └── web/                      # Existing web UI (also used by webapp)
│
├── mayara-signalk-wasm/          # SignalK WASM plugin
│   ├── Cargo.toml
│   ├── package.json              # SignalK WASM plugin manifest
│   └── src/
│       ├── lib.rs                # WASM exports
│       ├── signalk_ffi.rs        # Socket/delta FFI (UDP + TCP)
│       ├── locator.rs            # Poll-based radar locator
│       ├── radar_provider.rs     # SignalK Radar API
│       ├── furuno_controller.rs  # Direct TCP radar control
│       ├── spoke_receiver.rs     # Spoke data reception
│       └── protobuf.rs           # Protobuf encoding
│
└── mayara-webapp/                # NEW: SignalK WebApp (radar display)
    ├── package.json              # SignalK webapp manifest
    ├── public/
    │   ├── index.html            # Radar list
    │   ├── viewer.html           # PPI display
    │   ├── control.html          # Control panel
    │   ├── viewer.js
    │   ├── control.js
    │   ├── render_webgl.js
    │   ├── render_2d.js
    │   ├── style.css
    │   └── protobuf/
    └── README.md
```

---

## Dependencies

### mayara-core (minimal)
```toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"
# NO tokio, NO socket2, NO platform-specific crates
```

### mayara-lib (full native)
```toml
[dependencies]
mayara-core = { path = "../mayara-core" }
tokio = { version = "1", features = ["full"] }
socket2 = "0.5"
# ... existing dependencies
```

### mayara-wasm (minimal + FFI)
```toml
[dependencies]
mayara-core = { path = "../mayara-core" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
# NO tokio
```

---

## Testing Strategy

### mayara-core
- Unit tests for each protocol parser
- Property-based testing for packet parsing
- No mocking needed (pure functions)

### mayara-lib
- Integration tests with mock sockets
- Existing tests should pass unchanged

### mayara-wasm
- Test in SignalK with real hardware
- Use SignalK's WASM test infrastructure

---

---

## SignalK WebApp: mayara-webapp

The existing mayara web UI can be packaged as a SignalK WebApp for use when mayara-wasm provides the backend.

### Current Web Components

```
mayara-server/web/
├── index.html          # Radar list / status
├── viewer.html         # PPI radar display
├── viewer.js           # Spoke rendering logic
├── control.html        # Radar control panel
├── control.js          # Control WebSocket client
├── render_webgl.js     # WebGL spoke renderer
├── render_webgl_alt.js # Alternative WebGL renderer
├── render_2d.js        # 2D Canvas fallback
├── mayara.js           # Radar discovery client
├── style.css           # Styling
├── protobuf/           # Protobuf decoder
└── van-*.js            # VanJS UI library
```

### SignalK WebApp Packaging

```
mayara-webapp/
├── package.json        # SignalK webapp manifest
├── public/
│   ├── index.html
│   ├── viewer.html
│   ├── control.html
│   ├── *.js
│   └── style.css
└── README.md
```

**package.json** (SignalK WebApp manifest):
```json
{
  "name": "@mayara/radar-webapp",
  "version": "0.1.0",
  "description": "Mayara Radar PPI Display for SignalK",
  "signalk": {
    "webapp": true,
    "displayName": "Mayara Radar"
  }
}
```

### API Adaptation

The web UI currently calls:
- `GET /v1/api/radars` → Needs to call `/signalk/v2/api/vessels/self/radars`
- `WS streamUrl` → Can use either external or SignalK-proxied stream

Minimal changes needed:
```javascript
// Before (mayara-server)
fetch("/v1/api/radars")

// After (SignalK)
fetch("/signalk/v2/api/vessels/self/radars")
```

### Deployment Scenarios

| Scenario           | Backend       | WebApp        | Stream Source                 |
|--------------------|---------------|---------------|-------------------------------|
| Standalone         | mayara-server | Built-in      | mayara-server                 |
| SignalK + external | mayara-server | mayara-webapp | mayara-server (via streamUrl) |
| SignalK + WASM     | mayara-wasm   | mayara-webapp | SignalK proxy or external     |

---

## Furuno DRS4D-NXT Specifications

Reference specifications from official Furuno documentation.

### RF Transceiver

| Parameter | Value |
|-----------|-------|
| **Frequency** | |
| CH1 | 9380 MHz (P0N), 9400 MHz (Q0N) |
| CH2 | 9400 MHz (P0N), 9420 MHz (Q0N) |
| CH3 | 9420 MHz (P0N), 9440 MHz (Q0N) |
| **Pulse Length & PRR** | |
| P0N | 0.08μs to 1.2μs / 700 to 1100 Hz |
| Q0N | 5μs to 18μs / 700 to 1100 Hz |

### Antenna

| Parameter | Value |
|-----------|-------|
| **Rotation Speed** | 24\*/36/48 rpm (range coupled) or 24 rpm fixed |
| | \*In dual-range mode, speed is limited to 24 rpm |
| **Beam Width** | |
| Horizontal | 3.9° typical (-3 dB), adjustable 2° to 3.9° (RezBoost™) |
| Vertical | 25° |

### Range Scales

| Range | Notes |
|-------|-------|
| 0.0625 nm (1/16 nm) | Minimum |
| 0.125 nm (1/8 nm) | |
| 0.25 nm (1/4 nm) | |
| 0.5 nm (1/2 nm) | |
| 0.75 nm (3/4 nm) | |
| 1 nm | |
| 1.5 nm | |
| 2 nm | |
| 3 nm | |
| 4 nm | |
| 6 nm | |
| 8 nm | |
| 12 nm | Max in dual-range mode |
| 16 nm | |
| 24 nm | |
| 36 nm | |
| 48 nm | Maximum |

**Note**: In dual-range mode, range is limited to 12 nm.

### Range Table (meters)

Used in protocol commands and WASM plugin:

```rust
/// Furuno range table (wire_index -> meters)
/// Verified via Wireshark captures from TimeZero ↔ DRS4D-NXT
/// Note: Wire indices are non-sequential (21 is min, 19 is out of order)
pub const RANGE_TABLE: [(i32, i32); 18] = [
    (21, 116),   // 1/16 nm = 116m (minimum range)
    (0, 231),    // 1/8 nm = 231m
    (1, 463),    // 1/4 nm = 463m
    (2, 926),    // 1/2 nm = 926m
    (3, 1389),   // 3/4 nm = 1389m
    (4, 1852),   // 1 nm = 1852m
    (5, 2778),   // 1.5 nm = 2778m
    (6, 3704),   // 2 nm = 3704m
    (7, 5556),   // 3 nm = 5556m
    (8, 7408),   // 4 nm = 7408m
    (9, 11112),  // 6 nm = 11112m
    (10, 14816), // 8 nm = 14816m
    (11, 22224), // 12 nm = 22224m
    (12, 29632), // 16 nm = 29632m
    (13, 44448), // 24 nm = 44448m
    (14, 59264), // 32 nm = 59264m
    (19, 66672), // 36 nm = 66672m (out of sequence!)
    (15, 88896), // 48 nm = 88896m (maximum range)
];
```

---

## Questions for Review

1. Should mayara-core include protobuf encoding, or keep that in mayara-lib/mayara-wasm?
2. Should we support feature flags in mayara-core for individual brands?
3. Should mayara-wasm support spoke streaming, or only discovery/control?
4. Should mayara-webapp be a separate npm package or part of mayara-wasm?
