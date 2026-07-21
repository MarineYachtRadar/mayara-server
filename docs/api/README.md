# Mayara API

Mayara implements the [Signal K Radar API](https://github.com/SignalK/signalk-server/blob/master/docs/develop/rest-api/radar_api.md).

## Base URL

All radar API endpoints are under:

```
/signalk/v2/api/vessels/self/radars
```

## OpenAPI Specification

The full OpenAPI specification is available at runtime:

```
GET /signalk/v2/api/vessels/self/radars/resources/openapi.json
```

Or generate it with:

```bash
mayara-server --openapi
```

## Quick Reference

### REST Endpoints

| Method | Endpoint                                                     | Description                                        |
| ------ | ------------------------------------------------------------ | -------------------------------------------------- |
| GET    | `/signalk/v2/api/vessels/self/radars`                        | List all detected radars                           |
| GET    | `/signalk/v2/api/vessels/self/radars/interfaces`             | List network interfaces and radar discovery status |
| GET    | `/signalk/v2/api/vessels/self/radars/diagnostics`            | Download a gzipped JSON network diagnostics snapshot |
| GET    | `/signalk/v2/api/vessels/self/radars/{id}/capabilities`      | Get radar capabilities and legend                  |
| GET    | `/signalk/v2/api/vessels/self/radars/{id}/controls`          | Get all control values                             |
| GET    | `/signalk/v2/api/vessels/self/radars/{id}/controls/{cid}`    | Get specific control value                         |
| PUT    | `/signalk/v2/api/vessels/self/radars/{id}/controls/{cid}`    | Set control value                                  |
| GET    | `/signalk/v2/api/vessels/self/radars/{id}/targets`           | List tracked targets                               |
| POST   | `/signalk/v2/api/vessels/self/radars/{id}/targets`           | Acquire target at position                         |
| DELETE | `/signalk/v2/api/vessels/self/radars/{id}/targets/{tid}`     | Delete tracked target                              |
| GET    | `/signalk/v2/api/vessels/self/radars/resources/openapi.json` | OpenAPI specification                              |

### WebSocket Streams

| Endpoint                                          | Description                                    |
| ------------------------------------------------- | ---------------------------------------------- |
| `/signalk/v1/stream`                              | Signal K delta stream (controls, targets, AIS) |
| `/signalk/v2/api/vessels/self/radars/{id}/spokes` | Binary spoke data stream (protobuf)            |

### Recording & Playback

Endpoints under `/v2/api/vessels/self/radars/recordings`:

| Method | Endpoint                               | Description                 |
| ------ | -------------------------------------- | --------------------------- |
| GET    | `.../recordings/radars`                | List recordable radars      |
| POST   | `.../recordings/record/start`          | Start recording             |
| POST   | `.../recordings/record/stop`           | Stop recording              |
| GET    | `.../recordings/record/status`         | Get recording status        |
| POST   | `.../recordings/playback/load`         | Load recording for playback |
| POST   | `.../recordings/playback/play`         | Start/resume playback       |
| POST   | `.../recordings/playback/pause`        | Pause playback              |
| POST   | `.../recordings/playback/stop`         | Stop playback               |
| POST   | `.../recordings/playback/seek`         | Seek to position            |
| PUT    | `.../recordings/playback/settings`     | Update playback settings    |
| GET    | `.../recordings/playback/status`       | Get playback status         |
| GET    | `.../recordings/files`                 | List recording files        |
| GET    | `.../recordings/files/{name}`          | Get recording metadata      |
| PUT    | `.../recordings/files/{name}`          | Rename recording            |
| DELETE | `.../recordings/files/{name}`          | Delete recording            |
| GET    | `.../recordings/files/{name}/download` | Download recording file     |
| POST   | `.../recordings/files/upload`          | Upload recording file       |
| GET    | `.../recordings/directories`           | List recording directories  |
| POST   | `.../recordings/directories`           | Create recording directory  |
| DELETE | `.../recordings/directories/{name}`    | Delete directory            |

## WebSocket Protocol

### Connecting

```
ws://localhost:6502/signalk/v1/stream?subscribe=self&sendCachedValues=true
```

Query parameters:

| Parameter          | Values                | Default | Description                            |
| ------------------ | --------------------- | ------- | -------------------------------------- |
| `subscribe`        | `none`, `self`, `all` | `self`  | Baseline subscription (see below)      |
| `sendCachedValues` | `true`, `false`       | `true`  | Send current control values on connect |

The `subscribe` parameter is a **baseline context filter**, matching Signal K's
streaming model:

| Value  | Streams                                                                                          |
| ------ | ----------------------------------------------------------------------------------------------- |
| `none` | nothing until you send explicit `subscribe` messages (below)                                     |
| `self` | all **own-ship** data — radar controls/targets, `navigation.*`, `notifications.*` (the default)  |
| `all`  | everything `self` streams **plus** other contexts (AIS vessels, `vessels.*`)                      |

Explicit `subscribe` messages are **additive**: they add paths on top of the
baseline without replacing it. To receive exactly one set of paths and nothing
else, connect with `subscribe=none` and subscribe to just those paths (this is
what the built-in GUI does, and what a Signal K server bridging mayara should do
so it does not re-import its own navigation). See
[Subscribe](#client--server-subscribe) / [Unsubscribe](#client--server-unsubscribe).

### Server → Client: Delta Updates

Control value changes and target updates are sent as delta messages. Each delta
carries a `context` (the Signal K vessel it applies to): own-ship data uses the
detected own-ship context, falling back to `vessels.self`; AIS uses the observed
vessel's context.

```json
{
  "context": "vessels.self",
  "updates": [{
    "$source": "mayara",
    "timestamp": "2024-01-15T10:30:00Z",
    "values": [
      {"path": "radars.nav1034A.controls.gain", "value": 50},
      {"path": "radars.nav1034A.controls.sea", "value": 30, "auto": true}
    ]
  }]
}
```

Target updates use the same format with paths like `radars.{id}.targets.{tid}`.
AIS vessel updates arrive under their own context, e.g. `"context":
"vessels.urn:mrn:imo:mmsi:{mmsi}"`, and only when subscribed (baseline `all`, or
an explicit `vessels.*` subscription).

The stream opens with a hello carrying `name`, `version`, `roles`, and `self`
(the own-ship context). On first connection (when `sendCachedValues=true`),
metadata describing each control is sent in a `meta` array.

**Subscribable path prefixes:**

| Prefix           | Data                                    | Context   |
| ---------------- | --------------------------------------- | --------- |
| `radars.*`       | radar controls and ARPA targets         | own-ship  |
| `navigation.*`   | own-ship heading, position, COG/SOG     | own-ship  |
| `notifications.*`| radar alarms (e.g. guard zones)         | own-ship  |
| `vessels.*`      | AIS vessels                             | other     |

### Client → Server: Set Control Value

```json
{
  "path": "radars.nav1034A.controls.gain",
  "value": 50
}
```

For guard zones, include additional fields:

```json
{
  "path": "radars.nav1034A.controls.guardZone1",
  "value": 0,
  "endValue": 90,
  "startDistance": 100,
  "endDistance": 500,
  "enabled": true
}
```

### Client → Server: Subscribe

Subscribe to specific paths with optional rate limiting:

```json
{
  "subscribe": [
    {"path": "radars.*.controls.*", "period": 1000},
    {"path": "radars.nav1034A.controls.gain", "policy": "instant"}
  ]
}
```

Subscriptions work for controls, targets, navigation, and AIS:

```json
{
  "subscribe": [
    {"path": "radars.*.controls.*"},
    {"path": "radars.nav1034A.targets.*"},
    {"path": "navigation.*"},
    {"path": "vessels.*"}
  ]
}
```

Path patterns support wildcards:

- `radars.*.controls.*` — all controls on all radars
- `radars.nav1034A.controls.gain` — specific control on a specific radar
- `radars.nav1034A.targets.*` — all targets on a specific radar
- `radars.*.targets.*` — all targets on all radars
- `navigation.*` — heading, position, speed updates
- `vessels.*` — AIS vessel updates

Subscription options:

| Field       | Description                                                       |
| ----------- | ----------------------------------------------------------------- |
| `path`      | Path pattern (required, supports `*` wildcards)                   |
| `policy`    | `instant` (on change), `ideal` (rate-limited), `fixed` (periodic) |
| `period`    | Update interval in ms (for `fixed` policy)                        |
| `minPeriod` | Minimum interval between updates in ms (for `ideal`)              |

### Client → Server: Unsubscribe

```json
{
  "desubscribe": [
    {"path": "radars.*.controls.gain"}
  ]
}
```

## See Also

- [Signal K Radar API Specification](https://github.com/SignalK/signalk-server/blob/master/docs/develop/rest-api/radar_api.md) — full API specification
- [Power Control](controls/power.md) — power/transmit control details
- [Range Control](controls/range.md) — range control details
