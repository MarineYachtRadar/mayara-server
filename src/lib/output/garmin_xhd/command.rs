//! Control commands from the plotter (`<local addr>:50101`).
//!
//! The plotter sends one packet per control it changes, in the same TLV form
//! the radar reports its state in. Each is translated into a mayara control
//! change on the source radar and echoed back on the report stream, which is
//! how a real radar acknowledges a command.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use serde_json::json;
use socket2::SockAddr;
use tokio::net::UdpSocket;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::garmin::protocol::*;
use crate::radar::settings::{ControlId, ControlValue};
use crate::radar::{Power, RadarError};

use super::convert::nearest_xhd_range;
use super::{GAIN_MODE_AUTO, SEA_MODE_AUTO, SEA_MODE_MANUAL, SEA_MODE_OFF, Shared, packet_u8};

/// Rain clutter gain used when the plotter switches rain filtering on without
/// saying how much. The xHD protocol has a separate on/off and gain command,
/// and the plotter sends the gain only once the user moves the slider.
const RAIN_DEFAULT_PERCENT: f64 = 50.0;

/// Depth of the reply channel the control system answers on. Replies only
/// carry errors worth logging, so a command whose reply is dropped because
/// the channel is full has still been sent to the radar.
const REPLY_CHANNEL_DEPTH: usize = 16;

/// Largest command the plotter sends is a few dozen bytes; this is room to
/// spare for anything else that lands on the port.
const RECEIVE_BUFFER: usize = 2048;

pub(super) async fn run(
    local_addr: Ipv4Addr,
    shared: Arc<Shared>,
    subsys: &mut SubsystemHandle,
) -> Result<(), RadarError> {
    let socket = match listen(local_addr) {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("Garmin xHD command: cannot listen on {local_addr}:{COMMAND_PORT}: {e}");
            return Ok(());
        }
    };
    log::debug!("Garmin xHD command: listening on {local_addr}:{COMMAND_PORT}");

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<ControlValue>(REPLY_CHANNEL_DEPTH);
    let mut buffer = [0u8; RECEIVE_BUFFER];

    loop {
        tokio::select! {
            biased;
            _ = subsys.on_shutdown_requested() => return Ok(()),
            Some(reply) = reply_rx.recv() => {
                if let Some(error) = reply.error {
                    log::warn!("Garmin xHD command: {:?} rejected: {error}", reply.id);
                }
            }
            received = socket.recv_from(&mut buffer) => {
                let length = match received {
                    Ok((length, _)) => length,
                    Err(e) => {
                        log::warn!("Garmin xHD command: receive failed: {e}");
                        continue;
                    }
                };
                handle(&buffer[..length], &shared, &reply_tx);
            }
        }
    }
}

fn listen(local_addr: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = crate::network::new_socket()?;
    socket.bind(&SockAddr::from(SocketAddr::new(
        IpAddr::V4(local_addr),
        COMMAND_PORT,
    )))?;
    UdpSocket::from_std(socket.into())
}

