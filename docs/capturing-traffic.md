# Capturing Radar Traffic

This guide explains how to capture marine radar Ethernet traffic into a pcap file that can be replayed through `mayara-server --pcap <file>` or attached to a bug report.

- [Why capture](#why-capture)
- [The core problem](#the-core-problem)
- [Physical access by brand](#physical-access-by-brand)
- [Capture topology](#capture-topology)
- [WiFi capture](#wifi-capture)
- [What to record](#what-to-record)
- [Capture commands](#capture-commands)
- [Gotchas](#gotchas)
- [Packaging captures](#packaging-captures)
- [Reference](#reference)

## Why capture

Captures serve three purposes: attaching to bug reports against existing brands, generating fixtures for [adding a new radar brand](internals/new_radar_brand.md), and verifying on-the-wire behaviour while developing protocol code. A useful capture covers the full session — boot, idle, transmit, and operator control changes — and is taken at a point in the network where every radar packet is visible.

## The core problem

Marine radar networks are switched, not shared. A capture host attached to a normal switch port sees only its own unicast and broadcast traffic; unicast destined for the radar and most multicast frames are filtered out by the switch. Capturing therefore requires three things to be solved at once:

- **Physical access** to the cable carrying radar traffic (proprietary connectors, waterproof terminations).
- **Capture-host positioning** so the host actually sees the frames (transparent bridge, port mirror, or TAP).
- **Not breaking live traffic** during the capture — the radar and the MFD must remain talking.

## Physical access by brand

| Brand | Physical layer | What you need |
| ----- | -------------- | ------------- |
| Raymarine (RD, Magnum, Quantum wired) | RayNet — proprietary waterproof connector over 100Base-TX, T-568B internal pinout | RayNet-to-RJ45 adapter cable |
| Navico (Simrad, Lowrance, B&G) | Proprietary screw-lock connector over standard Ethernet | Vendor adapter or breakout cable |
| Furuno DRS / FAR | Standard Ethernet | RJ45 patch lead (DRS) or fibre-to-copper media converter for some FAR models |
| Garmin (GMR HD, xHD, Fantom) | Standard Ethernet | RJ45 patch lead |
| Raymarine Quantum (WiFi) | 2.4 GHz WiFi | Monitor-capable WiFi adapter; see [WiFi capture](#wifi-capture) |
| Furuno DRS4W (WiFi) | 2.4 GHz WiFi (radar acts as AP) | Monitor-capable WiFi adapter |

**Raymarine.** RayNet uses standard 100Base-TX over four pairs in a waterproof shell. Raymarine sells a RayNet-to-RJ45 adapter cable; search the Raymarine catalog for current part numbers. With an adapter, the radar drops into any 100Base-TX-capable switch.

**Navico.** The yellow screw-lock connector is electrically standard Ethernet. Navico and third parties sell breakout cables ending in RJ45; check the Navico/Simrad catalog for current part numbers.

**Furuno.** DRS-series radomes terminate in standard RJ45. Some FAR commercial radars use fibre on the radar side; in those cases a media converter is needed to bring the link onto a copper switch for capture.

**Garmin.** All current Garmin radars use standard RJ45 Ethernet.

## Capture topology

Four options, in order of usual preference.

### a) Linux transparent bridge (recommended default)

Two NICs on a Linux host — a Raspberry Pi with the on-board NIC plus a USB-Ethernet dongle works — bridged in software. The radar plugs into one NIC, the MFD (or the rest of the radar network) into the other. The capture host forwards frames between them and captures on the bridge interface.

```bash
sudo ip link add br0 type bridge
sudo ip link set eth0 master br0
sudo ip link set eth1 master br0
sudo ip link set eth0 mtu 9000
sudo ip link set eth1 mtu 9000
sudo ip link set br0  mtu 9000
sudo ip link set eth0 up
sudo ip link set eth1 up
sudo ip link set br0  up
```

Bringing the bridge up causes one brief link flap; after that the bridge is transparent to the radar and the MFD. This topology is the foundation for later MITM and packet-injection work.

### b) Managed switch with port mirror (SPAN)

Replace or augment the existing switch with a managed one and configure a mirror (SPAN) session from the radar's port to the capture port. This is the cleanest way to capture multiple devices simultaneously and avoids any in-line forwarding latency.

**Disable IGMP snooping** on the switch (or at minimum mark the capture port as a multicast router port), otherwise the switch will filter out multicast spokes the capture port never explicitly joined.

### c) Passive network TAP

Inline hardware that splits the link and presents a read-only copy to a monitor port. Zero risk of disturbing live traffic, and immune to bridge-flap effects. Slightly less flexible than a bridge because injection is not possible. Treated here as a category — pick whatever current product fits your link speed.

### d) Hub

Mentioned only for completeness. True Ethernet hubs are 100 Mbit at best, hard to source new, and offer no advantage over the bridge or TAP options. Recommend against.

## WiFi capture

For Raymarine Quantum on WiFi and the Furuno DRS4W (which is its own access point), the capture interface must be in monitor mode on the radar's channel.

If the WiFi is open, frames are plaintext and `tcpdump`/`wireshark` decode them directly. If WPA-protected, supply the network passphrase to the capture tool so it can derive per-session keys. Whether a given model defaults to open or protected depends on the radar's WiFi configuration — verify on the device rather than assuming.

Wired capture is preferable whenever the radar exposes a wired port, because monitor-mode capture is sensitive to channel hopping and signal strength.

## What to record

A useful capture covers all of these phases in a single file:

- **Boot** — power the radar from cold so discovery beacons and initial state are recorded.
- **Idle / standby** — at least a few seconds with the radar powered but not transmitting.
- **Transmit on** — at least one full antenna rotation while transmitting.
- **Active control changes** — change gain, range, sea clutter, rain clutter, power state, and mode. Many control responses appear on the wire only when something changes; captures that never poke the radar routinely miss the request/response pairs needed to decode commands.

Aim for at least one full sweep at each range the radar supports if range tables are a concern.

## Capture commands

The exact multicast addresses and ports for each brand are defined in `src/lib/brand/{brand}/protocol.rs` (Raymarine, Navico, Furuno, Garmin, Koden). Read the constants there rather than copying values into shell commands — they are the source of truth.

Full capture, full snaplen, file rotated by timestamp:

```bash
sudo tcpdump -i br0 -nn -s 0 -w "radar-$(date +%s).pcap"
```

Multicast-only, useful when sharing the wire with other unicast traffic:

```bash
sudo tcpdump -i br0 -nn -s 0 'multicast' -w "radar-mc-$(date +%s).pcap"
```

Per-brand narrowing — substitute the address and port from the brand's `protocol.rs`:

```bash
# Replace <addr> and <port> with values from src/lib/brand/<brand>/protocol.rs
sudo tcpdump -i br0 -nn -s 0 "host <addr> and udp port <port>" \
  -w "radar-brand-$(date +%s).pcap"
```

Remote capture, piped live into a local Wireshark for interactive inspection:

```bash
ssh capture-host 'sudo tcpdump -i br0 -nn -s 0 -U -w -' | wireshark -k -i -
```

## Gotchas

**IGMP snooping silently drops multicast.** Managed switches default to filtering multicast away from ports that have not joined the group. Disable IGMP snooping globally on the switch used for capture, or designate the capture port as a multicast router port.

**Jumbo frames truncate without `-s 0`.** Spoke data and chart-tile traffic routinely exceed 1500 bytes. Always use `-s 0` (full packet) and set the capture-interface MTU to 9000.

**DHCP requirement (Raymarine).** Raymarine radars need a DHCP server present to acquire an address; see [raymarine-setup.md](raymarine-setup.md#network-requirements) for the user-facing note. When bridging, make sure the radar's normal DHCP server (usually the MFD) is still reachable through the bridge.

**Promiscuous mode required when not bridging.** A NIC plugged into a SPAN port or TAP must be in promiscuous mode or it will drop frames not addressed to its MAC:

```bash
sudo ip link set eth0 promisc on
```

`tcpdump` enables promiscuous mode automatically; tools like raw `socket(AF_PACKET, …)` capture code may not.

**Self-assigned address ranges.** Radars commonly land in `169.254.0.0/16` (link-local) or vendor-specific ranges (see `src/lib/brand/furuno/protocol.rs`, `src/lib/brand/garmin/protocol.rs`). The capture host does not need an IP on that subnet to capture, but knowing the range helps when reading the pcap afterwards.

**Don't compress in flight.** Write the pcap to disk uncompressed; gzip afterwards. Inline compression risks dropping frames if the CPU stalls.

## Packaging captures

### Bug reports

Compress the pcap with gzip and attach it to the GitHub issue. Note that pcaps may contain MAC addresses, plus any Signal K or NMEA traffic sharing the wire — review the capture before uploading anything that might be sensitive on your boat (waypoints, AIS, position).

### New-brand fixtures

Place the full capture in the sibling `radar-recordings` repository checkout and follow the fixture-generation steps in [internals/new_radar_brand.md](internals/new_radar_brand.md#generate-a-pcap-fixture). The repo's fixture format is `.pcap.gz`; fixtures live under `testdata/pcap/` and are loaded by the `replay_*` integration tests.

## Reference

- `src/lib/brand/navico/protocol.rs`, `src/lib/brand/furuno/protocol.rs`, `src/lib/brand/garmin/protocol.rs`, `src/lib/brand/koden/protocol.rs`, `src/lib/brand/raymarine/protocol.rs` — authoritative multicast addresses, ports, and packet layouts.
- [internals/new_radar_brand.md](internals/new_radar_brand.md) — downstream fixture workflow and replay test pattern.
- [Wireshark documentation](https://www.wireshark.org/docs/) and [tcpdump manual](https://www.tcpdump.org/manpages/tcpdump.1.html) — capture tool references.
