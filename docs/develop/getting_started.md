# Getting Started with Mayara Development

This guide helps new developers understand the codebase structure and get up and running quickly.

---

## Prerequisites

- **Rust** (stable toolchain) - `rustup` recommended
- **A radar** (optional for GUI work) - Furuno, Navico, Raymarine, or Garmin
- **Linux** (primary development platform) - Windows/Mac possible but less tested

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone the repository
git clone <repo-url> mayara
cd mayara
```

---

## Project Structure

```
mayara/
├── mayara-core/          # Platform-independent radar library (THE source of truth)
├── mayara-server/        # Standalone native server (Tokio async)
├── mayara-signalk-wasm/  # SignalK WASM plugin (uses RadarEngine from core)
├── mayara-gui/           # Web GUI (vanilla JS, VanJS, WebGPU)
└── docs/
    ├── design/           # Architecture documentation
    └── develop/          # Developer guides (you are here)
```

### Core Principle: mayara-core is the Single Source of Truth

All radar logic lives in `mayara-core`:
- Protocol parsing and command formatting
- Model database (ranges, capabilities per model)
- Control definitions (gain, sea, rain, etc.)
- Unified brand controllers (Furuno, Navico, Raymarine, Garmin)
- RadarEngine (unified management of controllers + ARPA, GuardZones, Trails, DualRange)

The server and WASM crates are **thin I/O adapters** around core. If you need to add radar functionality, it almost always goes in `mayara-core`.

---

## Quick Start: Running the Server

```bash
cd mayara/mayara-server

# Basic run
cargo run

# With debug logging for Furuno data parsing
RUST_LOG=mayara_server::brand::furuno::data=debug,info cargo run

# With trace logging (very verbose)
RUST_LOG=mayara_server::brand::furuno::data=trace,info cargo run
```

The server starts on `http://localhost:6502`. Open this in a browser to see the GUI.

### What Happens on Startup

1. Server starts Axum web server on port 6502
2. `CoreLocatorAdapter` runs `RadarLocator` from mayara-core
3. RadarLocator sends discovery packets for all brands (multicast join, broadcast)
4. When a radar responds, a brand-specific receiver is spawned
5. Radar appears in the GUI at `/` and can be viewed at `/viewer.html?radar=radar-1`

---

## Quick Start: Building the WASM Plugin

```bash
cd mayara/mayara-signalk-wasm

# Add WASM target if not already installed
rustup target add wasm32-wasip1

# Build WASM binary
cargo build --target wasm32-wasip1 --release

# Output: target/wasm32-wasip1/release/mayara_signalk_wasm.wasm
```

### Installing in SignalK

1. Copy the `.wasm` file to SignalK's plugin directory
2. Restart SignalK server
3. Enable the Mayara Radar plugin in SignalK's plugin configuration

### Key Differences from Server

| Aspect | Server | WASM |
|--------|--------|------|
| **Runtime** | Tokio async | Poll-based (no async) |
| **I/O** | TokioIoProvider (tokio sockets) | WasmIoProvider (SignalK FFI) |
| **Spokes/Revolution** | 8192 (native) | 512 (reduced for WebSocket) |
| **WebSocket** | Direct to browser | Through SignalK's WebSocket |

### Spoke Reduction

SignalK's WebSocket has aggressive rate limiting that disconnects slow consumers
(code 1008 "Client cannot keep up"). The WASM plugin reduces Furuno's 8192
spokes to 512 by combining 16 consecutive spokes using `max()` per pixel.

The `spokes_per_revolution` in capabilities is adjusted to 512 so the GUI
correctly maps spoke angles to 360 degrees.

---

## Web GUI Overview

The GUI is vanilla JavaScript with a few libraries:

| File | Purpose |
|------|---------|
| `index.html` | Landing page - lists discovered radars |
| `viewer.html` | Radar PPI display - WebGPU rendering of spokes |
| `control.html` | Control panel - sliders for gain, sea, rain, etc. |
| `mayara.js` | Main entry, VanJS reactive components |
| `viewer.js` | WebSocket spoke handling, rendering coordination |
| `control.js` | Control UI generation from /capabilities API |
| `render_webgpu.js` | GPU-accelerated radar renderer |
| `api.js` | REST/WebSocket client, auto-detects standalone vs SignalK mode |
| `van-*.js` | VanJS reactive UI library |

### Key GUI Concepts

**Dynamic Control UI**: The GUI doesn't hardcode controls. It fetches `/radars/{id}/capabilities` and dynamically builds sliders, toggles, and selects based on the response. Adding a new control to mayara-core automatically makes it appear in the GUI.

**WebGPU Rendering**: The PPI display uses WebGPU shaders for efficient radar rendering. Spokes arrive via WebSocket as protobuf messages and are drawn on the GPU.

**VanJS**: A minimal reactive UI library. State changes automatically update the DOM. See `van.state()` and `van.derive()` patterns in the code.

---

## REST API

The server exposes a SignalK-compatible REST API:

```bash
# List all radars
curl http://localhost:6502/v2/api/radars

# Get radar capabilities (controls, ranges, features)
curl http://localhost:6502/v2/api/radars/radar-1/capabilities

# Get current state (control values)
curl http://localhost:6502/v2/api/radars/radar-1/state

# Set a control
curl -X PUT http://localhost:6502/v2/api/radars/radar-1/controls/gain \
  -H "Content-Type: application/json" \
  -d '{"value": 50}'

# Set control with auto mode
curl -X PUT http://localhost:6502/v2/api/radars/radar-1/controls/gain \
  -H "Content-Type: application/json" \
  -d '{"value": 50, "auto": true}'
```

