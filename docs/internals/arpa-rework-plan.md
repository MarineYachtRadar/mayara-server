# ARPA Rework Plan

This is a discussion document, not a committed roadmap. Lives on the `arpa-rework-plan` branch so it can be shared, annotated, then deleted once decisions are captured in tracked issues / PRs.

It catalogs concrete issues found during a top-to-bottom read of `src/lib/radar/target/` and `src/lib/radar/cpa.rs`, ordered by impact. Each item lists the symptom, the file/line, what's wrong, and what a fix would look like.

The current code works in the common case -- 56 tests pass, the emulator and a live Furuno track normally. These are improvements, not blockers.

## Summary

| # | Area | Severity | Touch size | Risk |
|---|------|----------|------------|------|
| 1 | IMM mixing step is a no-op | Medium (correctness) | ~80 lines + tests | Medium |
| 2 | CA and CT filters have CV dynamics | Medium (correctness) | ~150-300 lines + tests | High |
| 3 | `predict()` averages drifted filter states | Falls out of #1 | small once #1 done | Low |
| 4 | Turn rejection only runs in updates 2-5 | Medium (safety) | ~30 lines + tests | Low |
| 5 | Kalman gain falls back to identity silently | Low | ~10 lines | Low |
| 6 | `force_state` is dead code | Low (cleanup) | -50 lines or +callers | Low |
| 7 | Stationary threshold catches drifting targets | Low | tuning + 1 test | Low |
| 8 | Duplicate merge can drop close real vessels | Low | ~30 lines + tests | Low |
| 9 | CPA omitted once vessels diverge | Medium (UX) | ~40 lines + tests | Low |
| 10 | CPA uses raw position, not filtered state | Medium (UX) | ~5 lines + test | Low |
| 11 | No emission cadence control | Low (perf) | design needed | Low |
| 12 | Lost-target bearing in merged mode is ambiguous | Low | design needed | Low |
| 13 | AIS / ARPA correlation | Feature gap | major | High |

Plus a section on magic numbers worth justifying.

## Recommended ordering

If we tackle this incrementally (one logical change per PR per AGENTS.md), here's the order I'd suggest:

1. **#10 + #9** (CPA quality) -- small, user-visible, low risk. Good warm-up PR.
2. **#5** (Kalman gain fallback) -- one-liner with a log, no behaviour change in normal cases.
3. **#4** (turn rejection range) -- prevents real-world false matches on long-lived tracks.
4. **#1** (real IMM mixing) -- correctness improvement, contained to `motion.rs`.
5. **#2** (real CA/CT models) -- biggest change. Only worth doing if the test rig shows current IMM is the limiter. Should be benchmarked before committing.
6. **#6, #7, #8** (cleanups / edge cases) as separate small PRs.
7. **#11, #12, #13** -- design discussion, not immediate code.

Items 1-4 could land in a week of work and would meaningfully tighten the tracker without touching the IMM architecture. Items 5 onward are bigger decisions.

---

## 1. IMM `mix_states` does not mix states

