//! xHD spoke sender (port 50102).
//!
//! Furuno delivers spokes in batches of ~1500 every ~450ms. A real GMR xHD
//! sends spokes continuously at ~0.3ms/spoke. We buffer incoming batches and
//! play them back at a steady rate so the plotter sees a continuous sweep.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

use bytes::Bytes;
use protobuf::Message as _;
use tokio::sync::broadcast;

use crate::Brand;
use crate::brand::garmin::protocol::{DATA_ADDRESS, DATA_PORT};
use crate::protos::RadarMessage::RadarMessage;

use super::SharedState;
use super::convert::{FURUNO_RANGE_RATIO, nearest_xhd_range, to_xhd_spoke};

// At 24 RPM / 8192 spokes/rev, one spoke = ~0.305ms.
// We send one spoke per tick at this interval.
const SPOKE_INTERVAL: Duration = Duration::from_micros(305);

pub(super) async fn run(
    local_ip: Ipv4Addr,
    brand: Brand,
    spokes_per_rev: u32,
    mut message_rx: broadcast::Receiver<Bytes>,
    state: Arc<Mutex<SharedState>>,
    mut stop: oneshot::Receiver<()>,
) {
    let sock = match UdpSocket::bind((local_ip, DATA_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("GarminXhd spokes: failed to bind socket: {e}");
            return;
        }
    };
    if let Err(e) = sock.set_multicast_ttl_v4(1) {
        log::warn!("GarminXhd spokes: could not set TTL=1: {e}");
    }

    let dest = DATA_ADDRESS;

    // Ready-to-send packets, pre-converted.
    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut next_send = Instant::now();

    loop {
        // Drain the queue: send all packets that are due.
        while let Some(_) = queue.front() {
            let now = Instant::now();
            if now < next_send {
                break;
            }
            let pkt = queue.pop_front().unwrap();
            if let Err(e) = sock.send_to(&pkt, dest).await {
                log::warn!("GarminXhd spokes: send failed: {e}");
            }
            next_send = next_send + SPOKE_INTERVAL;
        }

        // Wait for next send time or a new batch — whichever comes first.
        let wait = next_send.saturating_duration_since(Instant::now());

        tokio::select! {
            biased;
            _ = &mut stop => break,
            res = message_rx.recv() => {
                let bytes = match res {
                    Ok(b) => b,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("GarminXhd spokes: lagged, dropped {n} messages");
                        continue;
                    }
                    Err(_) => break,
                };
                let msg = match RadarMessage::parse_from_bytes(&bytes) {
                    Ok(m) => m,
                    Err(e) => {
                        log::warn!("GarminXhd spokes: protobuf parse error: {e}");
                        continue;
                    }
                };
                for spoke in &msg.spokes {
                    if spoke.range == 0 {
                        continue;
                    }
                    let display_range = match brand {
                        Brand::Furuno => (spoke.range as f64 / FURUNO_RANGE_RATIO).round() as u32,
                        _ => spoke.range,
                    };
                    {
                        let mut st = state.lock().unwrap();
                        let now = Instant::now();
                        if now >= st.range_lock_until {
                            let new_range = nearest_xhd_range(display_range);
                            if new_range != st.range_m {
                                log::info!(
                                    "GarminXhd spokes: range update {}m → {}m (display {}m)",
                                    st.range_m, new_range, display_range
                                );
                                st.range_m = new_range;
                            }
                        }
                    }
                    let pkt = to_xhd_spoke(
                        spoke.angle,
                        spokes_per_rev,
                        &spoke.data,
                        display_range,
                        spoke.range,
                    );
                    queue.push_back(pkt);
                }
                // Reset send clock if queue was empty (we were idle).
                if next_send < Instant::now() {
                    next_send = Instant::now();
                }
            }
            _ = tokio::time::sleep(wait) => {
                // Timer fired — loop back to drain the queue.
            }
        }
    }
}
