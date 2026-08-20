# Raymarine Radar Setup

Quantum and Quantum 2, Cyclone, the RD radome series, Magnum and the open arrays.

## Network Requirements

The radar and the Mayara machine must be on the same wired Ethernet network, and that network **must have a DHCP server**. A Raymarine radar has no fallback address: until it gets a lease it stays silent.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/raymarine.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/raymarine.html`](../web/gui/help/raymarine.html) — edit that, not this file.

It covers wired (RayNet) setup, the Quantum WiFi credentials trap, the multicast groups and ports, supported models, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
