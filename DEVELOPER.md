# Mayara for developers

Mayara is designed to be used as a building block: run it alongside your own software and talk to it over HTTP and WebSocket. You never need to touch the proprietary radar protocols directly.

## Architecture

```
┌────────────┐    HTTP / WS    ┌───────────────┐     Ethernet     ┌─────────────┐
│ Your app   │ ◄─────────────► │ mayara-server │ ◄──────────────► │    Radar    │
│ (any lang) │   Signal K API  │               │   proprietary    │  (Navico,   │
└────────────┘                 │   REST API    │    protocols     │  Furuno, …) │
                               │   WebSocket   │                  └─────────────┘
                               │   Built-in    │
                               │   GUI         │
                               └───────────────┘
```

Mayara translates each brand's wire protocol into a uniform [Signal K Radar API](https://github.com/SignalK/signalk-server/blob/master/docs/develop/rest-api/radar_api.md). Your client code works the same regardless of which radar is connected. Multiple clients can connect simultaneously — PPI displays, chart overlays, or autonomous navigation systems.

## API overview

All endpoints are under `/signalk/v2/api/vessels/self/radars`.

| What                     | How                                             |
| ------------------------ | ----------------------------------------------- |
| List radars              | `GET /radars`                                   |
| Radar capabilities       | `GET /radars/{id}/capabilities`                 |
| Read/write controls      | `GET/PUT /radars/{id}/controls/{cid}`           |
| Acquire/delete targets   | `POST/DELETE /radars/{id}/targets`              |
| Spoke data stream        | `ws://.../radars/{id}/spokes` (binary protobuf) |
| Control & target updates | `ws://.../signalk/v1/stream` (JSON delta)       |
| OpenAPI spec             | `GET /radars/resources/openapi.json`            |

See [docs/api/](docs/api/README.md) for the full API reference.

## Client examples

The `client-examples/` directory has working clients in three languages that discover a radar, connect to spoke data, and render ASCII output:

- **Python** — `client-examples/python-client/run.sh`
- **JavaScript** — `client-examples/javascript-client/run.sh`
- **Bash/curl** — `client-examples/bash-client/radar_info.sh`

See [CLIENT.md](CLIENT.md) for details.

## Building from source

Requires Rust 1.90+. Quick version:

```sh
cargo build --release
cargo run --release -- --emulator -vv
```

See [BUILDING.md](BUILDING.md) for full instructions (all platforms, cross-compilation, feature flags, troubleshooting).

## Running for development

```sh
# Auto-detect radars on the network
cargo run

# Use the emulator when no radar is available
cargo run -- --emulator

# Verbose logging (debug / trace)
cargo run -- -v
cargo run -- -vv

# Specific brand only
cargo run -- --brand navico
```

See [USAGE.md](USAGE.md) for all command line options.

## Running tests

```sh
cargo test
```

Integration tests replay captured pcap files through the full radar pipeline. Unit tests cover protocol parsing, target tracking, and control logic.

## Target tracking

Mayara includes software-based ARPA (Automatic Radar Plotting Aid) target tracking. When enabled with `--targets arpa`, the server detects and tracks radar returns, computing course, speed, CPA, and TCPA.

The tracker uses Interacting Multiple Model (IMM) filtering, which runs multiple Kalman filters in parallel (constant velocity, coordinated turn, maneuvering) and blends their outputs based on which model best explains the observed motion. This handles targets that switch between cruising and turning without lag.

See [docs/internals/arpa.md](docs/internals/arpa.md) for details.

## Project structure

```
src/
  bin/mayara-server/     Server entry point
  lib/
    brand/               Per-brand protocol implementations
      navico/            Navico (BR24, 3G, 4G, HALO)
      furuno/            Furuno (DRS, FAR)
      garmin/            Garmin (HD, xHD, Fantom)
      raymarine/         Raymarine (Quantum, RD, HD)
      emulator/          Built-in radar simulator
    radar/               Brand-agnostic radar abstractions
      range.rs           Range management
      settings.rs        Control definitions
      target/            ARPA target tracking
    protos/              Protobuf definitions (spoke data)
web/gui/                 Built-in web GUI (reference client)
client-examples/         Example clients (Python, JS, Bash)
testdata/pcap/           Captured radar traffic for tests
docs/                    Additional documentation
```

## Internals

For a deeper dive into the architecture — brand plugin system, spoke data flow, controls, radar lifecycle — see [docs/internals/](docs/internals/README.md).

## Contributing

- See [CLAUDE.md](CLAUDE.md) for code quality standards, commit conventions, and PR guidelines.
- Run `cargo test` before submitting changes.
- One logical change per PR. Refactoring and behavior changes belong in separate PRs.

## License

Apache 2.0 — free for commercial and non-commercial use. See [LICENSE](LICENSE).
