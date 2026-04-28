# Mayara Server for radar_pi Developers

This document is for developers familiar with the `radar_pi` OpenCPN plugin (C++, `dualoverlay` branch) who want to understand the Mayara Server codebase (Rust). It maps concepts you already know to their Rust equivalents so you can explore the new codebase with confidence.

## High-Level Architecture

The overall architecture is remarkably similar. Both systems follow the same pipeline:

```
Network packets -> Receive thread -> Spoke processing -> Blob detection -> Target tracking -> Output
```

| Concept | radar_pi (C++) | Mayara Server (Rust) |
|---------|----------------|----------------------|
| Main entry point | `radar_pi` class | `src/main.rs` and `src/lib/mod.rs` |
| Per-radar state | `RadarInfo` | `RadarInfo` + `CommonRadar` |
| Vendor receive threads | `NavicoReceive`, `GarminHDReceive`, etc. | `src/lib/brand/navico/`, `src/lib/brand/garmin/`, etc. |
| Spoke processing | `RadarInfo::ProcessRadarSpoke()` | `CommonRadar::add_spoke()` in `src/lib/radar/mod.rs` |
| Guard zones | `GuardZone` class | Part of `BlobDetector` in `src/lib/radar/target/blob.rs` |
| Blob detection | `ArpaTarget::MultiPix()`, `GetContour()` | `BlobDetector::process_spoke()` in `src/lib/radar/target/blob.rs` |
| Target tracking | `Arpa` and `ArpaTarget` classes | `TargetTracker` and `ActiveTarget` in `src/lib/radar/target/tracker.rs` |
| Kalman filter | `KalmanFilter` class | `KalmanFilter` in `src/lib/radar/target/kalman.rs` |
| Output to host | NMEA `$RATTM` + `$AIVDM` sentences to OpenCPN | Signal K deltas over WebSocket |

### What is the same

- One receive thread per physical radar, decoding vendor-specific network protocols into generic spokes.
- Spokes are a polar representation: an angle (bearing), a range, and an array of pixel intensities.
- Guard zones define angular/radial wedges for automatic target acquisition.
- Blobs are groups of contiguous strong-return pixels.
- Target tracking uses an Extended Kalman Filter with a 4-element state vector: `[lat, lon, dlat/dt, dlon/dt]` in meters.
- Target lifecycle goes from acquisition through active tracking to lost.
- Multi-pass search strategy for matching blobs to existing targets.
- Turn rejection: both codebases reject matches that imply a turn greater than 130 degrees at speeds above 5 m/s during early acquisition.
- Multi-radar support: targets can be tracked across multiple radars.

### What is different

- **No GUI**. Mayara Server is a headless HTTP/WebSocket server. Rendering is handled by a separate web-based GUI.
- **Blob detection algorithm**. radar_pi uses Moore boundary tracing (contour following) to detect blobs one target at a time. Mayara uses a streaming union-find approach that processes all blobs in a spoke simultaneously.
- **Motion model**. radar_pi uses a single Extended Kalman Filter per target. Mayara uses IMM (Interacting Multiple Model) filtering with three concurrent Kalman filters per target: Constant Velocity, Constant Acceleration, and Coordinated Turn.
- **Coordinate system**. radar_pi works mostly in polar coordinates (spoke angle + pixel radius) and converts to geographic for reporting. Mayara converts to geographic (lat/lon in meters) at the blob stage and does all tracking in geographic coordinates.
- **Multi-radar tracking model**. radar_pi selects the best radar per target each refresh cycle (`CheckBestRadar()`), but each target lives in one `Arpa` instance. Mayara supports either a single merged `TargetTracker` across all radars or separate per-radar trackers.
- **Communication**. radar_pi sends NMEA `$RATTM` and `$AIVDM` sentences. Mayara broadcasts Signal K deltas with position, SOG, COG, CPA, and TCPA.

### File layout comparison

```
radar_pi/                               mayara-server/
  include/                                src/lib/
    radar_pi.h                              mod.rs (server setup)
    RadarInfo.h                             radar/mod.rs (RadarInfo, CommonRadar)
    Arpa.h                                  radar/target/tracker.rs
    Kalman.h                                radar/target/kalman.rs
    GuardZone.h                             radar/target/blob.rs
    RadarReceive.h                          brand/ (vendor modules)
  src/
    radar_pi.cpp                          src/main.rs
    RadarInfo.cpp                         src/lib/radar/mod.rs
    Arpa.cpp                              src/lib/radar/target/tracker.rs
    Kalman.cpp                            src/lib/radar/target/kalman.rs
    GuardZone.cpp                         src/lib/radar/target/blob.rs
    navico/NavicoReceive.cpp              src/lib/brand/navico/
    garminhd/GarminHDReceive.cpp          src/lib/brand/garmin/
```

