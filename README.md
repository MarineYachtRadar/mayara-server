### MAYARA

Welcome to **Ma**rine **Ya**cht **Ra**dar server.

This project will play as intermediary between marine yacht radars such as Navico's HALO series, Furuno, Garmin, Raymarine, etc, and modern client side tools acting as chartplotter or MFD. 
Intended use is for applications such as [Freeboard SK](https://github.com/SignalK/freeboard-sk), [OpenCPN](https://opencpn.org), [AvNav](https://wellenvogel.net/software/avnav/docs/beschreibung.html?lang=en).
__Note: no implication that this software will actually be available in any of the mentioned software packages is made!__

On the "client" side, it will offer a [Signal K](https://signalk.org) API for basic information and a `WebSocket` server for the actual radar data.
Changing the radar settings is possible, a [JSON Schema](https://json-schema.org) explains what settings can be made.

## Origins

This is basically a rewrite of the [OpenCPN radar plugin](https://github.com/opencpn-radar-pi/radar_pi) that I have worked over ten years or so.
The problem with that code is that it is written in C++ with wxWidgets, and very much meant to operate as a plugin to OpenCPN. That makes it hard to graft on
an extra layer that allows it to be used in other contexts.

## Philosophy

The code shall:

* Be able to run independently, and provide a simple API for clients to use. This shall be 'friendly' to web based software.
* As far as possible, detect all radars automatically without any configuration; in the `radar_pi` plugin you had to set which brand/type of radar is installed.
* Make it as simple as possible to add more radars. Our experience with `radar_pi` tells us that there are hardly any folks out there cruising with the right skillset to make this happen.
* Be robust and error-free. Again, C++ allows you to be doing stuff illegally and for many years we had race conditions and other bugs in `radar_pi`. Writing the new server in Rust will hopefully make this an easy thing to do.

## Radar Support 

| Brand | Status | Models |
|-------|--------|--------|
| **Furuno** | 🚧 Partial  | DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, FAR series |
| **Navico** | testers wanted | BR24, 3G, 4G, HALO20, HALO24, HALO3/4/6, HALO3000+ |
| **Raymarine** | 🚧 Partial | Quantum 2, RD series (untested) |
| **Garmin** | 📋 Planned | xHD series |

## Deployment Modes

Mayara can run in two modes:

### SignalK Plugin (mayara-signalk-wasm)
- No need to install the mayara-server
- Runs as a WASM plugin inside SignalK server 3.0+
- Integrates with SignalK's data model and notification system
- Best for boats already running SignalK

### Standalone Server (mayara-server)
- Self-contained binary with built-in web server
- No SignalK dependency required
- Same API as SignalK mode - GUIs work unchanged

## Download

Pre-built binaries are available on the [Releases page](https://github.com/MarineYachtRadar/mayara-server/releases):

- **Linux x86_64** - Static binary (works on most Linux distros)
- **Linux ARM64** - For Raspberry Pi 4/5 (Raspbian/Debian)
- **macOS Intel** - For Intel Macs
- **macOS Apple Silicon** - For M1/M2/M3 Macs  
- **Windows x86_64** - For Windows 10/11

## API

Mayara implements the [SignalK Radar API](https://github.com/SignalK/signalk-radar-api):

```
GET  /radars                      - List discovered radars
GET  /radars/{id}/capabilities    - Get capabilities manifest
GET  /radars/{id}/state           - Get current state
PUT  /radars/{id}/state           - Update controls
WS   /radars/{id}/spokes          - WebSocket spoke stream

GET  /radars/{id}/targets         - List ARPA targets
POST /radars/{id}/targets         - Manual target acquisition
```

## Important Notes

### Furuno Multi-Client Behavior

Furuno radars support multiple TCP clients (multi-master mode). When mayara connects, it actively requests model information via `$R96`, so model detection works regardless of whether other clients (NavNet TZtouch, TimeZero, etc.) are already connected.

## Status

See [TODO](TODO.md)

## Help us

We're on Discord, here is an invite: [Discord channel](https://discord.gg/kC6h6JVxxC)


