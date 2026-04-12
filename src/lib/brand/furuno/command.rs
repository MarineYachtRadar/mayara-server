use async_trait::async_trait;
use std::fmt::Write;
use tokio::io::{AsyncWriteExt, WriteHalf};
use tokio::net::TcpStream;

use std::f64::consts::TAU;

use super::protocol::{
    CommandId, CommandMode, GUARD_MODE_FAN, GUARD_MODE_OFF, SPOKES, WIRE_UNIT_KM, WIRE_UNIT_NM,
    meters_to_wire_index_for_unit, wire_unit_for_meters,
};
use crate::brand::CommandSender;
use crate::radar::range::Ranges;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

pub(crate) struct Command {
    key: String,
    write: Option<WriteHalf<TcpStream>>,
    controls: SharedControls,
    ranges: Ranges,
    /// Dual range ID appended to per-range commands (0 = Range A, 1 = Range B).
    /// Set by the receiver before each set_control call to target the correct range.
    pub dual_range_id: i32,
    /// Whether this radar supports dual range (NXT models).
    pub has_dual_range: bool,
}

impl Command {
    pub fn new(info: &RadarInfo, has_dual_range: bool) -> Self {
        Command {
            key: info.key(),
            write: None,
            controls: info.controls.clone(),
            ranges: info.ranges.clone(),
            dual_range_id: 0,
            has_dual_range,
        }
    }

    pub fn set_writer(&mut self, write: WriteHalf<TcpStream>) {
        self.write = Some(write);
    }

    pub fn set_ranges(&mut self, ranges: Ranges) {
        self.ranges = ranges;
    }

    pub async fn send(
        &mut self,
        cm: CommandMode,
        id: CommandId,
        args: &[i32],
    ) -> Result<(), RadarError> {
        self.send_with_commas(cm, id, args, 0).await
    }

    pub async fn send_with_commas(
        &mut self,
        cm: CommandMode,
        id: CommandId,
        args: &[i32],
        commas: u32,
    ) -> Result<(), RadarError> {
        let mut message = format!("${}{:X}", cm.to_char(), id as u32);
        for arg in args {
            let _ = write!(&mut message, ",{}", arg);
        }
        for _ in 0..commas {
            message.push(',');
        }

        log::trace!("{}: sending {}", self.key, message);

        if commas == 0 {
            message.push('\r');
        }
        message.push('\n');

        let bytes = message.into_bytes();

        match &mut self.write {
            Some(w) => {
                w.write_all(&bytes).await.map_err(RadarError::Io)?;
            }
            None => return Err(RadarError::NotConnected),
        };

        Ok(())
    }

    fn get_timed_idle_enabled(controls: &SharedControls) -> i32 {
        controls
            .get(&ControlId::TimedIdle)
            .and_then(|c| c.value)
            .map(|v| v as i32)
            .unwrap_or(0)
    }

    fn get_timed_idle_transmit(controls: &SharedControls) -> i32 {
        controls
            .get(&ControlId::TimedRun)
            .and_then(|c| c.value)
            .map(|v| v as i32)
            .unwrap_or(60)
    }

    fn get_timed_idle_standby(controls: &SharedControls) -> i32 {
        // Standby period = 600 - transmit period (so total cycle stays at 10 minutes)
        // Clamped to 60..600 range
        let transmit = Self::get_timed_idle_transmit(controls);
        (600 - transmit).max(60)
    }

    fn get_zone_values(&self, control_id: &ControlId) -> (i32, i32, bool) {
        if let Some(control) = self.controls.get(control_id) {
            let start = control.value.map(|v| v as i32).unwrap_or(0);
            let end = control.end_value.map(|v| v as i32).unwrap_or(0);
            let enabled = control.enabled.unwrap_or(false);
            return (start, end, enabled);
        }
        (0, 0, false)
    }

    fn fill_blind_sector(
        &mut self,
        zone1: Option<(i32, i32, bool)>,
        zone2: Option<(i32, i32, bool)>,
    ) -> Vec<i32> {
        let mut cmd = Vec::with_capacity(5);

        // Get current values from zone controls
        let (s1_start, s1_end, _s1_enabled) =
            zone1.unwrap_or_else(|| self.get_zone_values(&ControlId::NoTransmitSector1));
        let (s2_start, s2_end, s2_enabled) =
            zone2.unwrap_or_else(|| self.get_zone_values(&ControlId::NoTransmitSector2));

        // Calculate widths from start/end angles
        let s1_width = if s1_end >= s1_start {
            s1_end - s1_start
        } else {
            360 + s1_end - s1_start
        };

        let s2_width = if s2_end >= s2_start {
            s2_end - s2_start
        } else {
            360 + s2_end - s2_start
        };

        // Format: $S77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}
        let s2_enable = if s2_enabled && s2_width > 0 { 1 } else { 0 };
        cmd.push(s2_enable);
        cmd.push(s1_start);
        cmd.push(s1_width);
        cmd.push(s2_start);
        cmd.push(s2_width);

        cmd
    }

