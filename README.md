### MAYARA

Welcome to **Ma**rine **Ya**cht **Ra**dar server.

This project will play as intermediary between marine yacht radars such as Navico's HALO series, Furuno, Garmin, Raymarine, etc, and modern client side tools acting as chartplotter or MFD. 
Intended use is for applications such as [Freeboard SK](https://github.com/SignalK/freeboard-sk), [OpenCPN](https://opencpn.org), [AvNav](https://wellenvogel.net/software/avnav/docs/beschreibung.html?lang=en).
__Note: no implication that this software will actually be available in any of the mentioned software packages is made!__

On the "client" side, it will offer a [Signal K](https://signalk.org) API for basic information and a `WebSocket` server for the actual radar data.
Changing the radar settings is possible, a [JSON Schema](https://json-schema.org) explains what settings can be made.

## Using `mayara-server`

See [USAGE.md](USAGE.md) for command line options and examples.

## Docker

Pre-built images for `amd64` and `arm64` are available on GitHub Container Registry:

```bash
docker pull ghcr.io/marineyachtradar/mayara-server:latest
```

Run with the built-in emulator:

```bash
docker run -p 6502:6502 ghcr.io/marineyachtradar/mayara-server:latest mayara-server --emulator
```

Run with a real radar (Linux, requires host networking for multicast):

```bash
docker run --net=host ghcr.io/marineyachtradar/mayara-server:latest \
    mayara-server --brand navico --interface eth0
```

Or use Docker Compose (starts the emulator by default):

```bash
docker compose -f docker/docker-compose.yml up
```

See `docker/docker-compose.yml` for example configurations including real radar and shore-based setups.

> **Note:** Real radar discovery relies on multicast/broadcast traffic. On Linux, use `--net=host` (or `network_mode: host` in Compose) to give the container direct network access. The emulator works fine with regular bridge networking.

## Building from Source

See [BUILDING.md](BUILDING.md) for instructions on installing Rust and building on Windows, Linux, and macOS.

## Writing a client 

See [CLIENT.md](CLIENT.md) for more information on how to communicate with `mayara-server`.

## Status

See [TODO](TODO.md)

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

## Radar support

The following radars are fully supported right now:

* Navico: all digital models e.g. BR24, 3G, 4G, HALO20, HALO24, HALO3/4/6, HALO3000+. 
* Raymarine Quantum 2 (Q24C, Q24D).
* Furuno DRS-NXT series (DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, DRS25A-NXT) including dual range.
* Furuno DRS4W WiFi radar ("1st Watch").
* Furuno FAR-2xx7 series (see [FAR setup guide](docs/furuno-far-setup.md) for network and IMO mode requirements).

We are working on:

* Furuno DRS, DRS4DL, DRS6A X-Class, FAR-15x3, FAR-3000 — implemented but need testing with real hardware.
* Garmin HD, xHD, xHD2, xHD3, Fantom, and Fantom Pro. All models are supported including dual range, MotionScope/Doppler, and per-model feature detection. Needs testing with real hardware — PCAP files welcome!
* Raymarine HD digital radars are likewise implemented but fully untested.

We are actively looking for people to add new radars. If your radar is not on the "fully supported" list, contact us!

## Help us

We're on Discord, here is an invite: [Discord channel](https://discord.gg/kC6h6JVxxC)


