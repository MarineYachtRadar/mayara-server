//! xHD status/report stream broadcaster (port 50100).
//!
//! Broadcasts ~40 individual status packets per second to `239.254.2.0:50100`.
//! The plotter uses these to track range, gain, sea/rain clutter, transmit
//! state, and other settings. Every value must match what the spoke packets
//! carry — mismatches cause the plotter to crash or freeze.

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

use crate::brand::garmin::protocol::{self, *};
use crate::radar::Power;
use crate::radar::settings::{ControlId, SharedControls};

use super::SharedState;
use super::convert::XHD_RANGES_M;

fn pkt_u8(msg_id: u32, value: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(9);
    p.extend_from_slice(&msg_id.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes());
    p.push(value);
    p
}

fn pkt_u16(msg_id: u32, value: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(10);
    p.extend_from_slice(&msg_id.to_le_bytes());
    p.extend_from_slice(&2u32.to_le_bytes());
    p.extend_from_slice(&value.to_le_bytes());
    p
}

fn pkt_u32(msg_id: u32, value: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&msg_id.to_le_bytes());
    p.extend_from_slice(&4u32.to_le_bytes());
    p.extend_from_slice(&value.to_le_bytes());
    p
}

fn pkt_i32(msg_id: u32, value: i32) -> Vec<u8> {
    pkt_u32(msg_id, value as u32)
}

fn build_capability_pkt() -> Vec<u8> {
    // 0x09B1 capability bitmap — bytes captured from a real GMR xHD.
    // Identical to bridge.py's _XHD_CAP_BODY and mayara capabilities.rs SAMPLE_0X09B1_BODY.
    const CAP_BODY: [u8; 48] = [
        0x01, 0x00, 0x30, 0x00, 0x9d, 0x00, 0x0a, 0x00, // header prefix
        0xdf, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // word 0
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // word 1
        0xfd, 0xff, 0xff, 0x07, 0x00, 0x00, 0x00, 0x00, // word 2
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // word 3
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // word 4
    ];
    let mut p = Vec::with_capacity(8 + 48);
    p.extend_from_slice(&MSG_CAPABILITY.to_le_bytes());
    p.extend_from_slice(&(CAP_BODY.len() as u32).to_le_bytes());
    p.extend_from_slice(&CAP_BODY);
    p
}

fn build_range_table_pkt() -> Vec<u8> {
    let count = XHD_RANGES_M.len() as u32;
    let body_len = 8 + count * 4;
    let mut body: Vec<u8> = Vec::with_capacity(body_len as usize);
    body.extend_from_slice(&1u16.to_le_bytes()); // version
    body.extend_from_slice(&(body_len as u16).to_le_bytes()); // length
    body.extend_from_slice(&count.to_le_bytes());
    for &m in XHD_RANGES_M {
        body.extend_from_slice(&m.to_le_bytes());
    }
    let mut p = Vec::with_capacity(8 + body.len());
    p.extend_from_slice(&MSG_RANGE_TABLE.to_le_bytes());
    p.extend_from_slice(&(body.len() as u32).to_le_bytes());
    p.extend_from_slice(&body);
    p
}

