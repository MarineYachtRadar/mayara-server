# Mayara Architecture

> This document describes the architecture of the Mayara radar system,
> showing what is shared between deployment modes and the path to maximum code reuse.

---

## FUNDAMENTAL PRINCIPLE: mayara-core is the Single Source of Truth

**This is the most important architectural concept in Mayara.**

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        mayara-core (THE DATABASE)                            │
│                                                                              │
│   Contains ALL knowledge about radars:                                       │
│   - Model database (ranges, spokes, capabilities per model)                  │
│   - Control definitions (what controls exist, their types, min/max, units)   │
│   - Protocol specifications (wire format, parsing, command dispatch)         │
│   - Feature flags (doppler, dual-range, no-transmit zones, etc.)            │
│   - Connection state machine (platform-independent)                          │
│   - I/O abstraction (IoProvider trait)                                      │
│   - RadarLocator (discovery logic)                                          │
│                                                                              │
│   THIS IS THE ONLY PLACE WHERE RADAR LOGIC IS DEFINED.                      │
│   SERVER AND WASM ARE THIN I/O ADAPTERS AROUND CORE.                        │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ adapters implement IoProvider
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           I/O Provider Layer                                 │
│                                                                              │
│  ┌─────────────────────────┐          ┌─────────────────────────┐           │
│  │    TokioIoProvider      │          │     WasmIoProvider      │           │
│  │    (mayara-server)      │          │  (mayara-signalk-wasm)  │           │
│  │                         │          │                         │           │
│  │  Wraps tokio sockets    │          │  Wraps SignalK FFI      │           │
│  │  in poll-based API      │          │  socket calls           │           │
│  └─────────────────────────┘          └─────────────────────────┘           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ exposes via
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           REST API (SignalK-compatible)                      │
│                                                                              │
│   GET /radars/{id}/capabilities    ← Returns model info from mayara-core    │
│   GET /radars/{id}/state           ← Current control values                 │
│   PUT /radars/{id}/controls/{id}   ← Set control values                     │
│                                                                              │
│   The API is the CONTRACT. All clients use ONLY the API.                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ consumed by
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              ALL CLIENTS                                     │
│                                                                              │
│   - WebGUI (mayara-gui/)           - Reads /capabilities to know what       │
│   - mayara-server internal logic     controls to display                    │
│   - Future: mayara_opencpn         - Dynamically builds UI from API         │
│   - Future: mobile apps            - NEVER hardcodes radar capabilities     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### What This Means in Practice

1. **mayara-core defines everything:**
   - All radar models and their specifications
   - All control types (gain, sea, rain, dopplerMode, etc.)
   - Valid ranges per model
   - Available features per model
   - Wire protocol encoding/decoding
   - **Command dispatch** (control ID → wire command)
   - **Connection state machine** (Disconnected → Connecting → Connected → Active)

2. **mayara-server and mayara-signalk-wasm are thin adapters:**
   - Implement `IoProvider` trait for their platform
   - Run the **same** RadarLocator code from mayara-core
   - Use the **same** dispatch functions for control commands
   - No hardcoded control names, range tables, or protocol details

3. **The REST API is the contract:**
   - `/capabilities` returns what the radar can do (from mayara-core)
   - Clients build their UI dynamically from this response
   - Same WebGUI works for ANY radar brand because it follows the API

4. **Adding a new control:**
   - Add definition to `mayara-core/capabilities/controls.rs`
   - Add dispatch entry in `mayara-core/protocol/{brand}/dispatch.rs`
   - Add to model's control list in `mayara-core/models/{brand}.rs`
   - **Server and WASM automatically pick it up - no changes needed!**

---

## Current Crate Structure (December 2025)

```
mayara/
├── mayara-core/                    # Platform-independent radar library
│   └── src/
│       ├── lib.rs                  # Re-exports: Brand, IoProvider, RadarLocator, controllers, etc.
│       ├── io.rs                   # IoProvider trait (UDP/TCP abstraction)
│       ├── locator.rs              # RadarLocator (multi-brand discovery)
│       ├── connection.rs           # ConnectionState, ConnectionManager, furuno login
│       ├── state.rs                # RadarState, PowerState (control values)
│       ├── brand.rs                # Brand enum (Furuno, Navico, Raymarine, Garmin)
│       ├── radar.rs                # RadarDiscovery struct
│       ├── error.rs                # ParseError type
│       ├── dual_range.rs           # Dual-range controller logic
│       │
│       ├── controllers/            # ★ UNIFIED BRAND CONTROLLERS ★
│       │   ├── mod.rs              # Re-exports all controllers
│       │   ├── furuno.rs           # FurunoController (TCP login + commands)
│       │   ├── navico.rs           # NavicoController (UDP multicast)
│       │   ├── raymarine.rs        # RaymarineController (Quantum/RD)
│       │   └── garmin.rs           # GarminController (UDP)
│       │
│       ├── protocol/               # Wire protocol (encoding/decoding)
│       │   ├── furuno/
│       │   │   ├── mod.rs          # Beacon parsing, spoke parsing, constants
│       │   │   ├── command.rs      # Format functions (format_gain_command, etc.)
│       │   │   ├── dispatch.rs     # Control dispatch (ID → wire command)
│       │   │   └── report.rs       # TCP response parsing
│       │   ├── navico.rs           # Navico: report parsing + nav packet formatting
│       │   ├── raymarine.rs        # Raymarine protocol
│       │   └── garmin.rs           # Garmin protocol
│       │
│       ├── models/                 # Radar model database
│       │   ├── furuno.rs           # DRS4D-NXT, DRS6A-NXT, etc. (ranges, controls)
│       │   ├── navico.rs           # HALO, 4G, 3G, BR24
│       │   ├── raymarine.rs        # Quantum, RD series
│       │   └── garmin.rs           # xHD series
│       │
│       ├── capabilities/           # Control definitions
│       │   ├── controls.rs         # 40+ definitions + batch getters (get_base_*, get_all_*)
│       │   └── builder.rs          # Capability manifest builder
│       │
│       ├── arpa/                   # ARPA target tracking
│       │   ├── detector.rs         # Contour detection
│       │   ├── tracker.rs          # Kalman filter tracking
│       │   ├── cpa.rs              # CPA/TCPA calculation
│       │   └── ...
│       │
│       ├── trails/                 # Target trail history
│       └── guard_zones/            # Guard zone alerting
│
├── mayara-server/                  # Standalone native server
│   └── src/
│       ├── main.rs                 # Entry point, tokio runtime
│       ├── lib.rs                  # Session, Cli, VERSION exports
│       ├── tokio_io.rs             # TokioIoProvider (implements IoProvider)
│       ├── core_locator.rs         # CoreLocatorAdapter (wraps mayara-core RadarLocator)
│       ├── locator.rs              # Legacy platform-specific locator
│       ├── web.rs                  # Axum HTTP/WebSocket handlers
│       ├── settings.rs             # SharedControls wrapper for radar state
│       ├── control_factory.rs      # Batch control builders (uses core get_base_*, get_all_*)
│       ├── storage.rs              # Local applicationData storage
│       ├── navdata.rs              # NMEA/SignalK navigation input
│       │
│       ├── brand/                  # Brand-specific async adapters
│       │   ├── furuno/             # Async report/data receivers, delegates to core
│       │   ├── navico/             # report.rs + info.rs use core protocol/navico.rs
│       │   ├── raymarine/          # Async report/data receivers, delegates to core
│       │   └── garmin/             # Discovery only (controller integration pending)
│       │
│       └── recording/              # Radar recording and playback
│           ├── mod.rs              # Module exports
│           ├── file_format.rs      # .mrr binary format read/write
│           ├── recorder.rs         # Subscribes to broadcast, writes .mrr files
│           ├── player.rs           # Reads .mrr, emits as virtual radar
│           └── manager.rs          # File listing, metadata, CRUD operations
│
├── mayara-signalk-wasm/            # SignalK WASM plugin 
│   └── src/
│       ├── lib.rs                  # WASM entry point, plugin exports
│       ├── wasm_io.rs              # WasmIoProvider (implements IoProvider)
│       ├── locator.rs              # Re-exports RadarLocator from mayara-core
│       ├── radar_provider.rs       # RadarProvider (needs update to unified controllers)
│       ├── spoke_receiver.rs       # UDP spoke data receiver
│       └── signalk_ffi.rs          # SignalK FFI bindings
│
├── mayara-gui/                     # Shared web GUI assets
│   ├── index.html                  # Landing page with radar list
│   ├── viewer.html                 # Radar PPI display page
│   ├── control.html                # Radar controls panel
│   ├── recordings.html             # Recording/playback control page
│   ├── mayara.js                   # Main entry, VanJS components
│   ├── viewer.js                   # WebSocket spoke handling, rendering coordination
│   ├── control.js                  # Control UI, API interactions
│   ├── recordings.js               # Recording/playback UI logic
│   ├── render_webgpu.js            # WebGPU-based radar renderer (GPU-accelerated)
│   ├── api.js                      # REST/WebSocket API client, auto-detects mode
│   └── van-*.js                    # VanJS reactive UI library
│
├── mayara-server-signalk-plugin/   # SignalK plugin (connects to mayara-server)
│   ├── package.json                # npm manifest, SignalK webapp config
│   ├── build.js                    # Copies mayara-gui to public/
│   └── plugin/
│       └── index.js                # Main plugin: MayaraClient, RadarProvider
│
└── mayara-server-signalk-playbackrecordings-plugin/  # SignalK playback plugin (developer tool)
    ├── package.json                # npm manifest, SignalK webapp config
    ├── build.js                    # Copies mayara-gui (minus recordings.html), adds playback.html
    └── plugin/
        ├── index.js                # MrrPlayer, playback API endpoints
        ├── mrr-reader.js           # JavaScript port of file_format.rs
        └── public/
            └── playback.html       # Custom upload/playback UI
```

