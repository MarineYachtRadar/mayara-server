//! CDM "V2 heartbeat" sender — wakes a sleeping Fantom radar.
//!
//! Garmin radars (Fantom, Fantom Pro and likely xHD2/xHD3 as well) gate
//! their report stream on observing a peer CDM device on the network.
//! The MFD signals "I'm here" by multicasting `0x038e` to
//! `239.254.2.2:50050` every 5 s (every 1 s for the first 30 sends
//! after boot — see `cdm_build_and_send_v2_heartbeat` at
//! `0x030879a4` in the MFD firmware decompilation).
//!
//! Legacy GMR HD/xHD radars achieve the same handshake via a hardwired
//! pin in the Ethernet cable that an attached chartplotter energises;
//! Fantom-class radars dropped that pin in favour of the Ethernet
//! mechanism. Without anyone sending the heartbeat, the radar stays
//! quiet and mayara never sees it.
//!
//! ## Wire format (30-byte UDP datagram)
//!
//! ```text
//! GMN header (8 bytes):
//!   8e 03 00 00       msg_id = 0x038e (LE)
//!   16 00 00 00       payload_len = 22
//!
//! Payload (22 bytes):
//!   +00 02            version_marker
//!   +01 00            pad
//!   +02 64 10         product_id = 0x1064 (LE)
//!   +04 00            simulator_mode
//!   +05 05            product_subtype
//!   +06 02            syc_group_id (mirrors the observed radar's value)
//!   +07 01            constant
//!   +08 00            service_count = 0
//!   +09 00 00 00      pad
//!   +0c 00 00 00 00   service-section padding (16-byte minimum prefix
//!                     per the firmware: `if (count == 0) uVar1 = 0x10`)
//!   +10 01 04         tail tag: type=1, len=4
//!   +12 NN NN NN NN   u32 LE sequence/uptime
//! ```
//!
//! ## What we don't impersonate
//!
//! - We use `product_id = 0x1064` (GPSMAP 9000 = "lionshark"). The
//!   radar does not appear to validate this cryptographically, but the
//!   MFD itself uses this value (`*(u16*)PTR_DAT_08c5ae00` in the
//!   firmware), so it is a known-good MFD identifier.
//! - We declare zero services. The firmware's heartbeat builder
//!   explicitly handles this case (sets `uVar1 = 0x10` minimum), so the
//!   resulting packet is syntactically valid V2. We may later need to
//!   advertise a service if the Fantom turns out to filter on it.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::time::sleep;

use crate::network;

use super::protocol::{CDM_HEARTBEAT_ADDRESS, GARMIN_NETMASK, GMN_HEADER_LEN, MSG_CDM_HEARTBEAT};

/// Product ID mayara declares in its CDM heartbeat. `0x1064` (4196) is
/// the GPSMAP 9000-series MFD — a known-good MFD product code lifted
/// from `PTR_DAT_08c5ae00` in the decompiled firmware. The radar does
/// not appear to validate this against a whitelist.
const MAYARA_PRODUCT_ID: u16 = 0x1064;

/// `product_subtype` byte. `5` is the default returned by
/// `cdm_get_product_subtype` (@ `0x0498b120`) when the lookup table
/// index is out of range — i.e. a benign fallback value.
const MAYARA_PRODUCT_SUBTYPE: u8 = 5;

/// Default SYC group ID. The firmware reads this from gmcfg
/// `"syc.group_id"` with default `6`. The pcap-captured radar used
/// `2`. We start with `2` (lowest common observed value) and switch to
/// whatever value we observe in an incoming radar heartbeat, so
/// mayara joins the same boat-local group.
pub(super) const DEFAULT_SYC_GROUP_ID: u8 = 2;

/// CDM multicast destination — `239.254.2.2:50050`.
fn cdm_dest() -> SocketAddrV4 {
    match CDM_HEARTBEAT_ADDRESS {
        std::net::SocketAddr::V4(v4) => v4,
        _ => unreachable!("CDM_HEARTBEAT_ADDRESS is statically V4"),
    }
}

/// Length of the heartbeat payload (everything after the GMN header).
/// 16-byte minimum prefix + 6-byte serialized tail (`01 04` + u32 LE).
const PAYLOAD_LEN: usize = 22;

/// Full datagram length: GMN header + payload.
const PACKET_LEN: usize = GMN_HEADER_LEN + PAYLOAD_LEN;

/// Build the full 30-byte GMN packet (8-byte header + 22-byte payload).
pub(crate) fn build(syc_group_id: u8, seq: u32) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];

    // GMN header: msg_id + payload_len.
    p[0..4].copy_from_slice(&MSG_CDM_HEARTBEAT.to_le_bytes());
    p[4..8].copy_from_slice(&(PAYLOAD_LEN as u32).to_le_bytes());

    // Payload — offsets relative to GMN_HEADER_LEN (= 8).
    const H: usize = GMN_HEADER_LEN;
    p[H] = 2; // +00 version_marker
    // p[H + 1] = 0; // pad
    p[H + 2..H + 4].copy_from_slice(&MAYARA_PRODUCT_ID.to_le_bytes());
    // p[H + 4] = 0; // simulator_mode
    p[H + 5] = MAYARA_PRODUCT_SUBTYPE;
    p[H + 6] = syc_group_id;
    p[H + 7] = 1; // constant
    // p[H + 8] = 0; // service_count
    // p[H + 9..H + 12] = pad
    // p[H + 12..H + 16] = service-section padding (firmware minimum)
    p[H + 16] = 0x01; // tail tag type
    p[H + 17] = 0x04; // tail tag length
    p[H + 18..H + 22].copy_from_slice(&seq.to_le_bytes());

    p
}

