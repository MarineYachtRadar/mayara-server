//! Furuno heading sender.
//!
//! A Furuno radar has no heading input of its own: a NavNet display relays
//! the heading sensor's sentences to the network, and the radar takes them
//! from there. Without a heading a DRS-NXT keeps echo averaging off and
//! sends spokes without one, so mayara relays the vessel's heading the same
//! way for an installation without a Furuno display.
//!
//! A radar hearing two headings takes them in turn, so mayara keeps quiet
//! while a display is relaying one. The broadcast reaches every radar on the
//! link, so one sender per interface serves them all.

use deku::DekuWrite;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::protocol::HEADING_ADDRESS;
use crate::navdata::get_heading_true;
use crate::network::{SocketType, create_broadcast_send, create_udp_listen};
use crate::radar::RadarError;
use crate::util::encode;

/// A NavNet display relays the heading ten times a second.
const HEADING_INTERVAL: Duration = Duration::from_millis(100);

/// How long mayara keeps quiet after a display last relayed a heading. The
/// radar holds a heading for 3 s, so mayara takes over before it is lost.
const DISPLAY_HOLD: Duration = Duration::from_secs(2);

/// Room for the longest frame a display relays.
const MAX_FRAME: usize = 1500;

/// The unit of [`SensorFrame::ticks`].
const TICK: Duration = Duration::from_millis(10);

/// How long to wait before relaying again after the relay failed, such as
/// while the interface is down.
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// One relayed sentence, as a NavNet display broadcasts it. The radar checks
/// only the 0x01 every NavNet packet opens with, and parses the sentence
/// after the header; the fields between are sent as a display sends them.
#[derive(DekuWrite, Debug)]
#[deku(endian = "big", magic = b"\x01")]
struct SensorFrame {
    /// Counts the frames sent, wrapping.
    sequence: u8,
    /// The length of `sentence`.
    #[deku(magic = b"\x00\x01\x00\x00\x00\x00")]
    length: u16,
    /// All 0xff from a display.
    unknown: [u8; 6],
    /// A display's free-running clock, in [`TICK`]s.
    ticks: u32,
    sentence: Vec<u8>,
}

/// Sends the vessel's heading out of `nic_addr` for as long as the subsystem
/// is alive, and nothing while mayara has no heading or a display is
/// relaying one. A failed relay starts again after [`RETRY_DELAY`], since
/// nothing else restarts it while its radars stay registered.
pub(super) async fn run(subsys: &mut SubsystemHandle, nic_addr: Ipv4Addr) {
    while let Err(e) = relay(subsys, nic_addr).await {
        log::warn!("{}: Unable to relay the heading: {}", nic_addr, e);
        tokio::select! {
            _ = subsys.on_shutdown_requested() => return,
            _ = sleep(RETRY_DELAY) => {}
        }
    }
}

async fn relay(subsys: &mut SubsystemHandle, nic_addr: Ipv4Addr) -> Result<(), RadarError> {
    let socket = create_broadcast_send(&HEADING_ADDRESS, &nic_addr)?;
    let mut listener = create_udp_listen(&HEADING_ADDRESS, &nic_addr, SocketType::Broadcast)?;
    let mut buf = Vec::with_capacity(MAX_FRAME);
    let start = Instant::now();
    let mut deadline = start + HEADING_INTERVAL;
    let mut sequence: u8 = 0;
    let mut display = DisplayWatch::default();

    loop {
        tokio::select! {
            _ = subsys.on_shutdown_requested() => {
                return Ok(());
            }
            received = listener.recv_buf_from(&mut buf) => {
                let (_, from) = received?;
                buf.clear();
                if from.ip() != IpAddr::V4(nic_addr) && display.heard(Instant::now()) {
                    log::info!("{}: a display relays the heading, so mayara does not", from.ip());
                }
            }
            _ = sleep_until(deadline) => {
                if display.gone_quiet(Instant::now()) {
                    log::info!("The display stopped relaying the heading; mayara relays it");
                }
                if !display.relaying()
                    && let Some(heading) = get_heading_true()
                {
                    let ticks = ((deadline - start).as_millis() / TICK.as_millis()) as u32;
                    socket.send(&sensor_frame(sequence, ticks, hdt_sentence(heading))).await?;
                    sequence = sequence.wrapping_add(1);
                }
                deadline += HEADING_INTERVAL;
            }
        }
    }
}

