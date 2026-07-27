//! CDM heartbeat broadcaster for the virtual xHD radar.
//!
//! Sends a 34-byte `0x038e` V2 heartbeat to `239.254.2.2:50050` every second
//! for the first 30 transmissions, then every 5 seconds. The Garmin plotter
//! uses this to discover the radar in Marine Network.

use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

use crate::brand::garmin::protocol::{
    CDM_HEARTBEAT_ADDRESS, CDM_HEARTBEAT_PORT, MSG_CDM_HEARTBEAT,
};

const PRODUCT_ID_XHD: u16 = 0x06d0;
const SYC_GROUP_ID: u8 = 6;
const PRODUCT_SUBTYPE: u8 = 5;
const SERVICE_CLASS: u8 = 1;
const SERVICE_VER: u16 = 2;
const SERVICE_ID: u32 = 0x08d40aa0;

fn build_heartbeat(seq: u32) -> Vec<u8> {
    let mut payload: Vec<u8> = Vec::with_capacity(26);
    payload.push(2); // version_marker = 2
    payload.push(0); // pad
    payload.extend_from_slice(&PRODUCT_ID_XHD.to_le_bytes());
    payload.push(0); // simulator_mode
    payload.push(PRODUCT_SUBTYPE);
    payload.push(SYC_GROUP_ID);
    payload.push(1); // constant
    payload.push(1); // service_count = 1
    payload.extend_from_slice(&[0u8; 3]); // pad

    // service entry
    payload.push(SERVICE_CLASS);
    payload.push(0); // instance
    payload.extend_from_slice(&SERVICE_VER.to_le_bytes());
    payload.extend_from_slice(&SERVICE_ID.to_le_bytes());

    // tail: tag=1 len=4, then sequence
    payload.push(1);
    payload.push(4);
    payload.extend_from_slice(&seq.to_le_bytes());

    let mut pkt = Vec::with_capacity(8 + payload.len());
    pkt.extend_from_slice(&MSG_CDM_HEARTBEAT.to_le_bytes());
    pkt.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    pkt.extend_from_slice(&payload);
    pkt
}

pub(super) async fn run(local_ip: Ipv4Addr, mut stop: oneshot::Receiver<()>) {
    let sock = match UdpSocket::bind((local_ip, CDM_HEARTBEAT_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("GarminXhd CDM: failed to bind socket: {e}");
            return;
        }
    };
    sock.set_multicast_ttl_v4(1).ok();

    let dest = CDM_HEARTBEAT_ADDRESS;
    let mut seq: u32 = 0;

    loop {
        let pkt = build_heartbeat(seq);
        if let Err(e) = sock.send_to(&pkt, dest).await {
            log::warn!("GarminXhd CDM: send failed: {e}");
        }
        log::debug!("GarminXhd CDM heartbeat seq={seq}");
        seq = seq.wrapping_add(1);

        let delay = if seq <= 30 {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = &mut stop => break,
        }
    }
}
