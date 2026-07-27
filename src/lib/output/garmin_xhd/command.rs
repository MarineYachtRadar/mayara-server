//! xHD command listener (unicast UDP port 50101).
//!
//! Receives control commands from the Garmin plotter, updates local shared
//! state, echoes the command back on the status stream so the plotter sees its
//! command acknowledged, and forwards the command to the radar's control
//! handler via `SharedControls::send_to_command_handler()`.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

use serde_json::Value;

use crate::brand::garmin::protocol::*;
use crate::radar::Power;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};

use super::convert::nearest_xhd_range;
use super::{SharedState, pkt_u8, pkt_u16, pkt_u32};

/// How long after a range command to ignore spoke-range updates.
const RANGE_LOCK_SECS: u64 = 5;

/// Default rain gain sent when the plotter turns rain on without a gain value.
const RAIN_DEFAULT_GAIN_PCT: u8 = 50;

fn send_control(
    controls: &SharedControls,
    id: ControlId,
    value: Option<Value>,
    auto: Option<bool>,
) {
    let cv = ControlValue {
        id,
        value,
        units: None,
        auto,
        auto_value: None,
        end_value: None,
        start_distance: None,
        end_distance: None,
        enabled: None,
        allowed: None,
        error: None,
        x1: None,
        y1: None,
        x2: None,
        y2: None,
        width: None,
        timestamp: None,
    };
    let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(1);
    if let Err(e) = controls.send_to_command_handler(cv, reply_tx) {
        log::warn!("GarminXhd command: send_to_command_handler failed: {e}");
    }
}