---

## The IoProvider Architecture

**Key Insight:** Both WASM and Server use the **exact same** radar logic from mayara-core.
The only difference is how sockets are implemented.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           mayara-core                                        │
│                    (Pure Rust, no I/O, WASM-compatible)                      │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                       IoProvider Trait                               │    │
│  │  (mayara-core/io.rs)                                                 │    │
│  │                                                                      │    │
│  │  trait IoProvider {                                                  │    │
│  │      // UDP: create, bind, broadcast, multicast, send, recv, close   │    │
│  │      // TCP: create, connect, send, recv_line, recv_raw, close       │    │
│  │      // Utility: current_time_ms(), debug()                          │    │
│  │  }                                                                   │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                       RadarLocator                                   │    │
│  │  (mayara-core/locator.rs)                                           │    │
│  │                                                                      │    │
│  │  - Multi-brand discovery (Furuno, Navico, Raymarine, Garmin)         │    │
│  │  - Beacon packet construction                                        │    │
│  │  - Multicast group management                                        │    │
│  │  - Radar identification and deduplication                            │    │
│  │                                                                      │    │
│  │  Uses IoProvider for all I/O:                                        │    │
│  │    fn start<I: IoProvider>(&mut self, io: &mut I)                    │    │
│  │    fn poll<I: IoProvider>(&mut self, io: &mut I) -> Vec<Discovery>   │    │
│  │    fn shutdown<I: IoProvider>(&mut self, io: &mut I)                 │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                       ConnectionManager                              │    │
│  │  (mayara-core/connection.rs)                                         │    │
│  │                                                                      │    │
│  │  - ConnectionState enum (Disconnected → Connected → Active)          │    │
│  │  - Exponential backoff logic (1s, 2s, 4s, 8s, max 30s)              │    │
│  │  - Furuno login protocol constants and parsing                       │    │
│  │  - ReceiveSocketType (multicast/broadcast fallback)                  │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                       Dispatch Functions                             │    │
│  │  (mayara-core/protocol/furuno/dispatch.rs)                          │    │
│  │                                                                      │    │
│  │  - format_control_command(id, value, auto) → wire command            │    │
│  │  - format_request_command(id) → request command                      │    │
│  │  - parse_control_response(line) → ControlUpdate enum                 │    │
│  │                                                                      │    │
│  │  Controllers call dispatch, not individual format functions!         │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                       Unified Brand Controllers                      │    │
│  │  (mayara-core/controllers/)                                         │    │
│  │                                                                      │    │
│  │  FurunoController   - TCP login + command, uses dispatch functions   │    │
│  │  NavicoController   - UDP multicast, BR24/3G/4G/HALO support        │    │
│  │  RaymarineController - UDP, Quantum (solid-state) / RD (magnetron)  │    │
│  │  GarminController   - UDP multicast, xHD series                     │    │
│  │                                                                      │    │
│  │  All controllers use IoProvider for I/O:                            │    │
│  │    fn poll<I: IoProvider>(&mut self, io: &mut I) -> bool            │    │
│  │    fn set_gain<I: IoProvider>(&mut self, io: &mut I, value, auto)   │    │
│  │    fn shutdown<I: IoProvider>(&mut self, io: &mut I)                │    │
│  │                                                                      │    │
│  │  SAME CODE runs on both server (tokio) and WASM (FFI)!              │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    │                               │
                    ▼                               ▼
     ┌────────────────────────────┐    ┌────────────────────────────┐
     │      TokioIoProvider       │    │      WasmIoProvider        │
     │   (mayara-server)          │    │   (mayara-signalk-wasm)    │
     │                            │    │                            │
     │   impl IoProvider for      │    │   impl IoProvider for      │
     │   TokioIoProvider {        │    │   WasmIoProvider {         │
     │     fn udp_create() {      │    │     fn udp_create() {      │
     │       socket2::Socket::new │    │       sk_udp_create()      │
     │       tokio::UdpSocket     │    │     }                      │
     │     }                      │    │     fn udp_send_to() {     │
     │     fn udp_recv_from() {   │    │       sk_udp_send()        │
     │       socket.try_recv_from │    │     }                      │
     │     }                      │    │   }                        │
     │   }                        │    │                            │
     └────────────────────────────┘    └────────────────────────────┘
```

### Server's CoreLocatorAdapter

The server wraps mayara-core's sync RadarLocator in an async adapter:

```rust
// mayara-server/src/core_locator.rs

pub struct CoreLocatorAdapter {
    locator: RadarLocator,       // from mayara-core (sync)
    io: TokioIoProvider,         // platform I/O adapter
    discovery_tx: mpsc::Sender<LocatorMessage>,
    poll_interval: Duration,     // default: 100ms
}

