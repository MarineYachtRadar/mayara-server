//! Wake-up sender — powers on a sleeping Garmin radar and keeps it on.
//!
//! A Garmin radar without a chartplotter powers itself down: it sits at
//! almost no current and sends nothing, so mayara would never find it. A
//! chartplotter wakes it by sending a Wake-on-LAN magic packet, wrapped in a
//! Garmin `0x0a2c` message, to every host on the network. The radar's network
//! chip recognises the magic packet and powers the radar on. The radar then
//! powers itself down again about two minutes after the last one, so the
//! chartplotter keeps sending them for as long as it runs: ten at one-second
//! intervals when it starts, then one every 10–14 s.
//!
//! The magic packet carries one fixed address, the same on every Garmin
//! network; it is not any device's own MAC address. After it come three
//! NUL-terminated fields: the network generation (`GMN1`), the boot mode
//! (`SYS` for a normal start) and the sender's priority between chartplotters
//! (`20`, the chartplotter default). The radar checks none of them.
//!
//! ## Wire format (122-byte UDP datagram to `224.0.0.1:50030`)
//!
//! ```text
//! 2c 0a 00 00              msg_id = 0x0a2c (LE)
//! 72 00 00 00              payload_len = 114
//! ff ff ff ff ff ff        magic packet: synchronisation stream
//! 14 8f 21 30 43 77  ×16   magic packet: the Garmin wake address
//! "GMN1\0" "SYS\0" "20\0"
//! ```

use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::time::sleep;

use crate::network;

use super::cdm_heartbeat::garmin_eligible_nics;
use super::protocol::{GMN_HEADER_LEN, MSG_WAKE, WAKE_ADDRESS};

/// The address every Garmin chartplotter puts in its magic packet.
const WAKE_MAC: [u8; 6] = [0x14, 0x8f, 0x21, 0x30, 0x43, 0x77];

/// A Wake-on-LAN magic packet repeats the address this many times.
const MAGIC_REPEATS: usize = 16;

/// The fields after the magic packet, each NUL-terminated.
const TRAILER: &[u8] = b"GMN1\0SYS\x0020\0";

/// Wake-up frames sent one second apart when mayara starts, as a
/// chartplotter does when it powers on.
const BURST_COUNT: u32 = 10;
const BURST_INTERVAL: Duration = Duration::from_secs(1);

/// The interval after the burst. A chartplotter picks one between 10 and
/// 14 s; the radar only powers down after about two minutes without one.
const STEADY_INTERVAL: Duration = Duration::from_secs(11);

/// Build the 122-byte wake-up frame.
pub(crate) fn build() -> Vec<u8> {
    let payload_len = 6 + WAKE_MAC.len() * MAGIC_REPEATS + TRAILER.len();
    let mut frame = Vec::with_capacity(GMN_HEADER_LEN + payload_len);
    frame.extend_from_slice(&MSG_WAKE.to_le_bytes());
    frame.extend_from_slice(&(payload_len as u32).to_le_bytes());
    frame.extend_from_slice(&[0xff; 6]);
    for _ in 0..MAGIC_REPEATS {
        frame.extend_from_slice(&WAKE_MAC);
    }
    frame.extend_from_slice(TRAILER);
    frame
}

/// Send the wake-up frame on every NIC with an address in the Garmin subnet,
/// for as long as mayara runs. NICs are enumerated on each send, so an
/// interface configured later joins in without a restart.
pub(crate) async fn run() {
    let frame = build();
    let mut sent: u32 = 0;
    loop {
        for nic in garmin_eligible_nics() {
            send_one(&frame, &nic).await;
        }
        sent = sent.saturating_add(1);
        sleep(if sent < BURST_COUNT {
            BURST_INTERVAL
        } else {
            STEADY_INTERVAL
        })
        .await;
    }
}

async fn send_one(frame: &[u8], nic: &Ipv4Addr) {
    match network::create_connected_send(&WAKE_ADDRESS, nic) {
        Ok(sock) => {
            if let Err(e) = sock.send(frame).await {
                log::debug!("Garmin wake-up via {}: send failed: {}", nic, e);
            }
        }
        Err(e) => {
            log::debug!("Garmin wake-up via {}: socket creation failed: {}", nic, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame a GPSMAP 723xsv sends, byte for byte.
    const CHARTPLOTTER_FRAME: &str = "2c0a000072000000ffffffffffff\
        148f21304377148f21304377148f21304377148f21304377\
        148f21304377148f21304377148f21304377148f21304377\
        148f21304377148f21304377148f21304377148f21304377\
        148f21304377148f21304377148f21304377148f21304377\
        474d4e310053595300323000";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_frame_is_the_chartplotters() {
        assert_eq!(hex(&build()), CHARTPLOTTER_FRAME);
    }

    #[test]
    fn the_header_announces_the_payload() {
        let frame = build();
        assert_eq!(frame.len(), 122);
        assert_eq!(
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            MSG_WAKE
        );
        assert_eq!(
            u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize,
            frame.len() - GMN_HEADER_LEN
        );
    }
}
