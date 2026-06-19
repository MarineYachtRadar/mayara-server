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

## ARPA counts as a subscriber

A radar with an active ARPA / MARPA tracker is **not idle**, even if no GUI is open. ARPA is
a legitimate consumer of the spoke stream — if it's tracking targets, something (autopilot,
guard zone, plotter) cares about that radar. Do not treat "no WebSocket viewers" as "nobody
is watching."

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
