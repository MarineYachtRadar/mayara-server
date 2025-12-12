# Mayara Architecture

> This document describes the architecture of the Mayara radar system,
> showing what is shared between deployment modes and the path to maximum code reuse.

## Design Principle: Unified SignalK-Compatible API

**Key Insight:** The SignalK WASM plugin has a fully tested, working implementation of the
SignalK Radar API v5 with Furuno. Instead of maintaining two different APIs, **Standalone
implements the same SignalK-compatible API** (without requiring SignalK itself) so that:

1. **Same GUI** works unchanged in WASM and Standalone modes
2. **Same locator and controller code** can be shared (only I/O layer differs)
3. **Standalone can optionally register as a SignalK provider** later

### The API Contract

Standalone implements a SignalK-compatible API surface. The GUI code doesn't know or care
whether it's talking to SignalK or Standalone - the endpoints behave identically.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                  SignalK-Compatible API (implemented by both)                │
│                                                                              │
│  a) RADAR API                                                                │
│  ───────────────────────────────────────────────────────────────────────────│
│  GET  /radars                         - List discovered radars              │
│  GET  /radars/{id}                    - Get radar info                      │
│  GET  /radars/{id}/capabilities       - Get capabilities manifest           │
│  GET  /radars/{id}/state              - Get current state                   │
│  PUT  /radars/{id}/state              - Update state (controls)             │
│  WS   /radars/{id}/spokes             - WebSocket spoke stream              │
│                                                                              │
│  b) APPLICATION DATA API (for settings/storage)                              │
│  ───────────────────────────────────────────────────────────────────────────│
│  GET  /signalk/v1/applicationData/global/:appid/:version/:key               │
│  PUT  /signalk/v1/applicationData/global/:appid/:version/:key               │
│  (See: https://demo.signalk.org/documentation/Developing/Plugins/WebApps)   │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                ┌───────────────────┴───────────────────┐
                │                                       │
                ▼                                       ▼
┌───────────────────────────────────┐    ┌───────────────────────────────────┐
│         WASM Plugin               │    │           Standalone              │
│       (runs in SignalK)           │    │        (own Axum server)          │
├───────────────────────────────────┤    ├───────────────────────────────────┤
│                                   │    │                                   │
│  SignalK provides API endpoints   │    │  Axum provides SAME endpoints    │
│  SignalK provides storage API     │    │  Local file/DB provides storage  │
│                                   │    │                                   │
│  Mayara WASM implements:          │    │  Mayara Standalone implements:   │
│  - Locator (FFI I/O)              │    │  - Locator (tokio I/O)           │
│  - Controller (FFI I/O)           │    │  - Controller (tokio I/O)        │
│  - RadarProvider trait            │    │  - RadarProvider trait           │
│                                   │    │                                   │
│  GUI served by SignalK            │    │  GUI embedded via rust-embed     │
│                                   │    │                                   │
│                                   │    │  Optional: register to SignalK   │
│                                   │    │  as provider (future)            │
└───────────────────────────────────┘    └───────────────────────────────────┘
                │                                       │
                └───────────────────┬───────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────────┐
                    │           Same GUI                │
                    │     viewer.html, control.js       │
                    │                                   │
                    │  Uses:                            │
                    │  - /radars/* for radar control    │
                    │  - /signalk/v1/applicationData/*  │
                    │    for settings persistence       │
                    └───────────────────────────────────┘
```

---

## Deployment Modes

### Mode 1: SignalK WASM Plugin (Current, Working)

```
┌─────────────────────────────────────────────────────────────┐
│                    SignalK Server (Node.js)                  │
│  ┌────────────────────────────────────────────────────────┐ │
│  │              WASM Runtime (wasmer)                      │ │
│  │  ┌──────────────────────────────────────────────────┐  │ │
│  │  │         mayara-signalk-wasm                       │  │ │
│  │  │                                                   │  │ │
│  │  │  ┌─────────────┐  ┌──────────────────────────┐   │  │ │
│  │  │  │  Locator    │  │   FurunoController       │   │  │ │
│  │  │  │  (FFI I/O)  │  │   (FFI I/O)              │   │  │ │
│  │  │  └──────┬──────┘  └────────────┬─────────────┘   │  │ │
│  │  │         │                      │                  │  │ │
│  │  │         └──────────┬───────────┘                  │  │ │
│  │  │                    ▼                              │  │ │
│  │  │         ┌──────────────────────┐                  │  │ │
│  │  │         │   RadarProvider      │◄── Implements    │  │ │
│  │  │         │   (v5 API impl)      │    SignalK trait │  │ │
│  │  │         └──────────────────────┘                  │  │ │
│  │  └──────────────────────┬───────────────────────────┘  │ │
│  └─────────────────────────┼──────────────────────────────┘ │
│                            │ FFI calls                       │
│  ┌─────────────────────────┴──────────────────────────────┐ │
│  │           SignalK Radar API v5 Endpoints                │ │
│  │  (SignalK routes requests to RadarProvider methods)     │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
              │
              ▼
         Browser / GUI
```

**Characteristics:**
- Runs inside SignalK's WASM sandbox
- Uses SignalK FFI for all network I/O
- Poll-based (no async runtime in WASM)
- SignalK handles HTTP routing, WebSocket management

### Mode 2: Standalone (Target Architecture)

```
┌─────────────────────────────────────────────────────────────┐
│                    mayara-server (Rust)                      │
│                                                              │
│  ┌─────────────┐  ┌──────────────────────────┐              │
│  │  Locator    │  │   FurunoController       │              │
│  │  (tokio)    │  │   (tokio)                │              │
│  └──────┬──────┘  └────────────┬─────────────┘              │
│         │                      │                             │
│         └──────────┬───────────┘                             │
│                    ▼                                         │
│         ┌──────────────────────┐                             │
│         │   RadarProvider      │◄── Same trait as WASM!     │
│         │   (async impl)       │                             │
│         └──────────┬───────────┘                             │
│                    │                                         │
│  ┌─────────────────┴─────────────────────────────────────┐  │
│  │              Axum Router                               │  │
│  │  ┌─────────────────────────────────────────────────┐  │  │
│  │  │         SignalK Radar API v5 Handlers            │  │  │
│  │  │  (SAME logic as WASM, different I/O backend)     │  │  │
│  │  │                                                   │  │  │
│  │  │  GET  /radars                                     │  │  │
│  │  │  GET  /radars/{id}/capabilities                   │  │  │
│  │  │  GET  /radars/{id}/state                          │  │  │
│  │  │  PUT  /radars/{id}/state                          │  │  │
│  │  │  WS   /radars/{id}/spokes                         │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  │  ┌─────────────────────────────────────────────────┐  │  │
│  │  │         Static File Server (GUI)                 │  │  │
│  │  │  /                    → viewer.html              │  │  │
│  │  │  /control.html        → control.html             │  │  │
│  │  │  /style.css, etc.                                │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
              │
              ▼
         Browser / GUI (same files!)
```

**Characteristics:**
- Native Rust binary with tokio async runtime
- Direct network I/O (socket2, tokio)
- Axum web server hosts API + GUI
- **Same API paths as SignalK** → same GUI works

### Mode 3: Standalone + SignalK Provider (Future)

```
┌─────────────────────────────────────────────────────────────┐
│                    mayara-server (Rust)                      │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐    │
│  │   (Same as Mode 2: Locator, Controller, Provider)    │    │
│  └──────────────────────────┬──────────────────────────┘    │
│                             │                                │
│  ┌──────────────────────────┴──────────────────────────┐    │
│  │                    Axum Router                       │    │
│  │  ┌────────────────────┐  ┌────────────────────────┐ │    │
│  │  │  Local API (v5)    │  │  SignalK Provider      │ │    │
│  │  │  /radars/*         │  │  Client                │ │    │
│  │  │                    │  │                        │ │    │
│  │  │  For local GUI     │  │  Registers with SK     │ │    │
│  │  │  and direct access │  │  Forwards radar data   │ │    │
│  │  └────────────────────┘  └───────────┬────────────┘ │    │
│  └──────────────────────────────────────┼──────────────┘    │
└─────────────────────────────────────────┼───────────────────┘
              │                           │
              ▼                           ▼
         Browser / GUI          ┌─────────────────────────┐
         (local access)         │    SignalK Server       │
                                │                         │
                                │  Mayara registered as   │
                                │  radar provider         │
                                │                         │
                                │  Other SK clients see   │
                                │  radar via SignalK API  │
                                └─────────────────────────┘
```

**Characteristics:**
- All benefits of Mode 2
- Additionally registers with a SignalK server as a **provider**
- SignalK clients (chart plotters, other apps) can access radar through SignalK
- Mayara is the **source**, SignalK is the **hub**

---

## Code Sharing Strategy

### Key Insight: Share Locator and Controller

The WASM plugin has a fully working Furuno implementation with locator and controller.
Instead of extracting "state machines" separately, we share the **actual locator and
controller code** by abstracting only the I/O layer.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           SHARED CODE (mayara-core)                          │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                         Locator                                      │    │
│  │  - Beacon packet construction                                        │    │
│  │  - Discovery state machine                                           │    │
│  │  - Interface enumeration logic                                       │    │
│  │  - Radar identification                                              │    │
│  │                                                                      │    │
│  │  I/O abstracted via trait: fn send_beacon(), fn recv_response()     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                      Controller (per vendor)                         │    │
│  │  - FurunoController                                                  │    │
│  │  - NavicoController (future)                                         │    │
│  │  - RaymarineController (future)                                      │    │
│  │                                                                      │    │
│  │  - Command encoding/decoding                                         │    │
│  │  - State synchronization logic                                       │    │
│  │  - Capability-based control handling                                 │    │
│  │                                                                      │    │
│  │  I/O abstracted via trait: fn send_cmd(), fn recv_response()        │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                      API Handler Logic                               │    │
│  │  - Request/response transformation                                   │    │
│  │  - Validation                                                        │    │
│  │  - Error mapping                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                    │                               │
                    ▼                               ▼
     ┌──────────────────────────┐     ┌──────────────────────────┐
     │   WASM I/O Layer         │     │   Tokio I/O Layer        │
     │                          │     │                          │
     │   impl IoProvider for    │     │   impl IoProvider for    │
     │   FfiSocket {            │     │   TokioSocket {          │
     │     fn send() {          │     │     fn send() {          │
     │       sk_socket_send()   │     │       socket.send()      │
     │     }                    │     │     }                    │
     │   }                      │     │   }                      │
     └──────────────────────────┘     └──────────────────────────┘
```

### The IoProvider Trait (Thin I/O Abstraction)

```rust
// In mayara-core (no I/O dependencies)

/// Minimal I/O abstraction - implementations are platform-specific
pub trait IoProvider {
    /// Send data to socket
    fn send(&mut self, data: &[u8]) -> Result<usize, IoError>;

    /// Receive data from socket (non-blocking)
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, IoError>;

    /// Check if data available
    fn poll_readable(&self) -> bool;
}

/// The actual Locator logic - shared between WASM and Standalone
pub struct Locator<I: IoProvider> {
    io: I,
    state: DiscoveryState,
    // ... all the discovery logic
}

impl<I: IoProvider> Locator<I> {
    pub fn new(io: I) -> Self { ... }

    pub fn poll(&mut self) -> Vec<DiscoveredRadar> {
        // This is the SAME code in WASM and Standalone
        // Only the IoProvider implementation differs
        if self.io.poll_readable() {
            let mut buf = [0u8; 1024];
            if let Ok(n) = self.io.recv(&mut buf) {
                return self.process_response(&buf[..n]);
            }
        }
        self.maybe_send_beacon();
        vec![]
    }
}
```

### RadarProvider Trait (API Layer)

```rust
// In mayara-core

/// Platform-agnostic radar provider interface
pub trait RadarProvider {
    fn list_radars(&self) -> Vec<RadarInfo>;
    fn get_capabilities(&self, id: &str) -> Option<CapabilityManifest>;
    fn get_state(&self, id: &str) -> Option<RadarState>;
    fn set_state(&self, id: &str, updates: StateUpdate) -> Result<(), ControlError>;
    fn poll(&mut self);
}

/// Shared implementation - works with any IoProvider
pub struct MayaraProvider<I: IoProvider> {
    locator: Locator<I>,
    controllers: HashMap<String, Controller<I>>,
}

impl<I: IoProvider> RadarProvider for MayaraProvider<I> {
    // All logic is shared - only I differs
}
```

### Platform-Specific I/O

**WASM (FFI to SignalK):**
```rust
// mayara-signalk-wasm/src/io.rs
pub struct FfiSocket { handle: u32 }

impl IoProvider for FfiSocket {
    fn send(&mut self, data: &[u8]) -> Result<usize, IoError> {
        unsafe { sk_socket_send(self.handle, data.as_ptr(), data.len()) }
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        unsafe { sk_socket_recv(self.handle, buf.as_mut_ptr(), buf.len()) }
    }
}

// Entry point
type WasmProvider = MayaraProvider<FfiSocket>;
```

**Native (Tokio):**
```rust
// mayara-lib/src/io.rs
pub struct TokioSocket { socket: Arc<UdpSocket> }

impl IoProvider for TokioSocket {
    fn send(&mut self, data: &[u8]) -> Result<usize, IoError> {
        // Uses try_send (non-blocking) to match poll-based interface
        self.socket.try_send(data).map_err(Into::into)
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.socket.try_recv(buf).map_err(Into::into)
    }
}

// Entry point
type NativeProvider = MayaraProvider<TokioSocket>;
```

---

## Architecture Diagram (Target State)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              mayara-core                                     │
│                    (Pure Rust, no I/O, WASM-compatible)                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐   │
│  │  protocol/    │ │   models/     │ │ capabilities/ │ │   state/      │   │
│  │  - furuno     │ │ - furuno.rs   │ │ - controls.rs │ │ - radar.rs    │   │
│  │  - navico     │ │ - navico.rs   │ │ - builder.rs  │ │ - power.rs    │   │
│  │  - raymarine  │ │ - raymarine   │ │               │ │               │   │
│  │  - garmin     │ │ - garmin.rs   │ │               │ │               │   │
│  └───────────────┘ └───────────────┘ └───────────────┘ └───────────────┘   │
│                                                                              │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐   │
│  │  arpa/        │ │  trails/      │ │ guard_zones/  │ │  io.rs        │   │
│  │  - detector   │ │ - history.rs  │ │ - zone.rs     │ │ (IoProvider   │   │
│  │  - tracker    │ │               │ │ - alert.rs    │ │  trait)       │   │
│  │  - cpa.rs     │ │               │ │               │ │               │   │
│  │ (Kalman filt) │ │               │ │               │ │               │   │
│  └───────────────┘ └───────────────┘ └───────────────┘ └───────────────┘   │
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
     │  WasmRadarProvider         │    │  Locator (tokio UDP)       │
     │  - Uses core ARPA/trails   │    │  Controller (tokio TCP)    │
     │  - Uses FFI for I/O        │    │  NavData (NMEA/SignalK)    │
     │                            │    │                            │
     │  SignalK FFI imports:      │    │  Platform network:         │
     │  - socket_udp_bind         │    │  - linux (netlink)         │
     │  - socket_tcp_connect      │    │  - macos (CoreFoundation)  │
     │  - socket_send/recv        │    │  - windows (Win32)         │
     │                            │    │                            │
     └────────────────────────────┘    └────────────────────────────┘
                    │                               │
                    ▼                               ▼
     ┌────────────────────────────┐    ┌────────────────────────────┐
     │     SignalK Server         │    │     Axum HTTP Server       │
     │                            │    │                            │
     │  Routes /radars/* to       │    │  /radars/*  (same API!)    │
     │  WASM RadarProvider        │    │  Static files (same GUI!)  │
     │                            │    │                            │
     │  Serves GUI from           │    │  Optional:                 │
     │  plugin static files       │    │  SignalK Provider Client   │
     │                            │    │  (registers to SK server)  │
     │  ◄──────────────────────────────┤                            │
     │  (Mayara registers as      │    │                            │
     │   radar provider to SK)    │    │                            │
     └────────────────────────────┘    └────────────────────────────┘
                    │                               │
                    └───────────────┬───────────────┘
                                    │
                                    ▼
                     ┌────────────────────────────┐
                     │         mayara-gui         │
                     │     (shared web assets)    │
                     │                            │
                     │  viewer.html               │
                     │  control.html              │
                     │  *.js, *.css               │
                     │                            │
                     │  All fetch from /radars/*  │
                     │  Works in ANY mode!        │
                     └────────────────────────────┘
```

---

## What Gets Shared

| Component | Location | WASM | Standalone | mayara_opencpn | Notes |
|-----------|----------|:----:|:----------:|:---------:|-------|
| Protocol parsing | mayara-core/protocol/ | ✓ | ✓ | ✓ | Packet encoding/decoding |
| Model database | mayara-core/models/ | ✓ | ✓ | ✓ | Radar specs, range tables |
| Control definitions | mayara-core/capabilities/ | ✓ | ✓ | ✓ | v5 API control schemas |
| RadarState types | mayara-core/state/ | ✓ | ✓ | ✓ | State representation |
| **ARPA** | mayara-core/arpa/ | ✓ | ✓ | ✓ | **Target tracking, CPA/TCPA** |
| **Trails** | mayara-core/trails/ | ✓ | ✓ | ✓ | **Target position history** |
| **Guard zones** | mayara-core/guard_zones/ | ✓ | ✓ | ✓ | **Zone alerting logic** |
| IoProvider trait | mayara-core/io.rs | ✓ | ✓ | - | I/O abstraction |
| **Web GUI** | mayara-gui/ | ✓ | ✓ | - | Shared web assets |

---

## SignalK Notifications from ARPA

The WASM plugin publishes collision warnings to SignalK's notification system,
enabling chart plotters to display radar-based alerts alongside AIS warnings.

### Notification Paths

```
notifications.navigation.closestApproach.radar:{radarId}:target:{targetId}
notifications.navigation.radarGuardZone.radar:{radarId}:zone:{zoneId}
notifications.navigation.radarTargetLost.radar:{radarId}:target:{targetId}
```

### How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          mayara-signalk-wasm                                 │
│                                                                              │
│   Spokes ──► ARPA Tracker ──► Targets with CPA/TCPA                         │
│                    │                                                         │
│                    ▼                                                         │
│         ┌─────────────────────┐                                             │
│         │  Notification Logic │                                             │
│         │  - CPA < threshold? │                                             │
│         │  - Guard zone hit?  │                                             │
│         │  - Target lost?     │                                             │
│         └─────────┬───────────┘                                             │
│                   │                                                          │
│                   ▼ SignalK FFI: publish_notification()                      │
└───────────────────┼─────────────────────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          SignalK Server                                      │
│                                                                              │
│   notifications.navigation.closestApproach.radar:furuno-1:target:3          │
│   { "state": "warn", "message": "ARPA target 3: CPA 320m in 5m 24s" }       │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Chart Plotters / SignalK Clients                          │
│                                                                              │
│   Same collision warning UI as AIS-based alerts                              │
│   (Freeboard-SK, WilhelmSK, etc.)                                           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Alert States

| State | CPA Threshold | Description |
|-------|---------------|-------------|
| `normal` | > 1000m | Target tracked, no danger |
| `alert` | < 1000m | Approaching, monitor closely |
| `warn` | < 500m | Getting close |
| `alarm` | < 200m | Danger, take action |
| `emergency` | < 100m | Imminent collision |

### Target Grace Period

Targets may temporarily disappear behind waves or rain. The tracker maintains
prediction for 30-60 seconds (configurable) before marking as lost.

### API Reference

See [impl_sk_radar_api_v6.md](../radar%20API/impl_sk_radar_api_v6.md) for full specification.

---

## Application Data Storage API

The GUI needs to persist settings (like guard zone configurations, display preferences).
SignalK provides this via the applicationData API. Standalone implements the same interface.

### API Endpoints

```
GET  /signalk/v1/applicationData/global/mayara/1.0/:key
PUT  /signalk/v1/applicationData/global/mayara/1.0/:key

Examples:
  GET  /signalk/v1/applicationData/global/mayara/1.0/guardZones
  PUT  /signalk/v1/applicationData/global/mayara/1.0/displaySettings
```

### Storage Backend

**WASM (SignalK provides storage):**
- SignalK stores data in its own database
- GUI calls SignalK's applicationData API

**Standalone (local storage):**
- Axum implements same endpoints
- Data stored in local file (`~/.mayara/settings.json`) or SQLite

```rust
// mayara-server/src/storage.rs

pub struct LocalStorage {
    path: PathBuf,
    data: HashMap<String, Value>,
}

impl LocalStorage {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn put(&mut self, key: &str, value: Value) -> Result<(), StorageError> {
        self.data.insert(key.to_owned(), value);
        self.persist()?;
        Ok(())
    }
}

// Axum routes
async fn get_app_data(
    Path((appid, version, key)): Path<(String, String, String)>,
    State(storage): State<Arc<RwLock<LocalStorage>>>,
) -> Json<Value> {
    let storage = storage.read().unwrap();
    Json(storage.get(&key).cloned().unwrap_or(Value::Null))
}

async fn put_app_data(
    Path((appid, version, key)): Path<(String, String, String)>,
    State(storage): State<Arc<RwLock<LocalStorage>>>,
    Json(value): Json<Value>,
) -> StatusCode {
    storage.write().unwrap().put(&key, value).unwrap();
    StatusCode::OK
}
```

### GUI Usage (same code works in both modes)

```javascript
// mayara-gui/settings.js

const STORAGE_BASE = '/signalk/v1/applicationData/global/mayara/1.0';

async function loadSettings(key) {
    const response = await fetch(`${STORAGE_BASE}/${key}`);
    return response.json();
}

async function saveSettings(key, value) {
    await fetch(`${STORAGE_BASE}/${key}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(value)
    });
}

// Works identically whether talking to SignalK or Standalone
const guardZones = await loadSettings('guardZones');
await saveSettings('displaySettings', { colorScheme: 'night' });
```

---

## SignalK Provider Mode

When running standalone with SignalK provider mode enabled:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│                            Mayara Standalone                                 │
│                                                                              │
│   1. Discovers radars via UDP                                                │
│   2. Controls radars via TCP                                                 │
│   3. Hosts local API at http://localhost:6502/radars/*                      │
│   4. Connects to SignalK server                                              │
│   5. Registers as radar provider                                             │
│   6. Pushes radar data to SignalK                                            │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
         │                              │
         │ Local GUI                    │ Provider connection
         ▼                              ▼
┌─────────────────┐          ┌─────────────────────────────────┐
│    Browser      │          │        SignalK Server           │
│                 │          │                                 │
│  http://        │          │  Receives radar data from       │
│  localhost:6502 │          │  Mayara provider                │
│                 │          │                                 │
│  Uses local     │          │  Other SignalK clients          │
│  Mayara API     │          │  (Freeboard-SK, WilhelmSK, etc.)│
│                 │          │  can access radar via SignalK   │
└─────────────────┘          └─────────────────────────────────┘
```

**Provider Registration Protocol:**

```javascript
// Mayara connects to SignalK and registers as provider
POST /signalk/v2/api/radars/providers
{
  "name": "Mayara Standalone",
  "version": "0.3.0",
  "capabilities": ["furuno", "navico", "raymarine", "garmin"]
}

// SignalK assigns provider ID
Response: { "providerId": "mayara-1", "endpoints": { ... } }

// Mayara pushes radar discoveries
POST /signalk/v2/api/radars/providers/mayara-1/radars
{
  "id": "furuno-drs4d-nxt-172-31-3-212",
  "make": "Furuno",
  "model": "DRS4D-NXT",
  ...
}

// Mayara pushes state updates
PUT /signalk/v2/api/radars/providers/mayara-1/radars/{id}/state
{ ... current state ... }

// Mayara pushes spokes via WebSocket
WS /signalk/v2/api/radars/providers/mayara-1/radars/{id}/spokes
```

---

## Migration Path

### Phase 1: Define IoProvider Trait & Move Locator

1. **Define IoProvider trait** in mayara-core
   - Simple trait: `send()`, `recv()`, `poll_readable()`
   - No async - poll-based to match WASM constraints

2. **Move Locator to mayara-core**
   - Current WASM locator becomes `Locator<I: IoProvider>`
   - All discovery logic is shared
   - WASM: implement `IoProvider` for FFI sockets
   - Native: implement `IoProvider` for tokio sockets

3. **Move Controller to mayara-core**
   - `FurunoController<I: IoProvider>`
   - All control logic is shared

### Phase 2: Unified API in Standalone

1. **Add SignalK-compatible endpoints to mayara-server**
   - `/radars/*` - same as SignalK Radar API v5
   - `/signalk/v1/applicationData/*` - storage API

2. **Implement local storage backend**
   - File-based or SQLite for settings
   - Same API as SignalK's applicationData

3. **Native IoProvider implementation**
   - Wrapper around tokio sockets
   - Uses try_send/try_recv for poll-based interface

### Phase 3: Share GUI Package

1. **Create mayara-gui/** directory
   - Move web assets from `mayara-signalk-wasm/public/`
   - GUI uses relative API paths (works in both modes)

2. **Update build systems**
   - WASM: copy from mayara-gui to public/
   - Standalone: embed from mayara-gui via rust-embed

### Phase 4: SignalK Provider Mode (Future)

1. **Implement SignalK provider client**
   - WebSocket client to connect to SignalK
   - Provider registration protocol
   - Push radar data to SignalK

2. **Add CLI flag** `--signalk-provider ws://signalk.local:3000`

---

## Benefits Summary

| Benefit | Description |
|---------|-------------|
| **One API to maintain** | SignalK Radar API v5 is the standard, used everywhere |
| **One GUI to maintain** | Same HTML/JS/CSS works in all modes |
| **Tested implementation** | WASM plugin proves the API design works |
| **Flexibility** | Users choose: WASM plugin OR standalone OR standalone+provider |
| **SignalK ecosystem** | Standalone can participate in SignalK network as provider |
| **Code quality** | Shared logic means bugs fixed once, everywhere |

---

## Simplified Crate Structure (Target)

> **Decision:** Eliminate mayara-lib. The WASM plugin proves mayara-core is sufficient for protocol work.

### Current vs Target Structure

```
CURRENT:                              TARGET:
mayara-server                         mayara-server
    │                                     │
    ▼                                     │  (absorbs I/O layer)
mayara-lib ─────────┐                     │
    │               │                     ▼
    ▼               │                 mayara-core
mayara-core         │                     ▲
    ▲               │                     │
    │               │                     │
mayara-signalk-wasm ┘                 mayara-signalk-wasm
```

**Key insight:** mayara-signalk-wasm already works with only mayara-core (no mayara-lib dependency).
This proves the protocol layer in mayara-core is complete and self-sufficient.

### What Moves Where

| Component | From | To | Notes |
|-----------|------|-----|-------|
| Protocol parsing | mayara-core | mayara-core | Already there |
| Models database | mayara-core | mayara-core | Already there |
| Capabilities | mayara-core | mayara-core | Already there |
| **ARPA** | mayara-lib | **mayara-core** | Pure computation, shared everywhere |
| **Trails** | mayara-lib | **mayara-core** | Keep with ARPA |
| **Guard zones** | mayara-lib | **mayara-core** | Zone logic is pure computation |
| Locator (network) | mayara-lib | mayara-server | Platform-specific I/O |
| Controller (network) | mayara-lib | mayara-server | Platform-specific I/O |
| Settings sync | mayara-lib | mayara-server | Runtime state management |
| NavData (NMEA) | mayara-lib | mayara-server | I/O dependent |
| Platform network | mayara-lib | mayara-server | Linux/macOS/Windows specific |

### Benefits

1. **Clearer dependencies** - Two-level hierarchy instead of three
2. **Less code duplication** - WASM and native share more via mayara-core
3. **ARPA everywhere** - Collision warnings in WASM, Standalone, AND mayara_opencpn
4. **Easier maintenance** - Single source of truth for radar logic

### mayara-core Grows To Include

```
mayara-core/src/
├── protocol/           # Existing: packet parsing
├── models/             # Existing: radar model database
├── capabilities/       # Existing: control definitions
├── state/              # Existing: radar state types
├── arpa/               # NEW: target detection + tracking
│   ├── detector.rs     # Blob/contour detection
│   ├── tracker.rs      # Kalman filter tracking
│   └── cpa.rs          # CPA/TCPA calculation
├── trails/             # NEW: target trail history
│   └── history.rs      # Position history storage
└── guard_zones/        # NEW: zone alerting logic
    └── zone.rs         # Zone definition + hit detection
```

### mayara-server Absorbs From mayara-lib

```
mayara-server/src/
├── main.rs             # Entry point, Axum setup
├── web/                # HTTP/WebSocket handlers
├── locator/            # FROM mayara-lib: network discovery
├── controller/         # FROM mayara-lib: radar control I/O
├── navdata/            # FROM mayara-lib: NMEA/SignalK integration
├── network/            # FROM mayara-lib: platform-specific sockets
│   ├── linux.rs
│   ├── macos.rs
│   └── windows.rs
└── storage.rs          # Local settings persistence
```

---

## File Reference (Target State)

| Path | Purpose | Shared? |
|------|---------|:-------:|
| `mayara-core/src/protocol/` | Protocol parsing | ✓ |
| `mayara-core/src/models/` | Model database | ✓ |
| `mayara-core/src/capabilities/` | Control definitions | ✓ |
| `mayara-core/src/state/` | State types | ✓ |
| `mayara-core/src/arpa/` | ARPA target tracking | ✓ |
| `mayara-core/src/trails/` | Target trail history | ✓ |
| `mayara-core/src/guard_zones/` | Guard zone logic | ✓ |
| `mayara-core/src/io.rs` | IoProvider trait | ✓ |
| `mayara-core/src/provider.rs` | RadarProvider trait | ✓ |
| `mayara-gui/` | Web GUI assets | ✓ |
| `mayara-signalk-wasm/src/io.rs` | FfiSocket: IoProvider impl | WASM only |
| `mayara-signalk-wasm/src/lib.rs` | WASM entry point | WASM only |
| `mayara-server/src/main.rs` | Binary entry, Axum setup | Native only |
| `mayara-server/src/locator/` | Network radar discovery | Native only |
| `mayara-server/src/controller/` | Radar control I/O | Native only |
| `mayara-server/src/network/` | Platform-specific sockets | Native only |
| `mayara-server/src/navdata/` | NMEA/SignalK integration | Native only |
| `mayara-server/src/storage.rs` | Local applicationData storage | Native only |

---

## Future: OpenCPN Integration (mayara_opencpn)

> **Decision:** Create a standalone OpenCPN plugin (mayara_opencpn) that connects to Mayara Standalone.

### Background

OpenCPN users currently use [radar_pi](https://github.com/opencpn-radar-pi/radar_pi) for radar display.
While mature (10+ years), it lacks Furuno support and modern Garmin/Raymarine models.

**Decision Rationale (Option B - Standalone Plugin):**
- Clean slate implementation with full control over UI/UX
- No dependency on radar_pi maintainers for upstream changes
- Can share rendering approach with Mayara web GUI
- ARPA/trails logic already in mayara-core - no reimplementation needed
- Leverages Mayara's proven WebSocket/protobuf protocol

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     mayara_opencpn (OpenCPN Plugin)                   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    MayaraRadarPanel                       │   │
│  │  - PPI rendering (OpenGL)                                 │   │
│  │  - Guard zones (uses mayara-core)                         │   │
│  │  - ARPA display (uses mayara-core)                        │   │
│  │  - Trails display (uses mayara-core)                      │   │
│  │  - EBL/VRM tools                                          │   │
│  └──────────────────────────────────────────────────────────┘   │
│                              │                                   │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    MayaraClient                           │   │
│  │  - HTTP: GET /radars, GET /capabilities, PUT /state       │   │
│  │  - WebSocket: /spokes/{id} (protobuf stream)              │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │  Mayara Standalone  │
                    │  (localhost:6502)   │
                    └─────────────────────┘
                               │
                               ▼
                        Radar Hardware
                    (Furuno, Navico, etc.)
```

### Data Flow

```
mayara_opencpn                          Mayara Standalone
    │                                      │
    ├── HTTP GET /radars ─────────────────►│  Discovery
    │◄──────────────── [{id, model, ...}] ─┤
    │                                      │
    ├── HTTP GET /radars/{id}/capabilities►│  Capabilities
    │◄────────────── {controls, ranges} ───┤
    │                                      │
    ├── WS /radars/{id}/spokes ───────────►│  Spoke stream
    │◄─────────── binary protobuf spokes ──┤  (continuous)
    │                                      │
    ├── HTTP PUT /radars/{id}/state ──────►│  Control
    │◄──────────────────────── 200 OK ─────┤
    │                                      │
    ├── GET /radars/{id}/targets ─────────►│  ARPA targets
    │◄────────── [{id, cpa, tcpa, ...}] ───┤
    │                                      │
    ├── WS /radars/{id}/targets ──────────►│  Target stream
    │◄───────────── target updates ────────┤  (optional)
```

### Spoke Data Format

**Mayara protobuf (same as web GUI):**
```protobuf
message RadarMessage {
    message Spoke {
        uint32 angle = 1;       // 0..spokes_per_revolution
        uint32 bearing = 2;     // true north reference
        uint32 range = 3;       // meters
        uint64 time = 4;        // milliseconds since epoch
        bytes data = 5;         // intensity values (0-255)
        int64 lat = 6;          // 1e-16 degrees
        int64 lon = 7;          // 1e-16 degrees
    }
    repeated Spoke spokes = 2;
}
```

### What mayara_opencpn Gets "For Free"

Because ARPA and trails logic moves to mayara-core, mayara_opencpn benefits:

| Feature | Source | Notes |
|---------|--------|-------|
| Target detection | mayara-core/arpa/ | Contour detection, blob tracking |
| Target tracking | mayara-core/arpa/ | Kalman filtering, prediction |
| CPA/TCPA calculation | mayara-core/arpa/ | Collision warnings |
| Target trails | mayara-core/trails/ | Historical position storage |
| Guard zones | mayara-core | Zone definition + alerting logic |

mayara_opencpn only needs to implement:
- OpenGL PPI rendering
- wxWidgets UI integration
- HTTP/WebSocket client
- Protobuf parsing

### Open Questions

1. **Rendering approach:**
   - OpenGL shaders (like radar_pi)?
   - Share WebGL approach with web GUI?

2. **Discovery:**
   - mDNS/Bonjour for automatic Mayara discovery?
   - Manual configuration (host:port)?
   - Both?

3. **Multiple radars:**
   - One mayara_opencpn panel per radar?
   - Single panel with radar selector?

