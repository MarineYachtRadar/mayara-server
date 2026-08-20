# Furuno Radar Setup

DRS-NXT, DRS and X-Class, the FAR commercial series, and the DRS4W "1st Watch".

## Network Requirements

The machine running Mayara **must** have an IP address on the `172.31.0.0/16` subnet, for example `172.31.3.150/16`. Without it the radar is never detected. Wired Ethernet, except for the DRS4W, which is its own WiFi access point.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/furuno.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/furuno.html`](../web/gui/help/furuno.html) — edit that, not this file.

It covers the subnet setup, the broadcast and multicast addresses, DRS and FAR model detection by part code, FAR-2xx7 IMO mode configuration, the DRS4W, and troubleshooting including the one-control-session-per-IP limit.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