impl CoreLocatorAdapter {
    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<...> {
        self.locator.start(&mut self.io);  // Same code as WASM!

        loop {
            select! {
                _ = subsys.on_shutdown_requested() => break,
                _ = poll_timer.tick() => {
                    let discoveries = self.locator.poll(&mut self.io);  // Same!
                    for d in discoveries {
                        self.discovery_tx.send(LocatorMessage::RadarDiscovered(d)).await;
                    }
                }
            }
        }
        self.locator.shutdown(&mut self.io);
    }
}
```

---

## Implementation Status (December 2025)

### ✅ Fully Implemented (Server + WASM)

| Component | Location | Notes |
|-----------|----------|-------|
| **Protocol parsing** | mayara-core/protocol/ | All 4 brands: Furuno, Navico, Raymarine, Garmin |
| **Protocol formatting** | mayara-core/protocol/navico.rs | Navigation packets (heading/SOG/COG) |
| **Model database** | mayara-core/models/ | All models with ranges, spokes, capabilities |
| **Control definitions** | mayara-core/capabilities/ | 40+ controls (v5 API) |
| **Batch control init** | mayara-core/capabilities/controls.rs | get_base_controls_for_brand(), get_all_controls_for_model() |
| **IoProvider trait** | mayara-core/io.rs | Platform-independent I/O abstraction |
| **RadarLocator** | mayara-core/locator.rs | Multi-brand discovery via IoProvider |
| **ConnectionManager** | mayara-core/connection.rs | State machine, backoff, Furuno login |
| **RadarState types** | mayara-core/state.rs | Control values, update_from_response() |
| **Dispatch functions** | mayara-core/protocol/furuno/dispatch.rs | Control ID → wire command routing |
| **Unified Controllers** | mayara-core/controllers/ | All 4 brands: FurunoController, NavicoController, RaymarineController, GarminController |
| **RadarEngine** | mayara-core/engine/ | Unified management of controllers + feature processors |
| **ARPA tracking** | mayara-core/arpa/ | Kalman filter, CPA/TCPA, contour detection |
| **Trails history** | mayara-core/trails/ | Target position storage |
| **Guard zones** | mayara-core/guard_zones/ | Zone alerting logic |
| **Dual-range** | mayara-core/dual_range.rs | Dual-range controller for supported models |
| **TokioIoProvider** | mayara-server/tokio_io.rs | Tokio sockets implementing IoProvider |
| **CoreLocatorAdapter** | mayara-server/core_locator.rs | Async wrapper for RadarLocator |
| **Standalone server** | mayara-server/ | Full functionality, uses RadarEngine |
| **Web GUI** | mayara-gui/ | WebGPU rendering, VanJS framework |
| **Local storage API** | mayara-server/storage.rs | SignalK-compatible applicationData |
| **WasmIoProvider** | mayara-signalk-wasm/wasm_io.rs | SignalK FFI socket wrapper |
| **SignalK WASM plugin** | mayara-signalk-wasm/ | Uses RadarEngine, thin shell around core |

### Server Brand Controller Integration

The server's brand modules now delegate to unified core controllers:

| Brand | Core Controller | Server Integration | Status |
|-------|-----------------|-------------------|--------|
| **Furuno** | `FurunoController` (TCP login + commands) | `brand/furuno/report.rs` uses core | ✅ Complete |
| **Navico** | `NavicoController` (UDP multicast) | `report.rs` + `info.rs` use core protocol | ✅ Complete |
| **Raymarine** | `RaymarineController` (Quantum/RD) | `brand/raymarine/report.rs` uses core | ✅ Complete |
| **Garmin** | `GarminController` (UDP) | Core ready, server uses legacy locator | 🚧 Partial |

The server's `brand/` modules still handle:
- Async spoke data reception (tokio streams)
- Radar discovery and lifecycle management
- Control value caching and broadcasting
- WebSocket spoke streaming to clients
- Navigation data sending (Navico `info.rs` uses core formatting functions)

### ✅ Recently Implemented

| Component | Notes |
|-----------|-------|
| mayara-server-signalk-plugin | Native JS plugin connecting SignalK to mayara-server (see External Clients section) |
| Recording/Playback (mayara-server) | .mrr file format, recording, playback, REST API (see Recording and Playback System) |
| recordings.html/js (mayara-gui) | Web UI for recording and playback control |
| mayara-server-signalk-playbackrecordings-plugin | SignalK playback plugin for developers (no mayara-server required) |

### ❌ Not Yet Implemented

| Component | Notes |
|-----------|-------|
| mayara_opencpn plugin | OpenCPN integration (see External Clients section) |
| Garmin server controller | Server still uses old locator-based approach |
| Playback speed control | Currently plays at recorded speed only |
| Playback seek | Timeline seeking not yet implemented |

---

## Deployment Modes

### Mode 1: SignalK WASM Plugin (✅ Complete)

> **Note:** The WASM plugin is now fully integrated with the unified RadarEngine
> architecture from mayara-core. It shares the same controllers, ARPA, guard zones,
> trails, and dual-range logic as the server.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    SignalK Server (Node.js)                                  │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │              WASM Runtime (wasmer)                                      │ │
│  │  ┌──────────────────────────────────────────────────────────────────┐  │ │
│  │  │         mayara-signalk-wasm                                       │  │ │
│  │  │                                                                   │  │ │
│  │  │  ┌──────────────────┐  ┌───────────────────────────────────────┐ │  │ │
│  │  │  │  WasmIoProvider  │  │   RadarLocator (from mayara-core)     │ │  │ │
│  │  │  │  (FFI sockets)   │──│   SAME CODE AS SERVER                 │ │  │ │
│  │  │  └──────────────────┘  └───────────────────────────────────────┘ │  │ │
│  │  │                                                                   │  │ │
│  │  │  ┌──────────────────────────────────────────────────────────┐    │  │ │
│  │  │  │         Unified Controllers (from mayara-core)            │    │  │ │
│  │  │  │  FurunoController   │ NavicoController   (SAME CODE!)     │    │  │ │
│  │  │  │  RaymarineController│ GarminController   (AS SERVER!)     │    │  │ │
│  │  │  └──────────────────────────────────────────────────────────┘    │  │ │
│  │  └──────────────────────────────────────────────────────────────────┘  │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Characteristics:**
- Runs inside SignalK's WASM sandbox
- Uses SignalK FFI for all network I/O via WasmIoProvider
- Poll-based (no async runtime in WASM)
- **Same RadarLocator AND Controllers as server** (all 4 brands!)
- Uses RadarEngine from mayara-core for unified feature management

**Spoke Reduction:** The WASM plugin reduces Furuno's native 8192 spokes to 512
per revolution. This is necessary because SignalK's WebSocket cannot sustain
the data rate of full-resolution spokes (code 1008 "Client cannot keep up").
The `spokes_per_revolution` in capabilities is adjusted to match the actual
output, ensuring the GUI correctly maps spoke angles to 360 degrees.

### Mode 2: Standalone Server

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    mayara-server (Rust)                                      │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                     CoreLocatorAdapter                               │    │
│  │  ┌──────────────────┐  ┌───────────────────────────────────────┐    │    │
│  │  │  TokioIoProvider │  │   RadarLocator (from mayara-core)     │    │    │
│  │  │  (tokio sockets) │──│   SAME CODE AS WASM                   │    │    │
│  │  └──────────────────┘  └───────────────────────────────────────┘    │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │   Brand Adapters (brand/) + Core Controllers (controllers/)          │    │
│  │   - Async receivers in brand/ handle tokio sockets, spoke streaming  │    │
│  │   - Delegate control commands to mayara-core unified controllers     │    │
│  │   - TokioIoProvider implements IoProvider for controller I/O         │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │              Axum Router (web.rs)                                    │    │
│  │   /radars/*, /targets/*, static files (rust-embed from mayara-gui/) │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Characteristics:**
- Native Rust binary with tokio async runtime
- Direct network I/O via TokioIoProvider
- Axum web server hosts API + GUI
- **Same RadarLocator AND Controllers as WASM** (from mayara-core)
- **Same API paths as SignalK** → same GUI works unchanged

---

## What Gets Shared

| Component | Location | WASM | Server | Notes |
|-----------|----------|:----:|:------:|-------|
| **Protocol parsing** | mayara-core/protocol/ | ✓ | ✓ | Packet encode/decode |
| **Protocol formatting** | mayara-core/protocol/navico.rs | ✓ | ✓ | Heading/SOG/COG packets |
| **Model database** | mayara-core/models/ | ✓ | ✓ | Ranges, capabilities |
| **Control definitions** | mayara-core/capabilities/ | ✓ | ✓ | v5 API schemas |
| **Batch control init** | mayara-core/capabilities/controls.rs | ✓ | ✓ | get_base_*, get_all_* |
| **IoProvider trait** | mayara-core/io.rs | ✓ | ✓ | Socket abstraction |
| **RadarLocator** | mayara-core/locator.rs | ✓ | ✓ | **Same discovery code!** |
| **Unified Controllers** | mayara-core/controllers/ | ✓ | ✓ | **ALL 4 brands!** |
| **ConnectionManager** | mayara-core/connection.rs | ✓ | ✓ | State machine, backoff |
| **Dispatch functions** | mayara-core/protocol/furuno/dispatch.rs | ✓ | ✓ | Control routing |
| **RadarState** | mayara-core/state.rs | ✓ | ✓ | update_from_response() |
| **ARPA** | mayara-core/arpa/ | ✓ | ✓ | Target tracking |
| **Trails** | mayara-core/trails/ | ✓ | ✓ | Position history |
| **Guard zones** | mayara-core/guard_zones/ | ✓ | ✓ | Alerting logic |
| **Web GUI** | mayara-gui/ | ✓ | ✓ | Shared assets |

**What's platform-specific:**
- TokioIoProvider (mayara-server) - wraps tokio sockets
- WasmIoProvider (mayara-signalk-wasm) - wraps SignalK FFI
- Axum web server (mayara-server only)
- Spoke data receivers (async in server, poll-based in WASM)

---

## Unified Controllers Architecture

The most significant architectural advancement is the **unified controller system** in `mayara-core/controllers/`. This eliminates code duplication between server and WASM, ensuring identical behavior across platforms.

### Controller Design Principles

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      Controller Design Pattern                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  1. Poll-based (not async) → works in WASM without runtime                  │
│  2. IoProvider abstraction → no direct socket calls                         │
│  3. State machine → handles connect/disconnect/reconnect                    │
│  4. Brand-specific protocol → TCP (Furuno) or UDP (Navico/Raymarine/Garmin) │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                      Controller Interface                               │ │
│  │                                                                         │ │
│  │  fn new(radar_id, address, ...) -> Self                                │ │
│  │  fn poll<I: IoProvider>(&mut self, io: &mut I) -> bool                 │ │
│  │  fn is_connected(&self) -> bool                                        │ │
│  │  fn state(&self) -> ControllerState                                    │ │
│  │                                                                         │ │
│  │  // Control setters (all take IoProvider)                              │ │
│  │  fn set_power<I: IoProvider>(&mut self, io: &mut I, transmit: bool)    │ │
│  │  fn set_range<I: IoProvider>(&mut self, io: &mut I, meters: u32)       │ │
│  │  fn set_gain<I: IoProvider>(&mut self, io: &mut I, value: u32, auto)   │ │
│  │  fn set_sea<I: IoProvider>(&mut self, io: &mut I, value: u32, auto)    │ │
│  │  fn set_rain<I: IoProvider>(&mut self, io: &mut I, value: u32, auto)   │ │
│  │  ...                                                                    │ │
│  │                                                                         │ │
│  │  fn shutdown<I: IoProvider>(&mut self, io: &mut I)                     │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Controller State Machines

Each controller manages its own connection state:

```
                    ┌──────────────┐
                    │ Disconnected │ ◄──────────────────────────────┐
                    └──────┬───────┘                                │
                           │ poll() creates sockets                 │
                           ▼                                        │
                    ┌──────────────┐                                │
                    │  Listening   │  (UDP: waiting for reports)    │
                    │  Connecting  │  (TCP: waiting for connect)    │
                    └──────┬───────┘                                │
                           │ reports received / TCP connected       │
                           ▼                                        │
                    ┌──────────────┐                                │
                    │  Connected   │  (ready for commands)          │
                    └──────┬───────┘                                │
                           │ connection lost / timeout              │
                           └────────────────────────────────────────┘
```

### Brand-Specific Details

| Brand | Protocol | Connection | Special Features |
|-------|----------|------------|------------------|
| **Furuno** | TCP | Login sequence (root) | NXT Doppler modes, ~30 controls |
| **Navico** | UDP multicast | Report multicast join | BR24/3G/4G/HALO, Doppler (HALO) |
| **Raymarine** | UDP | Report multicast | Quantum (solid-state) vs RD (magnetron) |
| **Garmin** | UDP multicast | Report multicast | xHD series, simple protocol |

### RaymarineController Variants

Raymarine radars come in two fundamentally different types with incompatible command formats:

```
┌────────────────────────────────────────────────────────────────────────────┐
│                        RaymarineController                                  │
│  (mayara-core/controllers/raymarine.rs)                                    │
├────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  RaymarineVariant::Quantum (Solid-State)                                   │
│  ├── Command format: [opcode_lo, opcode_hi, 0x28, 0x00, 0x00, value, ...]  │
│  ├── One-byte values: quantum_one_byte_command(opcode, value)              │
│  ├── Two-byte values: quantum_two_byte_command(opcode, value)              │
│  └── Models: Quantum, Quantum 2                                            │
│                                                                             │
│  RaymarineVariant::RD (Magnetron)                                          │
│  ├── Command format: [0x00, 0xc1, lead_bytes..., value, 0x00, ...]        │
│  ├── Standard: rd_standard_command(lead, value)                            │
│  ├── On/Off: rd_on_off_command(lead, on_off)                              │
│  └── Models: RD418D, RD418HD, RD424D, RD424HD, RD848                       │
│                                                                             │
│  The server creates the correct variant when model is detected:            │
│    RaymarineController::new(..., RaymarineVariant::Quantum, ...)           │
│    RaymarineController::new(..., RaymarineVariant::RD, ...)                │
│                                                                             │
└────────────────────────────────────────────────────────────────────────────┘
```

### Usage Example (WASM)

```rust
// mayara-signalk-wasm/src/radar_provider.rs

use mayara_core::controllers::{
    FurunoController, NavicoController, RaymarineController, GarminController,
};
use mayara_core::Brand;