    pub(crate) async fn init(&mut self) -> Result<(), RadarError> {
        // Query firmware/model information
        self.send(CommandMode::Request, CommandId::Modules, &[])
            .await?; // $R96

        // Query operating hours
        self.send(CommandMode::Request, CommandId::OnTime, &[0])
            .await?; // $R8E,0

        // Query transmit hours
        self.send(CommandMode::Request, CommandId::TxTime, &[0])
            .await?; // $R8F,0

        // Query current state of all controls (Range A)
        self.send(CommandMode::Request, CommandId::Status, &[])
            .await?; // $R69

        self.send(CommandMode::Request, CommandId::Range, &[])
            .await?; // $R62

        if self.controls.contains_key(&ControlId::PulseWidth) {
            self.send(CommandMode::Request, CommandId::PulseWidth, &[])
                .await?; // $R68
        }

        self.send(CommandMode::Request, CommandId::Gain, &[])
            .await?; // $R63

        self.send(CommandMode::Request, CommandId::Sea, &[]).await?; // $R64

        self.send(CommandMode::Request, CommandId::Rain, &[])
            .await?; // $R65

        if self.controls.contains_key(&ControlId::Tune) {
            self.send(CommandMode::Request, CommandId::Tune, &[])
                .await?; // $R75
        }
        if self.controls.contains_key(&ControlId::ScanSpeed) {
            self.send(CommandMode::Request, CommandId::ScanSpeed, &[])
                .await?; // $R89
        }
        if self.controls.contains_key(&ControlId::MainBangSuppression) {
            self.send(CommandMode::Request, CommandId::MainBangSize, &[0, 0])
                .await?; // $R83,0,0
        }

        self.send(CommandMode::Request, CommandId::BlindSector, &[])
            .await?; // $R77

        self.send(CommandMode::Request, CommandId::JammingAble, &[])
            .await?; // $RE8

        // STC (Sensitivity Time Control) curves
        if self.controls.contains_key(&ControlId::NearStcCurve) {
            self.send(CommandMode::Request, CommandId::NearSTC, &[])
                .await?; // $R85
            self.send(CommandMode::Request, CommandId::MiddleSTC, &[])
                .await?; // $R86
            self.send(CommandMode::Request, CommandId::FarSTC, &[])
                .await?; // $R87
        }

        if self.controls.contains_key(&ControlId::BirdMode) {
            // NXT-specific features (query signal processing features)
            self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 3])
                .await?; // $R67,0,3 - Noise Reduction

