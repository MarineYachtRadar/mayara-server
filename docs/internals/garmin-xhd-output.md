# Garmin xHD Output Bridge — Internals

This document describes the implementation of the `garmin-xhd-output` feature
in `src/lib/output/garmin_xhd/`. For end-user setup and protocol overview see
[docs/garmin-xhd-output.md](../garmin-xhd-output.md).

## Architecture

The output bridge is an **output brand** — the inverse of the input brands in
`src/lib/brand/`. Where an input brand receives proprietary UDP packets and
feeds them into the `RadarInfo` / `CommonRadar` abstraction, an output brand
subscribes to that abstraction and re-encodes the data in a different protocol.

```
Input side (any brand)          Output side (garmin-xhd-output)
──────────────────────          ────────────────────────────────
FurunoReportReceiver            spokes.rs
  CommonRadar::add_spoke()  ──► message_tx.subscribe()
    broadcast_radar_message()       to_xhd_spoke()
                                    UDP → 239.254.2.0:50102

SharedControls (Power/Range/…)  status.rs
                            ──► build_status_packets()
                                    UDP → 239.254.2.0:50100

                                command.rs
Plotter UDP → nic_ip:50101  ──► send_to_command_handler()
                                    → ControlUpdate channel
                                    → FurunoCommand::set_control()
```

The feature compiles only when `--features garmin-xhd-output` is passed. The
feature flag also enables `furuno` (listed as a dependency in `Cargo.toml`).

## Module Structure

```
src/lib/output/
  mod.rs                  re-exports garmin_xhd
  garmin_xhd/
    mod.rs                SharedState, detect_garmin_ip(), spawn()
    convert.rs            to_xhd_spoke(), nearest_xhd_range(), unit tests
    cdm.rs                CDM V2 heartbeat task
    status.rs             Status/report stream task
    spokes.rs             Spoke buffer and UDP sender task
    command.rs            Plotter command listener task
```

`src/lib/mod.rs` conditionally includes the output module:

```rust
#[cfg(feature = "garmin-xhd-output")]
pub(crate) mod output;
```

`src/lib/brand/furuno/mod.rs` calls `spawn()` in `found()` immediately after
the radar is registered:

```rust
#[cfg(feature = "garmin-xhd-output")]
crate::output::garmin_xhd::spawn(&info, subsys);
```

## Shared State

`SharedState` (defined in `mod.rs`) is an `Arc<Mutex<SharedState>>` shared
between the status, spoke sender, and command tasks (CDM does not use it):

```rust
pub(super) struct SharedState {
    pub range_m: u32,            // snapped to nearest xHD table entry
    pub range_lock_until: Instant,
    pub transmitting: bool,      // fallback when Power control not yet set
    pub rain_mode: u8,           // 0=off, 1=on (cached, not read from Controls)
    pub rain_gain: u16,          // percent×100 (cached)
}
```

**Why rain is cached**: Mayara maps "rain off" to `auto=true` on the
`ControlId::Rain` control while leaving `value` unchanged. If `status.rs` read
rain state from `SharedControls`, it would report the old gain value after the
user turned rain off. The cache ensures `rain_mode=0, rain_gain=0` is reported
immediately.

**Why transmitting is a fallback**: `status.rs` reads `ControlId::Power` from
`SharedControls` directly (the live value updated by Furuno report parsing).
`SharedState.transmitting` is only consulted when `Power` has no value yet
(before the first Furuno report). It defaults to `false` (standby).

## IP Detection

`detect_garmin_ip(nic_addr)` (in `mod.rs`) finds the right local IP to bind
output sockets to:

1. Enumerate all NICs via `network_interface::NetworkInterface::show()`
2. Find the NIC that has `nic_addr` (the Furuno-side address from `RadarInfo`)
3. Return the first address on that NIC in `172.16.0.0/12`
4. Fallback: return the first `172.16.0.0/12` address on any NIC

Step 2 is essential on hosts that run Docker: Docker creates bridge interfaces
(`172.17.0.1`–`172.20.0.1`) that fall inside `172.16.0.0/12`. Without the
NIC-pinning step, the bridge would bind to a Docker bridge address and the
Garmin plotter would not receive any packets.

## Spoke Buffering (`spokes.rs`)

The input radar delivers spokes in batches. Without pacing, `spokes.rs` would
send an entire batch in a burst and then go silent until the next batch arrives.
During that silence the Garmin plotter continues sweeping and renders the gap as
missing sectors.

**Solution — jitter buffer:** incoming batches are pre-converted and pushed into
a `VecDeque`. A drain loop sends one packet per `SPOKE_INTERVAL`, regardless of
when the next batch arrives:

```text
Input batch                          Plotter
  arrives every ~N ms                receives one spoke every SPOKE_INTERVAL
        │                                  │
  ──────┤ push_back ──► VecDeque ──► pop_front ──► UDP
        │                                  │
  next batch arrives, refills queue        │
```

The drain rate is chosen so the queue empties slightly faster than the input
batch cadence, keeping display lag low while ensuring the next batch arrives
before the queue runs empty. See `SPOKE_INTERVAL` in `spokes.rs` for the
current value and `QUEUE_MAX` for the overflow cap.

## Spoke Conversion (`convert.rs`)

`to_xhd_spoke()` converts one source spoke to an xHD `0x0998` UDP payload.

### Critical constraints

The Garmin plotter crashes or freezes if these fields are wrong:

| Field | Offset | Required value |
|-------|--------|----------------|
| `fill_1` | byte 8 | `1` (not 0) |
| `fills_4` | byte 28 | `0x0108` |
| angle | byte 12 | 0..11512, multiple of 8 |

The angle constraint is enforced by:

```rust
let raw_angle = (src_angle as u64 * 11520 / spokes_per_rev as u64) % 11520;
let xhd_angle = ((raw_angle / 8) * 8) as u16;
```

The `% 11520` keeps the value in `[0, 11519]`; the `/8*8` snaps it to a
multiple of 8, giving `[0, 11512]` (since 11512 = 1439×8).

### Two range fields

The spoke header carries two separate range values:

- **`range_meters`** (byte 16): nearest entry from the 16-value xHD range table.
  The plotter uses this for its range UI display.
- **`display_meters`** (byte 20): raw `spoke.range` from the source radar,
  passed through unmodified. The plotter uses this to scale the chart overlay
  image.

Both fields must be set correctly for the image to be the right size on the
chart. Using the snapped value for `display_meters` causes the image to be
slightly wrong at non-table ranges.

### Furuno range ratio

Furuno's internal `spoke.range` is approximately 1.7822× the xHD range value
set by the user. The constant was derived empirically:
- set=5556 m → spoke.range=9902
- set=7408 m → spoke.range=13202

9902/5556 ≈ 13202/7408 ≈ 1.7822.

The bridge divides by this factor to get the actual display range before
snapping to the range table. `display_meters` receives the raw value. Other
brands (`_`) deliver correct meters and need no correction.

## Subsystem Lifecycle

`spawn()` creates four `tokio-graceful-shutdown` subsystems. Each subsystem owns
a `oneshot::Sender<()>` stop channel:

```rust
let (cdm_stop_tx, cdm_stop_rx) = oneshot::channel::<()>();
subsys.start(SubsystemBuilder::new(
    format!("{key}/GarminXhd/CDM"),
    async move |s: &mut SubsystemHandle| {
        tokio::select! {
            biased;
            _ = s.on_shutdown_requested() => { let _ = cdm_stop_tx.send(()); }
            _ = cdm::run(local_ip, cdm_stop_rx) => {}
        }
        Ok::<(), miette::Report>(())
    },
));
```

The `biased` select ensures the shutdown branch is checked first. If the inner
task finishes on its own (e.g., socket bind failure), the subsystem exits
cleanly without sending the stop signal — the `Receiver` drop is sufficient.

## Range Lock

When the plotter sends `MSG_RANGE_A`, `command.rs` sets:

```rust
st.range_lock_until = Instant::now() + Duration::from_secs(5);
```

`spokes.rs` checks this before updating `range_m`:

```rust
if now >= st.range_lock_until {
    st.range_m = nearest_xhd_range(display_range);
}
```

Without the lock, the first spoke arriving from the radar (still carrying the
old range) would immediately overwrite the range that the plotter just set,
causing the plotter to display the wrong range momentarily and potentially
re-sending another range command.

## Command Routing

Plotter commands are forwarded to the physical radar via:

```rust
controls.send_to_command_handler(cv, reply_tx)
```

This inserts a `ControlUpdate` into the `broadcast::Sender<ControlUpdate>`
channel that `CommonRadar` monitors. `CommonRadar` calls
`CommandSender::set_control()` on the brand implementation (e.g.,
`FurunoCommand::set_control()`), which sends the proprietary wire command to
the radar.

The reply channel `reply_tx` is created as a throwaway `mpsc::channel(1)`;
the reply is not used. The plotter does not wait for a round-trip
acknowledgement — it considers the command sent when it receives the echo on
the status multicast.

## Protocol Constants

All `MSG_*`, `STATE_*`, and address constants are reused from
`src/lib/brand/garmin/protocol.rs` (changed from `mod` to `pub(crate) mod` to
allow access from the output module). No constants are duplicated.