### WebSocket Endpoints

```
/v2/api/radars/{id}/spokes   - Binary protobuf spoke stream
/v2/api/radars/{id}/targets  - ARPA target updates (JSON)
```

---

## Code Navigation

### Where Things Live

| Task | Location |
|------|----------|
| Add/modify a control | `mayara-core/src/capabilities/controls.rs` |
| Add control to a model | `mayara-core/src/models/{brand}.rs` |
| Wire protocol encoding | `mayara-core/src/protocol/{brand}/` |
| Control dispatch (ID → command) | `mayara-core/src/protocol/{brand}/dispatch.rs` |
| Brand controllers | `mayara-core/src/controllers/{brand}.rs` |
| RadarEngine (unified features) | `mayara-core/src/engine/mod.rs` |
| Server HTTP handlers | `mayara-server/src/web.rs` |
| Server brand receivers | `mayara-server/src/brand/{brand}/` |
| WASM spoke handling | `mayara-signalk-wasm/src/spoke_receiver.rs` |
| GUI control panel | `mayara-gui/control.js` |
| GUI radar display | `mayara-gui/viewer.js` + `render_webgpu.js` |

### Following a Control Command

When a user moves a slider in the GUI:

1. **GUI** (`control.js`): PUT `/radars/{id}/controls/gain` with `{value: 50}`
2. **Server** (`web.rs`): Receives request, sends `ControlValue` to brand channel
3. **Brand receiver** (`brand/furuno/report.rs`): Calls controller method
4. **Core controller** (`controllers/furuno.rs`): `controller.set_gain(&mut io, 50, false)`
5. **Protocol dispatch** (`protocol/furuno/dispatch.rs`): Formats wire command
6. **IoProvider** (`tokio_io.rs`): Sends UDP/TCP packet to radar

### Following Spoke Data

When the radar sends a spoke:

1. **Radar hardware**: Sends UDP multicast packet
2. **Brand data receiver** (`brand/furuno/data.rs`): Parses packet using core protocol
3. **Spoke processing**: Applies trails, converts to protobuf
4. **WebSocket broadcast**: Sends to all connected clients
5. **GUI** (`viewer.js`): Decodes protobuf, passes to renderer
6. **WebGPU** (`render_webgpu.js`): Draws spoke on canvas

---

## Adding a New Feature

### Example: Adding a New Control

1. **Define the control** in mayara-core:
```rust
// mayara-core/src/capabilities/controls.rs
pub fn control_interference_rejection() -> ControlDefinition {
    ControlDefinition {
        id: "interferenceRejection".to_string(),
        name: "Interference Rejection".to_string(),
        control_type: ControlType::List,
        list_items: Some(vec!["Off", "Low", "Medium", "High"]),
        ..Default::default()
    }
}
```

2. **Add to model's control list**:
```rust
// mayara-core/src/models/furuno.rs
static CONTROLS_NXT: &[&str] = &[
    "gain", "sea", "rain", ...,
    "interferenceRejection",  // Add here
];
```

3. **Add dispatch entry**:
```rust
// mayara-core/src/protocol/furuno/dispatch.rs
pub fn format_control_command(id: &str, value: i32, auto: bool) -> Option<String> {
    match id {
        ...
        "interferenceRejection" => Some(format!("$S89,{}", value)),
        _ => None,
    }
}
```

4. **Done!** The GUI automatically shows the new control because it reads from `/capabilities`.

---

## Debugging Tips

### Logging

```bash
# See specific module logs
RUST_LOG=mayara_server::brand::furuno::data=debug,info cargo run

# Multiple modules
RUST_LOG=mayara_server::brand::furuno=debug,mayara_server::web=debug,info cargo run

# Everything (very verbose)
RUST_LOG=debug cargo run
```

### Network Capture

```bash
# Capture radar traffic on interface eth0
sudo tcpdump -i eth0 -w capture.pcap port 50100 or port 50102

# Watch multicast group joins
ip maddr show eth0

# Check which processes have ports open
ss -ulnp | grep mayara
```

### Common Issues

**"No radars found"**
- Check network connectivity to radar subnet
- Verify multicast routing: `ip route show table local | grep 239`
- Check firewall: `sudo iptables -L -n`
- Try running with `RUST_LOG=mayara_core=debug,info` to see discovery packets

**"Spokes not visible in GUI"**
- Check WebSocket connection in browser dev tools Network tab
- Look for "channel closed" warnings in server logs (normal if no clients connected)
- Verify radar is transmitting (not in standby)

**"Control changes don't work"**
- Check server logs for dispatch errors
- Verify control exists in `/capabilities` response
- Check if radar requires specific initialization sequence

---

## Testing

```bash
# Run all tests
cargo test --workspace

# Run specific crate tests
cargo test -p mayara-core

# Run with output
cargo test -- --nocapture
```

### Testing Without Hardware

For protocol and control logic testing, you can use the mock IoProvider pattern described in the architecture.md testing section.

---

## Next Steps

- Read [architecture.md](../design/architecture.md) for the full architectural picture
- Read [adding_radar_models.md](adding_radar_models.md) when adding support for new radar hardware
- Explore the codebase starting from `mayara-core/src/lib.rs`
- Run the server and play with the GUI to understand the user experience

---

## Getting Help

- Check existing code for patterns - the codebase is consistent
- Look at `mayara-core/src/protocol/{brand}/` for protocol examples
- The architecture.md document has detailed flow diagrams