/// Decode one command packet and act on it.
fn handle(datagram: &[u8], shared: &Shared, reply_tx: &tokio::sync::mpsc::Sender<ControlValue>) {
    if datagram.len() < GMN_HEADER_LEN {
        return;
    }
    let message_id = u32::from_le_bytes(datagram[0..4].try_into().unwrap());
    let payload_len = u32::from_le_bytes(datagram[4..8].try_into().unwrap()) as usize;
    let Some(payload) = GMN_HEADER_LEN
        .checked_add(payload_len)
        .and_then(|end| datagram.get(GMN_HEADER_LEN..end))
    else {
        log::warn!("Garmin xHD command: 0x{message_id:04x} truncated");
        return;
    };

    let byte = payload.first().copied();
    let word = payload
        .get(0..2)
        .map(|p| u16::from_le_bytes(p.try_into().unwrap()));
    let long = payload
        .get(0..4)
        .map(|p| u32::from_le_bytes(p.try_into().unwrap()));

    // Percent as the enhanced protocol carries it: percent × 100.
    let percent = || word.map(|w| w as f64 / GAIN_SCALE as f64);

    let control = match message_id {
        MSG_TRANSMIT_MODE => byte.map(|on| {
            let power = if on != 0 {
                Power::Transmit
            } else {
                Power::Standby
            };
            ControlValue::new(ControlId::Power, json!(power as u32))
        }),

        MSG_RANGE_A => long.map(|meters| {
            // The plotter can only pick a range mayara offered it, but a
            // radar with a range table of its own will land on whatever it
            // has that is closest.
            shared.set_pending_range(nearest_xhd_range(meters));
            ControlValue::new(ControlId::Range, json!(meters))
        }),

        MSG_RANGE_A_GAIN_MODE => byte.map(|mode| auto(ControlId::Gain, mode == GAIN_MODE_AUTO)),
        MSG_RANGE_A_GAIN => percent().map(|p| manual(ControlId::Gain, p)),

        MSG_RANGE_A_SEA_MODE => byte.and_then(|mode| match mode {
            SEA_MODE_AUTO => Some(auto(ControlId::Sea, true)),
            SEA_MODE_MANUAL => Some(auto(ControlId::Sea, false)),
            SEA_MODE_OFF => Some(manual(ControlId::Sea, 0.0)),
            // Switching sea clutter off is not the safe reading of a mode we
            // do not know: it is the one that quietly changes the picture.
            _ => {
                log::warn!("Garmin xHD command: unknown sea clutter mode {mode}");
                None
            }
        }),
        MSG_RANGE_A_SEA_GAIN => percent().map(|p| manual(ControlId::Sea, p)),

        // Rain filtering has no automatic mode on an xHD: it is off, or it is
        // set to a level. Switching it on before the user has picked a level
        // has to mean something, so it means half.
        MSG_RANGE_A_RAIN_MODE => byte.map(|mode| {
            let percent = if mode != 0 {
                shared
                    .control(ControlId::Rain)
                    .filter(|&v| v > 0.0)
                    .unwrap_or(RAIN_DEFAULT_PERCENT)
            } else {
                0.0
            };
            manual(ControlId::Rain, percent)
        }),
        MSG_RANGE_A_RAIN_GAIN => percent().map(|p| manual(ControlId::Rain, p)),

        _ => {
            log::debug!("Garmin xHD command: ignoring 0x{message_id:04x}");
            return;
        }
    };

    // Either the payload was too short for the value this message carries, or
    // it held a value we do not understand — which is logged where it is seen.
    let Some(control) = control else {
        log::warn!("Garmin xHD command: 0x{message_id:04x} carries nothing we can act on");
        return;
    };

    log::debug!("Garmin xHD command: 0x{message_id:04x} -> {control:?}");
    if let Err(e) = shared
        .controls
        .process_client_request(control, reply_tx.clone())
    {
        log::warn!("Garmin xHD command: 0x{message_id:04x} cannot be forwarded: {e}");
        return;
    }

    // Acknowledge by echoing the command, exactly as a real radar reports
    // back the value it was told to take. The plotter waits for this before
    // it shows the new setting.
    shared.echo(datagram.to_vec());
    if message_id == MSG_TRANSMIT_MODE {
        let state = if byte.is_some_and(|on| on != 0) {
            STATE_TRANSMIT
        } else {
            STATE_STANDBY
        };
        shared.echo(packet_u8(MSG_SCANNER_STATE, state as u8));
    }
}

/// A control set to a value, in whatever units mayara models it in.
fn manual(id: ControlId, value: f64) -> ControlValue {
    ControlValue {
        auto: Some(false),
        ..ControlValue::new(id, json!(value))
    }
}