struct RadarProvider {
    io: WasmIoProvider,
    furuno_controllers: BTreeMap<String, FurunoController>,
    navico_controllers: BTreeMap<String, NavicoController>,
    raymarine_controllers: BTreeMap<String, RaymarineController>,
    garmin_controllers: BTreeMap<String, GarminController>,
}

impl RadarProvider {
    fn poll(&mut self) {
        // Poll all controllers - same code regardless of platform!
        for controller in self.furuno_controllers.values_mut() {
            controller.poll(&mut self.io);
        }
        for controller in self.navico_controllers.values_mut() {
            controller.poll(&mut self.io);
        }
        // ... etc
    }

    fn set_gain(&mut self, radar_id: &str, value: u32, auto: bool) {
        if let Some(c) = self.furuno_controllers.get_mut(radar_id) {
            c.set_gain(&mut self.io, value, auto);
        } else if let Some(c) = self.navico_controllers.get_mut(radar_id) {
            c.set_gain(&mut self.io, value, auto);
        }
        // ... etc
    }
}
```

### Server Integration Pattern

The server's `brand/` modules wrap core controllers with async/tokio integration:

```rust
// mayara-server/src/brand/raymarine/report.rs (simplified)

use mayara_core::controllers::{RaymarineController, RaymarineVariant};
use crate::tokio_io::TokioIoProvider;

pub struct RaymarineReportReceiver {
    controller: Option<RaymarineController>,  // Core controller
    io: TokioIoProvider,                       // Platform I/O adapter
    // ... other fields for spoke data, trails, etc.
}

impl RaymarineReportReceiver {
    // When model is detected, create the appropriate variant
    fn on_model_detected(&mut self, model: &RaymarineModel) {
        self.controller = Some(RaymarineController::new(
            &self.key,
            &self.info.send_command_addr.ip().to_string(),
            self.info.send_command_addr.port(),
            &self.info.report_addr.ip().to_string(),
            self.info.report_addr.port(),
            if model.is_quantum() { RaymarineVariant::Quantum }
            else { RaymarineVariant::RD },
            model.doppler,
        ));
    }

    // Control requests come through ControlValue channel
    async fn send_control_to_radar(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        let controller = self.controller.as_mut()
            .ok_or_else(|| RadarError::CannotSetControlType("Controller not initialized".into()))?;

        match cv.id.as_str() {
            "power" => controller.set_power(&mut self.io, cv.value as u8),
            "range" => controller.set_range(&mut self.io, cv.value as u32),
            "gain" => controller.set_gain(&mut self.io, cv.value as u32, cv.auto.unwrap_or(false)),
            // ... 20+ more controls
            _ => return Err(RadarError::CannotSetControlType(cv.id.clone())),
        }
        Ok(())
    }
}
```

**Key insight:** The server's brand modules are now thin dispatchers that:
1. Create core controllers when radar model is detected
2. Route control requests to the appropriate core controller method
3. Handle async spoke data reception (still server-specific)
4. Manage WebSocket broadcasting to clients

### Benefits of Unified Controllers

| Benefit | Description |
|---------|-------------|
| **Single source of truth** | Fix bugs once, fixed everywhere |
| **Consistent behavior** | WASM and server behave identically |
| **Easier testing** | Mock IoProvider for unit tests |
| **Reduced code size** | ~1500 lines shared vs ~3000 lines duplicated |
| **Faster feature development** | Add control to core, works on all platforms |

---

## Debug Infrastructure (Dev Mode Only)

The debug infrastructure provides real-time protocol analysis for reverse engineering.
It's only available when built with `--features dev` and has zero overhead in production.

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              DebugHub                                        │
│  - Aggregates events from all DebugIoProviders                              │
│  - Ring buffer (10K events) for history                                      │
│  - Change detection (compares successive radar states)                       │
│  - WebSocket broadcast to debug panel                                        │
└────────────────────────────────┬────────────────────────────────────────────┘
                                 │
       ┌─────────────────────────┼─────────────────────────┐
       │                         │                         │
       ▼                         ▼                         ▼
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│DebugIoProvider   │  │DebugIoProvider   │  │ PassiveListener  │
│(wraps IoProvider)│  │(wraps IoProvider)│  │(multicast only)  │
│                  │  │                  │  │                  │
│ Captures:        │  │ Captures:        │  │ Captures:        │
│ - All send/recv  │  │ - All send/recv  │  │ - Multicast      │
│ - Socket ops     │  │ - Socket ops     │  │   broadcasts     │
│ - Decodes msgs   │  │ - Decodes msgs   │  │ - Chart plotter  │
│                  │  │                  │  │   triggered      │
│   Furuno         │  │   Navico         │  │   state changes  │
└──────────────────┘  └──────────────────┘  └──────────────────┘
         │                     │                      │
         └─────────────────────▼──────────────────────┘
                        TokioIoProvider
```

### Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `DebugHub` | `debug/hub.rs` | Central event aggregator and broadcaster |
| `DebugIoProvider<T>` | `debug/io_wrapper.rs` | Wrapper that captures all IoProvider traffic |
| `PassiveListener` | `debug/passive_listener.rs` | Listens to multicast for chart plotter effects |
| `ProtocolDecoder` | `debug/decoders/*.rs` | Brand-specific message decoding |
| `ChangeDetector` | `debug/change_detection.rs` | Correlates commands with state changes |
| `DebugRecorder` | `debug/recording.rs` | Records sessions to `.mdbg` files |

### Integration Point

In `core_locator.rs`, when `cfg!(feature = "dev")`, the IoProvider can be wrapped:

```rust
#[cfg(feature = "dev")]
let io = DebugIoProvider::new(
    TokioIoProvider::new(...),
    debug_hub.clone(),
    radar_id.clone(),
    brand.to_string(),
);

#[cfg(not(feature = "dev"))]
let io = TokioIoProvider::new(...);
```

### Visibility Limitations

| Traffic | Through DebugIoProvider | Through PassiveListener |
|---------|:-----------------------:|:-----------------------:|
| Our commands → radar | ✅ | - |
| Radar responses → us | ✅ | - |
| Chart plotter → radar | ❌ | ❌ |
| Radar multicast status | ✅ | ✅ |

For full traffic capture including chart plotter commands, developers should use
`tcpdump` alongside the Protocol Debugger.

See the [Protocol Debugger User Guide](../user-guide/protocol-debugger.md) for usage.

---

## RadarEngine: Unified Feature Management

The `RadarEngine` in `mayara-core/engine/mod.rs` provides unified management of
radar controllers along with all feature processors (ARPA, GuardZones, Trails,
DualRange). Both server and WASM use the same RadarEngine, eliminating code
duplication for feature management.

### RadarEngine Structure

```rust
// mayara-core/src/engine/mod.rs

/// Wrapper around a controller with all its feature processors
pub struct ManagedRadar {
    pub controller: RadarController,  // Enum: Furuno/Navico/Raymarine/Garmin
    pub arpa: ArpaProcessor,          // Target tracking
    pub guard_zones: GuardZoneProcessor,  // Zone alerting
    pub trails: TrailStore,           // Position history
    pub dual_range: Option<DualRangeController>,  // For supported models
}

/// Central engine managing all radars
pub struct RadarEngine {
    radars: BTreeMap<String, ManagedRadar>,
}

impl RadarEngine {
    // Lifecycle
    pub fn add_radar(&mut self, id: &str, brand: Brand, ...) -> Result<()>
    pub fn remove_radar(&mut self, id: &str)
    pub fn poll<I: IoProvider>(&mut self, io: &mut I) -> Vec<EngineEvent>

    // Controls (unified dispatch)
    pub fn set_control(&mut self, id: &str, control: &str, value: &Value) -> Result<()>
    pub fn get_state(&self, id: &str) -> Option<RadarStateV5>
    pub fn get_capabilities(&self, id: &str) -> Option<CapabilityManifest>

    // ARPA targets
    pub fn get_targets(&self, id: &str) -> Vec<ArpaTarget>
    pub fn acquire_target(&mut self, id: &str, bearing: f64, dist: f64) -> Result<u32>
    pub fn cancel_target(&mut self, id: &str, target_id: u32) -> Result<()>

    // Guard zones
    pub fn get_guard_zones(&self, id: &str) -> Vec<GuardZone>
    pub fn set_guard_zone(&mut self, id: &str, zone: GuardZone) -> Result<()>

    // Trails
    pub fn get_trails(&self, id: &str) -> TrailData
    pub fn clear_trails(&mut self, id: &str)
}
```

### RadarController Enum

The `RadarController` enum wraps brand-specific controllers, providing a unified
interface for the engine:

```rust
pub enum RadarController {
    Furuno(FurunoController),
    Navico(NavicoController),
    Raymarine(RaymarineController),
    Garmin(GarminController),
}
```

### Server Integration

The server uses `Arc<RwLock<RadarEngine>>` as shared state:

```rust
// mayara-server/src/web.rs

pub type SharedEngine = Arc<RwLock<RadarEngine>>;

pub struct Web {
    session: Session,
    engine: SharedEngine,  // Single unified engine
}

// HTTP handlers become thin wrappers:
async fn get_targets(State(state): State<Web>, ...) -> Response {
    let engine = state.engine.read().unwrap();
    Json(engine.get_targets(&radar_id)).into_response()
}
```

### WASM Integration

The WASM plugin embeds RadarEngine directly:

```rust
// mayara-signalk-wasm/src/radar_provider.rs

pub struct RadarProvider {
    io: WasmIoProvider,
    locator: RadarLocator,
    spoke_receiver: SpokeReceiver,
    engine: RadarEngine,  // Same engine as server!
}

// Methods become one-liners:
pub fn get_targets(&self, radar_id: &str) -> Vec<ArpaTarget> {
    self.engine.get_targets(radar_id)
}
```

### Benefits of RadarEngine

