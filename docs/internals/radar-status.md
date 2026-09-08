# Radar status model (design intent)

This is **design guidance** for the in-flight radar-status work, not a description of
finished code. Anchor any change against the real code on `main` — references below point
to functions and types by name (not line numbers), so use your editor's symbol search.

Some of this is already partially realised: #346 added Navico error surfacing and a `Fault`
power state, and #347 added the GUI for radar notifications and that fault state. This
document captures the broader model those PRs are converging on.

## Surface a radar early, never silently drop it

A radar should appear on `/radars` as soon as it is **discovered**, not only once its model
is recognised and its ranges have arrived. Today `get_active()` in `src/lib/radar/mod.rs`
filters on `ranges.len() > 0`, so a radar whose model ID isn't recognised — or whose
capability report hasn't landed yet — **vanishes** from `/radars` even though `/radars/<id>`
still answers. Operators report this as "the radar responds on `/radars/xxxx` but isn't in
the list."

The cure is not to special-case the vanish but to **report the radar with a status that
explains why it isn't fully usable yet.** (The internals README's lifecycle note — "the radar
becomes visible in the API once ranges are set" — describes the behavior this model changes.)

## Two orthogonal axes — do not conflate them

A radar carries two independent pieces of state. Modelling them as one field is the mistake
to avoid.

### 1. Lifecycle / health status — what the radar *is*

Proposed shape (exact `Error` variants left open — they grow as hardware faults are decoded):

```text
Initializing { Locating, ModelDetecting } | Available | Error { HardwareError, SetupError, … }
```

Every non-`Available` state carries a human-readable English explanation the GUI can show
directly — few users read the log, so the *why* rides along with the status. Anchor in cases
already in the code:

