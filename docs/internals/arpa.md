# ARPA Target Tracking

The tracking system is a full ARPA (Automatic Radar Plotting Aid) implementation located in `src/lib/radar/target/`. It uses IMM (Interacting Multiple Model) Kalman filtering for motion estimation.

## Key Files

| File | Purpose |
|------|---------|
| `tracker.rs` | Core state machine, candidate matching, lifecycle |
| `blob.rs` | Blob detection from radar pixels, guard zone checks |
| `kalman.rs` | 4-state Extended Kalman Filter (lat, lon, vlat, vlon in meters) |
| `motion.rs` | IMM combining 3 Kalman filters (CV, CA, CT) |
| `manager.rs` | Multi-radar management, MARPA requests, Signal K broadcasting |
| `cpa.rs` | CPA/TCPA collision avoidance calculations |

## Pipeline

1. **Blob detection** -- each spoke is processed pixel-by-pixel; strong pixels are grouped into connected blobs (5-1000m, >= 25 pixels)
2. **Acquisition** -- blobs become `TargetCandidate`s via: guard zones (automatic), Doppler (if enabled), MARPA (manual click), or matching an existing target
3. **Association** -- candidates are matched to existing `ActiveTarget`s using physics-based distance: `max(50m, max_speed * dt * 1.5)`. Turn rejection (>130 deg) prevents false matches early in tracking
4. **Motion estimation** -- an IMM filter runs 3 concurrent Kalman filters: Constant Velocity (60%), Constant Acceleration (20%), Coordinated Turn (20%). After each measurement, Bayesian probability update selects the best model mix
5. **Lifecycle** -- targets go through `Acquiring` (< 4 updates) -> `Tracking` (>= 4, motion converged) -> `Lost` (3 revolutions without update, 10 for stationary). Duplicate young targets within 100m are merged
6. **CPA/TCPA** -- relative velocity approach computes closest point of approach and time to it; only reported when vessels are closing
7. **Signal K output** -- tracked targets are broadcast as deltas with position, bearing, distance, SOG, COG, CPA, TCPA

## Multi-Radar Modes

- **Merged** -- single shared tracker, global IDs (1-99M)
- **Per-radar** -- separate tracker per radar, IDs partitioned by radar index (N * 100M)
