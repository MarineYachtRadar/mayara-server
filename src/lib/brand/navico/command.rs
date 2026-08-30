use async_trait::async_trait;
use tokio::net::UdpSocket;

use crate::brand::CommandSender;
use crate::network::create_connected_send;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

use super::Model;
use super::protocol::{
    CATEGORY_CONTROL, CMD_ACCENT_LIGHT, CMD_BEARING_ALIGNMENT, CMD_DOPPLER,
    CMD_DOPPLER_SPEED_THRESHOLD, CMD_GAIN_VARIANT, CMD_HALO_SEA, CMD_HALO_TARGET_EXPANSION,
    CMD_INSTALLATION, CMD_INTERFERENCE_REJECTION, CMD_LOCAL_INTERFERENCE_REJECTION,
    CMD_NOISE_REJECTION, CMD_NOTRANSMIT_ENABLE, CMD_NOTRANSMIT_SECTOR, CMD_POWER_ON, CMD_RANGE,
    CMD_SCAN_SPEED, CMD_SEA_STATE, CMD_TARGET_BOOST, CMD_TARGET_EXPANSION, CMD_TARGET_SEPARATION,
    CMD_TRANSMIT, CMD_USE_MODE, COMMAND_STAY_ON_A, INSTALL_TAG_ANTENNA_HEIGHT,
    INSTALL_TAG_ANTENNA_OFFSET, REQUEST_STATE_BATCH, REQUEST_STATE_PROPERTIES,
};