pub(super) async fn run(
    local_ip: Ipv4Addr,
    state: Arc<Mutex<SharedState>>,
    controls: SharedControls,
    mut stop: oneshot::Receiver<()>,
) {
    let sock = match UdpSocket::bind((local_ip, COMMAND_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("GarminXhd command: failed to bind {local_ip}:{COMMAND_PORT}: {e}");
            return;
        }
    };

    // Echo socket for sending command acknowledgements on the status multicast stream.
    let echo_sock = match UdpSocket::bind((local_ip, 0u16)).await {
        Ok(s) => {
            // Set TTL=1 via the underlying std socket.
            if let Ok(std_sock) = s.into_std() {
                std_sock.set_multicast_ttl_v4(1).ok();
                tokio::net::UdpSocket::from_std(std_sock).ok()
            } else {
                None
            }
        }
        Err(e) => {
            log::warn!("GarminXhd command: echo socket failed: {e}");
            None
        }
    };

    log::info!("GarminXhd command: listening on {local_ip}:{COMMAND_PORT}");

    let mut buf = [0u8; 4096];

    loop {
        let n = tokio::select! {
            biased;
            _ = &mut stop => break,
            res = sock.recv_from(&mut buf) => match res {
                Ok((n, _)) => n,
                Err(e) => {
                    log::warn!("GarminXhd command: recv error: {e}");
                    continue;
                }
            },
        };

        if n < 8 {
            continue;
        }
        let msg_id = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let pay_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        if n < 8 + pay_len {
            continue;
        }
        let payload = &buf[8..8 + pay_len];

        log::info!("GarminXhd CMD msg=0x{msg_id:04x} pay_len={pay_len}");

        let echo_pkt: Option<Vec<u8>> = match msg_id {
            x if x == MSG_TRANSMIT_MODE && pay_len >= 1 => {
                let on = payload[0] != 0;
                {
                    let mut st = state.lock().unwrap();
                    st.transmitting = on;
                }
                let power = if on { Power::Transmit } else { Power::Standby };
                send_control(
                    &controls,
                    ControlId::Power,
                    Some(Value::Number(serde_json::Number::from(power as u32))),
                    None,
                );
                let scanner_state: u8 = if on {
                    STATE_TRANSMIT as u8
                } else {
                    STATE_STANDBY as u8
                };
                // Echo both transmit mode and scanner state
                let mut combined = pkt_u8(MSG_TRANSMIT_MODE, payload[0]);
                combined.extend_from_slice(&pkt_u8(MSG_SCANNER_STATE, scanner_state));
                Some(combined)
            }

            x if x == MSG_RANGE_A && pay_len >= 4 => {
                let meters = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let xhd_range = nearest_xhd_range(meters);
                {
                    let mut st = state.lock().unwrap();
                    st.range_m = xhd_range;
                    st.range_lock_until = Instant::now() + Duration::from_secs(RANGE_LOCK_SECS);
                }
                log::info!(
                    "GarminXhd CMD range: plotter={}m → xhd={}m",
                    meters,
                    xhd_range
                );
                send_control(
                    &controls,
                    ControlId::Range,
                    Some(Value::Number(serde_json::Number::from(xhd_range))),
                    None,
                );
                // Echo exact plotter value so plotter sees its command confirmed.
                Some(pkt_u32(MSG_RANGE_A, meters))
            }

            x if x == MSG_RANGE_A_GAIN_MODE && pay_len >= 1 => {
                let mode = payload[0]; // 0=manual, 2=auto
                let auto = mode == 2;
                send_control(&controls, ControlId::Gain, None, Some(auto));
                Some(pkt_u8(MSG_RANGE_A_GAIN_MODE, mode))
            }

            x if x == MSG_RANGE_A_GAIN && pay_len >= 2 => {
                let raw = u16::from_le_bytes([payload[0], payload[1]]);
                let pct = raw as f64 / 100.0;
                send_control(
                    &controls,
                    ControlId::Gain,
                    Some(Value::Number(
                        serde_json::Number::from_f64(pct).unwrap_or(serde_json::Number::from(50)),
                    )),
                    Some(false),
                );
                Some(pkt_u16(MSG_RANGE_A_GAIN, raw))
            }

            x if x == MSG_RANGE_A_SEA_MODE && pay_len >= 1 => {
                let mode = payload[0]; // 0=off, 1=manual, 2=auto
                let auto = mode == 2;
                let value = if mode == 0 {
                    Some(Value::Number(serde_json::Number::from(0)))
                } else {
                    None
                };
                send_control(&controls, ControlId::Sea, value, Some(auto));
                Some(pkt_u8(MSG_RANGE_A_SEA_MODE, mode))
            }

            x if x == MSG_RANGE_A_SEA_GAIN && pay_len >= 2 => {
                let raw = u16::from_le_bytes([payload[0], payload[1]]);
                let pct = raw as f64 / 100.0;
                send_control(
                    &controls,
                    ControlId::Sea,
                    Some(Value::Number(
                        serde_json::Number::from_f64(pct).unwrap_or(serde_json::Number::from(0)),
                    )),
                    Some(false),
                );
                Some(pkt_u16(MSG_RANGE_A_SEA_GAIN, raw))
            }

            x if x == MSG_RANGE_A_RAIN_MODE && pay_len >= 1 => {
                let on = payload[0] != 0;
                {
                    let mut st = state.lock().unwrap();
                    st.rain_mode = payload[0];
                    if !on {
                        st.rain_gain = 0;
                    }
                }
                // Garmin has no "rain auto" — map off→auto on Furuno, on→50%
                if on {
                    send_control(
                        &controls,
                        ControlId::Rain,
                        Some(Value::Number(serde_json::Number::from(
                            RAIN_DEFAULT_GAIN_PCT,
                        ))),
                        Some(false),
                    );
                } else {
                    send_control(&controls, ControlId::Rain, None, Some(true));
                }
                Some(pkt_u8(MSG_RANGE_A_RAIN_MODE, payload[0]))
            }

            x if x == MSG_RANGE_A_RAIN_GAIN && pay_len >= 2 => {
                let raw = u16::from_le_bytes([payload[0], payload[1]]);
                let pct = raw as f64 / 100.0;
                {
                    let mut st = state.lock().unwrap();
                    st.rain_mode = 1;
                    st.rain_gain = raw;
                }
                send_control(
                    &controls,
                    ControlId::Rain,
                    Some(Value::Number(
                        serde_json::Number::from_f64(pct).unwrap_or(serde_json::Number::from(0)),
                    )),
                    Some(false),
                );
                Some(pkt_u16(MSG_RANGE_A_RAIN_GAIN, raw))
            }

            x if x == MSG_RPM_MODE => {
                // Furuno doesn't support RPM mode — ignore silently.
                None
            }

            _ => {
                log::debug!("GarminXhd CMD: unhandled msg=0x{msg_id:04x}");
                None
            }
        };

        if let (Some(pkt), Some(sock)) = (echo_pkt, &echo_sock) {
            if let Err(e) = sock.send_to(&pkt, REPORT_ADDRESS).await {
                log::warn!("GarminXhd command: echo failed: {e}");
            }
        }
    }
}