| Benefit | Impact |
|---------|--------|
| **Bug fixes in one place** | ARPA/GuardZone/Trail bugs fixed once, works everywhere |
| **Consistent API** | Server and WASM expose identical feature APIs |
| **Reduced duplication** | ~1400 lines removed from server + WASM combined |
| **Easier testing** | Test RadarEngine with mock IoProvider |

---

## Navigation Data Formatting

Navico radars require navigation data (heading, SOG, COG) to be sent as UDP multicast packets for proper HALO/4G operation. The packet formatting functions in `mayara-core/protocol/navico.rs` are pure functions that create byte arrays, enabling both server and WASM to send identical packets.

### Packet Types

| Packet | Function | Multicast Address | Purpose |
|--------|----------|-------------------|---------|
| **Heading** | `format_heading_packet()` | 236.6.7.8:50200 | Ship heading for display orientation |
| **Navigation** | `format_navigation_packet()` | 236.6.7.8:50200 | SOG + COG for trail orientation |
| **Speed** | `format_speed_packet()` | 236.6.7.5:50201 + 236.6.7.6:50201 | Speed/course for target motion |

### Packet Parsing

The same `navico.rs` file also provides packet parsing via `transmute()` methods on the packed structs:

```rust
// mayara-core/src/protocol/navico.rs

// Parsing received packets (in server's report.rs):
impl HaloHeadingPacket {
    pub fn transmute(bytes: &[u8]) -> Result<Self, &'static str>
    pub fn heading_degrees(&self) -> f64  // Convenience accessor
}

impl HaloNavigationPacket {
    pub fn transmute(bytes: &[u8]) -> Result<Self, &'static str>
    pub fn sog_knots(&self) -> f64
    pub fn cog_degrees(&self) -> f64
}

// Formatting packets to send (in server's info.rs):
pub fn format_heading_packet(heading_deg: f64, counter: u16, timestamp_ms: i64) -> [u8; 72]
pub fn format_navigation_packet(sog_ms: f64, cog_deg: f64, counter: u16, timestamp_ms: i64) -> [u8; 72]
pub fn format_speed_packet(sog_ms: f64, cog_deg: f64) -> [u8; 23]
```

### Address Constants

All multicast addresses are defined once in mayara-core:

```rust
// mayara-core/src/protocol/navico.rs
pub const INFO_ADDR: &str = "236.6.7.8";
pub const INFO_PORT: u16 = 50200;
pub const SPEED_ADDR_A: &str = "236.6.7.5";
pub const SPEED_ADDR_B: &str = "236.6.7.6";
pub const SPEED_PORT_A: u16 = 50201;
pub const SPEED_PORT_B: u16 = 50201;
```

**Key insight:** The server's `navico/info.rs` and `navico/report.rs` import these constants from core, eliminating duplicate address definitions.

---

## Batch Control Initialization

The capabilities module provides batch functions to generate all controls for a brand or model, enabling server's `control_factory.rs` to initialize controls without hardcoding lists:

### Core Functions (mayara-core/capabilities/controls.rs)

```rust
/// Get base controls that exist on all radars of a brand
pub fn get_base_controls_for_brand(brand: Brand) -> Vec<ControlDefinition> {
    // Returns: power, gain, sea, rain, etc.
}

/// Get all controls for a specific model (base + extended)
pub fn get_all_controls_for_model(brand: Brand, model_name: Option<&str>) -> Vec<ControlDefinition> {
    // Uses models::get_model() to look up model's control list
    // Returns base controls + model-specific extended controls
}
```

### Server Builders (mayara-server/control_factory.rs)

```rust
/// Convert core ControlDefinitions to server's Control objects
pub fn build_base_controls_for_brand(brand: Brand) -> HashMap<String, Control> {
    let core_defs = controls::get_base_controls_for_brand(brand);
    core_defs.into_iter()
        .map(|def| (def.id.clone(), build_control(&def)))
        .collect()
}

/// Build all controls for a model
pub fn build_all_controls_for_model(brand: Brand, model_name: Option<&str>) -> HashMap<String, Control>

/// Build only extended controls for a model (when model detected after startup)
pub fn build_extended_controls_for_model(brand: Brand, model_name: &str) -> HashMap<String, Control>
```

### Initialization Flow

```
1. Radar discovered (unknown model)
   └── settings.rs calls build_base_controls_for_brand(Brand::Navico)
       └── Core returns base controls: power, gain, sea, rain, range, etc.

2. Model identified via report packet (e.g., "HALO24")
   └── settings.rs calls build_extended_controls_for_model(Brand::Navico, "HALO24")
       └── Core looks up HALO24 in models/navico.rs
       └── Returns: dopplerMode, dopplerSpeed, accentLight, seaState, etc.

3. Controls merged into radar state
   └── API /capabilities reflects all available controls
```

**Key insight:** The model database in `mayara-core/models/` is the single source of truth for which controls exist on each radar model. Adding a control to a model's list automatically makes it available through the API.

---

## Adding a New Feature: The Workflow

### Example: Adding a New Control (e.g., "pulseWidth")

**Step 1: Add control definition (mayara-core)**
```rust
// mayara-core/src/capabilities/controls.rs
pub fn control_pulse_width() -> ControlDefinition {
    ControlDefinition {
        id: "pulseWidth",
        name: "Pulse Width",
        control_type: ControlType::Number,
        min: Some(0.0),
        max: Some(3.0),
        ...
    }
}
```

**Step 2: Add to model capabilities (mayara-core)**
```rust
// mayara-core/src/models/furuno.rs
static CONTROLS_NXT: &[&str] = &[
    "beamSharpening", "dopplerMode", ...,
    "pulseWidth",  // ← Add here
];
```

**Step 3: Add dispatch entry (mayara-core)**
```rust
// mayara-core/src/protocol/furuno/dispatch.rs
pub fn format_control_command(control_id: &str, value: i32, auto: bool) -> Option<String> {
    match control_id {
        ...
        "pulseWidth" => Some(format_pulse_width_command(value)),  // ← Add here
        _ => None,
    }
}
```

**Step 4: Done!**
- Server automatically uses new dispatch entry
- WASM automatically uses new dispatch entry
- GUI automatically shows control (reads from /capabilities)
- No server code changes needed!

---