// Last byte of the HALO sea clutter command: which of the three things the
// frame changes.
const HALO_SEA_MODE: u8 = 0x01;
const HALO_SEA_MANUAL: u8 = 0x02;
const HALO_SEA_AUTO_OFFSET: u8 = 0x04;

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
    fn halo_sea_frames(auto: bool, setting: Option<f64>, mode_requested: bool) -> Vec<[u8; 6]> {
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
    fn halo_sea_command(auto: bool, setting: Option<f64>) -> [u8; 6] {
        let mode = match (setting, auto) {
            (None, _) => HALO_SEA_MODE,
            (Some(_), false) => HALO_SEA_MANUAL,
            (Some(_), true) => HALO_SEA_AUTO_OFFSET,
        };
        let setting = setting.unwrap_or(0.).round() as i8;

        [
            CMD_HALO_SEA,
            CATEGORY_CONTROL,
            auto as u8,
            setting.max(0) as u8,
            setting as u8,
            mode,
        ]
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
        let mut cmd = Vec::with_capacity(12);

        log::info!(
            "send_no_transmit({}, {}, {}, {})",
            sector,
            value_start,
            value_end,
            enabled
        );

        cmd.extend_from_slice(&[
            CMD_NOTRANSMIT_ENABLE,
            CATEGORY_CONTROL,
            sector,
            0,
            0,
            0,
            enabled,
        ]);
        self.send(&cmd).await?;
        cmd.clear();
        cmd.extend_from_slice(&[
            CMD_NOTRANSMIT_SECTOR,
            CATEGORY_CONTROL,
            sector,
            0,
            0,
            0,
            enabled,
        ]);
        cmd.extend_from_slice(&value_start.to_le_bytes());
        cmd.extend_from_slice(&value_end.to_le_bytes());

        Ok(cmd)
    }

    pub(super) async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        self.send(&REQUEST_STATE_PROPERTIES).await?;
        self.send(&REQUEST_STATE_BATCH).await?;
        self.send(&COMMAND_STAY_ON_A).await?;
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
        let mut cmd = Vec::with_capacity(12);

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

                cmd.extend_from_slice(&[CMD_POWER_ON, CATEGORY_CONTROL, 0x01]);
                self.send(&cmd).await?;
                cmd.clear();
                cmd.extend_from_slice(&[CMD_TRANSMIT, CATEGORY_CONTROL, value]);
            }

            ControlId::Range => {
                let decimeters: i32 = deci_value;
                log::trace!("range {value} -> {decimeters}");

                cmd.extend_from_slice(&[CMD_RANGE, CATEGORY_CONTROL]);
                cmd.extend_from_slice(&decimeters.to_le_bytes());
            }
            ControlId::BearingAlignment => {
                let value: i16 = Self::mod_deci_degrees(deci_value) as i16;

                cmd.extend_from_slice(&[CMD_BEARING_ALIGNMENT, CATEGORY_CONTROL]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlId::Gain => {
                let v = Self::scale_100_to_byte(value);
                let auto = auto as u32;

                cmd.extend_from_slice(&[
                    CMD_GAIN_VARIANT,
                    CATEGORY_CONTROL,
                    0x00,
                    0x00,
                    0x00,
                    0x00,
                ]);
                cmd.extend_from_slice(&auto.to_le_bytes());
                cmd.extend_from_slice(&v.to_le_bytes());
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
                        log::debug!("{}: Send command {:02X?}", self.info.key(), frame);
                        self.send(&frame).await?;
                    }
                    cmd.extend_from_slice(&last);
                } else {
                    let v: u32 = Self::scale_100_to_byte(value) as u32;
                    let auto = auto as u32;

                    cmd.extend_from_slice(&[CMD_GAIN_VARIANT, CATEGORY_CONTROL, 0x02]);
                    cmd.extend_from_slice(&auto.to_be_bytes());
                    cmd.extend_from_slice(&v.to_be_bytes());
                }
            }
            ControlId::Rain => {
                let v = Self::scale_100_to_byte(value);
                cmd.extend_from_slice(&[
                    CMD_GAIN_VARIANT,
                    CATEGORY_CONTROL,
                    0x04,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    v,
                ]);
            }
            ControlId::SideLobeSuppression => {
                let v = Self::scale_100_to_byte(value);

                cmd.extend_from_slice(&[
                    CMD_GAIN_VARIANT,
                    CATEGORY_CONTROL,
                    0x05,
                    0,
                    0,
                    0,
                    auto,
                    0,
                    0,
                    0,
                    v,
                ]);
            }
            ControlId::InterferenceRejection => {
                cmd.extend_from_slice(&[CMD_INTERFERENCE_REJECTION, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::TargetExpansion => {
                if self.model.is_halo() {
                    cmd.extend_from_slice(&[
                        CMD_HALO_TARGET_EXPANSION,
                        CATEGORY_CONTROL,
                        value as u8,
                    ]);
                } else {
                    cmd.extend_from_slice(&[CMD_TARGET_EXPANSION, CATEGORY_CONTROL, value as u8]);
                }
            }
            ControlId::TargetBoost => {
                cmd.extend_from_slice(&[CMD_TARGET_BOOST, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::SeaState => {
                cmd.extend_from_slice(&[CMD_SEA_STATE, CATEGORY_CONTROL, value as u8]);
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
                cmd.extend_from_slice(&[
                    CMD_LOCAL_INTERFERENCE_REJECTION,
                    CATEGORY_CONTROL,
                    value as u8,
                ]);
            }
            ControlId::ScanSpeed => {
                cmd.extend_from_slice(&[CMD_SCAN_SPEED, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::Mode => {
                // Bird Plus (value 6) maps to tUseMode { mode: 5, variant: 1 }
                // All other modes: variant 0
                let (mode, variant) = if value as u8 == 6 {
                    (5u8, 1u8)
                } else {
                    (value as u8, 0u8)
                };
                cmd.extend_from_slice(&[CMD_USE_MODE, CATEGORY_CONTROL, mode, variant]);
            }
            ControlId::NoiseRejection => {
                cmd.extend_from_slice(&[CMD_NOISE_REJECTION, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::TargetSeparation => {
                cmd.extend_from_slice(&[CMD_TARGET_SEPARATION, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::Doppler => {
                cmd.extend_from_slice(&[CMD_DOPPLER, CATEGORY_CONTROL, value as u8]);
            }
            ControlId::DopplerSpeedThreshold => {
                let value = f64::round(value * 100.0) as u16;
                let value = value.clamp(0, 1594);
                cmd.extend_from_slice(&[CMD_DOPPLER_SPEED_THRESHOLD, CATEGORY_CONTROL]);
                cmd.extend_from_slice(&value.to_le_bytes());
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
                cmd.extend_from_slice(&[
                    CMD_INSTALLATION,
                    CATEGORY_CONTROL,
                    INSTALL_TAG_ANTENNA_OFFSET,
                    0,
                    0,
                    0,
                ]);
                cmd.extend_from_slice(&ahead_mm.to_le_bytes());
                cmd.extend_from_slice(&starboard_mm.to_le_bytes());
            }
            ControlId::AntennaHeight => {
                let height_mm = (value * 1000.) as i32;
                cmd.extend_from_slice(&[
                    CMD_INSTALLATION,
                    CATEGORY_CONTROL,
                    INSTALL_TAG_ANTENNA_HEIGHT,
                    0,
                    0,
                    0,
                ]);
                cmd.extend_from_slice(&height_mm.to_le_bytes());
            }
            ControlId::AccentLight => {
                cmd.extend_from_slice(&[CMD_ACCENT_LIGHT, CATEGORY_CONTROL, value as u8]);
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
    use super::Command;

    /// A number and a mode are two commands. Asking for both has to send
    /// both, mode first, or the radar applies the number and stays in the
    /// mode it was in -- the user's "manual at 90" silently becoming "auto,
    /// with 90 stored for later".
    #[test]
    fn a_sea_request_changing_mode_and_number_sends_the_mode_first() {
        let frames = Command::halo_sea_frames(false, Some(90.), true);

        assert_eq!(
            frames,
            vec![
                [0x11, 0xc1, 0x00, 0x00, 0x00, 0x01], // mode manual
                [0x11, 0xc1, 0x00, 0x5a, 0x5a, 0x02], // level 90
            ]
        );
    }

    /// A number on its own leaves the radar's mode alone: nothing about a
    /// level says the user wanted out of auto.
    #[test]
    fn a_sea_request_carrying_only_a_number_sends_one_frame() {
        let frames = Command::halo_sea_frames(false, Some(90.), false);

        assert_eq!(frames, vec![[0x11, 0xc1, 0x00, 0x5a, 0x5a, 0x02]]);
    }

    /// A mode on its own is already one frame, and must not be sent twice.
    #[test]
    fn a_sea_request_carrying_only_a_mode_sends_one_frame() {
        assert_eq!(
            Command::halo_sea_frames(true, None, true),
            vec![[0x11, 0xc1, 0x01, 0x00, 0x00, 0x01]]
        );
    }

    /// Switching into auto with an offset needs the mode too, or the offset
    /// lands while the radar is still manual.
    #[test]
    fn a_sea_request_entering_auto_with_an_offset_sends_the_mode_first() {
        let frames = Command::halo_sea_frames(true, Some(-50.), true);

        assert_eq!(
            frames,
            vec![
                [0x11, 0xc1, 0x01, 0x00, 0x00, 0x01], // mode auto
                [0x11, 0xc1, 0x01, 0x00, 0xce, 0x04], // auto offset -50
            ]
        );
    }

    /// Every frame an MFD was captured sending for sea clutter, so a HALO is
    /// told about a change the same way whoever sends it.
    #[test]
    fn halo_sea_command_matches_mfd_captures() {
        // Mode alone, carrying no number.
        assert_eq!(
            Command::halo_sea_command(false, None),
            [0x11, 0xc1, 0x00, 0x00, 0x00, 0x01]
        );
        assert_eq!(
            Command::halo_sea_command(true, None),
            [0x11, 0xc1, 0x01, 0x00, 0x00, 0x01]
        );

        // Auto, adjusted by an offset that is signed in the second byte and
        // only present in the first while positive.
        assert_eq!(
            Command::halo_sea_command(true, Some(0.)),
            [0x11, 0xc1, 0x01, 0x00, 0x00, 0x04]
        );
        assert_eq!(
            Command::halo_sea_command(true, Some(-1.)),
            [0x11, 0xc1, 0x01, 0x00, 0xff, 0x04]
        );
        assert_eq!(
            Command::halo_sea_command(true, Some(-50.)),
            [0x11, 0xc1, 0x01, 0x00, 0xce, 0x04]
        );
        assert_eq!(
            Command::halo_sea_command(true, Some(50.)),
            [0x11, 0xc1, 0x01, 0x32, 0x32, 0x04]
        );

        // Manual, where the level reaches 100 and both bytes carry it.
        assert_eq!(
            Command::halo_sea_command(false, Some(100.)),
            [0x11, 0xc1, 0x00, 0x64, 0x64, 0x02]
        );
        assert_eq!(
            Command::halo_sea_command(false, Some(0.)),
            [0x11, 0xc1, 0x00, 0x00, 0x00, 0x02]
        );
    }

    /// The manual level and the auto offset are separate settings; sending one
    /// must never leak the other into the frame. This is what stopped auto
    /// adjustments from taking effect: the first byte carried the manual
    /// level, so the radar was handed an offset it had not been asked for.
    #[test]
    fn halo_sea_auto_offset_ignores_the_manual_level() {
        let manual_level_is_irrelevant = Command::halo_sea_command(true, Some(-50.));

        assert_eq!(
            manual_level_is_irrelevant,
            [0x11, 0xc1, 0x01, 0x00, 0xce, 0x04]
        );
    }
}
