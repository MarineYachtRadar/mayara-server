# Navico Radar Setup

Simrad, Lowrance and B&G radars: BR24, Broadband 3G and 4G, and all HALO models.

## Network Requirements

The radar and the Mayara machine must be on the same wired Ethernet network. There is no subnet requirement and no DHCP server is needed — the radar announces itself by multicast, which any unmanaged switch passes.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/navico.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/navico.html`](../web/gui/help/navico.html) — edit that, not this file.

It covers the multicast groups and ports, supported models, heading data for HALO Doppler, dual range, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
