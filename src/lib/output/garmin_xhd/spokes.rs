//! xHD spoke sender (port 50102).
//!
//! Subscribes to `RadarInfo::message_tx`, decodes each protobuf RadarMessage,
//! converts every spoke to xHD wire format, and sends it as a UDP multicast
//! datagram to `239.254.2.0:50102`.
//!
//! Range tracking: `spoke.range` from mayara carries the radar's internal
//! display range, which differs from the xHD range set via mayara by a
//! brand-specific factor. After conversion the nearest xHD table value is
//! stored in `SharedState::range_m` so the status stream stays consistent —
//! unless a range-lock is in effect (set by command.rs after a plotter range
//! command, to prevent the incoming spoke range from immediately overriding
//! what the plotter just set).

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
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
    sock.set_multicast_ttl_v4(1).ok();

    let dest = DATA_ADDRESS;

    loop {
        let bytes = tokio::select! {
            res = message_rx.recv() => match res {
                Ok(b) => b,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("GarminXhd spokes: lagged, dropped {n} messages");
                    continue;
                }
                Err(_) => break,
            },
            _ = &mut stop => break,
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

            // Convert spoke.range (radar-internal) to actual display range.
            let display_range = match brand {
                Brand::Furuno => (spoke.range as f64 / FURUNO_RANGE_RATIO).round() as u32,
                _ => spoke.range,
            };

            // Update shared range unless the plotter's command lock is active.
            {
                let mut st = state.lock().unwrap();
                let now = Instant::now();
                if now >= st.range_lock_until {
                    let new_range = nearest_xhd_range(display_range);
                    if new_range != st.range_m {
                        log::info!(
                            "GarminXhd spokes: range update {}m → {}m (display {}m)",
                            st.range_m,
                            new_range,
                            display_range
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
            if let Err(e) = sock.try_send_to(&pkt, dest) {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    log::warn!("GarminXhd spokes: send failed: {e}");
                }
            }
        }
    }
}
