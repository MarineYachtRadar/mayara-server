# Mayara

**Ma**rine **Ya**cht **Ra**dar server — unlocking the capabilities of marine yacht radars through open protocols.

Mayara translates proprietary radar network protocols into a well-described, open API based on the [Signal K](https://signalk.org) marine data standard. Your software communicates with Mayara over HTTP and WebSocket to control radars and receive spoke data, without needing to understand the vendor-specific wire formats. Mayara works alongside the brand's own MFD — it does not replace it.

## Features

- Classic PPI radar display (built-in reference GUI)
- Radar control: power, range, gain, sea/rain clutter, filters, modes
- Spoke data streaming via WebSocket
- ARPA target tracking with CPA/TCPA
- AIS integration
- Dual-range support (where the radar supports it)
- Doppler (Navico HALO, Garmin Fantom, Raymarine Quantum 24D, Furuno DRS NXT)

Mayara can serve multiple clients simultaneously — PPI displays, chart applications with radar overlay, or autonomous navigation systems that act on the radar image.

## Radar support

Fully supported and tested with real hardware:

- [**Navico**](docs/navico-setup.md) — BR24, 3G, 4G, HALO 20, HALO 20+, HALO 24, HALO 2000–6000
- [**Raymarine**](docs/raymarine-setup.md) — Quantum, RD series
- [**Furuno**](docs/furuno-setup.md) — DRS-NXT series (DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, DRS25A-NXT) including dual range
- [**Furuno**](docs/furuno-setup.md) — DRS4W WiFi ("1st Watch")
- [**Furuno**](docs/furuno-setup.md) — FAR-2xx7 series

Implemented but awaiting real-hardware validation:

- [**Garmin**](docs/garmin-setup.md) — HD, xHD, xHD2, xHD3, Fantom, Fantom Pro (all models including dual range and MotionScope/Doppler)
- [**Furuno**](docs/furuno-setup.md) — DRS, DRS4DL, DRS6A X-Class, FAR-15x3, FAR-3000
- [**Raymarine**](docs/raymarine-setup.md) — HD, Magnum, Cyclone

If your radar is not on the fully supported list, contact us to help with testing!

## End users

Mayara runs on Windows, macOS, and Linux (including Raspberry Pi). Download a pre-built binary, start it, and open a web browser — no programming required.

See [ENDUSER.md](ENDUSER.md) for download links, installation, and how to get started.

## Developers

See [DEVELOPER.md](DEVELOPER.md) for build instructions, command line usage, and the client API.

## Help us

We're on Discord: [join the channel](https://discord.gg/kC6h6JVxxC)
