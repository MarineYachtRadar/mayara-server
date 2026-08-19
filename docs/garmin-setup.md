# Garmin Radar Setup

GMR HD, xHD, xHD2 and xHD3, Fantom and Fantom Pro.

## Network Requirements

The machine running Mayara **must** have an IP address in the `172.16.0.0/12` range (`172.16.x.x` to `172.31.x.x`), for example `172.16.3.150` with netmask `255.240.0.0`. Without it the radar is never detected. Wired Ethernet only.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/garmin.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/garmin.html`](../web/gui/help/garmin.html) — edit that, not this file.

It covers the subnet setup, the multicast groups and ports, supported models, dual range, MotionScope/Doppler, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
