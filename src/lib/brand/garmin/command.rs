use std::io;
use std::net::SocketAddrV4;

use async_trait::async_trait;
use tokio::net::UdpSocket;

use super::GarminRadarType;
use super::protocol::*;
use crate::brand::CommandSender;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{DopplerMode, Power, RadarError};

/// Garmin command sender. In dual-range mode each range gets its own
/// `Command` instance — Range B's instance has `range_b = true` and
/// sends Range B opcodes instead of Range A ones.
pub(crate) struct Command {
    radar_type: GarminRadarType,
    send_addr: SocketAddrV4,
    socket: Option<UdpSocket>,
    range_b: bool,
}

impl Command {
    pub(crate) fn new(radar_type: GarminRadarType, send_addr: SocketAddrV4) -> Self {
        Command {
            radar_type,
            send_addr,
            socket: None,
            range_b: false,
        }
    }

    pub(crate) fn new_range_b(radar_type: GarminRadarType, send_addr: SocketAddrV4) -> Self {
        Command {
            radar_type,
            send_addr,
            socket: None,
            range_b: true,
        }
    }

    async fn ensure_socket(&mut self) -> io::Result<&UdpSocket> {
        if self.socket.is_none() {
            let socket = UdpSocket::bind("0.0.0.0:0").await?;
            socket.connect(self.send_addr).await?;
            self.socket = Some(socket);
        }
        Ok(self.socket.as_ref().unwrap())
    }

    /// Send a 9-byte command (packet_type + len=1 + u8 value)
    async fn send_9(&mut self, packet_type: u32, value: u8) -> io::Result<()> {
        let buf = build_packet_9(packet_type, value);
        let socket = self.ensure_socket().await?;
        socket.send(&buf).await?;
        log::debug!(
            "Garmin {}: sent 9-byte command {:04X} value {}",
            self.radar_type,
            packet_type,
            value
        );
        Ok(())
    }

    /// Send a 10-byte command (packet_type + len=2 + u16 value)
    async fn send_10(&mut self, packet_type: u32, value: u16) -> io::Result<()> {
        let buf = build_packet_10(packet_type, value);
        let socket = self.ensure_socket().await?;
        socket.send(&buf).await?;
        log::debug!(
            "Garmin {}: sent 10-byte command {:04X} value {}",
            self.radar_type,
            packet_type,
            value
        );
        Ok(())
    }

    /// Send a 12-byte command (packet_type + len=4 + u32 value)
    async fn send_12(&mut self, packet_type: u32, value: u32) -> io::Result<()> {
        let buf = build_packet_12(packet_type, value);
        let socket = self.ensure_socket().await?;
        socket.send(&buf).await?;
        log::debug!(
            "Garmin {}: sent 12-byte command {:04X} value {}",
            self.radar_type,
            packet_type,
            value
        );
        Ok(())
    }

    /// Send a 12-byte command with a signed payload (packet_type + len=4 + i32 value).
    /// Use this for any field that may legitimately be negative — bearing
    /// alignment, no-transmit zones, park position — so that the sign survives
    /// the round-trip through the wire.
    async fn send_12_signed(&mut self, packet_type: u32, value: i32) -> io::Result<()> {
        self.send_12(packet_type, value as u32).await
    }

    async fn set_transmit_hd(&mut self, on: bool) -> io::Result<()> {
        self.send_10(CMD_HD_SET_TRANSMIT, if on { 2 } else { 1 }).await
    }

    async fn set_transmit(&mut self, on: bool) -> io::Result<()> {
        self.send_9(MSG_TRANSMIT_MODE, if on { 1 } else { 0 })
            .await
    }

    async fn set_range_hd(&mut self, meters: u32) -> io::Result<()> {
        // HD wire encodes meters - 1.
        self.send_12(CMD_HD_SET_RANGE_A, meters.saturating_sub(1))
            .await
    }