---

## Deep Dive: Blob Detection

In radar_pi this is called "contour detection" or "ARPA target acquisition". In Mayara it is called "blob detection".

### How it works in radar_pi (C++)

Blob detection is split across `GuardZone.cpp` and `Arpa.cpp`. Raw spokes are stored in a history ring buffer (`m_history[bearing]`) by `RadarInfo::ProcessRadarSpoke()`, which encodes each pixel's intensity and Doppler state into bit flags:

- Bit 7 (0x80): pixel is above threshold (strong return)
- Bit 6 (0x40): backup bit
- Bit 5 (0x20): Doppler approaching
- Bit 4 (0x10): Doppler receding

The `Pix(angle, radius)` helper reads a pixel from this history buffer. The Doppler-aware version interprets the bit flags based on the target's Doppler classification (ANY, APPROACHING, RECEDING, ANY_DOPPLER, etc.).

**Guard zone search**: `GuardZone::SearchTargets()` (line 159) is multi-radar aware. It iterates over all transmitting radars, converts the guard zone's range/bearing to spoke/pixel coordinates for each radar, and scans every 2nd spoke within the zone's angular/radial window. When it finds a strong pixel, it calls `MultiPix()` to validate the blob.

**MultiPix()** (`Arpa.cpp:1168`) uses **Moore boundary tracing** (contour following). Starting from a pixel on the blob edge, it walks clockwise around the contour using a 4-directional lookup table:

```cpp
Polar transl[4] = {{0,1}, {1,0}, {0,-1}, {-1,0}};  // right, down, left, up
```

It counts the perimeter length and returns `true` only if the contour is long enough (`m_min_contour_length`). If the blob is too small, it clears the pixels (`line[r] &= 63`) to prevent re-detection. This is a per-pixel, per-target operation: you start from one strong pixel and trace its blob boundary.

**GetContour()** (`Arpa.cpp:229`) is the full contour extraction. It stores all contour pixels in `m_contour[]` (up to 400) and computes the blob centroid as the midpoint of the angular and radial bounds:

```cpp
pol->angle = (m_max_angle.angle + m_min_angle.angle) / 2;
pol->r = (m_max_r.r + m_min_r.r) / 2;
```

**FindContourFromInside()** (`Arpa.cpp:192`) and **FindNearestContour()** (`Arpa.cpp:863`) are the two ways to locate a blob when refreshing a tracked target: either the predicted position is inside the blob and you walk outward to find an edge, or it is outside and you search in an expanding square pattern.

**Doppler target search**: `SearchDopplerTargets()` (`Arpa.cpp:1539`) scans the full rotation for pixels with ANY_DOPPLER that are not already part of a known target.

Key characteristics:
- Operates in polar coordinates (angle, radius in pixels).
- Processes one blob at a time, triggered by finding a single strong pixel.
- History buffer stores processed spokes in `m_history[bearing]`, a ring buffer indexed by spoke angle.
- The `Pix(angle, radius)` helper reads pixels from this history buffer.
- After processing a target, its pixels are cleared (`ResetPixels()`) to prevent duplicate detection.

### How it works in Mayara (Rust)

Blob detection lives entirely in `src/lib/radar/target/blob.rs`, in the `BlobDetector` struct.

**Entry point**: `BlobDetector::process_spoke()` (line 447). Unlike radar_pi which searches for blobs on demand from the history buffer, this method is called for every incoming spoke and detects all blobs as a streaming operation. There is no history buffer to look back at; blobs are assembled incrementally as spokes arrive.

The algorithm uses **union-find with a spatial index**:

1. Each pixel in the spoke above the intensity threshold is checked against a `pixel_index: HashMap<(spoke, pixel), blob_id>` for 8-neighbor adjacency to existing blobs.
2. If a pixel touches no existing blob, a new `BlobInProgress` is created.
3. If it touches one blob, it is added to that blob.
4. If it touches two or more blobs, they are merged (union operation) into the lowest ID.
5. A blob is completed when a full spoke passes without adding any pixels to it (the antenna has swept past).

