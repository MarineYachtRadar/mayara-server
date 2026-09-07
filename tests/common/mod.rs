//! Shared assertions for the replay integration tests.
//!
//! Cargo compiles this module into every test binary that declares `mod
//! common;`, and each of them uses a different part of it — a helper only the
//! HALO tests need is dead code in the other eight, which `-D warnings` would
//! otherwise refuse to build.
#![allow(dead_code)]

//!
//! A replay test that stops at discovery proves only that a radar was found.
//! A radar can be discovered correctly, report the right model and the right
//! range list, and still draw nothing: the 3G decoded its spokes with the BR24
//! header layout, which put angle and heading in the right place and the range
//! in the wrong one, and every test stayed green. These helpers let each brand's
//! test look at what was actually decoded.

use std::collections::HashSet;
use std::time::Duration;

use mayara::protos::RadarMessage::RadarMessage;
use mayara::protos::RadarMessage::radar_message::Spoke;
use mayara::radar::RadarInfo;
use protobuf::Message;

/// How many distinct ranges one sample may hold: one per range on a dual-range
/// radar, and one more either side in case the operator changed range while the
/// capture was running.
const MAX_DISTINCT_RANGES: usize = 4;

/// How many spokes in a sample must carry a range, as a percentage. The first
/// few spokes of a capture can arrive before the report that says what the
/// range is.
const MIN_RANGED_PERCENT: usize = 95;

/// The heading a spoke implies: its bearing is the angle referred to true
/// north, so the difference between the two is where the bow was pointing.
fn heading_of(bearing: u32, angle: u32, per_revolution: u32) -> u32 {
    (bearing + per_revolution - angle) % per_revolution
}

/// How far the heading may wander across one sample, as a fraction of a
/// revolution. A quarter of the circle is far more swing than any capture of a
/// few seconds shows -- a HALO holds to one or two distinct headings, and a
/// Furuno capture with the boat turning reached 224 of 8192, about ten degrees.
/// It is still nowhere near what a heading read from the wrong offset does,
/// which is to scatter through as many distinct values as there are spokes.
const HEADING_DRIFT_DIVISOR: usize = 4;

/// How many distinct angles a sample must hold. See [`assert_spokes`]: four
/// times the 8 a collapsed decoder produces, and four times under the 135 the
/// slowest fixture managed on the slowest machine seen.
const MIN_DISTINCT_ANGLES: usize = 32;

/// Collect decoded spokes from a radar's broadcast until `wanted` have arrived
/// or `timeout` passes.
///
/// The replay dispatcher makes a second pass once discovery has registered its
/// listeners, so a caller that subscribes as soon as the radar appears still
/// sees the spokes that were replayed before it existed.
pub async fn collect_spokes(info: &RadarInfo, wanted: usize, timeout: Duration) -> Vec<Spoke> {
    let mut rx = info.message_tx.subscribe();
    // Subscribing is also what takes the radar out of its idle state, in which
    // it drains the spoke socket without decoding anything.
    info.wake_up();

    let deadline = tokio::time::Instant::now() + timeout;
    let mut spokes = Vec::new();
    while spokes.len() < wanted {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(bytes)) => {
                let message = RadarMessage::parse_from_bytes(&bytes).expect("decode RadarMessage");
                spokes.extend(message.spokes);
            }
            // Lagged: the fixture outran this receiver, which is fine — the
            // spokes already collected are just as good a sample.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    spokes
}