/// Run the periodic heartbeat sender.
///
/// Sends on every NIC that has an IPv4 address inside `172.16.0.0/12`
/// (the Garmin marine subnet). Re-enumerates NICs each tick so freshly
/// configured interfaces start participating without a restart.
///
/// Cadence: 1 s for the first 30 sends (matches the MFD's "fast
/// startup" phase), then 5 s steady-state.
pub(crate) async fn run(syc_group_id: Arc<AtomicU8>) {
    const FAST_TICKS: u32 = 30;
    const FAST_INTERVAL: Duration = Duration::from_secs(1);
    const STEADY_INTERVAL: Duration = Duration::from_secs(5);

    let dest = cdm_dest();
    let mut seq: u32 = 0;

    loop {
        let nics = garmin_eligible_nics();
        if nics.is_empty() {
            log::trace!("CDM heartbeat: no Garmin-eligible NIC (172.16/12); skipping tick");
        } else {
            let packet = build(syc_group_id.load(Ordering::Relaxed), seq);
            for nic in &nics {
                send_one(&packet, &dest, nic).await;
            }
            log::trace!(
                "CDM heartbeat: sent seq={} to {} via {} NIC(s)",
                seq,
                dest,
                nics.len()
            );
        }

        seq = seq.wrapping_add(1);
        let interval = if seq < FAST_TICKS {
            FAST_INTERVAL
        } else {
            STEADY_INTERVAL
        };
        sleep(interval).await;
    }
}

async fn send_one(packet: &[u8], dest: &SocketAddrV4, nic: &Ipv4Addr) {
    match network::create_multicast_send(dest, nic) {
        Ok(sock) => {
            // Suppress local loopback: otherwise the kernel delivers
            // our own packet right back to mayara's receive socket on
            // 239.254.2.2:50050 and the locator briefly treats it as
            // a peer device.
            if let Err(e) = sock.set_multicast_loop_v4(false) {
                log::trace!("CDM heartbeat via {}: disabling MC loop failed: {}", nic, e);
            }
            if let Err(e) = sock.send(packet).await {
                log::debug!("CDM heartbeat via {}: send failed: {}", nic, e);
            }
        }
        Err(e) => {
            log::debug!("CDM heartbeat via {}: socket creation failed: {}", nic, e);
        }
    }
}

/// Enumerate NICs that have at least one IPv4 address inside the
/// Garmin marine subnet (`172.16.0.0/12`). Returns the first matching
/// address per interface.
fn garmin_eligible_nics() -> Vec<Ipv4Addr> {
    let interfaces = match NetworkInterface::show() {
        Ok(v) => v,
        Err(e) => {
            log::debug!("CDM heartbeat: NetworkInterface::show() failed: {}", e);
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for itf in interfaces {
        for addr in itf.addr {
            if let std::net::IpAddr::V4(ip) = addr.ip()
                && network::match_ipv4(&ip, &Ipv4Addr::new(172, 16, 0, 0), &GARMIN_NETMASK)
            {
                out.push(ip);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brand::garmin::discovery;

    #[test]
    fn build_packet_has_correct_header_and_length() {
        let p = build(2, 0);
        // GMN header: msg_id = 0x038e, payload_len = 22
        assert_eq!(
            u32::from_le_bytes(p[0..4].try_into().unwrap()),
            MSG_CDM_HEARTBEAT
        );
        assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 22);
        assert_eq!(p.len(), 30);
    }

    #[test]
    fn build_payload_layout_matches_documented_v2() {
        let p = build(2, 0x12345678);
        // Payload starts at p[8].
        assert_eq!(p[8], 2, "version_marker");
        assert_eq!(p[9], 0, "pad");
        assert_eq!(
            u16::from_le_bytes(p[10..12].try_into().unwrap()),
            MAYARA_PRODUCT_ID
        );
        assert_eq!(p[12], 0, "simulator_mode");
        assert_eq!(p[13], MAYARA_PRODUCT_SUBTYPE);
        assert_eq!(p[14], 2, "syc_group_id");
        assert_eq!(p[15], 1, "constant");
        assert_eq!(p[16], 0, "service_count");
        assert_eq!(&p[17..20], &[0, 0, 0], "pad after service_count");
        assert_eq!(&p[20..24], &[0, 0, 0, 0], "service-section padding");
        // Tail tag at payload +0x10 = absolute byte 24.
        assert_eq!(p[24], 0x01, "tail tag type");
        assert_eq!(p[25], 0x04, "tail tag length");
        assert_eq!(
            u32::from_le_bytes(p[26..30].try_into().unwrap()),
            0x12345678,
            "tail sequence"
        );
    }

    #[test]
    fn built_heartbeat_round_trips_through_parse() {
        let p = build(7, 42);
        // discovery::parse expects the payload only (after GMN header).
        let hb = discovery::parse(&p[GMN_HEADER_LEN..]).expect("must parse");
        assert_eq!(hb.version, 2);
        assert_eq!(hb.product_id, MAYARA_PRODUCT_ID);
        assert_eq!(hb.simulator_mode, 0);
        assert_eq!(hb.product_subtype, MAYARA_PRODUCT_SUBTYPE);
        assert_eq!(hb.syc_group_id, 7);
    }
}