    async fn set_range(&mut self, meters: u32) -> io::Result<()> {
        // Enhanced protocol encodes meters directly.
        let msg = if self.range_b { MSG_RANGE_B } else { MSG_RANGE_A };
        self.send_12(msg, meters).await
    }

    async fn set_gain_hd(&mut self, auto: bool, value: u32) -> io::Result<()> {
        // HD 0x02B4: payload is [u8 gain][u8 auto_flag] packed into 4 LE
        // bytes. The gain value is always sent — it becomes the manual
        // level the radar reverts to when auto is turned off.
        let gain = value.min(100) as u8;
        let auto_flag = if auto { 1u8 } else { 0u8 };
        let packed = (gain as u32) | ((auto_flag as u32) << 8);
        self.send_12(CMD_HD_SET_GAIN, packed).await
    }

    async fn set_gain(&mut self, auto: bool, auto_high: bool, value: u32) -> io::Result<()> {
        let (mode_msg, gain_msg, auto_level_msg) = if self.range_b {
            (MSG_RANGE_B_GAIN_MODE, MSG_RANGE_B_GAIN, MSG_RANGE_B_RADAR_MODE)
        } else {
            (MSG_RANGE_A_GAIN_MODE, MSG_RANGE_A_GAIN, MSG_RANGE_A_AUTO_LEVEL)
        };
        if auto {
            self.send_9(mode_msg, 2).await?;
            self.send_9(auto_level_msg, if auto_high { 1 } else { 0 })
                .await
        } else {
            self.send_9(mode_msg, 0).await?;
            let scaled = (value * GAIN_SCALE as u32).min(10_000) as u16;
            self.send_10(gain_msg, scaled).await
        }
    }

    async fn set_bearing_alignment_hd(&mut self, radians: f64) -> io::Result<()> {
        // HD: signed 16-bit degrees on the wire.
        let degrees = radians.to_degrees() as i16;
        self.send_10(CMD_HD_SET_BEARING_ALIGNMENT, degrees as u16)
            .await
    }

    async fn set_bearing_alignment(&mut self, radians: f64) -> io::Result<()> {
        // Bearing alignment is a signed int32 in degrees × 32. Pass it
        // through send_12_signed so that negative offsets (e.g. a port-side
        // antenna) are not silently mangled by an i32→u32 cast on a value
        // computed from a chained multiplication.
        let value = (radians.to_degrees() as i32) * DEGREE_SCALE;
        self.send_12_signed(MSG_BEARING_ALIGNMENT, value).await
    }

    async fn set_interference_hd(&mut self, value: u8) -> io::Result<()> {
        // HD interference rejection is the dither toggle (0x02BE).
        self.send_9(CMD_HD_SET_DITHER, value).await
    }

    async fn set_interference(&mut self, value: u8) -> io::Result<()> {
        // Dither mode + noise blanker (matches radar_pi).
        self.send_9(MSG_DITHER_MODE, value).await?;
        self.send_9(MSG_NOISE_BLANKER, value).await
    }

    async fn set_rain_hd(&mut self, value: u8) -> io::Result<()> {
        self.send_12(CMD_HD_SET_RAIN, value as u32).await
    }

    async fn set_rain(&mut self, enabled: bool, value: u8) -> io::Result<()> {
        let (mode_msg, gain_msg) = if self.range_b {
            (MSG_RANGE_B_RAIN_MODE, MSG_RANGE_B_RAIN_GAIN)
        } else {
            (MSG_RANGE_A_RAIN_MODE, MSG_RANGE_A_RAIN_GAIN)
        };
        if enabled {
            self.send_9(mode_msg, 1).await?;
            self.send_10(gain_msg, (value as u16) * GAIN_SCALE).await
        } else {
            self.send_9(mode_msg, 0).await
        }
    }

