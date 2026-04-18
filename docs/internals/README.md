# Mayara Internals

This directory documents the internal architecture and design of mayara-server. It is aimed at contributors and developers working on the codebase itself.

## Architecture Overview

Mayara is structured in three layers:

```
┌──────────────────────────────────────────────────┐
│                  Web Server (Axum)                │
│         REST API · WebSocket · Embedded GUI       │
├──────────────────────────────────────────────────┤
│               Radar Abstraction Layer             │
│    RadarInfo · CommonRadar · SharedRadars         │
│    Controls · Ranges · Spokes · Target Tracking   │
├──────────────────────────────────────────────────┤
│              Brand Implementations                │
│  Navico · Furuno · Garmin · Koden · Raymarine     │
│     Locator · Report Parser · Command Sender      │
└──────────────────────────────────────────────────┘
         │                            │
    Ethernet (UDP)              Navigation Data
    multicast/broadcast         Signal K / NMEA
```

Brand implementations handle the proprietary wire protocols. The radar abstraction layer provides a uniform model for controls, spokes, and targets. The web server exposes everything as a Signal K API.

## Source Tree

```
src/
  bin/mayara-server/
    main.rs              Entry point
    web.rs               Axum HTTP/WebSocket server
    web/signalk/v2.rs    Signal K REST + WebSocket endpoints
    web/recordings.rs    Recording management endpoints

  lib/
    mod.rs               Cli args, start_session(), Brand enum
    brand/
      mod.rs             RadarLocator and CommandSender traits
      navico/            Navico protocol (BR24, 3G, 4G, HALO)
      furuno/            Furuno protocol (DRS, FAR)
      garmin/            Garmin protocol (HD, xHD, Fantom)
      raymarine/         Raymarine protocol (Quantum, RD, HD)
      emulator/          Built-in radar simulator
    radar/
      mod.rs             RadarInfo, CommonRadar, SharedRadars
      range.rs           Range management and classification
      settings.rs        ControlId, Controls, SharedControls
      spoke.rs           Spoke data types
      target/            ARPA target tracking (blob, tracker, kalman, IMM)
      cpa.rs             CPA/TCPA calculation
      exclusion.rs       Exclusion zone masking (stationary mode)
    locator.rs           Radar discovery on all NICs
    network/             UDP socket creation, multicast, per-platform code
    config.rs            Persistent settings (JSON on disk)
    stream.rs            Signal K delta formatting, subscriptions
    navdata.rs           Own-ship navigation (heading, GPS, COG, SOG)
    ais.rs               AIS vessel store
    pcap.rs              PCAP file parser/writer
    replay.rs            PCAP replay dispatcher
    recording/           Radar recording/playback (.mrr files)
    protos/              Protobuf definitions (RadarMessage)
    util.rs              Shared utilities

web/gui/                 Embedded web GUI (reference client)
client-examples/         Python, JavaScript, Bash example clients
testdata/pcap/           Captured radar traffic for integration tests
```

## Startup Flow

`start_session()` in `lib/mod.rs` orchestrates startup:

1. Create `SharedRadars` (the central radar registry)
2. Start the `Locator` subsystem — spawns UDP listeners on all NICs for all enabled brands
3. Start `NavigationData` — connects to Signal K server or NMEA source for heading/GPS
4. Optionally start PCAP replay dispatcher (if `--pcap <file>` given)
5. Optionally start AIS vessel tracking (if `--pass-ais` given)
6. Start `TargetTracker` subsystem (if `--targets arpa`)
7. Start the web server — serves REST API, WebSocket streams, and embedded GUI

All subsystems use `tokio-graceful-shutdown` for clean SIGTERM handling.

## Brand Plugin Architecture

Each brand implements two traits defined in `brand/mod.rs`:

**`RadarLocator`** — radar discovery. Each brand registers one or more multicast/broadcast addresses where radars send beacons. The locator receives packets on these addresses and calls `RadarLocator::process()`, which parses brand-specific beacon formats to identify radars, extract serial numbers, and determine connection addresses.