## Architecture Diagram: Full Picture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              mayara-core                                     │
│                    (Pure Rust, no I/O, WASM-compatible)                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐   │
│  │  protocol/    │ │   models/     │ │ capabilities/ │ │   state.rs    │   │
│  │  - furuno/    │ │ - furuno.rs   │ │ - controls.rs │ │   RadarState  │   │
│  │    - dispatch │ │ - navico.rs   │ │   get_base_*  │ │   PowerState  │   │
│  │    - command  │ │ - raymarine   │ │   get_all_*   │ │               │   │
│  │    - report   │ │ - garmin.rs   │ │ - builder.rs  │ │               │   │
│  │  - navico.rs  │ │               │ │               │ │               │   │
│  │    (parse +   │ │               │ │               │ │               │   │
│  │     format)   │ │               │ │               │ │               │   │
│  │  - raymarine  │ │               │ │               │ │               │   │
│  │  - garmin.rs  │ │               │ │               │ │               │   │
│  └───────────────┘ └───────────────┘ └───────────────┘ └───────────────┘   │
│                                                                              │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐   │
│  │  io.rs        │ │ locator.rs    │ │ connection.rs │ │  arpa/        │   │
│  │  IoProvider   │ │ RadarLocator  │ │ ConnManager   │ │  trails/      │   │
│  │  trait        │ │ (discovery)   │ │ ConnState     │ │  guard_zones/ │   │
│  └───────────────┘ └───────────────┘ └───────────────┘ └───────────────┘   │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    controllers/  (★ UNIFIED ★)                       │   │
│  │   FurunoController │ NavicoController │ RaymarineController │ Garmin │   │
│  │   (TCP login)      │ (UDP multicast)  │ (Quantum/RD)        │ (UDP)  │   │
│  │                                                                      │   │
│  │   ALL controllers use IoProvider - SAME code on server AND WASM!    │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    │                               │
                    ▼                               ▼
     ┌────────────────────────────┐    ┌────────────────────────────┐
     │   mayara-signalk-wasm      │    │       mayara-server        │
     │      (WASM + FFI)          │    │    (Native + tokio)        │
     ├────────────────────────────┤    ├────────────────────────────┤
     │                            │    │                            │
     │  wasm_io.rs:               │    │  tokio_io.rs:              │
     │  - WasmIoProvider          │    │  - TokioIoProvider         │
     │  - impl IoProvider         │    │  - impl IoProvider         │
     │                            │    │                            │
     │  locator.rs:               │    │  core_locator.rs:          │
     │  - Re-exports RadarLocator │    │  - CoreLocatorAdapter      │
     │    from mayara-core        │    │  - Wraps RadarLocator      │
     │                            │    │                            │
     │  radar_provider.rs:        │    │  brand/:                   │
     │  - Uses controllers from   │    │  - Can use core controllers│
     │    mayara-core directly!   │    │    with TokioIoProvider    │
     │  - FurunoController        │    │  - OR async wrappers       │
     │  - NavicoController        │    │                            │
     │  - RaymarineController     │    │  web.rs:                   │
     │  - GarminController        │    │  - Axum handlers           │
     │                            │    │                            │
     │  signalk_ffi.rs:           │    │  storage.rs:               │
     │  - FFI bindings            │    │  - Local applicationData   │
     └────────────────────────────┘    └────────────────────────────┘
                    │                               │
                    ▼                               ▼
     ┌────────────────────────────┐    ┌────────────────────────────┐
     │     SignalK Server         │    │     Axum HTTP Server       │
     │                            │    │                            │
     │  Routes /radars/* to       │    │  /radars/*  (same API!)    │
     │  WASM RadarProvider        │    │  Static files (same GUI!)  │
     └────────────────────────────┘    └────────────────────────────┘
                    │                               │
                    └───────────────┬───────────────┘
                                    │
                                    ▼
                     ┌────────────────────────────┐
                     │         mayara-gui/        │
                     │     (shared web assets)    │
                     │                            │
                     │  Works in ANY mode!        │
                     │  api.js auto-detects       │
                     └────────────────────────────┘
```

---

## Benefits of This Architecture

| Benefit | Description |
|---------|-------------|
| **Single source of truth** | All radar logic in mayara-core |
| **Fixes apply everywhere** | Bug fixed in core → fixed in WASM and Server |
| **No code duplication** | Same RadarLocator, same controllers, same dispatch |
| **All 4 brands everywhere** | Furuno, Navico, Raymarine, Garmin work on WASM AND Server |
| **Easy to add features** | Add to core, both platforms get it automatically |
| **Testable** | Core is pure Rust, mock IoProvider for unit tests |
| **WASM-compatible** | Core has zero tokio dependencies |
| **Same GUI** | Works unchanged with SignalK or Standalone |
| **Same API** | Clients don't know which backend they're talking to |

---

## Architecture Evolution

The architecture evolved through several phases to achieve maximum code reuse:

### Phase 1: Server-Only (Historical)
- Each brand had its own locator, command, report, and data modules
- No sharing between brands or platforms
- Code duplication between brands (~2000+ lines per brand)

### Phase 2: Protocol Extraction
- Wire protocol parsing moved to mayara-core
- Model database (ranges, capabilities) centralized
- Control definitions unified across brands
- Server still had brand-specific controllers

### Phase 3: IoProvider Abstraction
- `IoProvider` trait created for platform-independent I/O
- `RadarLocator` moved to core (discovery logic shared)
- `TokioIoProvider` for server, `WasmIoProvider` for WASM
- Both platforms use identical discovery code

### Phase 4: Unified Controllers
- Brand controllers moved to mayara-core:
  - `FurunoController` - TCP login + command protocol
  - `NavicoController` - UDP multicast commands
  - `RaymarineController` - Quantum/RD variant handling
  - `GarminController` - UDP commands
- Server's brand modules become thin dispatchers
- WASM and server share identical control logic

### Phase 5: RadarEngine + WASM Migration (Current - December 2025)
- `RadarEngine` created in mayara-core to unify feature processors
- Server migrated from separate state types to single `SharedEngine`
- WASM plugin overhauled: discarded buggy logic, now uses RadarEngine
- Spoke reduction implemented for WASM (512 spokes vs server's 8192)
- Capabilities API updated to report actual spoke output count

### Remaining Work
- Garmin server integration (core controller exists, server still uses legacy)
- SignalK provider mode (standalone → SignalK registration)
- OpenCPN plugin (HTTP/WebSocket client)

---

## Data Flow Diagrams

### Control Command Flow

When a user changes a control (e.g., sets gain to 50):

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Control Flow                                    │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  WebGUI                                                                      │
│    │ PUT /radars/{id}/controls/gain {value: 50, auto: false}                │
│    ▼                                                                         │
│  Axum Handler (web.rs)                                                       │
│    │ Sends ControlValue to brand module via channel                         │
│    ▼                                                                         │
│  Brand Report Receiver (e.g., raymarine/report.rs)                          │
│    │ Receives ControlValue from channel                                     │
│    │ Calls send_control_to_radar(&cv)                                       │
│    ▼                                                                         │
│  Core Controller (controllers/raymarine.rs)                                  │
│    │ controller.set_gain(&mut io, 50, false)                                │
│    │ Builds command bytes for Quantum or RD variant                         │
│    ▼                                                                         │
│  TokioIoProvider                                                             │
│    │ io.udp_send_to(&socket, command_bytes, &radar_addr, port)              │
│    ▼                                                                         │
│  UDP Socket → Radar Hardware                                                 │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Spoke Data Flow

When radar sends spoke data:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Spoke Data Flow                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Radar Hardware                                                              │
│    │ UDP multicast spoke packets                                            │
│    ▼                                                                         │
│  Brand Data Receiver (e.g., raymarine/data.rs)                              │
│    │ Async tokio::net::UdpSocket.recv()                                     │
│    │ Parses frame header, decompresses spoke data                           │
│    │ Uses mayara-core protocol parsing                                       │
│    ▼                                                                         │
│  Spoke Processing                                                            │
│    │ Apply trails (mayara-core/trails/)                                     │
│    │ Convert to protobuf spoke format                                        │
│    ▼                                                                         │
│  RadarInfo.broadcast_radar_message()                                         │
│    │ Sends to all connected WebSocket clients                               │
│    ▼                                                                         │
│  WebSocket Stream                                                            │
│    │ Binary protobuf message                                                │
│    ▼                                                                         │
│  WebGUI (viewer.js)                                                          │
│    │ Decodes protobuf, renders on canvas                                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Spoke Resolution: Server vs WASM

The server and WASM handle different spoke resolutions due to transport constraints:

| Platform | Spokes/Revolution | Reason |
|----------|------------------|--------|
| **mayara-server** | 8192 (native) | Direct WebSocket to browser can sustain high data rate |
| **mayara-signalk-wasm** | 512 (reduced) | SignalK WebSocket has rate limiting (code 1008) |

**WASM Spoke Reduction Logic** (`spoke_receiver.rs`):
1. Furuno sends 8192 spokes per revolution
2. WASM accumulates 16 consecutive spokes
3. Combines using `max()` per pixel (preserves radar targets)
4. Emits 1 combined spoke with angle `original_angle / 16`
5. Results in 512 spokes/revolution (8192 / 16)

**Critical:** The `spokes_per_revolution` in capabilities must match the actual output.
The GUI uses this value to map spoke angles to 360 degrees:
- Server: `spokes_per_revolution: 8192`, angles 0-8191
- WASM: `spokes_per_revolution: 512`, angles 0-511

The WASM uses `build_capabilities_from_model_with_spokes()` to override the
model's native spoke count with the reduced output count.

### Discovery Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Discovery Flow                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  RadarLocator (mayara-core/locator.rs)                                       │
│    │ Poll-based, runs in CoreLocatorAdapter (server) or directly (WASM)     │
│    ▼                                                                         │
│  Brand-specific beacon detection                                             │
│    │ Furuno: broadcast request → unicast response                           │
│    │ Navico: multicast join → beacon packets                                │
│    │ Raymarine: multicast join → info packets                               │
│    │ Garmin: multicast join → beacon packets                                │
│    ▼                                                                         │
│  RadarDiscovery struct created                                               │
│    │ Contains: brand, model, address, capabilities                          │
│    ▼                                                                         │
│  Server: Spawns brand-specific receiver task                                 │
│    │ Creates FurunoReportReceiver / NavicoReportReceiver / etc.             │
│    │ Receiver creates Core Controller when model confirmed                  │
│    ▼                                                                         │
│  Radar registered in Radars collection                                       │
│    │ Available via REST API /radars                                         │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Persistent Installation Settings

Some radar controls are **write-only** - they can be sent to the radar but cannot be reliably
read back. Examples include Furuno's `autoAcquire` (ARPA), `bearingAlignment`, and `antennaHeight`.

These Installation category controls are persisted using the **Signal K Application Data API**,
which is implemented in both mayara-server (`storage.rs`) and Signal K itself. This ensures:
1. GUI code works identically in standalone and Signal K modes
2. Settings survive server restarts
3. Settings are restored to radar on reconnect

### Storage Location (aligned with WASM SignalK plugin)
- API: `/signalk/v1/applicationData/global/@mayara/signalk-radar/1.0.0`
- Files: `~/.local/share/mayara/applicationData/@mayara/signalk-radar/1.0.0.json`

### Storage Schema

The `radars` object is keyed by **unique radar identifier** (`{Brand}-{SerialNumber}`),
allowing multiple radars from different brands to be stored in the same file:

```json
{
  "radars": {
    "Furuno-RD003212": {
      "bearingAlignment": -5,
      "antennaHeight": 15,
      "autoAcquire": true
    },
    "Raymarine-Q24C-ABC123": {
      "bearingAlignment": 3,
      "antennaHeight": 8
    },
    "Navico-HALO-XYZ789": {
      "bearingAlignment": 0,
      "antennaHeight": 12
    }
  }
}
```

The unique key is obtained from `capabilities.key` in the REST API, which corresponds
to the radar's internal key (e.g., `Furuno-{serial}` or `Navico-{serial}`).

### Persistence Flow

```
User sets bearingAlignment to -5° in GUI
  │
  │  GUI gets capabilities.key = "Furuno-RD003212" (unique identifier)
  │
  ├─► GUI: PUT /radars/radar-2/controls/bearingAlignment {value: -5}
  │         Server sends $S81,-50,0 to radar (tenths of degrees)
  │
  └─► GUI: PUT /signalk/v1/applicationData/global/@mayara/signalk-radar/1.0.0
           Body: {"radars":{"Furuno-RD003212":{"bearingAlignment":-5,...}}}
           (uses capabilities.key, not the REST API id)

On server restart / radar reconnect:
  │
  └─► Server loads 1.0.0.json, looks up settings for radar's key
      Server sends: $S81,-50,0  $S84,0,15,0  $S87,1
      REST API /state reflects restored values
```

### Write-Only Control Pattern

Controls with `wire_hints.write_only = true` in mayara-core indicate that:
- The control can be SET but not reliably READ from hardware
- GUI should persist the value via Application Data API
- Server should restore values on controller connect

### Implementation Files
- `mayara-gui/api.js` - `saveInstallationSetting()` and `getInstallationSettings()`
- `mayara-gui/control.js` - Persists Installation category controls after successful change
- `mayara-server/src/storage.rs` - `load_installation_settings()` for server-side loading
- `mayara-server/src/brand/furuno/report.rs` - `restore_installation_settings()` on model detection
- `mayara-core/src/capabilities/controls.rs` - `write_only: true` in wire_hints

---

## External Clients: The Shared API Architecture

The mayara-server REST API (`/v2/api/radars/*`) is the **shared interface** that enables
multiple client applications to connect to the same radar infrastructure. All radar logic
(protocol handling, ARPA tracking, signal processing) runs on mayara-server - clients are
thin display and control layers.

```
                                    ┌─────────────────────┐
                                    │  mayara-server      │
                                    │  (localhost:6502)   │
                                    │                     │
                                    │  /v2/api/radars/*   │
                                    │  (REST + WebSocket) │
                                    └─────────┬───────────┘
                                              │
                                              │  HTTP + WebSocket
                    ┌─────────────────────────┼─────────────────────────┐
                    │                         │                         │
                    ▼                         ▼                         ▼
     ┌──────────────────────┐   ┌──────────────────────┐   ┌──────────────────────┐
     │   mayara-gui         │   │   mayara-server-     │   │   mayara_opencpn     │
     │   (Web Browser)      │   │   signalk-plugin     │   │   (Future)           │
     │                      │   │   (SignalK/Node.js)  │   │   (C++)              │
     │   - Direct access    │   │                      │   │                      │
     │   - WebGPU rendering │   │   - Exposes radars   │   │   - OpenGL rendering │
     │   - VanJS UI         │   │     via SignalK API  │   │   - Chart overlay    │
     └──────────────────────┘   └──────────────────────┘   └──────────────────────┘
                                              │
                                              ▼
                                ┌──────────────────────────────┐
                                │  SignalK Server              │
                                │  /signalk/v2/api/.../radars  │
                                │                              │
                                │  - Security (JWT)            │
                                │  - Multi-provider support    │
                                │  - Built-in binary streaming │
                                └──────────────────────────────┘
```

### Deployment Mode: mayara-server-signalk-plugin

The **mayara-server-signalk-plugin** is a native SignalK (JavaScript) plugin that:
1. Connects to mayara-server's REST API
2. Registers as a RadarProvider with SignalK's Radar API
3. Forwards spoke data via SignalK's `binaryStreamManager`

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            SignalK Server                                    │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                   mayara-server-signalk-plugin                          │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐ │ │
│  │  │  MayaraClient   │  │  RadarProvider  │  │    SpokeForwarder       │ │ │
│  │  │  (HTTP client)  │  │  (API methods)  │  │  (WS → emitData)        │ │ │
│  │  └────────┬────────┘  └────────┬────────┘  └────────────┬────────────┘ │ │
│  │           │                    │                        │              │ │
│  └───────────┼────────────────────┼────────────────────────┼──────────────┘ │
│              │   radarApi.register()      binaryStreamManager.emitData()    │
│              │                    │                        │                │
│  ┌───────────┼────────────────────┼────────────────────────┼──────────────┐ │
│  │           │        SignalK Radar API v2                 │              │ │
│  │           │   /signalk/v2/api/vessels/self/radars/*     │              │ │
│  │           │   Security: JWT via authorizeWS()           │              │ │
│  └───────────┼────────────────────────────────────────────────────────────┘ │
└──────────────┼──────────────────────────────────────────────────────────────┘
               │ HTTP + WebSocket
               ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                            mayara-server                                     │
│              /v2/api/radars/*            /v2/api/radars/*/spokes             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key Features:**
- Pure JavaScript (no native dependencies beyond `ws`)
- Implements `RadarProviderMethods` interface from SignalK
- Uses SignalK's built-in `binaryStreamManager` for spoke streaming (no custom proxy)
- Auto-discovery of radars connected to mayara-server
- Auto-reconnection on network failures
- Embeds mayara-gui for web display