    async fn set_sea_hd(&mut self, auto: bool, value: u8) -> io::Result<()> {
        // HD: 0x2b5 with an 8-byte payload (gain + auto flag, both u32 LE).
        let socket = self.ensure_socket().await?;
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&CMD_HD_SET_SEA.to_le_bytes());
        buf[4..8].copy_from_slice(&8u32.to_le_bytes()); // len = 8
        buf[8..12].copy_from_slice(&(value as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&(if auto { 2u32 } else { 1u32 }).to_le_bytes());
        socket.send(&buf).await?;
        Ok(())
    }

    async fn set_sea(&mut self, auto: bool, auto_level: u8, value: u8) -> io::Result<()> {
        let (mode_msg, gain_msg, state_msg) = if self.range_b {
            (MSG_RANGE_B_SEA_MODE, MSG_RANGE_B_SEA_GAIN, MSG_RANGE_B_SEA_STATE)
        } else {
            (MSG_RANGE_A_SEA_MODE, MSG_RANGE_A_SEA_GAIN, MSG_RANGE_A_SEA_STATE)
        };
        if auto {
            self.send_9(mode_msg, 2).await?;
            self.send_9(state_msg, auto_level).await
        } else if value == 0 {
            self.send_9(mode_msg, 0).await
        } else {
            self.send_9(mode_msg, 1).await?;
            self.send_10(gain_msg, (value as u16) * GAIN_SCALE).await
        }
    }

    async fn set_scan_speed_hd(&mut self, value: u8) -> io::Result<()> {
        // HD scan speed is the RPM mode toggle (0x02B9, 0=normal, 1=slow).
        self.send_9(CMD_HD_SET_RPM_MODE, value).await
    }

    async fn set_scan_speed(&mut self, value: u8) -> io::Result<()> {
        // radar_pi sends value × 2.
        self.send_9(MSG_RPM_MODE, value * 2).await
    }

    async fn set_target_expansion_hd(&mut self, on: bool) -> io::Result<()> {
        // HD FTC: 0x02FC, payload [u8 gain][u8 mode]. We model FTC as a
        // simple on/off list control, so the gain byte is fixed to a
        // mid-range value (matches what radar_pi sends when toggled).
        let socket = self.ensure_socket().await?;
        let mut buf = [0u8; 10];
        buf[0..4].copy_from_slice(&CMD_HD_SET_FTC.to_le_bytes());
        buf[4..8].copy_from_slice(&2u32.to_le_bytes());
        buf[8] = 50; // gain
        buf[9] = if on { 1 } else { 0 };
        socket.send(&buf).await?;
        log::debug!(
            "Garmin {}: sent FTC command on={}",
            self.radar_type,
            on
        );
        Ok(())
    }

    async fn set_sentry_mode(&mut self, on: bool) -> io::Result<()> {
        self.send_9(MSG_SENTRY_MODE, if on { 1 } else { 0 })
            .await
    }

    async fn set_sentry_transmit_time(&mut self, seconds: u16) -> io::Result<()> {
        self.send_10(MSG_SENTRY_TRANSMIT_TIME, seconds).await
    }

    /// Set MotionScope / Doppler scan mode on the radar.
    /// Maps the internal DopplerMode enum to Garmin's wire values
    /// (which are swapped relative to Navico's numbering).
    async fn set_doppler_mode(&mut self, mode: DopplerMode) -> io::Result<()> {
        // Garmin wire: 0=off, 1=approaching, 2=both
        let value: u8 = match mode {
            DopplerMode::None => 0,
            DopplerMode::Approaching => 1,
            DopplerMode::Both => 2,
        };
        let msg = if self.range_b { MSG_RANGE_B_DOPPLER_MODE } else { MSG_RANGE_A_DOPPLER_MODE };
        self.send_9(msg, value).await
    }

