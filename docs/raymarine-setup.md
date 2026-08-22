# Raymarine Radar Setup

Quantum and Quantum 2, Cyclone, the RD radome series, Magnum and the open arrays.

## Network Requirements

The radar and the Mayara machine must be on the same wired Ethernet network.

A **Quantum** needs a DHCP server on that network: it has no fallback address and stays silent until it gets a lease. An **RD or HD radome** never asks for one — it gives itself a fixed `10.x.x.x` address and keeps it forever, which is why it needs the Mayara machine to hold an address in that range too.

Where a chartplotter is involved the network must be `198.18.0.0/21` (netmask `255.255.248.0`). Let DHCP place the Mayara machine if you can; if you set its address by hand, use `198.18.0.2`–`198.18.0.19`. Never give it a fixed address in `198.18.0.32`–`198.18.3.255` — chartplotters claim those for themselves without checking whether anything already has one.

## Full guide

The complete setup guide is served by Mayara itself, so it is available on the boat with no internet connection:

```
http://<mayara-host>:6502/gui/help/raymarine.html
```

It is also reachable from the "Network Configuration Help" panel on Mayara's radar list page. The source file is [`web/gui/help/raymarine.html`](../web/gui/help/raymarine.html) — edit that, not this file.

It covers wired (RayNet) setup, the Quantum WiFi credentials trap, the address ranges a Raymarine network uses, the multicast groups and ports, supported models, and troubleshooting.

## See also

- [Radar networking](../web/gui/help/networking.html) — why the radar itself must be wired
- [Capturing Radar Traffic](./capturing-traffic.md) — packet captures for bug reports
