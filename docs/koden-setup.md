# Koden Radar Setup

MDS-R series radars connected through a RADARpc Ethernet control box (MDS-5R, MDS-6R, MDS-11R).

> **Untested.** Koden support has never been exercised against a real radar. If you have one, please get in touch.

## Network Requirements

Koden radars use UDP broadcast on port 10001, so the Mayara machine must be on the same subnet as the radar — typically `192.168.0.x`. Wired Ethernet only.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/koden.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/koden.html`](../web/gui/help/koden.html) — edit that, not this file.

It covers the network setup, the model-code table, the supported controls, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
