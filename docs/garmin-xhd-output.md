# Garmin xHD Output Bridge

The `garmin-xhd-output` feature makes Mayara emulate a Garmin GMR xHD radar on
the local Garmin Marine Network. A Garmin GPSMAP chartplotter discovers the
virtual radar automatically and displays spoke data from **any** radar source
that Mayara supports — Furuno, Navico, Koden, the built-in emulator, or a PCAP
replay.

## Use Case

A typical setup has a Furuno DRS radar connected to a Linux computer running
Mayara, and a Garmin GPSMAP chartplotter on the same network. Without this
feature the Garmin can only display Garmin radars. With it, the Garmin sees the
Furuno data as if a GMR xHD were connected.

Controls flow in both directions: adjusting gain, sea clutter, rain clutter,
range, or transmit state on the Garmin chartplotter sends the corresponding
command to the real radar via Mayara's normal control system.

## Requirements

- Mayara built with the `garmin-xhd-output` feature (see [Building](#building))
- The host machine must have an IP address in the `172.16.0.0/12` subnet
  (Garmin Marine Network) — typically `172.16.x.x`
- If the host also runs Furuno (which uses `172.31.0.0/16`), both address
  families must be on the **same physical NIC**; Mayara auto-detects the Garmin
  address by looking at the interface that carries the Furuno address

## Building

The feature is gated behind a Cargo feature flag:

```sh
cargo build --release --features garmin-xhd-output
```

The `garmin-xhd-output` feature implies the `furuno` feature (because `spawn()`
is currently called from the Furuno brand's `found()` method). Future work could
make the spawn point brand-agnostic.

## Network Configuration

### Single NIC (recommended)

Configure two IP addresses on the same physical interface — one in the Furuno
subnet, one in the Garmin subnet:

```
interface: enp0s13f0u2
  172.31.254.217/16   ← Furuno communication
  172.16.254.217/16   ← Garmin Marine Network
```

Mayara detects the Garmin address automatically: it finds the NIC that carries
the Furuno address (chosen via `-i`), then picks the first `172.16/12` address
on that same NIC. This avoids accidentally choosing a Docker bridge address
(`172.17–20.x.x`) if those are present.

### Multicast TTL

All xHD output sockets are created with `IP_MULTICAST_TTL=1`. This confines
multicast traffic to the local network segment and avoids flooding through
routers or Wi-Fi access points that do not implement IGMP snooping.

## Protocol Overview

The bridge emulates the Garmin xHD enhanced protocol (`0x09xx` message IDs,
`product_id=0x06d0`). Four tasks run concurrently:

| Task | Direction | Address | Port | Description |
|------|-----------|---------|------|-------------|
| CDM heartbeat | out | `239.254.2.2` | 50050 | Radar discovery (CDM V2 `0x038e`) |
| Status stream | out | `239.254.2.0` | 50100 | Settings/state broadcast (~1 Hz) |
| Spoke sender | out | `239.254.2.0` | 50102 | Sweep data (`0x0998` spoke packets) |
| Command listener | in | `<nic_ip>` | 50101 | Control commands from the plotter |

Each socket binds to its own well-known port (not an ephemeral port). The
plotter uses the source port to identify the radar type; binding to the wrong
port causes discovery to fail.

### CDM Heartbeat (discovery)

Sent every second for the first 30 packets, then every 5 seconds. The heartbeat
carries `product_id=0x06d0` which identifies the device as a GMR xHD. The
Garmin chartplotter listens for this heartbeat to discover radars on the
Marine Network.

### Status Stream

Broadcasts the full set of ~40 individual status packets per second. Each
packet contains a single value (one per message ID), mirroring the format of a
real xHD radar. The packets include:

- Scanner state (transmitting / standby)
- Current range A (meters, snapped to the xHD range table)
- Gain mode and gain value
- Sea clutter mode and gain
- Rain clutter mode and gain
- Bearing alignment, AFC mode, noise blanker, etc.
- Capability bitmap (`0x09B1`) — copied from a real GMR xHD capture
- Range table (`0x09B2`) — the 16 xHD nautical ranges

The transmit state is read directly from `SharedControls::Power` so it reflects
the real radar's state immediately when the Furuno reports a power change.

Rain mode and rain gain are cached in `SharedState` (not read from
`SharedControls`) because Mayara maps "rain off" to `auto=true` internally while
leaving the previous gain value unchanged. Caching ensures the plotter sees
`rain_mode=0, rain_gain=0` immediately after the user turns rain off.

### Spoke Sender

Subscribes to `RadarInfo::message_tx` and converts each incoming
protobuf `RadarMessage` spoke to the xHD wire format (`0x0998`):

**Angle mapping**: source angle (0..spokes_per_rev) → xHD units (0..11519,
step=8). Values above 11512 crash the plotter; the conversion uses integer
modulo and floor-to-8 to guarantee this.

**Sample resampling**: nearest-neighbour from the source sample count to the
xHD fixed count of 695 samples/spoke.

**Range fields**: the spoke header carries two range fields:
- `range_meters` — nearest value from the xHD 16-entry range table; used by
  the plotter for its range display UI
- `display_meters` — the raw `spoke.range` value from the source radar
  (unmodified); used by the plotter to scale the chart overlay image

The distinction between the two fields is critical for correct chart overlay
scaling. `display_meters` must be the raw source value, not the snapped table
value.

**Brand-specific range correction**: Furuno's internal `spoke.range` is
approximately 1.7822× the xHD range set by the user. The bridge divides by
this factor before snapping to the nearest xHD table entry (for `range_meters`)
but passes the raw value through as `display_meters`. Other brands (Navico,
emulator, …) deliver the correct meters directly and need no correction.

### Command Listener

Receives unicast UDP on `<nic_ip>:50101`. The plotter sends one packet per
control change. Each packet is a standard GMN header (`u32 msg_id`, `u32
payload_len`) followed by the value.

Handled message IDs:

| Msg ID | Name | Action |
|--------|------|--------|
| `0x0919` | `MSG_TRANSMIT_MODE` | Power Standby=1 / Transmit=2 → `ControlId::Power` |
| `0x091e` | `MSG_RANGE_A` | Range in meters → `ControlId::Range`; sets 5 s range lock |
| `0x0924` | `MSG_RANGE_A_GAIN_MODE` | 0=manual / 2=auto → `ControlId::Gain` auto flag |
| `0x0925` | `MSG_RANGE_A_GAIN` | Gain 0..10000 (percent×100) → `ControlId::Gain` |
| `0x0939` | `MSG_RANGE_A_SEA_MODE` | 0=off / 1=manual / 2=auto → `ControlId::Sea` |
| `0x093a` | `MSG_RANGE_A_SEA_GAIN` | Sea gain → `ControlId::Sea` |
| `0x0933` | `MSG_RANGE_A_RAIN_MODE` | 0=off → rain auto; 1=on → rain 50% → `ControlId::Rain` |
| `0x0934` | `MSG_RANGE_A_RAIN_GAIN` | Rain gain → `ControlId::Rain` |
| `0x0916` | `MSG_RPM_MODE` | Ignored (Furuno does not support slow-turn mode) |

Commands are forwarded to the real radar via
`SharedControls::send_to_command_handler()`, which routes them through the
existing `ControlUpdate` broadcast channel to the brand's `CommandSender`.

Each received command is echoed back on the status multicast address
(`239.254.2.0:50100`) so the plotter sees its command acknowledged immediately
without waiting for the next status broadcast cycle.

**Range lock**: after the plotter sets a range, the spoke thread ignores
`spoke.range` updates for 5 seconds. Without this lock, the first incoming
spoke from the radar (still carrying the old range) would immediately overwrite
the range the plotter just set, causing the plotter to revert.

## xHD Range Table

The 16 supported ranges (nautical fractions in meters):

```
232, 463, 926, 1389, 1852, 2778, 3704, 5556,
7408, 11112, 14816, 22224, 29632, 44448, 66672, 88896
```

Range commands from the plotter are snapped to the nearest entry. Ranges from
the source radar are also snapped (for `range_meters`) while the raw value is
preserved (for `display_meters`).

## Limitations

- **Single range only**: the bridge always presents a single-range radar. Dual
  range is not emulated even if the source radar supports it.
- **No Sentry Mode / Timed Transmit**: these features shorten magnetron life and
  are deliberately not forwarded to the physical radar.
- **Furuno only at startup**: `spawn()` is currently called from the Furuno
  brand's `found()` method. Other brands will need their own call site added.
- **No bearing alignment forwarding**: bearing alignment commands from the
  plotter are echoed back but not forwarded (the offset is managed by Mayara
  itself).

## Troubleshooting

**Plotter shows "Not Available":**
- Check that Mayara logs `GarminXhd output starting on 172.16.x.x`. If it logs
  `no interface in 172.16.0.0/12 found`, the host has no address in the Garmin
  subnet.
- Verify the CDM heartbeat is sent: `sudo tcpdump -i <iface> udp port 50050`.
  You should see packets from `172.16.x.x` to `239.254.2.2` every second.
- Ensure no firewall blocks UDP ports 50050, 50100, 50101, 50102.

**Chart overlay not scaled correctly:**
- The spoke `display_meters` field must carry the raw `spoke.range` from the
  source radar, not the snapped xHD value. See the Spoke Sender section above.

**Range/gain buttons on plotter have no effect:**
- Run Mayara with `RUST_LOG=mayara::output::garmin_xhd=debug` and watch for
  `GarminXhd CMD msg=0x...` lines. If commands appear but the radar does not
  respond, the `send_to_command_handler` call may be failing (check for `WARN`
  lines).

**Plotter shows wrong transmit state at startup:**
- The transmit state is read from `SharedControls::Power`. If the radar has not
  yet sent its first status report when the bridge starts, the state defaults to
  Standby. It will correct itself within a few seconds once the first report
  arrives.