    /// Set the radar's device alias via CDM message 0x0393. This is
    /// sent to port 50051 (CDM control), not the normal radar command
    /// port 50101 — so we use a separate one-shot socket.
    async fn set_device_alias(&self, alias: &str) -> io::Result<()> {
        let packet = super::discovery::build_set_alias(alias);
        let dest = std::net::SocketAddrV4::new(
            *self.send_addr.ip(),
            super::discovery::CDM_CONTROL_PORT,
        );
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
        sock.send_to(&packet, dest).await?;
        log::info!(
            "Garmin {}: sent device alias {:?} to {}",
            self.radar_type,
            alias,
            dest
        );
        Ok(())
    }

    /// Send the three-message no-transmit zone update for the given zone.
    /// Zone 1 uses opcodes 0x093f/0x0940/0x0941 and is supported by every
    /// enhanced-protocol radar; zone 2 uses 0x096a/0x096b/0x096c and is only meaningful on
    /// Fantom Pro / multi-zone radars (the settings module gates the
    /// control on `cap::NO_TX_ZONE_2_MODE`).
    async fn set_no_transmit_zone(
        &mut self,
        zone: NoTxZone,
        enabled: bool,
        start_rad: f64,
        end_rad: f64,
    ) -> io::Result<()> {
        let (mode, start_op, stop_op) = zone.opcodes();
        if enabled {
            // No-TX zone bounds are signed int32 deg × 32 in the range
            // [-180°, +180°]. Use send_12_signed so the sign is preserved.
            let start = (start_rad.to_degrees() as i32) * DEGREE_SCALE;
            let end = (end_rad.to_degrees() as i32) * DEGREE_SCALE;
            self.send_9(mode, 1).await?;
            self.send_12_signed(start_op, start).await?;
            self.send_12_signed(stop_op, end).await
        } else {
            self.send_9(mode, 0).await
        }
    }
}

/// Selector for the two no-transmit zones (mirrors the same enum in
/// the report module). Lives next to the command builder so the opcode
/// triple stays out of the call site.
#[derive(Copy, Clone, Debug)]
enum NoTxZone {
    One,
    Two,
}

