# ONWA Radar Setup

The ONWA KRA-1009 radar, as used with the K-ASTRAL chartplotters, which list it as radar type KRA-5001.

## Network Requirements

The radar sits at a fixed address, `223.168.1.168`. It broadcasts its picture and status, but takes commands by unicast, so the Mayara machine needs an address of its own in `223.168.1.x` — for example `223.168.1.200/24`. Wired Ethernet only.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/onwa.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/onwa.html`](../web/gui/help/onwa.html) — edit that, not this file.

It covers the network setup, the supported controls, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
