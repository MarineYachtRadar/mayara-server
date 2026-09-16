use async_trait::async_trait;
use deku::{DekuContainerWrite, DekuWrite};
use tokio::net::UdpSocket;

use crate::brand::CommandSender;
use crate::network::create_connected_send;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

use super::Model;
use super::protocol::{
    CMD_ACCENT_LIGHT, CMD_BEARING_ALIGNMENT, CMD_DOPPLER, CMD_DOPPLER_SPEED_THRESHOLD,
    CMD_GAIN_VARIANT, CMD_HALO_SEA, CMD_HALO_TARGET_EXPANSION, CMD_INSTALLATION,
    CMD_INTERFERENCE_REJECTION, CMD_LOCAL_INTERFERENCE_REJECTION, CMD_NOISE_REJECTION,
    CMD_NOTRANSMIT_ENABLE, CMD_NOTRANSMIT_SECTOR, CMD_POWER_ON, CMD_RANGE, CMD_SCAN_SPEED,
    CMD_SEA_STATE, CMD_TARGET_BOOST, CMD_TARGET_EXPANSION, CMD_TARGET_SEPARATION, CMD_TRANSMIT,
    CMD_USE_MODE, COMMAND_STAY_ON_A, GAIN_VARIANT_GAIN, GAIN_VARIANT_RAIN, GAIN_VARIANT_SEA,
    GAIN_VARIANT_SIDELOBE, INSTALL_TAG_ANTENNA_HEIGHT, INSTALL_TAG_ANTENNA_OFFSET,
    REQUEST_STATE_BATCH, REQUEST_STATE_PROPERTIES, control_opcode,
};

// Last byte of the HALO sea clutter command: which of the three things the
// frame changes.
const HALO_SEA_MODE: u8 = 0x01;
const HALO_SEA_MANUAL: u8 = 0x02;
const HALO_SEA_AUTO_OFFSET: u8 = 0x04;

/// A command frame: `[sub-opcode][0xC1][body]`. The two opcode bytes are one
/// little-endian u16, so each variant's id reads as the `0xC1xx` opcode
/// `protocol.rs` documents it under.
#[derive(DekuWrite, Debug, PartialEq)]
#[deku(endian = "little", id_type = "u16")]
pub(super) enum ControlCommand {
    #[deku(id = "control_opcode(CMD_POWER_ON)")]
    PowerOn { on: u8 },
    #[deku(id = "control_opcode(CMD_TRANSMIT)")]
    Transmit { transmit: u8 },
    #[deku(id = "control_opcode(CMD_RANGE)")]
    Range { decimeters: i32 },
    #[deku(id = "control_opcode(CMD_BEARING_ALIGNMENT)")]
    BearingAlignment { deci_degrees: i16 },
    #[deku(id = "control_opcode(CMD_GAIN_VARIANT)")]
    GainStyle(GainCommand),
    #[deku(id = "control_opcode(CMD_INTERFERENCE_REJECTION)")]
    InterferenceRejection { level: u8 },
    #[deku(id = "control_opcode(CMD_TARGET_EXPANSION)")]
    TargetExpansion { level: u8 },
    #[deku(id = "control_opcode(CMD_HALO_TARGET_EXPANSION)")]
    HaloTargetExpansion { level: u8 },
    #[deku(id = "control_opcode(CMD_TARGET_BOOST)")]
    TargetBoost { level: u8 },
    #[deku(id = "control_opcode(CMD_SEA_STATE)")]
    SeaState { level: u8 },
    /// Both value bytes carry the one setting being changed; see
    /// [`Command::halo_sea_command`].
    #[deku(id = "control_opcode(CMD_HALO_SEA)")]
    HaloSea {
        auto: u8,
        positive_setting: u8,
        signed_setting: u8,
        mode: u8,
    },
    #[deku(id = "control_opcode(CMD_NOTRANSMIT_ENABLE)")]
    NoTransmitEnable {
        sector: u8,
        #[deku(pad_bytes_before = "3")]
        enabled: u8,
    },
    #[deku(id = "control_opcode(CMD_NOTRANSMIT_SECTOR)")]
    NoTransmitSector {
        sector: u8,
        #[deku(pad_bytes_before = "3")]
        enabled: u8,
        start_deci_degrees: i16,
        end_deci_degrees: i16,
    },
    #[deku(id = "control_opcode(CMD_LOCAL_INTERFERENCE_REJECTION)")]
    LocalInterferenceRejection { level: u8 },
    #[deku(id = "control_opcode(CMD_SCAN_SPEED)")]
    ScanSpeed { level: u8 },
    #[deku(id = "control_opcode(CMD_USE_MODE)")]
    UseMode { mode: u8, variant: u8 },
    #[deku(id = "control_opcode(CMD_NOISE_REJECTION)")]
    NoiseRejection { level: u8 },
    #[deku(id = "control_opcode(CMD_TARGET_SEPARATION)")]
    TargetSeparation { level: u8 },
    #[deku(id = "control_opcode(CMD_DOPPLER)")]
    Doppler { mode: u8 },
    #[deku(id = "control_opcode(CMD_DOPPLER_SPEED_THRESHOLD)")]
    DopplerSpeedThreshold { cm_per_second: u16 },
    #[deku(id = "control_opcode(CMD_INSTALLATION)")]
    Installation(InstallCommand),
    #[deku(id = "control_opcode(CMD_ACCENT_LIGHT)")]
    AccentLight { level: u8 },
}