**Size filtering** happens at completion. A blob must have at least 25 pixels (`MIN_TARGET_PIXELS`) and its physical size must be between 5m and 1000m. Valid blobs become a `CompletedBlob` struct:

```rust
pub struct CompletedBlob {
    pub contour: Vec<(u16, usize)>,        // edge pixels (spoke, pixel_index)
    pub center_spoke: u16,                 // angular centroid
    pub center_pixel: usize,               // radial centroid
    pub size_meters: f64,
    pub in_guard_zones: Vec<u8>,           // which guard zones contain this blob
    pub has_doppler_approaching: bool,
}
```

Guard zones are checked inline during `process_spoke()` rather than in a separate class. The `BlobDetector` holds a `Vec<GuardZoneInternal>` that defines the zones in spoke/pixel coordinates. When a completed blob's center falls inside a zone, that zone ID is added to `in_guard_zones`.

**Key differences in approach**:

| Aspect | radar_pi | Mayara |
|--------|----------|--------|
| Algorithm | Moore boundary tracing (contour following) | Streaming union-find (connected components) |
| When it runs | On demand, when searching for a specific target | Continuously, processing every spoke |
| Scope | Finds one blob at a time | Finds all blobs simultaneously |
| Pixel cleanup | Clears detected pixels with `ResetPixels()` | Not needed (pixels are consumed once) |
| Coordinate system | Polar (spoke angle, pixel radius) | Polar at detection, converted to geographic for output |
| Data source | History ring buffer (`m_history[]`) | Live spoke stream (no history buffer) |
| Centroid | Midpoint of bounding box | Average of all pixels |
| Guard zones | Separate `GuardZone` class searches per radar | Inline check during blob completion |
| Doppler search | Separate `SearchDopplerTargets()` loop | Doppler flag set on blob during detection |

---

## Deep Dive: Target Tracking

In radar_pi this is "ARPA/MARPA tracking". In Mayara it is simply "target tracking".

### How it works in radar_pi (C++)

Target tracking is implemented in the `Arpa` class (manager) and `ArpaTarget` class (single target), both in `Arpa.cpp`.

**Refresh cycle**: `Arpa::RefreshAllArpaTargets()` (line 1447) is called every 500ms from `radar_pi::TimedUpdate()`. It runs a **three-pass strategy** over all targets, sorted by status (highest first):

- **Pass 0**: Moving targets only (speed >= 2.5 knots), search speed = target_speed / 4
- **Pass 1**: All targets, search speed = target_speed / 3
- **Pass 2** (LAST_PASS): All targets, full search speed (20 m/s)

Each pass calls `ArpaTarget::RefreshTarget(speed, pass)` (line 449), which runs this cycle:

1. **Timing check** (line 359): Has the antenna beam swept past this target's position since the last update? Uses `m_history[angle].time` to verify the beam has passed.
2. **Best radar selection**: `CheckBestRadar()` (line 427) picks the transmitting radar with the smallest range that still covers the target. This is a `dualoverlay` feature: targets can migrate between radars.
3. **Predict**: `m_kalman.Predict(&predicted_local, delta_t)` projects position forward. Delta time accounts for missed sweeps: `rotation_period * (m_lost_count + 1)`.
4. **Search**: `GetTarget()` (line 897) looks for a blob near the predicted position. Search radius is computed as `speed * rotation_period * pixels_per_meter / 1000`, and varies by pass. If the predicted position is inside a blob, it uses `FindContourFromInside()`. Otherwise it uses `FindNearestContour()` with an expanding square search.
5. **Validate contour**: Rejects blobs that are too large (>= 400 pixels, likely land), and rejects blobs whose contour length changed by more than 3x compared to the running average (likely a different object or interference).
6. **Kalman update**: `m_kalman.SetMeasurement()` feeds the measured polar position to the filter.
7. **Fast target bypass** (line 613): For small, fast targets in early acquisition (status == 2), if the target moved more than its own size, the Kalman is overridden with a direct position measurement (decaying with a factor of 0.8 per status level).
8. **Compute speed/course** (line 663): From Kalman velocity state `dlat_dt`, `dlon_dt`. Speed is validated against MAX_DETECTION_SPEED * 1.5 (30 m/s). Turn rejection kicks in for targets with speed > 5 m/s and turn > 130 degrees when status < 5.
9. **Report**: `PassTTMtoOCPN()` (line 950) sends a `$RATTM` NMEA sentence. Optionally, `EncodeAIVDM()` (line 1043) sends a synthetic AIS Type 1 message.

