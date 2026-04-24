# Client Examples

The `client-examples/` directory contains minimal client implementations that demonstrate how to connect to the mayara-server API and consume radar data.

## Python Client

`client-examples/python-client/` — Two viewers sharing a virtual environment. Requires Python 3; the venv is created automatically on first run.

**Spoke viewer** — Connects to the first radar's spoke data stream and displays sampled spoke data as ASCII art.

```sh
./client-examples/python-client/run.sh [--url http://localhost:6502]
```

**Target viewer** — Subscribes to ARPA/MARPA targets via the Signal K WebSocket and displays a live-updating table with bearing, distance, course, speed, and CPA/TCPA.

```sh
./client-examples/python-client/run-target-viewer.sh [--url http://localhost:6502]
```

## JavaScript Client

`client-examples/javascript-client/` — Same functionality as the Python client, using Node.js.

```sh
./client-examples/javascript-client/run.sh [--url http://localhost:6502]
```

Installs npm dependencies automatically on first run. Requires Node.js 18+.

## Bash Client

`client-examples/bash-client/` — Walks through every REST API endpoint using curl and jq, showing discovery, capabilities, controls, targets, and the OpenAPI spec.

```sh
./client-examples/bash-client/radar_info.sh [http://localhost:6502]
```

Requires curl and jq.

## What They Demonstrate

### Spoke viewers (Python, JavaScript)

1. **Discover radars** via `GET /signalk/v2/api/vessels/self/radars`
2. **Fetch capabilities** via `GET /signalk/v2/api/vessels/self/radars/{id}/capabilities`
3. **Connect to the spoke WebSocket** at the `spokeDataUrl` from step 1
4. **Decode protobuf messages** using `RadarMessage.proto` from the source tree
5. **Sample 32 spokes** across one revolution and display them as ASCII art

Both spoke viewers read `src/lib/protos/RadarMessage.proto` directly so the protobuf definition stays in sync with the server.

### Target viewer (Python)

1. **Discover radars** via `GET /signalk/v2/api/vessels/self/radars`
2. **Connect to the Signal K WebSocket** at `/signalk/v1/stream?subscribe=none`
3. **Subscribe to targets** by sending `{"subscribe": [{"path": "radars.*.targets.*", "policy": "instant"}]}`
4. **Render a live table** of tracked targets (bearing, distance, course, speed, CPA/TCPA)

## Full GUI

For a complete radar display with real-time rendering, control panels, and target tracking, see the web GUI in `web/gui/`. It is served by the server at `/gui/` and uses the same REST and WebSocket APIs demonstrated here.
