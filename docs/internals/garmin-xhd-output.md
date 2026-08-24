# Garmin xHD Output Bridge

`src/lib/output/garmin_xhd/` is the mirror image of a brand in
`src/lib/brand/`. A brand receives a proprietary protocol and feeds it into
`RadarInfo`; this bridge subscribes to `RadarInfo` and emits a proprietary
protocol — the Garmin enhanced radar protocol, as a GMR xHD speaks it. See
[the setup guide](../garmin-xhd-output.md) for what it does for a user, and
`research/garmin/enhanced-radar-protocol.md` for the wire format.

## Where it hangs off the radar

`RadarInfo::start_outputs()` starts every output that republishes a radar's
spokes outside the Signal K API — the `--output` stdout forwarder and this
bridge. Every brand calls it once the radar is registered with `SharedRadars`,
which is what makes the bridge brand-agnostic: nothing in
`src/lib/output/` knows which brand produced the spokes it is converting.

`spawn()` refuses in three cases: the radar is a Garmin (the plotter can talk
to it directly), the host has no address on the Garmin Marine Network, or
another radar is already being emulated. The last is tracked in a `OnceLock`,
because the xHD multicast groups and ports are fixed and two emulated radars
would interleave their spokes.

## Tasks

Four subsystems run per bridge. Each owns its socket; none of them shares one.

| Module       | Direction | Endpoint             | Contents                     |
| ------------ | --------- | -------------------- | ---------------------------- |
| `cdm.rs`     | out       | `239.254.2.2:50050`  | discovery heartbeat `0x038e` |
| `status.rs`  | out       | `239.254.2.0:50100`  | settings and state reports   |
| `spokes.rs`  | out       | `239.254.2.0:50102`  | sweep data `0x0998`          |
| `command.rs` | in        | `<local addr>:50101` | control changes from plotter |

Each outgoing socket binds the well-known port it sends to. The plotter
identifies a stream by its source port and ignores traffic from an ephemeral
one, which is why `network::create_multicast_send()` is used rather than a
plain bind. The outgoing interface is pinned with `IP_MULTICAST_IF` rather than
left to the routing table, which on a host whose Garmin network is not its
default route would send the traffic out of the wrong interface.

Multicast loopback is left on, so a display on the same host sees the emulated
radar too — a second mayara, or an OpenCPN with radar_pi. That leaves mayara
free to discover the radar it is emulating itself, which `spawn()` prevents by
recording the address it announces from in `EMULATED_SOURCE`;
`locator::is_own_emulated_radar()` drops beacons from it. The filter is that
narrow on purpose: every local address would also cover a second mayara on the
same host, which shares them and is a display, not an echo of ourselves.

## Spoke conversion

`convert.rs` holds everything that differs between a source radar and an xHD.

**Angular resolution.** An xHD turns 1440 spokes per revolution; sources range
from 250 (Raymarine Quantum) to 8192 (Furuno). `SpokeStream` maps each source
spoke onto an xHD spoke index. Several source spokes landing on one index are
merged by taking the strongest sample of each pair, so downsampling a Furuno
does not throw away five spokes out of six. A source coarser than 1440 leaves
gaps, which are filled by repeating the spoke across the sector it covers —
capped at `MAX_REPEAT`, beyond which a gap means spokes went missing rather
than that the source is coarse.

Because the sector a spoke covers is only known once the next one arrives, the
stream always emits one spoke behind.

**Sample values.** mayara spokes carry legend indices, not echo strength: index
`n` means "the n-th colour of this radar's legend", and how many there are
depends on the radar. `intensity_table()` builds a 256-entry lookup from the
`Legend`, scaling the normal colours linearly onto the xHD's `0..=255`. The
Doppler bands above them have no equivalent on an xHD and become plain echoes:
approaching and receding at full strength, Furuno's rain class at the legend's
own notion of a medium return.

A radar's legend and spoke count are not final when it is discovered — a
Raymarine reports how many intensity levels it has only once it says which
model it is — so `spokes.rs` re-reads both from `SharedRadars` every few
seconds and calls `SpokeStream::reconfigure()`.

**The two range fields.** `range_meters` is the range the plotter displays and
is always an entry of the xHD range table. `display_meters` is the distance the
samples actually cover, which is what the plotter scales the image by. They are
not the same number on every radar: a Furuno keeps sweeping past the range it
was set to and delivers the overshoot, so its `spoke.range` is roughly 1.78× the
range in use. Passing `spoke.range` through as `display_meters` is what makes
the chart overlay line up, on any brand, without knowing which brand it is.

**Pacing.** Radars deliver spokes in bursts — a Furuno hands over some 1500
every 450 ms — while an xHD trickles them out one at a time. `spokes.rs`
buffers a burst and drains it over the time the burst took to arrive, so the
plotter sees a sweep that turns steadily. The pace is remeasured on every
burst; nothing has to know the antenna's rotation speed.

## Controls

`command.rs` translates a plotter command into a `ControlValue` and hands it to
`SharedControls::process_client_request()` — the same entry point the REST and
WebSocket API use, so the value is validated and unit-converted exactly as a
request from mayara's own GUI would be. The reply channel is drained and any
rejection logged.

The plotter waits for the radar to confirm a command before it shows the new
setting, so each accepted command is echoed back on the report stream. The echo
has to leave from port 50100, which the status task owns, so `command.rs` sends
it through a channel and `status.rs` forwards it immediately.

Range needs one thing more. Between the plotter's command and the radar acting
on it, the Range control still holds the old value, and a report stream that
told the truth would make the plotter's range ring snap back. `Shared` therefore
remembers the range the plotter asked for and reports it until the radar
confirms it or `RANGE_CONFIRM_TIMEOUT` passes — after which a command the radar
ignored stops being reported as though it had worked.

Rain clutter has no automatic mode on an xHD: it is off, or it is set to a
level. "Off" becomes a rain value of 0, and switching it on without a level
becomes 50%.

## What is not emulated

The `0x09B1` capability bitmap is what a display reads to decide which controls
to offer, so `EMULATED_CAPABILITIES` in `status.rs` lists exactly the features
the bridge translates — transmit, range, and the gain, sea and rain controls of
range A — and `GarminCapabilities::to_body()` serializes them. A bit claimed
there but ignored in `command.rs` would be a knob on the plotter that does
nothing, so the list is the honest one rather than a capture replayed verbatim.
For comparison, `capabilities::LEGACY_HD_BITS` is what a Garmin MFD hardcodes
for a legacy HD radar, which is likewise single-range and Doppler-less.

Left out, and why:

- **Dual range.** The bridge presents a single range whatever the source radar
  supports. The GMR xHD in the capture claims dual range despite having none —
  the MFD appears to set that whole capability word unconditionally — and a
  display that believes it offers a second range that stays black. mayara's own
  Garmin receiver registers a phantom Range B radar when told this.
- **Doppler.** An xHD has none; see the intensity mapping above.
- **Timed transmit, no-transmit zones, rotation speed, park position.** The
  source radar may well support these, but the bridge does not forward them.
- **Bearing alignment.** Deliberate: the source radar has already applied its
  own before it hands over a spoke, so the emulated radar reports itself as
  aligned and offers no control to change that.