**State machine** uses numeric status codes:

```
ACQUIRE0 (0) -> ACQUIRE1 (1) -> ACQUIRE2 (2) -> ACQUIRE3 (3) -> Q_NUM (4) -> ... -> STATUS_TO_OCPN (6) -> T_NUM (8) -> ...
LOST (-1)
```

A target needs 4+ successful updates to become active. Targets are reported to OpenCPN when status reaches `STATUS_TO_OCPN` (6): as "Q" (acquiring) when status < T_NUM (8), and "T" (tracked) from T_NUM onwards. Lost targets are marked after `MAX_LOST_COUNT` (12) missed scans.

**Pixel clearing**: After each successful target refresh, `ResetPixels()` (line 1126) clears the blob's pixels from the history buffer to prevent other targets from detecting the same blob. For large targets near the radar (contour >= 80 pixels, range < 3km), it also clears a "shadow" region behind the target.

**Doppler state transitions**: `StateTransition()` (line 804) counts Doppler pixels within the contour using `PixelCounter()` (line 759) and transitions the target's Doppler classification if > 85% of pixels are approaching or receding (or back to ANY if < 80%).

**Kalman filter** (`Kalman.cpp`): A single 4-state EKF per target with state `[lat, lon, dlat/dt, dlon/dt]` in meters from own ship. The measurement model is nonlinear (polar angle and radius from Cartesian position), requiring a Jacobian `H` computed from the partial derivatives of the polar-to-Cartesian conversion. Process noise `NOISE = 0.015` and measurement noise `R = diag(100, 25)` are fixed constants.

### How it works in Mayara (Rust)

Target tracking is spread across several files in `src/lib/radar/target/`:

| File | Equivalent in C++ |
|------|-------------------|
| `manager.rs` | Closest to `Arpa` class (manager role) |
| `tracker.rs` | Core tracking logic, combines `Arpa` + `ArpaTarget` roles |
| `kalman.rs` | `KalmanFilter` class |
| `motion.rs` | No direct equivalent (IMM is new) |

**TrackerManager** (`manager.rs`) receives `CompletedBlob` messages over a channel, converts them to `TargetCandidate` structs in geographic coordinates, and dispatches them to the appropriate `TargetTracker`. It also handles Signal K broadcasting -- immediate broadcast on target promotion, batched updates once per revolution.

**TargetTracker** (`tracker.rs`) is the core state machine. Instead of the timer-driven three-pass sweep model, it processes candidates as they arrive from the blob detector:

`process_candidate()` (line 549):
1. Calls `match_active_target()` (line 613) to find the best existing target for this candidate.
2. **Matching** uses physics-based distance: the maximum matching distance is `max(50m, max_target_speed * delta_time * 1.5)`. This replaces the pixel-radius search of radar_pi with a speed-aware geographic distance check.
3. If matched, the target's motion model is updated. If not matched and the candidate comes from a guard zone or Doppler, a new target is created.

`check_revolution()` runs once per full antenna rotation and handles:
- **Timeouts**: Targets not seen for 3 revolutions become Lost (10 for stationary targets).
- **Deduplication**: `deduplicate_targets()` (line 425) merges young targets (< 4 updates) within 100m of each other. Large vessels often produce multiple blobs per revolution; this prevents tracking the same ship as two targets.

**State machine** uses a clean enum instead of numeric codes:

```rust
enum TargetStatus {
    Acquiring,   // < 4 updates
    Tracking,    // >= 4 updates, motion converged
    Lost,        // missed timeout revolutions
}
```