/// The `0xC106` commands, told apart by the variant byte that follows the
/// opcode.
#[derive(DekuWrite, Debug, PartialEq)]
#[deku(ctx = "endian: deku::ctx::Endian", endian = "endian", id_type = "u8")]
pub(super) enum GainCommand {
    #[deku(id = "GAIN_VARIANT_GAIN")]
    Gain {
        #[deku(pad_bytes_before = "3")]
        auto: u32,
        value: u8,
    },
    #[deku(id = "GAIN_VARIANT_SEA")]
    Sea {
        #[deku(endian = "big")]
        auto: u32,
        #[deku(endian = "big")]
        value: u32,
    },
    #[deku(id = "GAIN_VARIANT_RAIN")]
    Rain {
        #[deku(pad_bytes_before = "7")]
        value: u8,
    },
    #[deku(id = "GAIN_VARIANT_SIDELOBE")]
    SideLobeSuppression {
        #[deku(pad_bytes_before = "3")]
        auto: u8,
        #[deku(pad_bytes_before = "3")]
        value: u8,
    },
}

/// The `0xC130` settings, told apart by the 4-byte tag that follows the opcode.
#[derive(DekuWrite, Debug, PartialEq)]
#[deku(ctx = "endian: deku::ctx::Endian", endian = "endian", id_type = "u32")]
pub(super) enum InstallCommand {
    #[deku(id = "INSTALL_TAG_ANTENNA_HEIGHT as u32")]
    AntennaHeight { height_mm: i32 },
    #[deku(id = "INSTALL_TAG_ANTENNA_OFFSET as u32")]
    AntennaOffset { ahead_mm: i32, starboard_mm: i32 },
}

pub(crate) struct Command {
    key: String,
    info: RadarInfo,
    model: Model,
    sock: Option<UdpSocket>,
    fake_errors: bool,
}

impl Command {
    pub(crate) fn new(fake_errors: bool, info: RadarInfo) -> Self {
        Command {
            key: info.key(),
            info,
            model: Model::Unknown,
            sock: None,
            fake_errors,
        }
    }

    pub(crate) fn set_model(&mut self, model: Model) {
        self.model = model;
    }