impl NoTxZone {
    /// Returns `(mode_opcode, start_opcode, stop_opcode)` for the zone.
    fn opcodes(self) -> (u32, u32, u32) {
        match self {
            NoTxZone::One => (
                MSG_NO_TX_ZONE_1_MODE,
                MSG_NO_TX_ZONE_1_START,
                MSG_NO_TX_ZONE_1_STOP,
            ),
            NoTxZone::Two => (
                MSG_NO_TX_ZONE_2_MODE,
                MSG_NO_TX_ZONE_2_START,
                MSG_NO_TX_ZONE_2_STOP,
            ),
        }
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        _controls: &SharedControls,
    ) -> Result<(), RadarError> {
        let value = cv.as_i32().unwrap_or(0);
        let auto = cv.auto.unwrap_or(false);
        let enabled = cv.enabled.unwrap_or(true);
        let auto_value = cv.auto_as_f64().unwrap_or(0.0);

        log::debug!(
            "Garmin {}: set_control {:?} value={} auto={} enabled={}",
            self.radar_type,
            cv.id,
            value,
            auto,
            enabled
        );

        let result = match (&cv.id, self.radar_type) {
            (ControlId::Power, GarminRadarType::HD) => {
                let on = match Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby) {
                    Power::Transmit => true,
                    _ => false,
                };
                self.set_transmit_hd(on).await
            }
            (ControlId::Power, GarminRadarType::XHD) => {
                let on = match Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby) {
                    Power::Transmit => true,
                    _ => false,
                };
                self.set_transmit(on).await
            }
            (ControlId::Range, GarminRadarType::HD) => self.set_range_hd(value as u32).await,
            (ControlId::Range, GarminRadarType::XHD) => self.set_range(value as u32).await,
            (ControlId::Gain, GarminRadarType::HD) => self.set_gain_hd(auto, value as u32).await,
            (ControlId::Gain, GarminRadarType::XHD) => {
                // auto_value 0 = low, 1 = high
                let auto_high = auto_value > 0.5;
                self.set_gain(auto, auto_high, value as u32).await
            }
            (ControlId::BearingAlignment, GarminRadarType::HD) => {
                let radians = cv.as_f64().unwrap_or(0.0);
                self.set_bearing_alignment_hd(radians).await
            }
            (ControlId::BearingAlignment, GarminRadarType::XHD) => {
                let radians = cv.as_f64().unwrap_or(0.0);
                self.set_bearing_alignment(radians).await
            }
            (ControlId::InterferenceRejection, GarminRadarType::HD) => {
                self.set_interference_hd(value as u8).await
            }
            (ControlId::InterferenceRejection, GarminRadarType::XHD) => {
                self.set_interference(value as u8).await
            }
            (ControlId::Rain, GarminRadarType::HD) => self.set_rain_hd(value as u8).await,
            (ControlId::Rain, GarminRadarType::XHD) => {
                self.set_rain(enabled, value as u8).await
            }
            (ControlId::Sea, GarminRadarType::HD) => self.set_sea_hd(auto, value as u8).await,
            (ControlId::Sea, GarminRadarType::XHD) => {
                let auto_level = auto_value as u8;
                self.set_sea(auto, auto_level, value as u8).await
            }
            (ControlId::ScanSpeed, GarminRadarType::HD) => {
                self.set_scan_speed_hd(value as u8).await
            }
            (ControlId::ScanSpeed, GarminRadarType::XHD) => {
                self.set_scan_speed(value as u8).await
            }
            (ControlId::NoTransmitSector1, GarminRadarType::XHD) => {
                let start_rad = cv.as_f64().unwrap_or(0.0);
                let end_rad = cv.end_as_f64().unwrap_or(0.0);
                self.set_no_transmit_zone(NoTxZone::One, enabled, start_rad, end_rad)
                    .await
            }
            (ControlId::NoTransmitSector2, GarminRadarType::XHD) => {
                let start_rad = cv.as_f64().unwrap_or(0.0);
                let end_rad = cv.end_as_f64().unwrap_or(0.0);
                self.set_no_transmit_zone(NoTxZone::Two, enabled, start_rad, end_rad)
                    .await
            }
            (ControlId::TargetExpansion, GarminRadarType::HD) => {
                self.set_target_expansion_hd(value != 0).await
            }
            (ControlId::TargetExpansion, GarminRadarType::XHD) => {
                // Pulse expansion (xHD2+): on/off
                let msg = if self.range_b {
                    MSG_RANGE_B_PULSE_EXPANSION
                } else {
                    MSG_RANGE_A_PULSE_EXPANSION
                };
                self.send_9(msg, if value != 0 { 1 } else { 0 }).await
            }
            (ControlId::TargetBoost, GarminRadarType::XHD) => {
                // Target size mode (xHD2/Fantom)
                let msg = if self.range_b {
                    MSG_RANGE_B_TARGET_SIZE
                } else {
                    MSG_RANGE_A_TARGET_SIZE
                };
                self.send_9(msg, value as u8).await
            }
            (ControlId::ScanAverageMode, GarminRadarType::XHD) => {
                let msg = if self.range_b {
                    MSG_RANGE_B_SCAN_AVERAGE_MODE
                } else {
                    MSG_RANGE_A_SCAN_AVERAGE_MODE
                };
                self.send_9(msg, value as u8).await
            }
            (ControlId::ScanAverageSensitivity, GarminRadarType::XHD) => {
                let msg = if self.range_b {
                    MSG_RANGE_B_SCAN_AVERAGE_SENSITIVITY
                } else {
                    MSG_RANGE_A_SCAN_AVERAGE_SENSITIVITY
                };
                let scaled = (value as u16 * GAIN_SCALE).min(10000);
                self.send_10(msg, scaled).await
            }
            (ControlId::TimedIdle, GarminRadarType::XHD) => {
                self.set_sentry_mode(value != 0).await
            }
            (ControlId::TimedRun, GarminRadarType::XHD) => {
                let seconds = (value as u16).max(1);
                self.set_sentry_transmit_time(seconds).await
            }
            (ControlId::Doppler, GarminRadarType::XHD) => {
                let mode = match value {
                    1 => DopplerMode::Both,
                    2 => DopplerMode::Approaching,
                    _ => DopplerMode::None,
                };
                self.set_doppler_mode(mode).await
            }
            (ControlId::ParkPosition, GarminRadarType::XHD) => {
                let radians = cv.as_f64().unwrap_or(0.0);
                let value = (radians.to_degrees() as i32) * DEGREE_SCALE;
                self.send_12_signed(MSG_PARK_POSITION, value).await
            }
            (ControlId::Tune, GarminRadarType::XHD) => {
                // AFC: auto=1 sends mode=1, manual sends mode=0 + trigger
                if auto {
                    self.send_9(MSG_AFC_MODE, 1).await
                } else {
                    self.send_9(MSG_AFC_MODE, 0).await?;
                    self.send_9(MSG_AFC_TUNING_MODE, 1).await
                }
            }
            (ControlId::TransmitChannel, GarminRadarType::XHD) => {
                if auto {
                    self.send_9(MSG_TRANSMIT_CHANNEL_MODE, 1).await
                } else {
                    self.send_9(MSG_TRANSMIT_CHANNEL_MODE, 0).await?;
                    self.send_10(MSG_TRANSMIT_CHANNEL_SELECT, value.max(1) as u16)
                        .await
                }
            }
            (ControlId::UserName, _) => {
                if let Some(serde_json::Value::String(name)) = &cv.value {
                    self.set_device_alias(name).await
                } else {
                    Ok(())
                }
            }
            _ => {
                log::debug!("Garmin {}: unhandled control {:?}", self.radar_type, cv.id);
                Ok(())
            }
        };

        result.map_err(RadarError::Io)
    }
}