Promotion from `Acquiring` to `Tracking` happens after 4 updates (same as radar_pi's `ACQUIRE3 -> active` transition).

**Turn rejection** (lines 194-233 in `tracker.rs`) mirrors radar_pi: during early tracking (updates 2-4), if a candidate would imply a course change of more than 130 degrees and the target is moving faster than 5 m/s, the match is rejected.

**IMM motion model** (`motion.rs`): Instead of a single Kalman filter, each target runs three filters in parallel through `ImmMotionModel`:

| Model | Process Noise | Use |
|-------|---------------|-----|
| Constant Velocity (CV) | 0.01 | Straight-line motion |
| Constant Acceleration (CA) | 0.05 | Speeding up or slowing down |
| Coordinated Turn (CT) | 0.15 | Maneuvering |

After each measurement, a Bayesian probability update determines which model best explains the observed motion. The three predictions are combined as a weighted average. This means Mayara can track turning ships more accurately than radar_pi's single constant-velocity filter.

The transition probability matrix governs how likely each model is to switch to another:

```
From\To     CV    CA    CT
CV        [0.90, 0.05, 0.05]
CA        [0.10, 0.80, 0.10]
CT        [0.05, 0.15, 0.80]
```

**Kalman filter** (`kalman.rs`): Similar 4-state EKF, but operates entirely in geographic coordinates (meters of latitude and longitude) rather than polar. The measurement is a direct position observation (lat/lon from the blob centroid), so the measurement model is linear (H is a 2x4 selection matrix) instead of the nonlinear polar-to-Cartesian mapping in radar_pi. This simplifies the filter and avoids the Jacobian computation.

### Key differences summary

| Aspect | radar_pi | Mayara |
|--------|----------|--------|
| Refresh model | Timer-driven (500ms), 3 passes per cycle | Event-driven, per blob as detected |
| Target search | Pixel-radius search, pass-dependent | Physics-based distance threshold |
| Motion model | Single EKF (constant velocity only) | IMM with 3 EKFs (CV, CA, CT) |
| Coordinate system | Polar throughout, geographic for output | Geographic throughout |
| Measurement model | Nonlinear (polar), requires Jacobian | Linear (geographic position) |
| Fast target handling | Direct position override at status 2 | Handled by CA/CT motion models |
| Contour validation | Size consistency check (1/3 to 3x avg) | Size filtering at blob detection stage |
| Pixel cleanup | `ResetPixels()` clears history buffer | Not needed (streaming detection) |
| Doppler transitions | `StateTransition()` tracks per-target doppler | Doppler flag per blob, no state machine |
| Deduplication | Pixel clearing prevents double detection | Explicit merge of young targets within 100m |
| Lost timeout | 12 missed scans (`MAX_LOST_COUNT`) | 3 revolutions (10 for stationary) |
| Multi-radar | Per-target best-radar selection each refresh | Merged single tracker or per-radar trackers |
| Output | NMEA `$RATTM` + synthetic `$AIVDM` | Signal K deltas (JSON over WebSocket) |

### Method mapping

For quick cross-referencing when reading the code:

| radar_pi method | Mayara equivalent |
|-----------------|-------------------|
| `Arpa::RefreshAllArpaTargets()` | `TrackerManager::process_blob()` in `manager.rs` |
| `Arpa::AcquireNewARPATarget()` | `TargetTracker::process_candidate()` in `tracker.rs` |
| `ArpaTarget::RefreshTarget()` | `TargetTracker::process_candidate()` + `match_active_target()` |
| `ArpaTarget::CheckBestRadar()` | Handled by `TrackerManager` dispatch (merged vs per-radar mode) |
| `ArpaTarget::MultiPix()` | `BlobDetector::process_spoke()` in `blob.rs` |
| `ArpaTarget::GetContour()` | Part of `BlobDetector::process_spoke()` (contour built incrementally) |
| `ArpaTarget::FindNearestContour()` | `TargetTracker::match_active_target()` (distance-based, not pixel-search) |
| `ArpaTarget::GetTarget()` | No equivalent (blob detection is decoupled from tracking) |
| `ArpaTarget::ResetPixels()` | Not needed (streaming blob detection consumes pixels once) |
| `ArpaTarget::StateTransition()` | Doppler flag on `CompletedBlob`, no per-target state machine |
| `GuardZone::SearchTargets()` | Inline in `BlobDetector::process_spoke()` |
| `KalmanFilter::Predict()` | `KalmanFilter::predict()` in `kalman.rs` |
| `KalmanFilter::SetMeasurement()` | `KalmanFilter::update()` in `kalman.rs` |
| `ArpaTarget::Polar2Pos()` | Conversion happens in `TrackerManager::process_blob()` |
| `ArpaTarget::PassTTMtoOCPN()` | Signal K broadcasting in `TrackerManager` |