    async fn start_socket(&mut self) -> Result<(), RadarError> {
        match create_connected_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                log::debug!(
                    "{} {} via {}: sending commands",
                    self.key,
                    self.info.send_command_addr,
                    self.info.nic_addr
                );
                self.sock = Some(sock);

                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: send socket failed: {}",
                    self.key,
                    self.info.send_command_addr,
                    self.info.nic_addr,
                    e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    async fn send(&mut self, message: &[u8]) -> Result<(), RadarError> {
        if self.sock.is_none() {
            self.start_socket().await?;
        }
        if let Some(sock) = &self.sock {
            sock.send(message).await.map_err(RadarError::Io)?;
            log::debug!("{}: sent command {:02X?}", self.key, message);
        }

        Ok(())
    }

    fn scale_100_to_byte(a: f64) -> u8 {
        // Map range 0..100 to 0..255
        let r = (a * 255.0 / 100.0).clamp(0.0, 255.0);
        r.round() as u8
    }

    fn mod_deci_degrees(a: i32) -> i32 {
        (a + 7200) % 3600
    }

    /// The frames a HALO needs for one Sea request.
    ///
    /// Mode and number are two separate commands: the number command carries
    /// no mode, so a request that changes both has to send the mode first or
    /// the radar keeps the mode it was in and silently applies only the
    /// number. `mode_requested` says the caller actually asked for a mode --
    /// a request carrying only a number leaves the radar's mode alone rather
    /// than inferring one from it.
    ///
    /// The mode frame goes out whenever it was asked for, without consulting
    /// the mode we believe the radar is in: that belief arrives by report and
    /// can lag, and a repeated mode command costs one frame, while a skipped
    /// one costs the user the change they asked for.
    fn halo_sea_frames(
        auto: bool,
        setting: Option<f64>,
        mode_requested: bool,
    ) -> Vec<ControlCommand> {
        let mut frames = Vec::with_capacity(2);
        if setting.is_some() && mode_requested {
            frames.push(Self::halo_sea_command(auto, None));
        }
        frames.push(Self::halo_sea_command(auto, setting));
        frames
    }

    /// Build the HALO sea clutter command.
    ///
    /// Both value bytes carry the same number — the one setting being changed:
    /// the -50..+50 offset in auto mode, the 0..100 level in manual mode. The
    /// first holds it only while it is positive, the second always, as a
    /// signed byte. `setting` is `None` for a change of mode alone.
    ///
    /// Capture data from an MFD:
    /// ```text
    /// 11c101000004 = Auto      11c100646402 = 100
    /// 11c10100ff04 = Auto-1    11c100000002 = 0
    /// 11c10100ce04 = Auto-50   11c100000001 = Mode manual
    /// 11c101323204 = Auto+50   11c101000001 = Mode auto
    /// ```
    fn halo_sea_command(auto: bool, setting: Option<f64>) -> ControlCommand {
        let mode = match (setting, auto) {
            (None, _) => HALO_SEA_MODE,
            (Some(_), false) => HALO_SEA_MANUAL,
            (Some(_), true) => HALO_SEA_AUTO_OFFSET,
        };
        let setting = setting.unwrap_or(0.).round() as i8;

        ControlCommand::HaloSea {
            auto: auto as u8,
            positive_setting: setting.max(0) as u8,
            signed_setting: setting as u8,
            mode,
        }
    }

    fn generate_fake_error(v: i32) -> Result<(), RadarError> {
        match v {
            11 => Err(RadarError::CannotSetControlId(ControlId::Rain)),
            12 => Err(RadarError::CannotSetControlId(ControlId::Power)),
            _ => Err(RadarError::NoSuchRadar("n1234a".to_string())),
        }
    }

    async fn send_no_transmit_cmd(
        &mut self,
        value_start: i16,
        value_end: i16,
        enabled: u8,
        sector: u8,
    ) -> Result<Vec<u8>, RadarError> {
        log::info!(
            "send_no_transmit({}, {}, {}, {})",
            sector,
            value_start,
            value_end,
            enabled
        );

        let enable = ControlCommand::NoTransmitEnable { sector, enabled }.to_bytes()?;
        self.send(&enable).await?;

        Ok(ControlCommand::NoTransmitSector {
            sector,
            enabled,
            start_deci_degrees: value_start,
            end_deci_degrees: value_end,
        }
        .to_bytes()?)
    }

    /// The datagrams of one report-request tick. The stay-alive is what holds
    /// the radar up for us, so it is left out while the radar should stand
    /// down; the two queries stay so the radar's state keeps arriving.
    fn report_request_frames(stay_alive: bool) -> Vec<&'static [u8]> {
        let mut frames: Vec<&'static [u8]> = vec![&REQUEST_STATE_PROPERTIES, &REQUEST_STATE_BATCH];
        if stay_alive {
            frames.push(&COMMAND_STAY_ON_A);
        }
        frames
    }

