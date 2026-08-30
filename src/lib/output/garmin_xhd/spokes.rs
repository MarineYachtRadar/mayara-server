//! Sweep data stream of the emulated xHD radar (`239.254.2.0:50102`).
//!
//! Source radars deliver spokes in bursts — a Furuno hands over some 1500 of
//! them every 450 ms — while a real xHD trickles them out one at a time. A
//! plotter fed the bursts draws a sweep that jumps and then stalls, so spokes
//! are buffered and paced out at the rate they come in.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use protobuf::Message as _;
use tokio::sync::broadcast;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::garmin::protocol::{DATA_ADDRESS, SPOKES_PER_REVOLUTION};
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::{RadarError, SharedRadars};

use super::convert::SpokeStream;
use super::{Shared, multicast_send};

/// Buffer depth, in xHD spokes. Two revolutions is more delay than any radar's
/// burst needs; beyond it the network is not keeping up and the oldest spokes
/// are worth less than the newest.
const QUEUE_MAX: usize = SPOKES_PER_REVOLUTION * 2;

/// Longest gap between bursts that still says something about how fast spokes
/// arrive. A radar in standby sends nothing for minutes; pacing the first
/// burst after that by how long the silence lasted would trickle it out.
const MAX_BURST_SPACING: Duration = Duration::from_secs(1);

/// Pacing bounds, from an antenna turning implausibly fast to one turning
/// implausibly slow — 1440 spokes at 4 ms each is a revolution in under 6
/// seconds, slower than any radar mayara supports.
const MIN_INTERVAL: Duration = Duration::from_micros(100);
const MAX_INTERVAL: Duration = Duration::from_millis(4);

/// How often the radar's legend and spoke geometry are picked up again. Both
/// only settle once the radar has reported which model it is.
const RECONFIGURE_INTERVAL: Duration = Duration::from_secs(5);