**Plugin Location:** `mayara-server-signalk-plugin/` (separate repository)

**Why NOT embed mayara-core in the plugin?**
- SignalK's WASM plugin already provides embedded radar support via mayara-signalk-wasm
- mayara-server-signalk-plugin is for deployments where mayara-server runs separately
- Separation allows mayara-server to run on different hardware (e.g., dedicated radar PC)
- Single mayara-server can serve multiple clients (SignalK, direct browser, future OpenCPN)

### Deployment Mode: Future OpenCPN Integration (mayara_opencpn)

A future OpenCPN plugin will connect to mayara-server using the same API. OpenCPN
includes the IXWebSocket library, providing full HTTP and WebSocket support in C++.

```
┌─────────────────────────────────────────────────────────────────┐
│                     mayara_opencpn (OpenCPN Plugin)             │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    MayaraRadarPanel                       │   │
│  │  - PPI rendering (OpenGL/GLES with shaders)               │   │
│  │  - Guard zones, ARPA targets, trails display              │   │
│  │  - Chart overlay mode                                     │   │
│  │  - All data from mayara-server API                        │   │
│  └──────────────────────────────────────────────────────────┘   │
│                              │                                   │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    MayaraClient (C++)                     │   │
│  │  - Uses IXWebSocket (built into OpenCPN)                  │   │
│  │  - HTTP: GET /radars, GET /capabilities, PUT /controls    │   │
│  │  - WebSocket: /radars/{id}/spokes (protobuf stream)       │   │
│  │  - WebSocket: /radars/{id}/targets (ARPA stream)          │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                               │
                               │  HTTP + WebSocket (same API!)
                               ▼
                    ┌─────────────────────┐
                    │  mayara-server      │
                    │  (localhost:6502)   │
                    └─────────────────────┘
                               │
                               ▼
                        Radar Hardware
                    (Furuno, Navico, etc.)
```

**Why this architecture works well:**
- ARPA logic already in mayara-core (no reimplementation needed)
- OpenCPN plugin is just a thin rendering client
- Same API used by mayara-gui and SignalK plugin
- IXWebSocket provides cross-platform HTTP/WebSocket in OpenCPN

### Client Comparison

| Client | Language | Use Case | Radar Logic |
|--------|----------|----------|-------------|
| **mayara-gui** | JavaScript | Direct browser access | mayara-server |
| **mayara-signalk-wasm** | Rust/WASM | Embedded in SignalK | mayara-core (in WASM) |
| **mayara-server-signalk-plugin** | JavaScript | SignalK + remote mayara-server | mayara-server |
| **mayara_opencpn** (future) | C++ | OpenCPN chart plotter | mayara-server |

---

## Recording and Playback System

The recording and playback system enables capturing radar data to `.mrr` files and replaying
them later. This provides two key capabilities:

1. **Developer testing** - SignalK Radar API consumers can test `render()` functions with
   consistent recorded data without live radar hardware