**File:** [`src/lib/radar/target/motion.rs:135-160`](../../src/lib/radar/target/motion.rs#L135-L160)

The function computes mixing probabilities `mixing_probs[i][j]` and the normalising vector `c_bar[j]`, then assigns `c_bar` to `self.model_probs` and returns. The mixing matrix is discarded. The three Kalman filters never share information -- each runs independently on the same measurement, then a softmax over likelihoods picks weights for the combination.

This is "three parallel Kalmans plus a softmax," not IMM. The test suite passes because the high-process-noise filter (labelled CT) tracks turns with a wider gate; not because the IMM machinery is actually working.

**Standard IMM mixing** (Bar-Shalom, Estimation with Applications to Tracking and Navigation, ch. 11):

```
For each j ∈ {CV, CA, CT}:
  x0_j = Σ_i μ_{i|j} · x_i
  P0_j = Σ_i μ_{i|j} · ( P_i + (x_i - x0_j)(x_i - x0_j)ᵀ )
Then each filter runs from (x0_j, P0_j) as its prior for this measurement.
```

**Fix scope:** add an interface to `KalmanFilter` that exposes `(state, P)` and accepts a `(state, P)` to be set as the prior for the next update. Implement the mixing in `ImmMotionModel::update` before calling each filter's update. Add a test that compares predictions under continuous turn against a known reference (the existing `test_imm_model_continuous_circling` is good but checks `model_probs[2] > 0.2`, which the current broken implementation already satisfies).

**Alternative:** if we don't care about full IMM and the current behaviour is good enough, rename the method to `update_model_probs` and document that this is "weighted-Kalman parallel, not IMM mixing." Either honest path is fine. Calling it IMM while not mixing is the problem.

## 2. CV, CA, CT filters all have CV dynamics

**File:** [`src/lib/radar/target/motion.rs:97-119`](../../src/lib/radar/target/motion.rs#L97-L119), [`src/lib/radar/target/kalman.rs:135-144`](../../src/lib/radar/target/kalman.rs#L135-L144)

All three "models" use the same 4-state constant-velocity state transition. The state vector is `[lat, lon, vlat, vlon]` for all three; the state transition matrix is `[[1,0,Δt,0],[0,1,0,Δt],[0,0,1,0],[0,0,0,1]]` for all three. The only difference between filters is `set_process_noise()` (`Q`).

A textbook CA model needs a 6-state filter `[lat, lon, vlat, vlon, alat, alon]` with `Δt²/2` blocks in the transition. A textbook CT model needs a non-linear (or augmented) state with explicit turn rate ω.

The current high-Q filter (CT) tracks turns by allowing rapid velocity drift, not by modelling turn rate. This works -- circling test passes -- but it's not what the comments and module names claim. And it means the CT model has no advantage over a single Kalman with a higher process noise.

**Fix scope:** this is the big one. Real CT would change the state size and require a non-linear update step (EKF Jacobian or unscented). Probably 200-400 lines plus a substantial test rework. Worth doing only if benchmarking against representative data (recorded emulator runs, or a real-vessel trace) shows the current approach is the limiter on tracking accuracy.

**Alternative:** if we're not going to do this, fix the comments to say "three Kalmans with different Q, blended by likelihood" rather than "IMM with CV/CA/CT."

## 3. `predict()` weights three drifted filter states

**File:** [`src/lib/radar/target/motion.rs:298-313`](../../src/lib/radar/target/motion.rs#L298-L313)

`ImmMotionModel::predict` computes a weighted average of the three filter predictions. Because the filters never mix (see #1) their states drift apart over many updates. The averaged prediction is between three increasingly different positions, weighted by stale probabilities.

This falls out automatically once #1 is fixed (mixing pulls the filter states back together every update).

## 4. Turn rejection only fires for `update_count ∈ [2, 5)`

**File:** [`src/lib/radar/target/tracker.rs:204-233`](../../src/lib/radar/target/tracker.rs#L204-L233)

A target that has been tracked steadily for 30 minutes and then suddenly shows a 170° turn at 20 m/s is accepted without question, because `update_count >= 5`. The check is meant to reject implausible early measurements, but the same logic should apply to mature tracks too -- a real boat can't physically reverse course in 3 seconds.

The comment cites radar_pi, but radar_pi's check is lifelong, not early-only.

Secondary issue: `MAX_TURN_ANGLE_DEG = 130°` is generous. At 5 m/s and 3 s revolution, a 130° turn represents about 13 m of "implausibility budget" along the new heading -- well within measurement noise for a single radar return. The threshold rejects huge implausibilities but lets most false matches through.

**Fix scope:** remove the `< 5` upper bound, keep the lower bound at 2 (need a previous COG). Lower the threshold (90° at 5 m/s? 60° at 10 m/s?). Add tests for matured-track rejection and for genuine sharp turns by slow boats that shouldn't be rejected.

## 5. Kalman gain falls back to identity silently

**File:** [`src/lib/radar/target/kalman.rs:221`](../../src/lib/radar/target/kalman.rs#L221)

```rust
let s_inv = s.try_inverse().unwrap_or(Matrix2x2::identity());
```

If the 2x2 innovation covariance `S = H·P·Hᵀ + R` is singular, the fallback identity produces a wrong Kalman gain that still seems plausible. In practice this won't happen with positive-definite `P` and `R = 25·I`, but if it does the filter corrupts silently.

**Fix:** log the singularity (this should never happen) and skip the update rather than corrupting state. Keep the filter at its predicted state for the next measurement.

## 6. `force_state` is dead code

**File:** [`src/lib/radar/target/kalman.rs:289-310`](../../src/lib/radar/target/kalman.rs#L289-L310), [`src/lib/radar/target/motion.rs:329-337`](../../src/lib/radar/target/motion.rs#L329-L337)

`KalmanFilter::force_state` and the IMM model's wrapper are public, documented, and have no callers. The comment in `tracker.rs:204` mentions "forced position override for fast targets" as the radar_pi heritage, but the override is never invoked. Either it's an unfinished idea, or it's there for an experiment that didn't pan out.

**Fix:** decide -- either wire it in (when do we want to force-override, and based on what?) or delete. Dead public APIs accumulate technical debt and confuse readers.

## 7. Stationary detection catches drifting targets

**File:** [`src/lib/radar/target/tracker.rs:275-281`](../../src/lib/radar/target/tracker.rs#L275-L281)

`STATIONARY_SPEED_THRESHOLD = 0.5 m/s` (~1 kn). Any target moving slower than 1 kn after 5 updates is treated as stationary and gets the 10-revolution lost timeout and 10-rev delete timeout (20 revs total ≈ 60 s on a 3 s radar). For genuine buoys this is correct. For an anchored vessel drifting in a strong tide, or a slow inflatable, the threshold may misfire.

Probably fine as-is -- the cost of over-classifying as stationary is a longer wait before garbage-collecting an actually-gone target. Worth flagging in case there are field reports of stuck "lost" targets in harbour traffic.

Note also: `MIN_UPDATES_FOR_STATIONARY = 5` is lower than the `update_count >= 4` promotion threshold, so a tracked target spends one update transiently in the stationary regime. Effect is minimal but worth a comment.

## 8. Duplicate merge can drop genuine close-by vessels

**File:** [`src/lib/radar/target/tracker.rs:417-476`](../../src/lib/radar/target/tracker.rs#L417-L476)

The rule: any young target (< 4 updates) within `DUPLICATE_MERGE_DISTANCE_M = 100 m` of another target is merged into it. This is correct for a large vessel producing multiple blobs.

Failure mode: two real vessels close together (tug and tow within 100 m, anchored cluster of boats) where one promotes quickly and the other never gets to 4 updates because it's repeatedly merged into the first.

**Fix scope:** add a motion-aware exclusion -- don't merge if the candidate's measured COG is noticeably different from the established target's COG (e.g., > 30° at similar speeds). Or: don't merge if both targets have been seen for >= 2 revolutions (genuine sustained presence). Probably 30 lines + a test for the tug-and-tow case.

Also worth measuring: is 100 m the right number? It's range-independent. At long range and large beam-width it might be too small; at close range it might be too large.

## 9. CPA omitted once vessels diverge

**File:** [`src/lib/radar/cpa.rs:83-85`](../../src/lib/radar/cpa.rs#L83-L85), [`src/lib/radar/target/mod.rs:99-113`](../../src/lib/radar/target/mod.rs#L99-L113)

`calculate_cpa` returns `None` if `tcpa <= 0`. The API skips emitting `TargetDangerApi` entirely (`is_empty` returns true). So a vessel that *was* a close-quarters situation 5 s ago has no danger field in its current delta.

A navigator wants to know "we just passed at 50 m." The current behaviour erases that.

**Fix:** also emit CPA with a negative TCPA (or a sign-bearing "approaching/passed" flag). Add an `is_dangerous` boolean computed against operator-configurable thresholds (CPA < 0.5 nm AND |TCPA| < 6 min by IMO defaults). This is more useful for downstream consumers than the raw CPA/TCPA pair.

Touches the Signal K schema -- coordinate with whatever's consuming. The schema is in `src/lib/radar/target/mod.rs` (`TargetDangerApi`).

## 10. CPA uses raw measurement position

**File:** [`src/lib/radar/target/manager.rs:812`](../../src/lib/radar/target/manager.rs#L812)

CPA is computed from `target.position`, which is the latest raw measurement (set in `ActiveTarget::update`). For a noisy track the position swings 50 m revolution-to-revolution, so the CPA computation swings with it. The COG/SOG come from the Kalman estimate, but the position doesn't.

**Fix:** use `target.predict_position(now)` instead. That uses the Kalman/IMM estimate which is the smoothed state. Five lines plus a test that adds noise to inputs and checks that CPA jitter is reduced.

## 11. No emission cadence control

**File:** [`src/lib/radar/target/manager.rs:342-345`](../../src/lib/radar/target/manager.rs#L342-L345)

Every revolution (~3 s), every target's delta is emitted. At 50 active targets that's 50 deltas every 3 s; at 200 it's 67/s sustained. Signal K consumers may drop. There's no per-target rate limit, no priority, no skipping of essentially-unchanged targets.

**Fix:** design discussion. Options:
- Per-target emission only when position/COG/SOG changes by > epsilon.
- Per-target minimum interval (5 s for far targets, 1 s for close targets).
- Operator-configurable max targets per radar.

Not a bug, but worth thinking about before someone deploys mayara in a busy harbour.

## 12. Lost-target bearing in merged mode is ambiguous

**File:** [`src/lib/radar/target/manager.rs:558-562`](../../src/lib/radar/target/manager.rs#L558-L562)

In merged mode, a target may have been last updated by radar B but is being displayed on a GUI showing radar A. The lost-target delta uses `target.last_radar_position` for bearing/distance, so the bearing is relative to radar B even though the consumer expects radar A's frame.

**Fix:** design discussion. Probably the right answer is: in merged mode, always emit absolute lat/lon and let the consumer compute bearing/distance against the radar they're displaying. The current code does emit lat/lon -- but it *also* emits a bearing/distance that may be wrong, which is worse than no bearing/distance.

## 13. AIS / ARPA correlation

There's no fusion between AIS targets (forwarded from Signal K nav data) and ARPA targets. A vessel showing on both produces two icons. Standard ECDIS practice is to gate-correlate on position, COG, SOG within thresholds (~50 m, 1 kn, 10°) and merge into one track with the AIS MMSI tag.

**Fix scope:** substantial feature, separate design doc. Not blocking anything.

---

## Magic numbers worth justifying

| Constant | Location | Concern |
|----------|----------|---------|
| `MIN_TARGET_PIXELS = 25` | [`blob.rs:25`](../../src/lib/radar/target/blob.rs#L25) | Pixel size depends on range / spoke length, so a fixed count means very different physical thresholds at 100 m vs 24 nm. Should probably be range-aware. |
| `DUPLICATE_MERGE_DISTANCE_M = 100.0` | [`tracker.rs:35`](../../src/lib/radar/target/tracker.rs#L35) | At long range, smaller than ARPA position uncertainty; at short range, larger than separation between adjacent vessels. See #8. |
| `MAX_TURN_ANGLE_DEG = 130.0` | [`tracker.rs:52`](../../src/lib/radar/target/tracker.rs#L52) | See #4 -- too generous. |
| `PROCESS_NOISE = 0.015` | [`kalman.rs:24`](../../src/lib/radar/target/kalman.rs#L24) | Unreachable in practice -- always overridden by `set_process_noise()` from IMM. Either remove or document it as the default for non-IMM users. |
| `MIN_MATCH_DISTANCE_M = 50.0` | [`tracker.rs:43`](../../src/lib/radar/target/tracker.rs#L43) | Range-independent minimum -- fine for short range, may be too small at long range where one pixel is 40 m. |
| `LOST_REVOLUTION_COUNT = 3`, `STATIONARY_LOST_REVOLUTION_COUNT = 10` | [`tracker.rs:14-19`](../../src/lib/radar/target/tracker.rs#L14-L19) | Reasonable defaults; the 10 vs 3 ratio is the interesting choice. |

---

## Open questions for the other maintainer

1. **Is IMM the right architecture, or should we go simpler?** If a single tuned Kalman with adaptive `Q` performs comparably on representative traces, the rework in #1 + #2 may not be worth it. Worth measuring before committing.
2. **What's the target deployment scenario?** Single-radar leisure boat, multi-radar commercial, or both? Drives priorities for #11 and #12.
3. **Is AIS correlation in scope for mayara, or should it live in a higher-level Signal K plugin?** This is the biggest "feature vs scope" question.
4. **Test corpus:** do we have recorded emulator runs or real-vessel traces against which to benchmark before/after? The current tests are unit-level; tracking quality really wants integration testing against representative data.

## Things I am explicitly *not* proposing

- Rewriting the blob detector. The spoke-arc handling for wrap-around is clever and tested; the merge logic looks correct.
- Replacing the EKF with a UKF. The non-linearity in the current setup (geo-to-local linearisation at the reference latitude) is mild; UKF would add complexity for marginal gain.
- Changing the Signal K delta format. The current schema is fine; #9 is an additive extension, not a breaking change.