/// Whether a display is relaying the heading, judged by when it last did.
#[derive(Default)]
struct DisplayWatch {
    last_heard: Option<Instant>,
}

impl DisplayWatch {
    /// Notes a display's frame; true when it is the first in a while.
    fn heard(&mut self, now: Instant) -> bool {
        self.last_heard.replace(now).is_none()
    }

    /// Forgets a display that has been quiet for [`DISPLAY_HOLD`]; true when
    /// it just went quiet.
    fn gone_quiet(&mut self, now: Instant) -> bool {
        let quiet = self
            .last_heard
            .is_some_and(|t| now.duration_since(t) >= DISPLAY_HOLD);
        if quiet {
            self.last_heard = None;
        }
        quiet
    }

    fn relaying(&self) -> bool {
        self.last_heard.is_some()
    }
}

fn sensor_frame(sequence: u8, ticks: u32, sentence: Vec<u8>) -> Vec<u8> {
    encode(&SensorFrame {
        sequence,
        length: sentence.len() as u16,
        unknown: [0xff; 6],
        ticks,
        sentence,
    })
}

/// `$IIHDT` for a true heading in radians. The radar prefers a heading by
/// its talker, and ranks `II` lowest, so a heading sensor's relay wins.
fn hdt_sentence(heading: f64) -> Vec<u8> {
    let tenths = (heading.to_degrees() * 10.).round().rem_euclid(3600.);
    let body = format!("IIHDT,{:.1},T", tenths / 10.);
    let checksum = body.bytes().fold(0, |sum, b| sum ^ b);
    format!("${body}*{checksum:02X}\r\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};

    /// TEST-NET-1: no interface holds it, so binding a relay to it fails.
    const NO_SUCH_NIC: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);

    /// Long enough for the failed relay to reach its retry delay.
    const SETTLE: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn a_failing_relay_stops_at_shutdown_while_it_waits_to_retry() {
        assert!(
            create_broadcast_send(&HEADING_ADDRESS, &NO_SUCH_NIC).is_err(),
            "the relay must fail for this test to reach its retry delay"
        );
        Toplevel::new(async |subsys: &mut SubsystemHandle| {
            let (done, stopped) = tokio::sync::oneshot::channel();
            subsys.start(SubsystemBuilder::new(
                "heading",
                async move |s: &mut SubsystemHandle| {
                    run(s, NO_SUCH_NIC).await;
                    let _ = done.send(());
                    Ok::<(), anyhow::Error>(())
                },
            ));
            tokio::time::sleep(SETTLE).await;

            subsys.request_shutdown();

            let stopped_in_time = tokio::time::timeout(RETRY_DELAY / 2, stopped).await;
            assert!(
                stopped_in_time.is_ok_and(|r| r.is_ok()),
                "the relay outlived shutdown"
            );
        })
        .handle_shutdown_requests(Duration::from_secs(1))
        .await
        .unwrap();
    }

    #[test]
    fn a_frame_matches_what_a_display_sends() {
        // A TZtouch relaying an SCX-20, captured on UDP 10036.
        let captured = [
            &[0x01, 0xc3, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x13][..],
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x0a, 0x2a],
            b"$GPHDT,232.6,T*30\r\n",
        ]
        .concat();
        assert_eq!(
            sensor_frame(0xc3, 0x0a2a, b"$GPHDT,232.6,T*30\r\n".to_vec()),
            captured
        );
    }

    #[test]
    fn mayara_keeps_quiet_while_a_display_relays_the_heading() {
        let start = Instant::now();
        let mut display = DisplayWatch::default();
        assert!(!display.relaying());

        assert!(display.heard(start), "the first frame is news");
        assert!(!display.heard(start + HEADING_INTERVAL), "the next is not");
        assert!(!display.gone_quiet(start + DISPLAY_HOLD));
        assert!(display.relaying());

        let quiet = start + HEADING_INTERVAL + DISPLAY_HOLD;
        assert!(display.gone_quiet(quiet));
        assert!(!display.relaying());
        assert!(!display.gone_quiet(quiet), "it went quiet only once");
    }

    #[test]
    fn a_heading_rounds_to_within_one_turn() {
        assert_eq!(
            hdt_sentence(359.96_f64.to_radians()),
            b"$IIHDT,0.0,T*22\r\n"
        );
        assert_eq!(
            hdt_sentence((-90.0_f64).to_radians()),
            b"$IIHDT,270.0,T*27\r\n"
        );
    }
}
