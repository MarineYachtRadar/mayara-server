//! Passively snoop the well-known noisy IPv4 multicast groups on each
//! caller-supplied NIC and report any source IPs we hear during a short
//! window. Used by [`super::devices::collect_per_interface`] to surface
//! devices that are currently chatting but haven't shown up in the host's
//! ARP cache yet.
//!
//! All listen targets are high-port multicast (5353 mDNS, 1900 SSDP,
//! 5355 LLMNR), so no root / `CAP_NET_RAW` is required.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

use crate::network::{SocketType, create_udp_listen};

#[derive(Debug, Clone)]
pub struct PassiveHit {
    /// Source address of the packet we heard.
    pub ip: Ipv4Addr,
    /// The NIC the packet arrived on (matches one of `nic_addrs`).
    pub nic: Ipv4Addr,
    /// The well-known protocol/group we caught it on.
    pub protocol: &'static str,
}

/// (Label, group address, port) for each multicast group we passively
/// listen on. Receiving on these groups joins the group on the NIC, which
/// is sufficient to harvest the source IP of every packet sent into the
/// group by any device on the same L2 segment.
const NOISY_GROUPS: &[(&str, Ipv4Addr, u16)] = &[
    ("mdns", Ipv4Addr::new(224, 0, 0, 251), 5353),
    ("ssdp", Ipv4Addr::new(239, 255, 255, 250), 1900),
    ("llmnr", Ipv4Addr::new(224, 0, 0, 252), 5355),
];

/// Listen for up to `duration` on each (NIC × multicast group) pair and
/// return every (source IP, NIC, protocol) triple we hear. Per-(NIC ×
/// group) socket failures (e.g. IGMP join refused on a virtual NIC) are
/// soft-failed at `debug` level so a single broken pair doesn't kill the
/// rest.
pub async fn capture_one_shot(nic_addrs: &[Ipv4Addr], duration: Duration) -> Vec<PassiveHit> {
    if nic_addrs.is_empty() {
        return Vec::new();
    }

    let deadline = Instant::now() + duration;
    let mut set: JoinSet<Vec<PassiveHit>> = JoinSet::new();

    for &nic_addr in nic_addrs {
        for (protocol, group_ip, port) in NOISY_GROUPS {
            let group = SocketAddrV4::new(*group_ip, *port);
            let socket = match create_udp_listen(&group, &nic_addr, SocketType::Multicast) {
                Ok(s) => s,
                Err(e) => {
                    log::debug!(
                        "passive_capture: skip {} on {}: {}",
                        group, nic_addr, e
                    );
                    continue;
                }
            };
            let protocol: &'static str = protocol;
            set.spawn(async move {
                let mut socket = socket;
                let mut hits = Vec::new();
                let mut buf: Vec<u8> = Vec::with_capacity(64);
                loop {
                    buf.clear();
                    tokio::select! {
                        _ = sleep_until(deadline) => return hits,
                        res = socket.recv_buf_from(&mut buf) => {
                            match res {
                                Ok((_, SocketAddr::V4(src))) => {
                                    let src_ip = *src.ip();
                                    // Filter loopback and self — we're
                                    // looking for *other* devices on the
                                    // segment, not our own packets.
                                    if src_ip != nic_addr && !src_ip.is_loopback() {
                                        hits.push(PassiveHit {
                                            ip: src_ip,
                                            nic: nic_addr,
                                            protocol,
                                        });
                                    }
                                }
                                Ok((_, SocketAddr::V6(_))) => {}
                                Err(_) => return hits,
                            }
                        }
                    }
                }
            });
        }
    }

    let mut all = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(hits) = res {
            all.extend(hits);
        }
    }
    all
}