2. **Demos/exhibitions** - Playback works standalone without radar connection

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              RECORDING PATH                                  │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                         mayara-server (Rust)                          │   │
│  │  ┌─────────────┐    ┌─────────────┐    ┌──────────────────────────┐  │   │
│  │  │Radar Drivers│───►│  Recorder   │───►│  ~/.../recordings/*.mrr  │  │   │
│  │  │(Furuno,etc) │    │             │    └──────────────────────────┘  │   │
│  │  └─────────────┘    └─────────────┘                                  │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                           PLAYBACK PATHS (2 options)                         │
│                                                                              │
│  Option A: Standalone (mayara-server only)                                  │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │  mayara-server ─► Player ─► Virtual Radar ─► mayara-gui              │   │
│  │                                                                       │   │
│  │  Good for: demos, exhibitions, testing without SignalK               │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
│  Option B: SignalK (for radar API consumers)                                │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │  .mrr file ─► SignalK Plugin ─► radarApi.register() ─► SignalK       │   │
│  │                    │                                        │         │   │
│  │                    │            binaryStreamManager         │         │   │
│  │                    └───────────────────────────────────────►│         │   │
│  │                                                             ▼         │   │
│  │                                           ┌─────────────────────────┐│   │
│  │                                           │  Any Radar Consumer:   ││   │
│  │                                           │  - mayara-gui          ││   │
│  │                                           │  - OpenCPN (future)    ││   │
│  │                                           │  - SignalK dev testing ││   │
│  │                                           └─────────────────────────┘│   │
│  │                                                                       │   │
│  │  Good for: SignalK developers testing render(), chart plotter devs  │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### .mrr File Format (MaYaRa Radar Recording)

Binary format optimized for efficient seeking and playback:

```
┌──────────────────────────┐
│ Header (256 bytes)       │  magic "MRR1", version, radar metadata
├──────────────────────────┤
│ Capabilities (JSON)      │  length-prefixed JSON (v5 capabilities)
├──────────────────────────┤
│ Initial State (JSON)     │  length-prefixed JSON (controls state)
├──────────────────────────┤
│ Frame 0                  │  timestamp + protobuf RadarMessage + state delta
│ Frame 1                  │
│ ...                      │
├──────────────────────────┤
│ Index (for seeking)      │  array of (timestamp, file_offset)
├──────────────────────────┤
│ Footer (32 bytes)        │  index offset, frame count, duration
└──────────────────────────┘
```

**File sizes:** ~15-30 MB/minute, ~1-2 GB/hour

**Compression strategy:**
- Storage: Uncompressed `.mrr` for fast seeking/playback
- Download: Gzip-compressed `.mrr.gz` for transfer (~95% size reduction)
- Upload: SignalK plugin accepts `.mrr.gz`, auto-decompresses

### REST API Endpoints (mayara-server)

All at `/v2/api/recordings/`:

**Recording Control:**
```
GET  /v2/api/recordings/radars          # List available radars to record
POST /v2/api/recordings/record/start    # {radarId, filename?}
POST /v2/api/recordings/record/stop
GET  /v2/api/recordings/record/status
```

**Playback Control:**
```
POST /v2/api/recordings/playback/load   # {filename}
POST /v2/api/recordings/playback/play
POST /v2/api/recordings/playback/pause
POST /v2/api/recordings/playback/stop
POST /v2/api/recordings/playback/seek   # {timestamp_ms}
PUT  /v2/api/recordings/playback/settings  # {loop?, speed?}
GET  /v2/api/recordings/playback/status
```

**File Management:**
```
GET    /v2/api/recordings/files              # ?dir=subdir
GET    /v2/api/recordings/files/:filename
DELETE /v2/api/recordings/files/:filename
PUT    /v2/api/recordings/files/:filename    # {newName?, directory?}
POST   /v2/api/recordings/files/upload       # Accepts .mrr or .mrr.gz
GET    /v2/api/recordings/files/:filename/download  # Returns .mrr.gz
```

### Virtual Radar Registration

During playback, the player registers as a "virtual radar" that appears in the radar list.
Playback radars are identified by their ID prefix `playback-*`:

```rust
// Playback radar is distinguished from real radars
let radar_id = format!("playback-{}", base_name);

// Capabilities include isPlayback flag
let capabilities = Capabilities {
    id: radar_id,
    name: format!("Playback: {}", base_name),
    brand: "Playback",
    model: "Recording",
    isPlayback: true,  // GUI uses this to disable controls
    ...metadata_from_mrr_file
};
```

### GUI Playback Mode

The mayara-gui detects playback radars and adjusts its behavior:

```javascript
// api.js
export function isPlaybackRadar(radarId) {
  return radarId && radarId.startsWith('playback-');
}

// control.js - Disable controls for playback
if (isPlaybackRadar(radarId)) {
  container.querySelectorAll('input, select, button').forEach(el => {
    el.disabled = true;
  });
  header.appendChild(span({class: 'playback-badge'}, 'PLAYBACK'));
}
```

### SignalK Playback Plugin

The `mayara-server-signalk-playbackrecordings-plugin` is a **self-contained** developer tool
that reads `.mrr` files directly (no mayara-server required). It:

1. Parses `.mrr` files using JavaScript port of `file_format.rs`
2. Registers as RadarProvider via SignalK Radar API
3. Emits frames through `binaryStreamManager` at correct timing
4. Provides simple playback UI (upload, play/pause/stop, loop)
5. Links to mayara-gui's `viewer.html` for radar display

**Why separate plugin:**
- Keeps main `mayara-server-signalk-plugin` simple for normal users
- Self-contained for developers (single plugin install)
- No coordination between plugins needed

### Implementation Files

| Component | Location | Purpose |
|-----------|----------|---------|
| **file_format.rs** | mayara-server/recording/ | .mrr binary format read/write |
| **recorder.rs** | mayara-server/recording/ | Subscribe to radar, write frames |
| **player.rs** | mayara-server/recording/ | Read frames, emit as virtual radar |
| **manager.rs** | mayara-server/recording/ | File listing, metadata, CRUD |
| **recordings.html/js** | mayara-gui/ | Recording/playback UI |
| **mrr-reader.js** | signalk-playback-plugin/ | JS port of file_format.rs |
| **playback.html** | signalk-playback-plugin/ | Minimal playback control UI |

---

## Testing Strategy

The unified architecture enables effective testing at multiple levels:

### Unit Tests (mayara-core)

Core logic can be tested without real hardware using mock IoProvider:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockIoProvider {
        sent_data: Vec<(String, u16, Vec<u8>)>,
    }

    impl IoProvider for MockIoProvider {
        fn udp_send_to(&mut self, _socket: &UdpSocketHandle, data: &[u8],
                       addr: &str, port: u16) -> Result<usize, IoError> {
            self.sent_data.push((addr.to_string(), port, data.to_vec()));
            Ok(data.len())
        }
        // ... other methods
    }

    #[test]
    fn test_gain_command_quantum() {
        let mut io = MockIoProvider { sent_data: vec![] };
        let mut controller = RaymarineController::new(
            "test", "192.168.1.100", 50100, "239.0.0.1", 50100,
            RaymarineVariant::Quantum, false
        );

        controller.set_gain(&mut io, 50, false);

        assert_eq!(io.sent_data.len(), 1);
        let (addr, port, data) = &io.sent_data[0];
        assert_eq!(addr, "192.168.1.100");
        // Verify Quantum command format
        assert_eq!(data[2], 0x28);  // Quantum magic byte
    }
}
```

### Integration Tests (mayara-server)

Test REST API endpoints with mock radar:

```rust
#[tokio::test]
async fn test_radar_capabilities_endpoint() {
    // Start server with test radar registered
    let app = create_test_app();

    let response = app
        .oneshot(Request::get("/v2/api/radars/test-radar/capabilities").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body_bytes(response).await)?;
    assert!(body["controls"].is_array());
}
```

### Replay Testing

Recorded radar data can be replayed to test parsing and processing:

```bash
# Record live radar traffic
tcpdump -i eth0 -w capture.pcap port 50100 or port 50102

# Replay in test mode
mayara-server --replay capture.pcap
```

The `receiver.replay` flag prevents controller creation during replay,
allowing spoke processing to be tested independently.

---

## Known Issues and Workarounds

### mDNS SignalK Discovery Floods Network (December 2025)

**Problem:** When no `--navigation-address` is specified, mayara defaulted to mDNS
discovery for SignalK servers. The `mdns-sd` library sends continuous query packets
on all network interfaces, flooding the network with `_signalk-tcp._tcp.local.` queries.
This caused severe network congestion (ping timeouts, high CPU) especially in
multi-NIC setups where radar and LAN share layer 2.

**Workaround:** mDNS discovery is now disabled by default. The `ConnectionType::Disabled`
variant prevents the mDNS daemon from starting when `--navigation-address` is not specified.

**To enable SignalK integration:** Use one of these options:
- `--navigation-address eth0` - mDNS on specific interface
- `--navigation-address tcp:192.168.1.100:3000` - Direct TCP connection
- `--navigation-address udp:192.168.1.100:10110` - UDP NMEA listener

**Future fix:** The mdns-sd library needs rate limiting or the browse loop needs
throttling. For now, explicit configuration is required for SignalK integration.

---

## OpenCPN Plugin Integration

### Overview

The `mayara-server-opencpn-plugin` is a C++ plugin for OpenCPN that connects
to mayara-server via the same REST/WebSocket API used by the SignalK plugin.
This provides radar display capabilities within OpenCPN without requiring
the radar_pi plugin's direct protocol implementations.

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         OpenCPN                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              mayara-server-opencpn-plugin               │    │
│  │  ┌──────────────┐  ┌─────────────┐  ┌───────────────┐  │    │
│  │  │ MayaraClient │  │SpokeReceiver│  │ RadarRenderer │  │    │
│  │  │   (REST)     │  │    (WS)     │  │   (OpenGL)    │  │    │
│  │  └──────┬───────┘  └──────┬──────┘  └───────────────┘  │    │
│  └─────────┼─────────────────┼────────────────────────────┘    │
│            │                 │                                   │
└────────────┼─────────────────┼───────────────────────────────────┘
             │ HTTP            │ WebSocket
             │                 │ (protobuf)
┌────────────▼─────────────────▼───────────────────────────────────┐
│                      mayara-server                                │
│                     localhost:6502                                │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │                      RadarEngine                             │ │
│  │  ┌─────────┐ ┌─────────┐ ┌──────────┐ ┌─────────┐          │ │
│  │  │ Furuno  │ │ Navico  │ │Raymarine │ │ Garmin  │          │ │
│  │  └────┬────┘ └────┬────┘ └────┬─────┘ └────┬────┘          │ │
│  └───────┼───────────┼───────────┼────────────┼────────────────┘ │
└──────────┼───────────┼───────────┼────────────┼──────────────────┘
           │           │           │            │
      ┌────▼───┐  ┌────▼───┐  ┌────▼────┐  ┌────▼───┐
      │ DRS4D  │  │ HALO   │  │ Quantum │  │  xHD   │
      └────────┘  └────────┘  └─────────┘  └────────┘
```

### API Usage

The plugin uses the same endpoints as the SignalK plugin:

| Method | Endpoint | Purpose |
|--------|----------|---------|
| GET | `/v2/api/radars` | Discover radars |
| GET | `/v2/api/radars/{id}/capabilities` | Get radar specs |
| GET | `/v2/api/radars/{id}/state` | Get current settings |
| PUT | `/v2/api/radars/{id}/controls/{ctrl}` | Set control value |
| WS | `/v2/api/radars/{id}/spokes` | Binary spoke stream |
| GET | `/v2/api/radars/{id}/targets` | Get ARPA targets |

### Display Modes

1. **Chart Overlay**: Renders radar on OpenCPN's chart canvas using
   `RenderGLOverlayMultiCanvas()` callback with OpenGL shaders

2. **PPI Window**: Separate `wxGLCanvas` window with traditional
   radar PPI display, range rings, and heading marker

### Benefits over radar_pi

| Aspect | radar_pi | mayara-server plugin |
|--------|----------|---------------------|
| Protocol handling | In plugin | In server |
| Multi-client | No | Yes (multiple UIs) |
| Platform code | Per radar brand | Single API client |
| Updates | Plugin rebuild | Server update only |
| Remote radar | No | Yes (server can run elsewhere) |

### Source Repository

- Plugin: https://github.com/MarineYachtRadar/mayara-server-opencpn-plugin
- Documentation: Included in plugin as AsciiDoc manual

---

## Related Documents

- [Forked Dependencies](forked-dependencies.md) - Why we use forked versions of nmea-parser and tungstenite
