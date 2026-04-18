use async_trait::async_trait;
use std::net::SocketAddrV4;
use tokio::net::UdpSocket;

use super::protocol::*;
use crate::brand::CommandSender;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

/// Scale a percentage (0–100) to a wire byte (0–255) with rounding.
fn pct_to_u8(pct: f64) -> u8 {
    (pct.clamp(0.0, 100.0) * 255.0 / 100.0).round() as u8
}

pub(crate) struct Command {
    key: String,
    socket: Option<UdpSocket>,
    controls: SharedControls,
}

impl Command {
    pub(crate) fn new(info: &RadarInfo) -> Self {
        Command {
            key: info.key(),
            socket: None,
            controls: info.controls.clone(),
        }
    }

    pub(crate) fn set_socket(&mut self, socket: UdpSocket) {
        self.socket = Some(socket);
    }

    /// Send a raw byte slice to the radar via broadcast.
    async fn send_raw(&self, data: &[u8]) -> Result<(), RadarError> {
        match &self.socket {
            Some(s) => {
                let dest = SocketAddrV4::new(std::net::Ipv4Addr::BROADCAST, RADAR_PORT);
                s.send_to(data, dest).await.map_err(RadarError::Io)?;
                Ok(())
            }
            None => Err(RadarError::NotConnected),
        }
    }

    /// Send a 4-byte SetByte command: `& cmd value \r`
    async fn set_byte(&self, cmd: u8, value: u8) -> Result<(), RadarError> {
        let packet = [CONTROL_PREFIX, cmd, value, PACKET_END];
        log::trace!(
            "{}: SetByte cmd=0x{:02X} val=0x{:02X}",
            self.key,
            cmd,
            value
        );
        self.send_raw(&packet).await
    }

    /// Send a 5-byte SetWord command: `& cmd hi lo \r`
    async fn set_word(&self, cmd: u8, value: u16) -> Result<(), RadarError> {
        let packet = [
            CONTROL_PREFIX,
            cmd,
            (value >> 8) as u8,
            (value & 0xFF) as u8,
            PACKET_END,
        ];
        log::trace!(
            "{}: SetWord cmd=0x{:02X} val=0x{:04X}",
            self.key,
            cmd,
            value
        );
        self.send_raw(&packet).await
    }

    /// Send the startup info request sequence.
    pub(crate) async fn send_startup(&self) -> Result<(), RadarError> {
        log::debug!("{}: Sending startup info request", self.key);
        self.send_raw(&STARTUP_REQUEST).await
    }

    /// Send a keep-alive packet.
    pub(crate) async fn send_keepalive(&self) -> Result<(), RadarError> {
        log::trace!("{}: Sending keep-alive", self.key);
        self.send_raw(&KEEPALIVE_PACKET).await
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        let value = match cv.as_i32() {
            Ok(v) => v,
            Err(_) if cv.auto.is_some() && cv.value.is_none() => controls
                .get(&cv.id)
                .and_then(|c| c.value)
                .map(|v| v as i32)
                .unwrap_or(0),
            Err(e) => return Err(e),
        };
        let value_f64 = cv.as_f64().unwrap_or(value as f64);

        match cv.id {
            ControlId::Power => {
                if value == Power::Transmit as u32 as i32 {
                    // Send transfer mode 0x22 to begin transmitting.
                    // The radar starts transmitting upon receiving this
                    // and confirms with a 0x74=0x11 response.
                    self.set_byte(CMD_TRANSFER_MODE, 0x22).await
                } else if value == Power::Standby as u32 as i32 {
                    // Send transfer mode 0x00 to go to standby.
                    self.set_byte(CMD_TRANSFER_MODE, 0x00).await
                } else {
                    Ok(())
                }
            }
            ControlId::Gain => {
                if cv.auto.unwrap_or(false) {
                    self.set_byte(CMD_AUTO_GAIN_MODE, WIRE_TRUE).await
                } else {
                    self.set_byte(CMD_AUTO_GAIN_MODE, WIRE_FALSE).await?;
                    self.set_byte(CMD_GAIN, pct_to_u8(value_f64)).await
                }
            }
            ControlId::Sea => {
                self.set_byte(CMD_STC, pct_to_u8(value_f64)).await
            }
            ControlId::Rain => {
                self.set_byte(CMD_FTC, pct_to_u8(value_f64)).await
            }
            ControlId::SeaState => {
                // 0=Manual, 1=Auto, 2=Harbor
                let wire_val = match value {
                    0 => 0x00,
                    1 => 0x11,
                    2 => 0x22,
                    _ => 0x00,
                };
                self.set_byte(CMD_AUTO_STC_MODE, wire_val).await
            }
            ControlId::PulseWidth => {
                self.set_byte(CMD_PULSE_LENGTH, value as u8).await
            }
            ControlId::InterferenceRejection => {
                let wire_val = match value {
                    0 => 0x00,
                    1 => 0x11,
                    2 => 0x22,
                    3 => 0x33,
                    _ => 0x00,
                };
                self.set_byte(CMD_INTERFERENCE_REJECTION, wire_val).await
            }
            ControlId::TargetExpansion => {
                // Koden wire protocol: 0x00=expansion ON, 0x11=OFF
                let wire_val = if value == 0 { WIRE_TRUE } else { WIRE_FALSE };
                self.set_byte(CMD_TARGET_EXPANSION, wire_val).await
            }
            ControlId::Tune => {
                if cv.auto.unwrap_or(false) {
                    self.set_byte(CMD_TUNING_MODE, WIRE_AUTO).await
                } else {
                    self.set_byte(CMD_TUNING_MODE, WIRE_FALSE).await?;
                    self.set_byte(CMD_COARSE_TUNING, value as u8).await
                }
            }
            ControlId::TuneFine => self.set_byte(CMD_FINE_TUNING, value as u8).await,
            ControlId::DisplayTiming => self.set_byte(CMD_TRIGGER_DELAY, value as u8).await,
            ControlId::ScanSpeed => self.set_byte(CMD_ANTENNA_SPEED, value as u8).await,
            ControlId::NoTransmitSector1 => {
                // Blanking sector: 7 bytes [0x26, 0x9C, start_hi, start_lo, end_hi, end_lo, 0x0D]
                // Wire angles: 0-1023 for 0-360 degrees
                let start = radians_to_koden_angle(value_f64);
                let end = radians_to_koden_angle(cv.end_as_f64().unwrap_or(0.));
                let packet = [
                    CONTROL_PREFIX,
                    CMD_BLANKING_SECTOR,
                    (start >> 8) as u8,
                    (start & 0xFF) as u8,
                    (end >> 8) as u8,
                    (end & 0xFF) as u8,
                    PACKET_END,
                ];
                self.send_raw(&packet).await
            }
            ControlId::ParkPosition => {
                let wire = radians_to_koden_angle(value_f64);
                self.set_word(CMD_PARK_ANGLE, wire).await
            }
            _ => {
                log::debug!(
                    "{}: Unhandled control {:?} = {:?}",
                    self.key,
                    cv.id,
                    cv.value
                );
                Ok(())
            }
        }
    }
}
