//! Shared assertions for the replay integration tests.
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
/// `full_revolution` is how much of the circle the sample must cover, as a
/// fraction. A capture rarely starts on a spoke boundary and the fixture holds
/// a bounded number of revolutions, so demanding every last angle would be
/// flaky; the point is to catch a decoder that collapses every spoke onto one
/// angle, not to audit the antenna.
pub fn assert_spokes(info: &RadarInfo, spokes: &[Spoke], full_revolution: f64) {
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

    let covered = angles.len() as f64 / per_revolution as f64;
    assert!(
        covered >= full_revolution,
        "{}: spokes cover {:.0}% of a revolution ({} of {} angles); a decoder \
         reading the angle from the wrong offset lands on a handful of angles",
        info.key(),
        covered * 100.0,
        angles.len(),
        per_revolution
    );

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