fn build_status_packets(state: &SharedState, controls: &SharedControls) -> Vec<Vec<u8>> {
    // Prefer the live Power value from controls (updated by Furuno reports);
    // fall back to SharedState which is set by plotter commands.
    let transmitting = controls
        .get(&ControlId::Power)
        .and_then(|c| c.value)
        .map(|v| v as u32 == Power::Transmit as u32)
        .unwrap_or(state.transmitting);
    let range_m = state.range_m;

    // Read gain/sea from SharedControls; rain is cached in SharedState (see command.rs).
    let gain_auto = controls
        .get(&ControlId::Gain)
        .and_then(|c| c.auto)
        .unwrap_or(true);
    let gain_val = controls
        .get(&ControlId::Gain)
        .and_then(|c| c.value)
        .map(|v| (v * 100.0) as u16)
        .unwrap_or(5000);
    let gain_mode: u8 = if gain_auto { 2 } else { 0 };

    let sea_auto = controls
        .get(&ControlId::Sea)
        .and_then(|c| c.auto)
        .unwrap_or(true);
    let sea_val = controls
        .get(&ControlId::Sea)
        .and_then(|c| c.value)
        .unwrap_or(0.0);
    let sea_mode: u8 = if sea_auto {
        2
    } else if sea_val > 0.0 {
        1
    } else {
        0
    };
    let sea_gain = (sea_val * 100.0) as u16;

    let rain_mode = state.rain_mode;
    let rain_gain = state.rain_gain;

    let scanner_state: u8 = if transmitting {
        STATE_TRANSMIT as u8
    } else {
        STATE_STANDBY as u8
    };

    vec![
        pkt_u8(MSG_SCANNER_STATE, scanner_state),
        pkt_u32(MSG_STATE_CHANGE, 0),
        pkt_u8(MSG_TRANSMIT_MODE, u8::from(transmitting)),
        pkt_u8(MSG_TRANSMIT_MODE_CURRENT, u8::from(transmitting)),
        pkt_u8(MSG_SCAN_TYPE, 1),  // single range
        pkt_u8(0x0912, 0),         // scan_type_b
        pkt_u8(0x0913, 0),         // scan_type_c
        pkt_u8(MSG_RANGE_MODE, 0), // single range mode
        pkt_u32(MSG_RANGE_A, range_m),
        pkt_u32(MSG_RANGE_B, 926), // secondary range unused (1/2 NM)
        pkt_u8(MSG_RANGE_A_GAIN_MODE, gain_mode),
        pkt_u16(MSG_RANGE_A_GAIN, gain_val),
        pkt_u8(MSG_RANGE_A_SEA_MODE, sea_mode),
        pkt_u16(MSG_RANGE_A_SEA_GAIN, sea_gain),
        pkt_u8(MSG_RANGE_A_SEA_STATE, 0),
        pkt_u8(MSG_RANGE_A_RAIN_MODE, rain_mode),
        pkt_u16(MSG_RANGE_A_RAIN_GAIN, rain_gain),
        pkt_u8(MSG_DITHER_MODE, 0),
        pkt_u8(MSG_NOISE_BLANKER, 0),
        pkt_i32(MSG_BEARING_ALIGNMENT, 0),
        pkt_u8(MSG_NO_TX_ZONE_1_MODE, 0),
        pkt_u8(MSG_SENTRY_MODE, 0),
        pkt_u16(MSG_SENTRY_STANDBY_TIME, 0),
        pkt_u16(MSG_SENTRY_TRANSMIT_TIME, 0),
        pkt_u8(MSG_AFC_MODE, 1), // auto AFC
        pkt_u8(MSG_RPM_MODE, 0),
        pkt_u16(0x0928, 350),        // antenna_height 3.5 m
        pkt_u16(0x0929, 150),        // antenna_forward 1.5 m
        pkt_u16(0x092a, 0),          // antenna_starboard
        pkt_u16(0x092b, 0x2134),     // antenna_power (from real capture)
        pkt_u32(0x0994, 0x000009b4), // trigger_period
        pkt_u32(0x0995, 0x00001710), // trigger_delay
        pkt_u32(0x0996, 0x000016f8), // trigger_period_b
        pkt_u8(0x0951, 2),           // tune_fine
        pkt_u8(0x0952, 0),           // tune_coarse
        pkt_u8(0x0953, 0),           // tune_mode
        pkt_u8(0x099c, 1),           // status seen=1 in transmit
        pkt_u8(0x099d, 1),
        pkt_u8(0x099e, 0),
        pkt_u32(MSG_MAX_RANGE, *XHD_RANGES_M.last().unwrap()),
        pkt_u32(MSG_SPOKE_TOTAL, 2400),
        pkt_u16(MSG_INPUT_VOLTAGE, 120),
        build_capability_pkt(),
        build_range_table_pkt(),
    ]
}

pub(super) async fn run(
    local_ip: Ipv4Addr,
    state: Arc<Mutex<SharedState>>,
    controls: SharedControls,
    mut stop: oneshot::Receiver<()>,
) {
    let sock = match UdpSocket::bind((local_ip, protocol::REPORT_PORT)) {
        Ok(s) => s,
        Err(e) => {
            log::error!("GarminXhd status: failed to bind socket: {e}");
            return;
        }
    };
    if let Err(e) = sock.set_multicast_ttl_v4(1) {
        log::warn!("GarminXhd status: could not set TTL=1: {e}");
    }

    let dest = REPORT_ADDRESS;

    loop {
        let pkts = {
            let st = state.lock().unwrap();
            build_status_packets(&st, &controls)
        };
        for pkt in &pkts {
            if let Err(e) = sock.send_to(pkt, dest) {
                log::warn!("GarminXhd status: send failed: {e}");
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = &mut stop => break,
        }
    }
}