/// Assert that `spokes` describe a real radar picture rather than bytes that
/// merely arrived: every angle inside one revolution, the whole revolution
/// covered, every range one the radar advertises, and echo that is not all
/// zero.
///
/// The angle bar is a count of distinct angles rather than a share of the
/// circle, because a share is a property of the machine as much as of the
/// decoder: collection is time-bounded, so a slower or busier one gathers fewer
/// spokes. The Garmin dual-range capture covers 31% of a revolution on a
/// development machine and 9% on Windows CI. A count holds still. What it has
/// to catch is a decoder that collapses spokes onto a handful of angles, which
/// is what reading the angle from the wrong offset does — the Furuno DRS2D
/// fixture manages 8 distinct angles out of 8192.
pub fn assert_spokes(info: &RadarInfo, spokes: &[Spoke]) {
    assert!(
        !spokes.is_empty(),
        "{}: no spokes were decoded — the fixture carries no echo, or the \
         spoke path is broken",
        info.key()
    );

    let per_revolution = info.spokes_per_revolution as u32;
    let angles: HashSet<u32> = spokes.iter().map(|s| s.angle).collect();

    for spoke in spokes {
        assert!(
            spoke.angle < per_revolution,
            "{}: angle {} is outside a revolution of {}",
            info.key(),
            spoke.angle,
            per_revolution
        );
    }

    assert!(
        angles.len() >= MIN_DISTINCT_ANGLES,
        "{}: only {} distinct angles across {} spokes; a decoder reading the \
         angle from the wrong offset lands on a handful of them",
        info.key(),
        angles.len(),
        spokes.len()
    );

    // Heading, where the capture carries it. Only some radars report it in the
    // spoke stream (a HALO does, on its own multicast group), so a fixture
    // without it is not a fault — but one that has it must decode it sanely.
    // `bearing` is the angle referred to true north, so `bearing - angle` is the
    // heading itself: steady over the couple of revolutions a fixture holds,
    // give or take the boat swinging. A heading read from the wrong offset
    // scatters that difference across the circle instead.
    let headings: HashSet<u32> = spokes
        .iter()
        .filter_map(|s| s.bearing.map(|b| heading_of(b, s.angle, per_revolution)))
        .collect();
    if !headings.is_empty() {
        for heading in &headings {
            assert!(
                *heading < per_revolution,
                "{}: heading {} is outside a revolution of {}",
                info.key(),
                heading,
                per_revolution
            );
        }
        let drift = per_revolution as usize / HEADING_DRIFT_DIVISOR;
        assert!(
            headings.len() <= drift,
            "{}: {} distinct headings across one sample, more than the {} a \
             swinging boat explains; heading is being read from the wrong place",
            info.key(),
            headings.len(),
            drift
        );
    }

    // A spoke's range is the distance to its last pixel, which is what the
    // radar is actually sweeping — not the menu entry it was set from. On a 3G
    // those differ: it sweeps 225/451/808 m while advertising the nautical
    // 231/463/926, so the check is that the range is real and steady, not that
    // it appears in the list.
    //
    // Steadiness is the assertion that matters. Reading the range from the
    // wrong offset in the header is what happened on the 3G, and it produced
    // 768 distinct ranges between 6 km and 118,000 km where the radar held one.
    let longest = info
        .ranges
        .all
        .iter()
        .map(|r| r.distance())
        .max()
        .expect("a discovered radar advertises ranges");

    // A spoke decoded before the radar's first range report has no range to
    // carry yet, so a handful at the start of a capture is normal. A radar that
    // mostly reports no range at all would draw nothing.
    let ranged: Vec<i32> = spokes
        .iter()
        .map(|s| s.range as i32)
        .filter(|r| *r != 0)
        .collect();
    assert!(
        ranged.len() * 100 >= spokes.len() * MIN_RANGED_PERCENT,
        "{}: only {} of {} spokes carry a range",
        info.key(),
        ranged.len(),
        spokes.len()
    );
    let seen: HashSet<i32> = ranged.into_iter().collect();
    for range in &seen {
        assert!(
            *range > 0 && *range <= longest,
            "{}: spoke range {} m is not a range this radar can sweep (longest \
             advertised is {} m)",
            info.key(),
            range,
            longest
        );
    }
    assert!(
        seen.len() <= MAX_DISTINCT_RANGES,
        "{}: {} distinct ranges across one sample; a radar holds its range, or \
         alternates between two on dual range: {:?}",
        info.key(),
        seen.len(),
        seen
    );

    let with_echo = spokes
        .iter()
        .filter(|s| s.data.iter().any(|&b| b > 0))
        .count();
    assert!(
        with_echo > 0,
        "{}: every one of {} spokes is empty; nothing would be drawn",
        info.key(),
        spokes.len()
    );
}

/// Assert the radar reported a heading at all. Called by the tests whose
/// capture carries one, so that losing it is a failure rather than a silently
/// skipped check in [`assert_spokes`].
pub fn assert_heading_present(info: &RadarInfo, spokes: &[Spoke]) {
    assert!(
        spokes.iter().any(|s| s.bearing.is_some()),
        "{}: this capture carries heading, so some spoke should have a bearing",
        info.key()
    );
}