            self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 0])
                .await?; // $R67,0,0 - Interference Rejection

            self.send(CommandMode::Request, CommandId::RezBoost, &[])
                .await?; // $REE - Beam sharpening (Target Separation)

            self.send(CommandMode::Request, CommandId::BirdMode, &[])
                .await?; // $RED - Bird mode

            self.send(CommandMode::Request, CommandId::TargetAnalyzer, &[])
                .await?; // $REF - Target Analyzer (Doppler)
        }

        // Note: dual range is NOT activated automatically. The radar only starts
        // sending Range B spokes after receiving a Range command with drid=1.
        // This happens when the user sets a range on Range B via the GUI.

        Ok(())
    }

    pub async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        log::debug!("{}: send_report_requests", self.key);

        self.send(CommandMode::Request, CommandId::AliveCheck, &[])
            .await?;
        self.init().await?;
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
        // For auto-only requests (no explicit value), use the current control
        // value so the radar receives a valid command.
        let value = match cv.as_i32() {
            Ok(v) => v,
            Err(_) if cv.auto.is_some() && cv.value.is_none() => {
                controls
                    .get(&cv.id)
                    .and_then(|c| c.value)
                    .map(|v| v as i32)
                    .unwrap_or(0)
            }
            Err(e) => return Err(e),
        };
        let auto: i32 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let _enabled: i32 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        log::trace!("set_control: {:?} = {:?} => {:.1}", cv.id, cv.value, value);

        let mut cmd = Vec::with_capacity(6);

        let id: CommandId = match cv.id {
            ControlId::Power => {
                // Wire format: $S69,{status},{drid},{wman},{w_send},{w_stop},0
                let value = match Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby) {
                    Power::Transmit => 2,
                    _ => 1,
                };

                let wman = Self::get_timed_idle_enabled(controls);
                let w_send = Self::get_timed_idle_transmit(controls);
                let w_stop = Self::get_timed_idle_standby(controls);

                cmd.push(value); // status
                cmd.push(self.dual_range_id);
                cmd.push(wman);
                cmd.push(w_send);
                cmd.push(w_stop);
                cmd.push(0);

                CommandId::Status
            }

            ControlId::TimedIdle | ControlId::TimedRun => {
                // Resend the Status command with updated watchman settings.
                // Wire format: $S69,{status},{drid},{wman},{w_send},{w_stop},0
                let power = controls
                    .get(&ControlId::Power)
                    .and_then(|c| c.value)
                    .map(|v| v as i32)
                    .unwrap_or(Power::Standby as i32);
                let status = if power == Power::Transmit as i32 {
                    2
                } else {
                    1
                };

                let wman = if cv.id == ControlId::TimedIdle {
                    value // the new value being set
                } else {
                    Self::get_timed_idle_enabled(controls)
                };
                let w_send = if cv.id == ControlId::TimedRun {
                    value
                } else {
                    Self::get_timed_idle_transmit(controls)
                };
                let w_stop = (600 - w_send).max(60);

                cmd.push(status);
                cmd.push(self.dual_range_id);
                cmd.push(wman);
                cmd.push(w_send);
                cmd.push(w_stop);
                cmd.push(0);

                CommandId::Status
            }

            ControlId::Range => {
                // Determine wire unit from the range value (metric vs nautical)
                let wire_unit = wire_unit_for_meters(value);
                let wire_index = meters_to_wire_index_for_unit(value, wire_unit);
                cmd.push(wire_index);
                cmd.push(wire_unit);
                cmd.push(self.dual_range_id);
                CommandId::Range
            }

            ControlId::RangeUnits => {
                // When changing range units, re-send the current range with the new unit.
                // The radar firmware reinterprets the range index in the new unit context.
                // value: 0=Nautical, 1=Metric
                let wire_unit = if value == 1 {
                    WIRE_UNIT_KM
                } else {
                    WIRE_UNIT_NM
                };

                // Get the current range in meters from the target range's
                // controls (not self.controls, which is Range A's handle and
                // would be wrong when the unit change is targeting Range B).
                let current_range = controls
                    .get(&ControlId::Range)
                    .and_then(|c| c.value)
                    .map(|v| v as i32)
                    .unwrap_or(11112); // default 6 NM

                // Find the closest range in the new unit's wire table
                let wire_index = meters_to_wire_index_for_unit(current_range, wire_unit);
                cmd.push(wire_index);
                cmd.push(wire_unit);
                cmd.push(self.dual_range_id);
                CommandId::Range
            }

            ControlId::Gain => {
                // Per-range: $S63,{auto},{value},{drid},{auto_val},0
                cmd.push(auto);
                cmd.push(value);
                cmd.push(self.dual_range_id);
                cmd.push(80);
                cmd.push(0);
                CommandId::Gain
            }
            ControlId::Sea => {
                // Per-range: $S64,{auto},{value},{auto_val},{drid},0,0
                cmd.push(auto);
                cmd.push(value);
                cmd.push(50);
                cmd.push(self.dual_range_id);
                cmd.push(0);
                cmd.push(0);
                CommandId::Sea
            }
            ControlId::Rain => {
                // Per-range: $S65,{auto},{value},0,{drid},0,0
                cmd.push(auto);
                cmd.push(value);
                cmd.push(0);
                cmd.push(self.dual_range_id);
                cmd.push(0);
                cmd.push(0);
                CommandId::Rain
            }

            ControlId::NoTransmitSector1 => {
                let end_value = cv.end_as_f64().map(|v| v as i32).unwrap_or(0);
                let enabled = cv.enabled.unwrap_or(false);
                cmd = self.fill_blind_sector(Some((value, end_value, enabled)), None);

                CommandId::BlindSector
            }
            ControlId::NoTransmitSector2 => {
                let end_value = cv.end_as_f64().map(|v| v as i32).unwrap_or(0);
                let enabled = cv.enabled.unwrap_or(false);
                cmd = self.fill_blind_sector(None, Some((value, end_value, enabled)));

                CommandId::BlindSector
            }
            ControlId::ScanSpeed => {
                // Format: $S89,{mode},0 where mode: 0=24RPM, 2=Auto
                cmd.push(value);
                cmd.push(0);
                CommandId::ScanSpeed
            }
            ControlId::Tune => {
                // Per-range: $S75,{auto},{value},{dual_range_id}
                cmd.push(auto);
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::Tune
            }
            ControlId::AntennaHeight => {
                // Format: $S84,0,{meters},0
                cmd.push(0);
                cmd.push(value);
                cmd.push(0);
                CommandId::AntennaHeight
            }
            ControlId::MainBangSuppression => {
                // Format: $S83,{value_255},0
                // Map 0-100% to 0-255
                let value_255 = (value * 255) / 100;
                cmd.push(value_255);
                cmd.push(0);
                CommandId::MainBangSize
            }

            // NXT-specific features
            ControlId::NoiseRejection => {
                // Format: $S67,0,3,{enabled},0
                // Feature 3 = Noise Reduction
                let enabled = if value > 0 { 1 } else { 0 };
                cmd.push(0);
                cmd.push(3);
                cmd.push(enabled);
                cmd.push(0);
                CommandId::SignalProcessing
            }
            ControlId::InterferenceRejection => {
                // Format: $S67,0,0,{enabled},0
                // Feature 0 = Interference Rejection
                // Note: enabled=2 (not 1) per protocol spec
                let enabled = if value > 0 { 2 } else { 0 };
                cmd.push(0);
                cmd.push(0);
                cmd.push(enabled);
                cmd.push(0);
                CommandId::SignalProcessing
            }
            ControlId::TargetSeparation => {
                // Format: $SEE,{level},0
                // RezBoost (beam sharpening): 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(0); // screen: 0=Primary
                CommandId::RezBoost
            }
            ControlId::BirdMode => {
                // Format: $SED,{level},0
                // BirdMode: 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(0); // screen: 0=Primary
                CommandId::BirdMode
            }
            ControlId::AntiJamming => {
                // Format: $SE8,{value}  (0=Off, 1=On)
                cmd.push(value);
                CommandId::JammingAble
            }
            ControlId::Doppler => {
                // Format: $SEF,{enabled},{mode},0
                // Target Analyzer: value 0=Off, 1=Target, 2=Rain
                // Wire format: enabled=0/1, mode=0(Target)/1(Rain)
                let (enabled, mode) = match value {
                    0 => (0, 0), // Off
                    1 => (1, 0), // Target
                    2 => (1, 1), // Rain
                    _ => (0, 0), // Invalid, default to Off
                };
                cmd.push(enabled);
                cmd.push(mode);
                cmd.push(0); // screen: 0=Primary
                CommandId::TargetAnalyzer
            }

            ControlId::NearStcCurve => {
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::NearSTC
            }
            ControlId::MiddleStcCurve => {
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::MiddleSTC
            }
            ControlId::FarStcCurve => {
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::FarSTC
            }
            ControlId::StcRange => {
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::STCRange
            }

            ControlId::GuardZone1 | ControlId::GuardZone2 => {
                let zone_index: i32 = if cv.id == ControlId::GuardZone1 {
                    0
                } else {
                    1
                };

                if let Some(zone) = controls.guard_zone(&cv.id) {
                    if zone.enabled {
                        let start_spoke = radians_to_spokes(zone.start_angle);
                        let end_spoke = radians_to_spokes(zone.end_angle);
                        // TODO: verify range unit empirically on hardware
                        let inner_range = zone.start_distance as i32;
                        let outer_range = zone.end_distance as i32;

                        self.send(
                            CommandMode::Set,
                            CommandId::GuardFan,
                            &[zone_index, start_spoke, end_spoke, inner_range, outer_range],
                        )
                        .await?;
                        self.send(
                            CommandMode::Set,
                            CommandId::GuardMode,
                            &[GUARD_MODE_FAN, 0, zone_index],
                        )
                        .await?;
                    } else {
                        self.send(
                            CommandMode::Set,
                            CommandId::GuardMode,
                            &[GUARD_MODE_OFF, 0, zone_index],
                        )
                        .await?;
                    }
                } else {
                    self.send(
                        CommandMode::Set,
                        CommandId::GuardMode,
                        &[GUARD_MODE_OFF, 0, zone_index],
                    )
                    .await?;
                }

                log::info!(
                    "{}: Guard zone {} sent to hardware",
                    self.key,
                    zone_index + 1
                );
                return Ok(());
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlId(cv.id)),
        };

        log::info!(
            "{}: Send command {:02X},{:?}",
            self.key,
            id.clone() as u32,
            cmd
        );

        self.send(CommandMode::Set, id, &cmd).await?;
        self.send(CommandMode::Request, CommandId::CustomPictureAll, &[])
            .await?; // $R66
        if self.controls.contains_key(&ControlId::PulseWidth) {
            self.send(CommandMode::Request, CommandId::PulseWidth, &[])
                .await?; // $R68
        }
        Ok(())
    }
}

/// Convert an angle in radians to Furuno spoke units (0–8191).
fn radians_to_spokes(radians: f64) -> i32 {
    ((radians / TAU * SPOKES as f64).round() as i32).rem_euclid(SPOKES as i32)
}