// -------------------------------------------------------------------------
// Pure packet builders
//
// The send_9 / send_10 / send_12 helpers above split into a buffer
// builder and a socket-send call. The builders are pure functions over
// `(packet_type, value)`, which makes them straightforward to unit-test
// against the byte sequences documented in the protocol research.
// -------------------------------------------------------------------------

/// Build a 9-byte command frame: `[u32 LE packet_type][u32 LE len=1][u8 value]`.
fn build_packet_9(packet_type: u32, value: u8) -> [u8; 9] {
    let mut buf = [0u8; 9];
    buf[0..4].copy_from_slice(&packet_type.to_le_bytes());
    buf[4..8].copy_from_slice(&1u32.to_le_bytes());
    buf[8] = value;
    buf
}

/// Build a 10-byte command frame: `[u32 LE packet_type][u32 LE len=2][u16 LE value]`.
fn build_packet_10(packet_type: u32, value: u16) -> [u8; 10] {
    let mut buf = [0u8; 10];
    buf[0..4].copy_from_slice(&packet_type.to_le_bytes());
    buf[4..8].copy_from_slice(&2u32.to_le_bytes());
    buf[8..10].copy_from_slice(&value.to_le_bytes());
    buf
}

/// Build a 12-byte command frame: `[u32 LE packet_type][u32 LE len=4][u32 LE value]`.
fn build_packet_12(packet_type: u32, value: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&packet_type.to_le_bytes());
    buf[4..8].copy_from_slice(&4u32.to_le_bytes());
    buf[8..12].copy_from_slice(&value.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_packet_9_layout() {
        // 0x919 = MSG_TRANSMIT_MODE; payload `1` puts the radar in
        // transmit mode (the same example used in
        // research/garmin/enhanced-radar-protocol.md:448).
        let buf = build_packet_9(MSG_TRANSMIT_MODE, 1);
        assert_eq!(
            buf,
            [
                0x19, 0x09, 0x00, 0x00, // packet_type = 0x0919
                0x01, 0x00, 0x00, 0x00, // payload_len = 1
                0x01, // value
            ]
        );
    }

    #[test]
    fn build_packet_10_layout() {
        // 0x925 = MSG_RANGE_A_GAIN; gain × 100, e.g. 50 % → 5000.
        let buf = build_packet_10(MSG_RANGE_A_GAIN, 5000);
        assert_eq!(
            buf,
            [
                0x25, 0x09, 0x00, 0x00, // packet_type = 0x0925
                0x02, 0x00, 0x00, 0x00, // payload_len = 2
                0x88, 0x13, // value = 5000 LE
            ]
        );
    }

    #[test]
    fn build_packet_12_layout() {
        // 0x91e = MSG_RANGE_A; range in meters direct (no off-by-one
        // for enhanced protocol). 3704 m = 2 NM, the same value reported in the
        // captured pcap.
        let buf = build_packet_12(MSG_RANGE_A, 3704);
        assert_eq!(
            buf,
            [
                0x1e, 0x09, 0x00, 0x00, // packet_type = 0x091e
                0x04, 0x00, 0x00, 0x00, // payload_len = 4
                0x78, 0x0e, 0x00, 0x00, // value = 3704 LE
            ]
        );
    }

    #[test]
    fn build_packet_12_signed_negative_via_unsigned_cast() {
        // Bearing alignment of -45° is encoded as -45 × 32 = -1440,
        // which casts to u32 = 0xFFFFFA60. The MFD sign-extends back on
        // receive — the Phase 1 fix that introduced send_12_signed
        // makes sure the *2's complement* hits the wire intact.
        let value = -1440_i32;
        let buf = build_packet_12(MSG_BEARING_ALIGNMENT, value as u32);
        assert_eq!(
            buf,
            [
                0x30, 0x09, 0x00, 0x00, // packet_type = 0x0930
                0x04, 0x00, 0x00, 0x00, // payload_len = 4
                0x60, 0xfa, 0xff, 0xff, // value = -1440 i32 LE = 0xFFFFFA60
            ]
        );
    }

    #[test]
    fn build_packet_12_hd_range_off_by_one() {
        // Legacy HD wire encodes meters - 1 (Phase 1 doc note).
        // The set_range_hd helper does the subtraction; this test
        // proves the framing is identical to the enhanced path.
        let buf = build_packet_12(CMD_HD_SET_RANGE_A, 1851);
        assert_eq!(buf[4..8], [0x04, 0x00, 0x00, 0x00]);
        assert_eq!(buf[0..4], CMD_HD_SET_RANGE_A.to_le_bytes());
        assert_eq!(buf[8..12], 1851u32.to_le_bytes());
    }

    #[test]
    fn no_tx_zone_opcodes_match_protocol() {
        // Zone 1 = 0x093f / 0x0940 / 0x0941 — every enhanced-protocol radar.
        assert_eq!(
            NoTxZone::One.opcodes(),
            (
                MSG_NO_TX_ZONE_1_MODE,
                MSG_NO_TX_ZONE_1_START,
                MSG_NO_TX_ZONE_1_STOP,
            )
        );
        // Zone 2 = 0x096a / 0x096b / 0x096c — Fantom Pro / multi-zone.
        assert_eq!(
            NoTxZone::Two.opcodes(),
            (
                MSG_NO_TX_ZONE_2_MODE,
                MSG_NO_TX_ZONE_2_START,
                MSG_NO_TX_ZONE_2_STOP,
            )
        );
        // Sanity-check the literal hex values too — these are the bytes
        // that hit the wire and they're easy to typo.
        assert_eq!(NoTxZone::One.opcodes(), (0x093f, 0x0940, 0x0941));
        assert_eq!(NoTxZone::Two.opcodes(), (0x096a, 0x096b, 0x096c));
    }
}
