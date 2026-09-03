# Usage

## Installation

### Pre-built binaries

Download the latest release for your platform from the [releases page](https://github.com/MarineYachtRadar/mayara-server/releases).

### Building from source

See [BUILDING.md](BUILDING.md) for detailed instructions on installing Rust and building from source.

Quick version:

```bash
cargo build --release
```

The binary will be at `target/release/mayara-server`.

## Quick Start

```bash
# Basic usage - auto-detects radars on all interfaces
mayara-server

# With verbose logging (debug level)
mayara-server -v

# With very verbose logging (trace level)
mayara-server -vv

# When you don't have a real radar and just want to experiment
mayara-server --emulator
```



## 
Command Line Options

### Network & Server

| Option                        | Description                                                                                             |
| ----------------------------- | ------------------------------------------------------------------------------------------------------- |
| `-p, --port <PORT>`           | HTTP/WebSocket server port (default: 6502)                                                              |
| `--tls-cert <FILE>`           | TLS certificate file (PEM format). Enables HTTPS when set with `--tls-key`.                             |
| `--tls-key <FILE>`            | TLS private key file (PEM format). Enables HTTPS when set with `--tls-cert`.                            |
| `-i, --interface <INTERFACE>` | Limit radar discovery to a specific network interface                                                   |
| `--allow-wifi`                | Allow radar discovery on WiFi interfaces (not recommended for most brands due to multicast limitations) |
| `--parent <PID>`              | Run as a helper process of a chart plotter; see [Running under a chart plotter](#running-under-a-chart-plotter) |

### Radar Selection

| Option                | Description                                                                            |
| --------------------- | -------------------------------------------------------------------------------------- |
| `-b, --brand <BRAND>` | Limit to a specific radar brand: `furuno`, `garmin`, `navico`, `raymarine`, `emulator` |
| `--multiple-radar`    | Keep searching for additional radars after finding one                                 |
| `--emulator`          | Use built-in radar emulator instead of real radar discovery                            |

### Target Tracking

| Option                 | Description                                           |
| ---------------------- | ----------------------------------------------------- |
| `-t, --targets <MODE>` | Target analysis mode (default: `arpa`)                |
|                        | `arpa` - Full ARPA tracking with CPA/TCPA calculation |
|                        | `trails` - Show target trails without full tracking   |
|                        | `none` - Disable target tracking                      |
| `--merge-targets`      | Merge targets from multiple radars into a shared list |

### Navigation Data

| Option                            | Description                                                                 |
| --------------------------------- | --------------------------------------------------------------------------- |
| `-n, --navigation-address <ADDR>` | Navigation service address for GPS/heading data                             |
|                                   | No value: auto-discover via mDNS                                            |
|                                   | Interface name: search mDNS on that interface                               |
|                                   | `tcp:ip:port`: anonymous Signal K TCP stream                                |
|                                   | `udp:ip:port`: listen for NMEA 0183 UDP broadcasts                          |
|                                   | `ws:ip:port`: Signal K WebSocket (via discovery)                            |
|                                   | `wss:ip:port`: Signal K secure WebSocket (requires `--accept-invalid-certs`)|
| `--nmea0183`                      | Use NMEA 0183 instead of Signal K for navigation                            |
| `--accept-invalid-certs`          | Accept self-signed TLS certificates when connecting to Signal K via         |
|                                   | HTTPS/WSS. Required for boat-LAN setups that use self-signed certs.         |
| `--signalk-token <TOKEN>`         | Signal K bearer token for authenticating to a `ws:` or `wss:` upstream.     |
|                                   | Sent as `?token=...` on the WebSocket and as `Authorization: Bearer ...`    |
|                                   | on the REST discovery and AIS-store seeding probes. Has no effect on        |
|                                   | `tcp:` or `udp:` transports.                                                |
| `--signalk-token-file <PATH>`     | Read the bearer token from a file (single line, trailing whitespace         |
|                                   | trimmed). Use this instead of `--signalk-token` to keep the token out       |
|                                   | of the process argv.                                                        |

**Note on authentication:** Authenticated Signal K servers can only be reached
via `ws:` or `wss:`. The plain `tcp:` transport is anonymous-only. Pass the
token via `--signalk-token`, `--signalk-token-file`, or the
`MAYARA_SIGNALK_TOKEN` environment variable (in that order of precedence).
Embedded control characters (e.g. `\r`/`\n` in a token read from a file) are
rejected at startup to prevent HTTP header injection.

The token must grant at least `readonly` on `vessels.*` so Mayara can seed
its in-memory AIS store from the upstream `/signalk/v1/api/vessels/` snapshot.
The standard way to obtain such a token is to POST an access request to
`POST /signalk/v1/access/requests` with your `clientId` and have the Signal K
administrator approve it under **Security → Access Requests**; the issued
token then survives Signal K restarts.

**Note on TLS SNI:** `wss:ip:port` and mDNS-discovered HTTPS Signal K servers
use the server's IP address (not hostname) during the TLS handshake. Some
strict reverse proxies (e.g. nginx with `server_name` enforcement and
TLS SNI matching) reject handshakes that present an IP as SNI. Boat-LAN
Signal K deployments typically do not enforce this and the handshake proceeds
because `--accept-invalid-certs` bypasses hostname verification entirely.
If you run Mayara against a strict cloud-hosted Signal K instance, configure
the server to accept IP-based SNI or point Mayara at an on-LAN instance.

### Stationary Installation

| Option                                | Description                                             |
| ------------------------------------- | ------------------------------------------------------- |
| `--stationary`                        | Shore-based radar mode (no vessel motion)               |
| `--static-position <LAT> <LON> <HDG>` | Fixed position: latitude, longitude, heading in degrees |

### Operation Modes

| Option          | Description                                          |
| --------------- | ---------------------------------------------------- |
| `--transmit`    | Automatically put detected radars into transmit mode |
| `-r, --replay`  | Replay mode for pcap file playback                   |
| `--fake-errors` | Testing mode that simulates control errors           |

### Output & Debugging

| Option          | Description                                |
| --------------- | ------------------------------------------ |
| `-v, --verbose` | Increase logging verbosity                 |
| `-q, --quiet`   | Decrease logging verbosity                 |
| `--output`      | Write RadarMessage protobuf data to stdout |
| `--openapi`     | Output OpenAPI specification and exit      |

### Anonymous Usage Stats

The GUI asks once whether to report that this install works. On a yes, mayara
sends at most two small reports per run: one when a radar first delivers data,
one when a radar first accepts a control change. Each holds the mayara version,
operating system, radar brand and model, its lifetime transmit hours where the
radar reports them, and a random install id -- never a position, serial number
or network address. Reports go to
`https://telemetry.keversoft.com/mayara/v1/event`. See
[ENDUSER.md](ENDUSER.md#anonymous-usage-stats).

The answer lives in the settings file, so a run that cannot store its settings
never asks and never reports.

`MAYARA_DEPLOYMENT` tells us how mayara reached the boat, which is the
difference between "nobody runs Garmin radars" and "nobody runs Garmin radars
*in the container*". Whoever packages mayara sets it: `container` (set by the
official image), `signalk-server-plugin`, or `mayara_pi`. Any other short label
is passed through. Unset, mayara reports `embedded` when a parent process
supervises it and `standalone` otherwise.

| Option / variable            | Description                                        |
| ---------------------------- | -------------------------------------------------- |
| `--no-telemetry`             | Never ask, never send                              |
| `MAYARA_TELEMETRY=false`     | Same, for containers and service units             |
| `MAYARA_TELEMETRY=true`      | Answer yes for the user; the GUI does not ask      |
| `MAYARA_TELEMETRY_URL=<url>` | Send to your own collector instead                 |
| `MAYARA_DEPLOYMENT=<id>`     | How mayara was installed (see below)               |

## Examples

### Basic yacht installation

```bash
# Auto-detect radar, enable ARPA tracking
mayara-server
```

### Specific radar brand

```bash
# Only search for Navico radars
mayara-server --brand navico
```

### Shore-based radar

```bash
# Fixed position radar installation
mayara-server --stationary --static-position 52.3676 4.9041 45.0
```

### Development with emulator

```bash
# Use built-in radar emulator for testing
mayara-server --emulator -vv
```

### Multiple radars

```bash
# Keep searching after finding first radar
mayara-server --multiple-radar --merge-targets
```

### Replay mode

```bash
# Replay captured radar data (requires tcpreplay of pcap file)
mayara-server --replay
```

### HTTPS with TLS

```bash
# Serve over HTTPS with PEM certificate and key
mayara-server --tls-cert /path/to/cert.pem --tls-key /path/to/key.pem
```

### Custom navigation source

```bash
# Listen for NMEA 0183 on specific UDP port
mayara-server --nmea0183 -n udp:0.0.0.0:10110
```

### Running under a chart plotter

A chart plotter such as OpenCPN can start mayara for its own use, passing its
own process id:

```bash
# Started by the plotter, which passes its pid
mayara-server --parent 12345
```

In this mode mayara:

- serves the web interface on `127.0.0.1` only, so it is not reachable from the
  rest of the network,
- does not advertise itself on mDNS, so it does not turn up in other machines'
  network browsers,
- exits as soon as process 12345 is gone, so a plotter that crashes or is killed
  does not leave mayara behind holding the radar sockets and the server port.

Radar discovery itself is unaffected and still uses the real network interfaces.
The parent is checked every two seconds; mayara exits straight away if the pid is
already gone at startup.

## Web Interface

The built-in web interface is available at `http://localhost:6502` (or your configured port).

Unless `--parent` is used, mayara announces itself on the local network with mDNS (Bonjour/Avahi), so from
another computer, tablet or phone on the same network the web interface is
reachable at `http://mayara.local:6502` without knowing the server's IP address.
The announcement is the DNS-SD service type `_mayara-http._tcp`, with TXT keys
`version`, `api` (Signal K radar API version), `path` and `scheme`
(`http` or `https`, depending on whether TLS is configured). Browse for other
mayara servers with, for example, `dns-sd -B _mayara-http._tcp` (macOS) or
`avahi-browse -r _mayara-http._tcp` (Linux).

Features:
- Real-time radar display (PPI view)
- Range and control adjustments
- Target tracking display
- Guard zone configuration

## API

Mayara provides a Signal K compatible REST API and WebSocket streams:

- **REST API**: `http://localhost:6502/signalk/v2/api/vessels/self/radars/`
- **WebSocket**: `ws://localhost:6502/signalk/v1/stream`
- **Spoke data**: `ws://localhost:6502/signalk/v2/api/vessels/self/radars/{id}/spokes`

See the [API documentation](docs/api/README.md) for details.

## Troubleshooting

### Radar not detected

1. Ensure you're on the same network subnet as the radar
2. Check that multicast traffic is allowed
3. Try specifying the interface: `mayara-server -i eth0`
4. Increase verbosity to see discovery attempts: `mayara-server -vv`

### Poor performance over WiFi

Radar data uses multicast which performs poorly over WiFi. Run mayara-server on a wired connection and access the web interface over WiFi instead.

### Missing spokes

This typically indicates network issues. Ensure:
- Wired ethernet connection to radar
- No network congestion
- Correct MTU settings (radar data uses large packets)