- Raymarine self-test fault `0x0A` — `STATUS_FAULT_SELF_TEST` in
  `src/lib/brand/raymarine/report/quantum.rs` (#335) → `Error { HardwareError }`, e.g.
  "self-test failure; no image will appear until it clears."
- Model not recognised / ranges not yet arrived → `Initializing { ModelDetecting }`, **not**
  a disappearance.

Kees has catalogued ~20 distinct Navico hardware-error codes, so `Error` is expected to fan
out — #346 began surfacing these as a `Fault` power state.

The "radar announces no data stream" case — a Quantum advertising report address
`0.0.0.0:0` behind a Raymarine MFD, so spokes only arrive unicast on the command socket
(`src/lib/brand/raymarine/mod.rs`, the `0.0.0.0:0` beacon handling) — is the **specific
case** this general status model subsumes. #334 proposed surfacing it as a standalone flag
but was closed unmerged; the right home is the status enum (an `Error { SetupError }` with an
explanation), not a parallel per-failure boolean.

### 2. Idle — whether *anyone is watching*

A radar is **idle** when it is powered but **no one is subscribed** to its data — no point
decoding spokes nobody consumes. Idle is **not** an error and must never be reported as one —
it's a CPU-saving signal: the data loop drains the spoke socket but skips frame decode and
blob detection while idle (~1.5 cores on Furuno radars that emit spokes even in Standby,
#274).

The current predicate is narrow — `should_idle(power, receiver_count)` in
`src/lib/radar/mod.rs` is `standby && receiver_count == 0`. Its exact semantics are pinned by
the `should_idle_*` unit tests in the same module: idle when standby + no subscribers; **not**
when transmitting even with no subscribers; not when subscribers present; not when power is
unknown.

Widening idle to cover the transmit-but-unwatched case is possible, but only once the
subscriber count counts ARPA (below).

### 3. Stand-down — letting an *unwatched* radar go (issue #633)

A headless mayara must not hold a radar transmitting for an audience of nobody: that ages
the magnetron and burns power. So when nobody has watched a radar for the period set by its
**Auto standby** control (Off / 1 / 5 / 15 / 30 min, default 1 min; category Installation)
the brand receiver stops holding it up and lets the radar decide for itself. Stand-down is
independent of idle and of the radar's power state; it is a *policy* for the receiver, not
a state of the radar.

"Watched" is deliberately narrow: a subscriber on the spoke broadcast (`message_tx`) — the
GUI or any spoke WebSocket client, the `--output` stdout forwarder, or a running recording.
Control PUTs, REST reads and the control WebSocket do **not** count. Neither does ARPA
tracking or an armed guard zone: a radar nobody is looking at stands down even while it is
tracking. That is an accepted consequence, not an oversight — the radar is meant to come
back only when a client asks for it.

The ranges of a dual-range radar share one antenna, so the decision is made **per
antenna** and written to every range: the antenna stands down only when *every* range has
auto standby enabled and has been unwatched for its own period. A range set to Off holds
the whole antenna up. The 5 s radar watchdog computes this (`SharedRadars::refresh_stand_down`
in `src/lib/radar/mod.rs`; the pure predicate `antenna_should_stand_down` and its tests pin
the rule) and each brand receiver reads `RadarInfo::stand_down()` on its own tick.

The control is offered only by brands whose receiver honours it (`new_auto_standby()` in
the brand's `settings.rs`); on other brands `SharedControls::auto_standby()` is `None` and
the radar never stands down. Per brand:

- **Navico**: the radar is held up by the stay-alive ping mayara sends with its periodic
  state queries. While standing down the ping is left out; the queries continue so the
  radar's state keeps arriving. No standby command is sent — the ping is a per-client
  watchdog, so an MFD that is also using the radar keeps it up with its own, and mayara
  needs no "am I the only controller" check.
  Once a client reconnects the ping resumes, but the radar stays in Standby until that
  client asks for Transmit. Measured on a HALO24: the radar leaves Transmit about 25 s
  after the last ping, so with the 1 min default the antenna goes quiet roughly 85 s after
  the last viewer disconnects.
- **Raymarine**: the radar drops a controller it has not heard from for about a minute, so
  the 1 s heartbeat (and the 5 s extended one) is left out while standing down. An MFD using
  the radar sends its own heartbeat and keeps it up. Whether every model then leaves
  Transmit by itself is still to be confirmed on hardware (#664).
- **Furuno**: the radar is not held up by anything mayara sends: it keeps transmitting until
  some client tells it to stop, and the firmware has no notion of standing down when a
  client goes away (see `research/furuno/drs4d-nxt-firmware.md`). So the receiver remembers
  whether the radar is transmitting because a client asked for it through mayara (both
  ranges share one transmitter, so this is one fact for the antenna), and while standing
  down sends a Standby request itself. A transmit an MFD started is never touched: the
  memory is set only by a Transmit request that reached the radar through mayara and
  cleared by a Standby request through mayara or by the radar reporting anything but
  Transmit.
- **Koden**: the radar answers a keep-alive mayara sends every 10 s; it is left out while
  standing down. Whether the radar then leaves Transmit by itself is still to be confirmed
  on hardware (#665).
- **Garmin**: nothing mayara sends holds the radar up either: its firmware drops a silent CDM
  peer after 30 s and then only stops broadcasting spokes, the transmitter keeps running (see
  `research/garmin/gmr-xhd-firmware.md`). So the receiver keeps the same claim as Furuno,
  shared by both ranges of a dual-range scanner, and while standing down sends a Standby
  itself; the claim ends when the radar reports Standby or Off, so a request the radar ignored
  is repeated on the next tick. mayara keeps sending its CDM heartbeat so the radar keeps
  reporting and discovery keeps working.

## ARPA counts as a subscriber — for idle

A radar with an active ARPA / MARPA tracker is **not idle**, even if no GUI is open. ARPA is
a legitimate consumer of the spoke stream — if it's tracking targets, something (autopilot,
guard zone, plotter) cares about that radar. For the *idle* predicate, do not treat "no
WebSocket viewers" as "nobody is watching." (Stand-down, above, deliberately does.)

### ⚠️ ARPA / idle gotcha — be careful here

Idle is computed **only** from `message_tx.receiver_count()` (in `RadarInfo::refresh_idle_flag`,
`src/lib/radar/mod.rs`), the spoke-broadcast WebSocket subscriber count. But **ARPA does not
subscribe to that broadcast** — the data loop feeds its `BlobDetector` and pushes detected
blobs over a *separate* mpsc channel (`blob_tx.try_send` in the spoke-processing loop),
consumed by the target tracker. And the idle flag is exactly what **skips blob detection**.

⇒ If ARPA is tracking targets while no GUI is connected, `receiver_count == 0` → the radar
idles → blob detection stops → **ARPA silently dies.** Any idle predicate must fold in an
active-tracker signal (not just `receiver_count`) so an ARPA-tracked radar stays awake. Get
this wrong and ARPA breaks invisibly the moment the last viewer closes the GUI.

A guard comment lives at the `should_idle` site so an implementer widening the predicate
can't miss this.