    pub(super) async fn send_report_requests(
        &mut self,
        stay_alive: bool,
    ) -> Result<(), RadarError> {
        for frame in Self::report_request_frames(stay_alive) {
            self.send(frame).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        let cmd: Vec<u8>;

        log::debug!("Command handling request {:?}", cv);

        let control = controls.get(&cv.id).unwrap();
        let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        let auto_value = cv
            .auto_as_f64()
            .unwrap_or(control.auto_as_f64().unwrap_or(0.));
        let value = cv.as_f64().unwrap_or(control.as_f64().unwrap_or(0.));
        let deci_value = f64::round(value * 10.0) as i32;
        log::info!(
            "set_control({:?},...) = {} / {},auto={},auto_value={},enabled={}",
            cv,
            value,
            deci_value,
            auto,
            auto_value,
            enabled
        );

        match cv.id {
            ControlId::Power => {
                let value = match Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby) {
                    Power::Transmit => 1,
                    _ => 0,
                };

                let power_on = ControlCommand::PowerOn { on: 0x01 }.to_bytes()?;
                self.send(&power_on).await?;
                cmd = ControlCommand::Transmit { transmit: value }.to_bytes()?;
            }

            ControlId::Range => {
                let decimeters: i32 = deci_value;
                log::trace!("range {value} -> {decimeters}");

                cmd = ControlCommand::Range { decimeters }.to_bytes()?;
            }
            ControlId::BearingAlignment => {
                let deci_degrees: i16 = Self::mod_deci_degrees(deci_value) as i16;

                cmd = ControlCommand::BearingAlignment { deci_degrees }.to_bytes()?;
            }
            ControlId::Gain => {
                cmd = ControlCommand::GainStyle(GainCommand::Gain {
                    auto: auto as u32,
                    value: Self::scale_100_to_byte(value),
                })
                .to_bytes()?;
            }
            ControlId::Sea => {
                if self.model.is_halo() {
                    // Which number the radar is being given depends on the
                    // mode: the auto offset when on auto, the manual level
                    // otherwise. Neither carries over into the other.
                    let setting = if cv.value.is_none() && cv.auto_value.is_none() {
                        None
                    } else if auto == 0 {
                        Some(value)
                    } else {
                        Some(auto_value)
                    };

                    let mut frames = Self::halo_sea_frames(auto != 0, setting, cv.auto.is_some());
                    // Everything but the last frame goes now; the last one
                    // leaves through the common tail below.
                    let last = frames.pop().expect("a Sea request is at least one frame");
                    for frame in frames {
                        let frame = frame.to_bytes()?;
                        log::debug!("{}: Send command {:02X?}", self.info.key(), frame);
                        self.send(&frame).await?;
                    }
                    cmd = last.to_bytes()?;
                } else {
                    cmd = ControlCommand::GainStyle(GainCommand::Sea {
                        auto: auto as u32,
                        value: Self::scale_100_to_byte(value) as u32,
                    })
                    .to_bytes()?;
                }
            }
            ControlId::Rain => {
                cmd = ControlCommand::GainStyle(GainCommand::Rain {
                    value: Self::scale_100_to_byte(value),
                })
                .to_bytes()?;
            }
            ControlId::SideLobeSuppression => {
                cmd = ControlCommand::GainStyle(GainCommand::SideLobeSuppression {
                    auto,
                    value: Self::scale_100_to_byte(value),
                })
                .to_bytes()?;
            }
            ControlId::InterferenceRejection => {
                cmd = ControlCommand::InterferenceRejection { level: value as u8 }.to_bytes()?;
            }
            ControlId::TargetExpansion => {
                let level = value as u8;
                cmd = if self.model.is_halo() {
                    ControlCommand::HaloTargetExpansion { level }.to_bytes()?
                } else {
                    ControlCommand::TargetExpansion { level }.to_bytes()?
                };
            }
            ControlId::TargetBoost => {
                cmd = ControlCommand::TargetBoost { level: value as u8 }.to_bytes()?;
            }
            ControlId::SeaState => {
                cmd = ControlCommand::SeaState { level: value as u8 }.to_bytes()?;
            }
            ControlId::NoTransmitSector1
            | ControlId::NoTransmitSector2
            | ControlId::NoTransmitSector3
            | ControlId::NoTransmitSector4 => {
                let sector = match cv.id {
                    ControlId::NoTransmitSector1 => 0,
                    ControlId::NoTransmitSector2 => 1,
                    ControlId::NoTransmitSector3 => 2,
                    ControlId::NoTransmitSector4 => 3,
                    _ => unreachable!(),
                };
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let end_value = cv
                    .end_as_f64()
                    .unwrap_or(control.end_as_f64().unwrap_or(0.));
                let deci_end_value = f64::round(end_value * 10.0) as i32;
                let value_end: i16 = Self::mod_deci_degrees(deci_end_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, sector)
                    .await?;
            }
            ControlId::LocalInterferenceRejection => {
                cmd =
                    ControlCommand::LocalInterferenceRejection { level: value as u8 }.to_bytes()?;
            }
            ControlId::ScanSpeed => {
                cmd = ControlCommand::ScanSpeed { level: value as u8 }.to_bytes()?;
            }
            ControlId::Mode => {
                // Bird Plus (value 6) maps to tUseMode { mode: 5, variant: 1 }
                // All other modes: variant 0
                let (mode, variant) = if value as u8 == 6 {
                    (5u8, 1u8)
                } else {
                    (value as u8, 0u8)
                };
                cmd = ControlCommand::UseMode { mode, variant }.to_bytes()?;
            }
            ControlId::NoiseRejection => {
                cmd = ControlCommand::NoiseRejection { level: value as u8 }.to_bytes()?;
            }
            ControlId::TargetSeparation => {
                cmd = ControlCommand::TargetSeparation { level: value as u8 }.to_bytes()?;
            }
            ControlId::Doppler => {
                cmd = ControlCommand::Doppler { mode: value as u8 }.to_bytes()?;
            }
            ControlId::DopplerSpeedThreshold => {
                let cm_per_second = (f64::round(value * 100.0) as u16).clamp(0, 1594);
                cmd = ControlCommand::DopplerSpeedThreshold { cm_per_second }.to_bytes()?;
            }
            ControlId::AntennaForward | ControlId::AntennaStarboard => {
                let (ahead_mm, starboard_mm) = if cv.id == ControlId::AntennaForward {
                    let other = controls.get(&ControlId::AntennaStarboard).unwrap();
                    (
                        (value * 1000.) as i32,
                        (other.as_f64().unwrap_or(0.) * 1000.) as i32,
                    )
                } else {
                    let other = controls.get(&ControlId::AntennaForward).unwrap();
                    (
                        (other.as_f64().unwrap_or(0.) * 1000.) as i32,
                        (value * 1000.) as i32,
                    )
                };
                cmd = ControlCommand::Installation(InstallCommand::AntennaOffset {
                    ahead_mm,
                    starboard_mm,
                })
                .to_bytes()?;
            }
            ControlId::AntennaHeight => {
                cmd = ControlCommand::Installation(InstallCommand::AntennaHeight {
                    height_mm: (value * 1000.) as i32,
                })
                .to_bytes()?;
            }
            ControlId::AccentLight => {
                cmd = ControlCommand::AccentLight { level: value as u8 }.to_bytes()?;
            }
            // RangeUnits is a client-side display preference on Navico:
            // the radar always reports distances in meters and the unit
            // choice only affects how the GUI labels them. Persist the
            // value in SharedControls without emitting a wire command.
            ControlId::RangeUnits => {
                if let Some(v) = cv.value.as_ref().and_then(|v| v.as_f64()) {
                    let _ = controls.set_value(&ControlId::RangeUnits, v.into());
                }
                return Ok(());
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlId(cv.id)),
        };

        log::debug!("{}: Send command {:02X?}", self.info.key(), cmd);
        self.send(&cmd).await?;

        if self.fake_errors && cv.id == ControlId::Rain && value > 10. {
            return Self::generate_fake_error(value as i32);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, ControlCommand, GainCommand, InstallCommand};
    use deku::DekuContainerWrite;

    fn encoded(frames: &[ControlCommand]) -> Vec<Vec<u8>> {
        frames.iter().map(|f| f.to_bytes().unwrap()).collect()
    }

    /// A number and a mode are two commands. Asking for both has to send
    /// both, mode first, or the radar applies the number and stays in the
    /// mode it was in -- the user's "manual at 90" silently becoming "auto,
    /// with 90 stored for later".
    #[test]
    fn a_sea_request_changing_mode_and_number_sends_the_mode_first() {
        let frames = Command::halo_sea_frames(false, Some(90.), true);

        assert_eq!(
            encoded(&frames),
            vec![
                vec![0x11, 0xc1, 0x00, 0x00, 0x00, 0x01], // mode manual
                vec![0x11, 0xc1, 0x00, 0x5a, 0x5a, 0x02], // level 90
            ]
        );
    }

    /// A number on its own leaves the radar's mode alone: nothing about a
    /// level says the user wanted out of auto.
    #[test]
    fn a_sea_request_carrying_only_a_number_sends_one_frame() {
        let frames = Command::halo_sea_frames(false, Some(90.), false);

        assert_eq!(
            encoded(&frames),
            vec![vec![0x11, 0xc1, 0x00, 0x5a, 0x5a, 0x02]]
        );
    }

    /// A mode on its own is already one frame, and must not be sent twice.
    #[test]
    fn a_sea_request_carrying_only_a_mode_sends_one_frame() {
        assert_eq!(
            encoded(&Command::halo_sea_frames(true, None, true)),
            vec![vec![0x11, 0xc1, 0x01, 0x00, 0x00, 0x01]]
        );
    }

    /// Switching into auto with an offset needs the mode too, or the offset
    /// lands while the radar is still manual.
    #[test]
    fn a_sea_request_entering_auto_with_an_offset_sends_the_mode_first() {
        let frames = Command::halo_sea_frames(true, Some(-50.), true);

        assert_eq!(
            encoded(&frames),
            vec![
                vec![0x11, 0xc1, 0x01, 0x00, 0x00, 0x01], // mode auto
                vec![0x11, 0xc1, 0x01, 0x00, 0xce, 0x04], // auto offset -50
            ]
        );
    }

    /// Every control command's bytes, so a layout change shows up here rather
    /// than on the water. The expected bytes are what the hand-built frames
    /// carried before deku.
    #[test]
    fn control_commands_encode_to_their_wire_frames() {
        let cases: [(ControlCommand, &[u8]); 11] = [
            (ControlCommand::PowerOn { on: 1 }, &[0x00, 0xc1, 0x01]),
            (
                ControlCommand::Transmit { transmit: 1 },
                &[0x01, 0xc1, 0x01],
            ),
            (
                ControlCommand::Range { decimeters: 1852 },
                &[0x03, 0xc1, 0x3c, 0x07, 0x00, 0x00],
            ),
            (
                ControlCommand::BearingAlignment { deci_degrees: 3550 },
                &[0x05, 0xc1, 0xde, 0x0d],
            ),
            (
                ControlCommand::GainStyle(GainCommand::Gain {
                    auto: 1,
                    value: 200,
                }),
                &[
                    0x06, 0xc1, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xc8,
                ],
            ),
            (
                // The only big-endian payload on the wire.
                ControlCommand::GainStyle(GainCommand::Sea {
                    auto: 1,
                    value: 128,
                }),
                &[
                    0x06, 0xc1, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x80,
                ],
            ),
            (
                ControlCommand::GainStyle(GainCommand::Rain { value: 77 }),
                &[0x06, 0xc1, 0x04, 0, 0, 0, 0, 0, 0, 0, 77],
            ),
            (
                ControlCommand::GainStyle(GainCommand::SideLobeSuppression { auto: 1, value: 50 }),
                &[0x06, 0xc1, 0x05, 0, 0, 0, 0x01, 0, 0, 0, 50],
            ),
            // A sector change is two frames: this one goes out first.
            (
                ControlCommand::NoTransmitEnable {
                    sector: 2,
                    enabled: 1,
                },
                &[0x0d, 0xc1, 0x02, 0, 0, 0, 0x01],
            ),
            (
                ControlCommand::NoTransmitSector {
                    sector: 2,
                    enabled: 1,
                    start_deci_degrees: -900,
                    end_deci_degrees: 900,
                },
                &[0xc0, 0xc1, 0x02, 0, 0, 0, 0x01, 0x7c, 0xfc, 0x84, 0x03],
            ),
            (
                ControlCommand::Installation(InstallCommand::AntennaOffset {
                    ahead_mm: 1010,
                    starboard_mm: 2900,
                }),
                &[
                    0x30, 0xc1, 0x04, 0, 0, 0, 0xf2, 0x03, 0x00, 0x00, 0x54, 0x0b, 0x00, 0x00,
                ],
            ),
        ];

        for (command, expected) in cases {
            assert_eq!(command.to_bytes().unwrap(), expected, "{:?}", command);
        }
    }

    /// Every frame an MFD was captured sending for sea clutter, so a HALO is
    /// told about a change the same way whoever sends it.
    #[test]
    fn halo_sea_command_matches_mfd_captures() {
        let bytes = |auto, setting| Command::halo_sea_command(auto, setting).to_bytes().unwrap();

        // Mode alone, carrying no number.
        assert_eq!(bytes(false, None), [0x11, 0xc1, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(bytes(true, None), [0x11, 0xc1, 0x01, 0x00, 0x00, 0x01]);

        // Auto, adjusted by an offset that is signed in the second byte and
        // only present in the first while positive.
        assert_eq!(bytes(true, Some(0.)), [0x11, 0xc1, 0x01, 0x00, 0x00, 0x04]);
        assert_eq!(bytes(true, Some(-1.)), [0x11, 0xc1, 0x01, 0x00, 0xff, 0x04]);
        assert_eq!(
            bytes(true, Some(-50.)),
            [0x11, 0xc1, 0x01, 0x00, 0xce, 0x04]
        );
        assert_eq!(bytes(true, Some(50.)), [0x11, 0xc1, 0x01, 0x32, 0x32, 0x04]);

        // Manual, where the level reaches 100 and both bytes carry it.
        assert_eq!(
            bytes(false, Some(100.)),
            [0x11, 0xc1, 0x00, 0x64, 0x64, 0x02]
        );
        assert_eq!(bytes(false, Some(0.)), [0x11, 0xc1, 0x00, 0x00, 0x00, 0x02]);
    }

    /// The manual level and the auto offset are separate settings; sending one
    /// must never leak the other into the frame. This is what stopped auto
    /// adjustments from taking effect: the first byte carried the manual
    /// level, so the radar was handed an offset it had not been asked for.
    #[test]
    fn halo_sea_auto_offset_ignores_the_manual_level() {
        let manual_level_is_irrelevant = Command::halo_sea_command(true, Some(-50.));

        assert_eq!(
            manual_level_is_irrelevant.to_bytes().unwrap(),
            [0x11, 0xc1, 0x01, 0x00, 0xce, 0x04]
        );
    }

    /// The stay-alive is what holds the radar up, so it rides along with the
    /// state queries while somebody is watching ...
    #[test]
    fn report_requests_include_the_stay_alive_while_watched() {
        let frames = Command::report_request_frames(true);

        assert_eq!(frames, vec![&[0x04, 0xc2], &[0x01, 0xc2], &[0xa0, 0xc1]]);
    }

    /// ... and only the stay-alive is left out when the radar should stand
    /// down: the queries keep the radar's state arriving.
    #[test]
    fn report_requests_drop_only_the_stay_alive_when_standing_down() {
        let frames = Command::report_request_frames(false);

        assert_eq!(frames, vec![&[0x04, 0xc2], &[0x01, 0xc2]]);
    }
}