pub(super) async fn run(
    local_addr: Ipv4Addr,
    key: String,
    radars: SharedRadars,
    mut message_rx: broadcast::Receiver<Bytes>,
    shared: Arc<Shared>,
    subsys: &mut SubsystemHandle,
) -> Result<(), RadarError> {
    let socket = match multicast_send(&DATA_ADDRESS, local_addr) {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("{key}: Garmin xHD spokes: cannot open socket: {e}");
            return Ok(());
        }
    };

    let Some(info) = radars.get_by_key(&key) else {
        log::error!("{key}: Garmin xHD spokes: radar is gone");
        return Ok(());
    };
    let mut stream = SpokeStream::new(info.spokes_per_revolution, &info.get_legend());
    let mut reconfigured = Instant::now();

    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut interval = MAX_INTERVAL;
    let mut next_send = Instant::now();
    let mut last_batch = Instant::now();

    loop {
        while let Some(packet) = queue.front() {
            if Instant::now() < next_send {
                break;
            }
            if let Err(e) = socket.send(packet).await {
                log::warn!("{key}: Garmin xHD spokes: send failed: {e}");
            }
            queue.pop_front();
            next_send += interval;
        }

        // With an empty queue there is nothing to pace, so wait for the next
        // batch and start the clock when it arrives.
        let wait = if queue.is_empty() {
            None
        } else {
            Some(next_send.saturating_duration_since(Instant::now()))
        };

        tokio::select! {
            biased;
            _ = subsys.on_shutdown_requested() => return Ok(()),
            _ = tokio::time::sleep(wait.unwrap_or_default()), if wait.is_some() => {}
            message = message_rx.recv() => {
                let bytes = match message {
                    Ok(bytes) => bytes,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("{key}: Garmin xHD spokes: dropped {n} messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };

                if reconfigured.elapsed() >= RECONFIGURE_INTERVAL {
                    reconfigured = Instant::now();
                    if let Some(info) = radars.get_by_key(&key) {
                        stream.reconfigure(info.spokes_per_revolution, &info.get_legend());
                    }
                }

                let was_empty = queue.is_empty();
                enqueue(&bytes, &shared, &mut stream, &mut queue, &key);

                // Drain what is buffered over the time the next burst is
                // expected to take, which is how long this one took to arrive.
                let elapsed = last_batch.elapsed().min(MAX_BURST_SPACING);
                last_batch = Instant::now();
                if !queue.is_empty() {
                    interval = (elapsed / queue.len() as u32).clamp(MIN_INTERVAL, MAX_INTERVAL);
                }
                if was_empty {
                    next_send = Instant::now();
                }
            }
        }
    }
}

/// Convert one broadcast message into xHD spokes.
fn enqueue(
    bytes: &[u8],
    shared: &Shared,
    stream: &mut SpokeStream,
    queue: &mut VecDeque<Vec<u8>>,
    key: &str,
) {
    let message = match RadarMessage::parse_from_bytes(bytes) {
        Ok(message) => message,
        Err(e) => {
            log::warn!("{key}: Garmin xHD spokes: cannot parse message: {e}");
            return;
        }
    };

    let range_m = shared.range_m();
    for spoke in &message.spokes {
        // The plotter scales the image by the distance the samples cover, so
        // a spoke that does not say what that is cannot be drawn.
        if spoke.range == 0 {
            continue;
        }
        stream.push(spoke.angle, &spoke.data, range_m, spoke.range, queue);
    }

    if queue.len() > QUEUE_MAX {
        let dropped = queue.len() - QUEUE_MAX;
        queue.drain(..dropped);
        log::warn!("{key}: Garmin xHD spokes: behind by {dropped} spokes, dropping oldest");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TargetMode;
    use crate::output::garmin_xhd::tests::controls;
    use crate::protos::RadarMessage::radar_message::Spoke;
    use crate::radar::default_legend;

    /// A batch of spokes as a brand would broadcast it, serialized.
    fn message(spokes: impl IntoIterator<Item = (u32, u32)>) -> Vec<u8> {
        let mut message = RadarMessage::new();
        for (angle, range) in spokes {
            let mut spoke = Spoke::new();
            spoke.angle = angle;
            spoke.range = range;
            spoke.data = vec![1u8; 512];
            message.spokes.push(spoke);
        }
        message.write_to_bytes().expect("serializable")
    }

    fn fixture() -> (Arc<Shared>, SpokeStream, VecDeque<Vec<u8>>) {
        let (shared, _echo_rx) = Shared::new(controls());
        let legend = default_legend(&TargetMode::None, 0, false, 16);
        (
            Arc::new(shared),
            SpokeStream::new(1440, &legend),
            VecDeque::new(),
        )
    }

    #[test]
    fn a_message_that_is_not_a_radar_message_is_dropped() {
        let (shared, mut stream, mut queue) = fixture();
        enqueue(
            b"\xff\xff not protobuf",
            &shared,
            &mut stream,
            &mut queue,
            "t",
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn spokes_without_a_range_are_skipped() {
        let (shared, mut stream, mut queue) = fixture();
        // Only the third spoke says how far its samples reach, and a spoke is
        // emitted once the following one bounds it — so nothing comes out.
        enqueue(
            &message([(0, 0), (1, 0), (2, 3704)]),
            &shared,
            &mut stream,
            &mut queue,
            "t",
        );
        assert!(queue.is_empty());

        enqueue(&message([(3, 3704)]), &shared, &mut stream, &mut queue, "t");
        assert_eq!(queue.len(), 1, "the two ranged spokes bound one xHD spoke");
    }

    #[test]
    fn a_queue_that_runs_away_is_trimmed_to_the_newest_spokes() {
        let (shared, mut stream, mut queue) = fixture();
        let batch = message((0..1440).map(|angle| (angle, 3704)));

        for _ in 0..3 {
            enqueue(&batch, &shared, &mut stream, &mut queue, "t");
        }

        assert_eq!(queue.len(), QUEUE_MAX);
    }
}