/// A control switched between automatic and manual, leaving its value alone.
fn auto(id: ControlId, automatic: bool) -> ControlValue {
    ControlValue {
        auto: Some(automatic),
        value: None,
        ..ControlValue::new(id, serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::garmin_xhd::tests::controls;
    use crate::output::garmin_xhd::{packet_u16, packet_u32};

    /// Feed one command packet to the bridge and return the control change it
    /// asked the radar for, if any.
    fn command(shared: &Shared, packet: &[u8]) -> Option<ControlValue> {
        let mut updates = shared.controls.control_update_subscribe();
        let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(1);
        handle(packet, shared, &reply_tx);
        updates.try_recv().ok().map(|update| update.control_value)
    }

    #[test]
    fn transmit_button_powers_the_radar() {
        let (shared, mut echo_rx) = Shared::new(controls());

        let control = command(&shared, &packet_u8(MSG_TRANSMIT_MODE, 1)).expect("control change");
        assert_eq!(control.id, ControlId::Power);
        assert_eq!(control.as_f64().unwrap(), Power::Transmit as u32 as f64);

        // The plotter is told the radar took the command, twice over: the
        // command itself and the scanner state that follows from it.
        assert_eq!(echo_rx.try_recv().unwrap(), packet_u8(MSG_TRANSMIT_MODE, 1));
        assert_eq!(
            echo_rx.try_recv().unwrap(),
            packet_u8(MSG_SCANNER_STATE, STATE_TRANSMIT as u8)
        );

        let control = command(&shared, &packet_u8(MSG_TRANSMIT_MODE, 0)).expect("control change");
        assert_eq!(control.as_f64().unwrap(), Power::Standby as u32 as f64);
    }

    #[test]
    fn range_is_forwarded_and_reported_back_before_the_radar_confirms() {
        let (shared, _echo_rx) = Shared::new(controls());

        let control = command(&shared, &packet_u32(MSG_RANGE_A, 5556)).expect("control change");
        assert_eq!(control.id, ControlId::Range);
        assert_eq!(control.as_f64().unwrap(), 5556.0);
        assert_eq!(shared.range_m(), 5556);
    }

    #[test]
    fn gain_arrives_as_a_percentage() {
        let (shared, _echo_rx) = Shared::new(controls());

        let control =
            command(&shared, &packet_u16(MSG_RANGE_A_GAIN, 7500)).expect("control change");
        assert_eq!(control.id, ControlId::Gain);
        assert_eq!(control.as_f64().unwrap(), 75.0);
        assert_eq!(control.auto, Some(false));

        let control = command(
            &shared,
            &packet_u8(MSG_RANGE_A_GAIN_MODE, super::GAIN_MODE_AUTO),
        )
        .expect("control change");
        assert_eq!(control.auto, Some(true));
        assert!(control.value.is_none(), "auto must not change the value");
    }

    #[test]
    fn sea_clutter_modes_reach_the_radar() {
        let (shared, _echo_rx) = Shared::new(controls());

        let off = command(&shared, &packet_u8(MSG_RANGE_A_SEA_MODE, 0)).expect("control change");
        assert_eq!(off.as_f64().unwrap(), 0.0);
        assert_eq!(off.auto, Some(false));

        let auto = command(
            &shared,
            &packet_u8(MSG_RANGE_A_SEA_MODE, super::SEA_MODE_AUTO),
        )
        .expect("control change");
        assert_eq!(auto.auto, Some(true));

        let manual = command(
            &shared,
            &packet_u8(MSG_RANGE_A_SEA_MODE, super::SEA_MODE_MANUAL),
        )
        .expect("control change");
        assert_eq!(manual.auto, Some(false));
        assert!(manual.value.is_none(), "manual must not change the value");

        // A mode we do not know must not be read as "switch sea clutter off",
        // which would quietly change the picture.
        assert!(command(&shared, &packet_u8(MSG_RANGE_A_SEA_MODE, 9)).is_none());
    }

    #[test]
    fn rain_filtering_switched_on_without_a_level_picks_one() {
        let (shared, _echo_rx) = Shared::new(controls());

        let on = command(&shared, &packet_u8(MSG_RANGE_A_RAIN_MODE, 1)).expect("control change");
        assert_eq!(on.id, ControlId::Rain);
        assert_eq!(on.as_f64().unwrap(), RAIN_DEFAULT_PERCENT);

        shared.controls.set(&ControlId::Rain, 20., None).unwrap();
        let on = command(&shared, &packet_u8(MSG_RANGE_A_RAIN_MODE, 1)).expect("control change");
        assert_eq!(on.as_f64().unwrap(), 20.0, "a level already set is kept");

        let off = command(&shared, &packet_u8(MSG_RANGE_A_RAIN_MODE, 0)).expect("control change");
        assert_eq!(off.as_f64().unwrap(), 0.0);
    }

    #[test]
    fn unhandled_and_malformed_commands_are_ignored() {
        let (shared, _echo_rx) = Shared::new(controls());

        assert!(command(&shared, &packet_u8(MSG_RPM_MODE, 1)).is_none());
        assert!(command(&shared, &[]).is_none());
        assert!(command(&shared, &[0x19, 0x09, 0, 0]).is_none());
        // Says it carries four bytes but carries none.
        assert!(command(&shared, &[0x1e, 0x09, 0, 0, 4, 0, 0, 0]).is_none());
    }
}