**`CommandSender`** — radar control. When a user changes a control (gain, range, power, etc.), the web server calls `CommandSender::set_control()` with a brand-agnostic `ControlValue`. The brand implementation translates this into the proprietary wire command and sends it to the radar.

Each brand also implements a **report receiver** that runs as an async task, receiving UDP packets from the radar (status reports, spoke data) and updating `RadarInfo`/`CommonRadar` accordingly.

### Brand registration

`create_brand_listeners()` in `brand/mod.rs` instantiates all enabled brands. Each brand's `create()` function returns a list of `LocatorAddress` entries specifying:

- The multicast/broadcast address to listen on
- Optional beacon request packets to send periodically
- The `RadarLocator` implementation

### Adding a new brand

1. Create `src/lib/brand/<name>/` with `mod.rs`, `protocol.rs`, `report.rs`, `command.rs`, `settings.rs`
2. Implement `RadarLocator` for beacon parsing
3. Implement `CommandSender` for control translation
4. Implement a report receiver task that calls `CommonRadar::add_spoke()` for each spoke
5. Register controls in a `settings::new()` function
6. Add the brand to the `Brand` enum and `create_brand_listeners()`
7. Add a feature flag in `Cargo.toml`

## Spoke Data Flow

```
Radar (UDP) ──► Brand report parser
                    │
                    ▼
              CommonRadar::add_spoke()
                    │
                    ├──► Exclusion zone masking
                    ├──► Blob detector (ARPA targets)
                    ├──► Trail buffer update
                    ├──► Antenna offset (GPS position)
                    │
                    ▼
              Accumulate in RadarMessage (protobuf)
                    │
                    ▼
              broadcast_radar_message()
                    │
                    ▼
              WebSocket clients (/radars/{id}/spokes)
```

Each spoke carries a bearing angle, range, and an array of pixel values. The brand parser converts the proprietary format into a `GenericSpoke` and calls `add_spoke()`. When the bearing wraps past 0 (full rotation), the accumulated `RadarMessage` is broadcast to all subscribed WebSocket clients as a protobuf binary message.

## Controls System

Controls are the bridge between the web API and the radar hardware. Each control has:

- A `ControlId` (brand-agnostic enum: `Gain`, `Sea`, `Rain`, `Range`, `Power`, etc.)
- A `ControlDefinition` with metadata (type, min/max, valid values, descriptions, read-only flag)
- A current value, optional auto flag, and optional auto adjustment value

Controls are categorized by destination:

| Destination | Description                                    | Examples                       |
| ----------- | ---------------------------------------------- | ------------------------------ |
| `Radar`     | Sent to the physical radar via `CommandSender` | Gain, Range, Power             |
| `Internal`  | Client-side only, not sent to radar            | Orientation, ColorPalette      |
| `ReadOnly`  | Informational, updated by the radar            | RotationSpeed, FirmwareVersion |
| `Trail`     | Affects trail rendering in CommonRadar         | TargetTrails                   |
| `Target`    | Affects target tracking                        | GuardZone, ClearTargets        |

Brand implementations define which controls are available via `settings::new()` and `update_when_model_known()`. Some controls are model-specific (e.g., Doppler only on HALO/Fantom).

## Radar Discovery and Lifecycle

1. **Locator** listens on brand-specific multicast addresses across all wired NICs
2. Brand `RadarLocator::process()` parses a beacon → creates `RadarInfo` with addresses, serial, pixel format
3. `SharedRadars::add()` registers the radar, restoring any persisted settings (model, ranges, guard zones)
4. The brand spawns a **report receiver** task (processes status/spoke packets) and optionally a **command sender** and **info sender**
5. The report receiver identifies the model (from status packets), sets up controls and ranges, then processes spokes
6. The radar becomes visible in the API once ranges are set

## Replay and Testing

The `--pcap <file>` mode replays captured radar traffic through the full pipeline. `RadarSocket` is an enum over `Udp(UdpSocket)` and `Replay(ReplayReceiver)` — brand code uses the same receive API regardless of source.

Integration tests in `tests/replay_*.rs` replay brand-specific pcap fixtures and verify that radars are discovered, models identified, and spokes processed.

## Further Reading

- [ARPA Target Tracking](arpa.md) — IMM filtering and blob detection
