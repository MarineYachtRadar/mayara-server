use async_trait::async_trait;
use std::fmt::Write;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use std::f64::consts::TAU;

use super::protocol::{
    CommandId, CommandMode, GUARD_MODE_FAN, GUARD_MODE_OFF, HEADING_ADJUST_FULL_TURN, SPOKES,
    TX_CHANNEL_AUTO, TX_CHANNEL_MAX, WIRE_UNIT_KM, WIRE_UNIT_NM, meters_to_wire_index_for_unit,
    wire_unit_for_meters,
};
use crate::brand::CommandSender;
use crate::radar::range::Ranges;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError, RadarInfo};

/// A sector's start and width on the wire wrap at a full circle of degrees.
const FULL_CIRCLE_DEGREES: i32 = 360;

/// The dual range id of Range B.
const RANGE_B: i32 = 1;

pub(crate) struct Command {
    key: String,
    write: Option<Box<dyn AsyncWrite + Send + Unpin>>,
    controls: SharedControls,
    ranges: Ranges,
    /// Dual range ID appended to per-range commands (0 = Range A, 1 = Range B).
    /// Set by the receiver before each set_control call to target the correct range.
    pub dual_range_id: i32,
    /// Whether this radar supports dual range (NXT models).
    pub has_dual_range: bool,
}

impl Command {
    pub(crate) fn new(info: &RadarInfo, has_dual_range: bool) -> Self {
        Command {
            key: info.key(),
            write: None,
            controls: info.controls.clone(),
            ranges: info.ranges.clone(),
            dual_range_id: 0,
            has_dual_range,
        }
    }

    pub(crate) fn set_writer<W: AsyncWrite + Send + Unpin + 'static>(&mut self, write: W) {
        self.write = Some(Box::new(write));
    }

    pub(crate) fn set_ranges(&mut self, ranges: Ranges) {
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
                if let Err(e) = w.write_all(&bytes).await {
                    // Drop the half-closed writer so the next call fast-
                    // fails with NotConnected and the periodic 5s report-
                    // request tick in data_loop sees the error and
                    // triggers a reconnect immediately, instead of every
                    // user PUT queuing more writes onto a dead socket.
                    // Furuno radars silently drop the control TCP socket
                    // after idle; SO_KEEPALIVE in start_command_stream
                    // shortens that window but doesn't eliminate it.
                    self.write = None;
                    return Err(RadarError::Io(e));
                }
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

    fn get_zone_values(controls: &SharedControls, control_id: &ControlId) -> (i32, i32, bool) {
        if let Some(control) = controls.get(control_id) {
            // A control holds SI, which for a sector angle is radians. The
            // radar wants the degrees its own report arrived in.
            let to_wire = |v: f64| {
                control
                    .item()
                    .wire_units
                    .map_or(v, |units| units.from_si(v))
                    .round() as i32
            };
            let start = control.value.map(to_wire).unwrap_or(0);
            let end = control.end_value.map(to_wire).unwrap_or(0);
            let enabled = control.enabled.unwrap_or(false);
            return (start, end, enabled);
        }
        (0, 0, false)
    }

    /// The `$S77` fields for both sectors. The radar blanks a sector whenever
    /// its width is not zero, so a disabled sector goes out as start 0 and
    /// width 0, as a chartplotter sends it.
    fn fill_blind_sector(
        controls: &SharedControls,
        zone1: Option<(i32, i32, bool)>,
        zone2: Option<(i32, i32, bool)>,
    ) -> Vec<i32> {
        let sector = |zone: Option<(i32, i32, bool)>, control_id| {
            let (start, end, enabled) =
                zone.unwrap_or_else(|| Self::get_zone_values(controls, control_id));
            // A control keeps angles within ±180°; the radar takes 0..359.
            if enabled {
                [
                    start.rem_euclid(FULL_CIRCLE_DEGREES),
                    (end - start).rem_euclid(FULL_CIRCLE_DEGREES),
                ]
            } else {
                [0, 0]
            }
        };
        let [s1_start, s1_width] = sector(zone1, &ControlId::NoTransmitSector1);
        let [s2_start, s2_width] = sector(zone2, &ControlId::NoTransmitSector2);

        // The radar ignores the first field.
        vec![0, s1_start, s1_width, s2_start, s2_width]
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
        if self.controls.contains_key(&ControlId::AntennaHeight) {
            self.send(CommandMode::Request, CommandId::AntennaHeight, &[0, 0, 0])
                .await?; // $R84,0,0,0
        }
        if self.controls.contains_key(&ControlId::BearingAlignment) {
            self.send(CommandMode::Request, CommandId::HeadingAdjust, &[])
                .await?; // $R81
        }
        if self.controls.contains_key(&ControlId::TransmitChannel) {
            self.send(CommandMode::Request, CommandId::TxChannel, &[])
                .await?; // $REC
        }

        self.send(CommandMode::Request, CommandId::BlindSector, &[])
            .await?; // $R77

        self.send(CommandMode::Request, CommandId::JammingAble, &[])
            .await?; // $RE8

        // The radar sends $N7D only when a signal bit changes, so ask.
        self.send(CommandMode::Request, CommandId::Alarm, &[])
            .await?; // $R7D

        // STC (Sensitivity Time Control) curves
        if self.controls.contains_key(&ControlId::NearStcCurve) {
            self.send(CommandMode::Request, CommandId::NearSTC, &[])
                .await?; // $R85
            self.send(CommandMode::Request, CommandId::MiddleSTC, &[])
                .await?; // $R86
            self.send(CommandMode::Request, CommandId::FarSTC, &[])
                .await?; // $R87
        }

        if self.controls.contains_key(&ControlId::NoiseRejection) {
            self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 3])
                .await?; // $R67,0,3 - Noise Reduction
        }
        if self.controls.contains_key(&ControlId::BirdMode) {
            // NXT-specific features (query signal processing features)
            self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 0])
                .await?; // $R67,0,0 - Interference Rejection

            self.send(CommandMode::Request, CommandId::RezBoost, &[])
                .await?; // $REE - Beam sharpening (Target Separation)

            self.send(CommandMode::Request, CommandId::BirdMode, &[])
                .await?; // $RED - Bird mode

            self.send(CommandMode::Request, CommandId::TargetAnalyzer, &[])
                .await?; // $REF - Target Analyzer (Doppler)
        }

        if self.has_dual_range {
            self.request_range_b().await?;
        }

        // Note: dual range is NOT activated automatically. The radar only starts
        // sending Range B spokes after receiving a Range command with drid=1.
        // This happens when the user sets a range on Range B via the GUI.

        Ok(())
    }

    /// Range B's own settings. A request without a range answers for Range
    /// A, so each of these names Range B in the field its report carries the
    /// range in, its power status included: Range B reports standby while
    /// Range A transmits alone.
    async fn request_range_b(&mut self) -> Result<(), RadarError> {
        let mut requests: Vec<(CommandId, &[i32])> = vec![
            (CommandId::Status, &[0, RANGE_B]),     // $R69,0,1
            (CommandId::Range, &[0, 0, RANGE_B]),   // $R62,0,0,1
            (CommandId::Gain, &[0, 0, RANGE_B]),    // $R63,0,0,1
            (CommandId::Sea, &[0, 0, 0, RANGE_B]),  // $R64,0,0,0,1
            (CommandId::Rain, &[0, 0, 0, RANGE_B]), // $R65,0,0,0,1
        ];
        if self.controls.contains_key(&ControlId::Tune) {
            requests.push((CommandId::Tune, &[0, 0, RANGE_B])); // $R75,0,0,1
        }
        if self.controls.contains_key(&ControlId::BirdMode) {
            requests.extend([
                (CommandId::SignalProcessing, &[0, 0, 0, RANGE_B][..]), // $R67,0,0,0,1
                (CommandId::RezBoost, &[0, RANGE_B]),                   // $REE,0,1
                (CommandId::BirdMode, &[0, RANGE_B]),                   // $RED,0,1
            ]);
        }
        for (command_id, args) in requests {
            self.send(CommandMode::Request, command_id, args).await?;
        }
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
            Err(_) if cv.auto.is_some() && cv.value.is_none() => controls
                .get(&cv.id)
                .and_then(|c| c.value)
                .map(|v| v as i32)
                .unwrap_or(0),
            // Power is the one control whose value need not be a number: it
            // reads the value itself, and takes "transmit" as readily as 2,
            // as Navico and Garmin do. A word it does not know is still
            // refused here, or it would arrive as the standby the parser
            // falls back to and stand a transmitting radar down.
            Err(_)
                if cv.id == ControlId::Power
                    && cv
                        .value
                        .as_ref()
                        .is_some_and(|v| Power::from_value(v).is_ok()) =>
            {
                0
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
                cmd = Self::fill_blind_sector(controls, Some((value, end_value, enabled)), None);

                CommandId::BlindSector
            }
            ControlId::NoTransmitSector2 => {
                let end_value = cv.end_as_f64().map(|v| v as i32).unwrap_or(0);
                let enabled = cv.enabled.unwrap_or(false);
                cmd = Self::fill_blind_sector(controls, None, Some((value, end_value, enabled)));

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
            ControlId::BearingAlignment => {
                // Format: $S81,{tenths of a degree},0
                // The radar takes 0..3599, so a negative alignment goes out
                // as the same bearing counted the other way round.
                let tenths = (cv.as_f64()? * 10.).round() as i32;
                cmd.push(tenths.rem_euclid(HEADING_ADJUST_FULL_TURN));
                cmd.push(0);
                CommandId::HeadingAdjust
            }
            ControlId::TransmitChannel => {
                // Format: $SEC,{channel}: 0 = Auto, 1-3 = that channel
                cmd.push(if auto == 1 {
                    TX_CHANNEL_AUTO
                } else {
                    value.clamp(1, TX_CHANNEL_MAX)
                });
                CommandId::TxChannel
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
                // Per-range: $SEE,{level},{drid}
                // RezBoost (beam sharpening): 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::RezBoost
            }
            ControlId::BirdMode => {
                // Per-range: $SED,{level},{drid}
                // BirdMode: 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(self.dual_range_id);
                CommandId::BirdMode
            }
            ControlId::AntiJamming => {
                // Format: $SE8,{value}  (0=Off, 1=On)
                cmd.push(value);
                CommandId::JammingAble
            }
            ControlId::EchoFormat => {
                cmd.push(value);
                CommandId::ImoEchoSwitch
            }
            ControlId::Doppler => {
                // Format: $SEF,{enabled},{mode},0,1
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
                // A chartplotter sends 1 here. Left out, the radar reads
                // 999 and reports that before putting the 1 back.
                cmd.push(1);
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
                let zone_index: i32 = if cv.id == ControlId::GuardZone1 { 0 } else { 1 };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Brand;
    use crate::Cli;
    use crate::brand::furuno::protocol::{PIXEL_VALUES, RadarModel, SPOKE_LEN};
    use crate::brand::furuno::settings;
    use crate::config::GuardZone;
    use crate::radar::SharedRadars;
    use clap::Parser;
    use serde_json::json;
    use std::io;
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    /// Everything the radar would have received, in order.
    #[derive(Clone, Default)]
    struct Wire(Arc<Mutex<Vec<u8>>>);

    impl Wire {
        /// The sentences sent so far, with their line endings stripped.
        fn sentences(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .split_terminator('\n')
                .map(|s| s.trim_end_matches('\r').to_string())
                .collect()
        }

        /// The first sentence, which for a control set is the command itself;
        /// what follows is the read-back every set is chased with.
        fn first(&self) -> String {
            self.sentences().first().expect("a sentence").clone()
        }
    }

    impl AsyncWrite for Wire {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A radar of `model` with its writer captured, as the receiver would hand
    /// it over once the control socket is up.
    fn radar(model: RadarModel) -> (Command, RadarInfo, Wire) {
        let radars = SharedRadars::new();
        let args = Cli::parse_from(["mayara-server"]);
        let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 10000);
        let mut info = RadarInfo::new(
            &radars,
            &args,
            Brand::Furuno,
            Some("TEST0001"),
            None,
            None,
            PIXEL_VALUES,
            SPOKES,
            SPOKE_LEN,
            addr,
            Ipv4Addr::new(10, 0, 0, 1),
            addr,
            addr,
            addr,
            |id, tx| settings::new(id, tx, &args),
            true,
            true,
        );
        // The discovery path names the radar before the model report arrives.
        info.controls.set_user_name(info.key());
        settings::update_when_model_known(&mut info, model, "1.00");

        let mut command = Command::new(&info, model.is_nxt());
        let wire = Wire::default();
        command.set_writer(wire.clone());
        (command, info, wire)
    }

    /// An NXT, the model with every control the brand knows about.
    fn nxt() -> (Command, RadarInfo, Wire) {
        radar(RadarModel::DRS4DNXT)
    }

    fn cv(id: ControlId, value: serde_json::Value) -> ControlValue {
        ControlValue::new(id, value)
    }

    async fn set(command: &mut Command, info: &RadarInfo, cv: ControlValue) {
        command
            .set_control(&cv, &info.controls)
            .await
            .expect("the control is one the radar has a command for");
    }

    // ----- The sentence itself -----

    /// Every command is an NMEA-style sentence: a `$`, the mode letter, the
    /// command id in hex, comma-separated arguments, CR LF.
    #[tokio::test]
    async fn a_command_is_a_sentence_the_radar_can_parse() {
        let (mut command, _info, wire) = nxt();

        command
            .send(CommandMode::Set, CommandId::Gain, &[0, 80, 0, 80, 0])
            .await
            .unwrap();

        assert_eq!(wire.0.lock().unwrap().as_slice(), b"$S63,0,80,0,80,0\r\n");
    }

    /// The login sentence is the one packet that ends in a bare LF: it carries
    /// its arguments as trailing commas, and the radar rejects it with a CR.
    #[tokio::test]
    async fn trailing_commas_replace_the_carriage_return() {
        let (mut command, _info, wire) = nxt();

        command
            .send_with_commas(CommandMode::Request, CommandId::Status, &[], 3)
            .await
            .unwrap();

        assert_eq!(wire.0.lock().unwrap().as_slice(), b"$R69,,,\n");
    }

    /// A Furuno radar drops its control socket when idle. The write that
    /// discovers this has to drop the writer, or every later PUT queues onto a
    /// socket that will never drain and the reconnect never happens.
    #[tokio::test]
    async fn a_command_without_a_connection_is_refused() {
        let (mut command, info, _wire) = nxt();
        command.write = None;

        let err = command
            .set_control(&cv(ControlId::Gain, json!(80)), &info.controls)
            .await
            .unwrap_err();

        assert!(matches!(err, RadarError::NotConnected), "{err:?}");
    }

    /// Controls mayara keeps to itself -- the ones the radar has no command
    /// for -- must be refused rather than sent as some default sentence.
    #[tokio::test]
    async fn a_control_the_radar_has_no_command_for_is_refused() {
        let (mut command, info, wire) = nxt();

        let err = command
            .set_control(&cv(ControlId::TargetTrails, json!(1)), &info.controls)
            .await
            .unwrap_err();

        assert!(
            matches!(err, RadarError::CannotSetControlId(ControlId::TargetTrails)),
            "{err:?}"
        );
        assert!(wire.sentences().is_empty(), "nothing goes out");
    }

    // ----- Power and the watchman -----

    /// Transmit is status 2, and carries the watchman periods with it: the
    /// radar takes power and timed idle in one sentence.
    #[tokio::test]
    async fn a_transmit_request_carries_the_watchman_periods() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::Power, json!(2))).await;

        assert_eq!(wire.first(), "$S69,2,0,0,60,540,0");
    }

    /// Standby is status 1. Anything that is not Transmit -- including a value
    /// the radar never sends, like Fault -- stands the antenna down rather
    /// than leaving it turning.
    #[tokio::test]
    async fn anything_that_is_not_transmit_stands_the_antenna_down() {
        for value in [json!(0), json!(1), json!(4)] {
            let (mut command, info, wire) = nxt();

            set(&mut command, &info, cv(ControlId::Power, value.clone())).await;

            assert_eq!(wire.first(), "$S69,1,0,0,60,540,0", "power {value}");
        }
    }

    /// A word the power parser does not know is refused, rather than
    /// arriving as the standby it falls back to: a client that misspells
    /// "transmit" must not stand a turning antenna down.
    #[tokio::test]
    async fn a_power_value_that_names_nothing_is_refused() {
        let (mut command, info, wire) = nxt();

        let err = command
            .set_control(&cv(ControlId::Power, json!("tranmsit")), &info.controls)
            .await
            .unwrap_err();

        assert!(
            matches!(err, RadarError::CannotSetControlId(ControlId::Power)),
            "{err:?}"
        );
        assert!(wire.sentences().is_empty());
    }

    /// Power is read by name as readily as by number, the way Navico and
    /// Garmin read it. The GUI sends the number, but a client speaking the
    /// API directly need not.
    #[tokio::test]
    async fn a_power_value_by_name_is_understood() {
        for (value, expected) in [
            (json!("transmit"), "$S69,2,0,0,60,540,0"),
            (json!("standby"), "$S69,1,0,0,60,540,0"),
            (json!("2"), "$S69,2,0,0,60,540,0"),
        ] {
            let (mut command, info, wire) = nxt();

            set(&mut command, &info, cv(ControlId::Power, value.clone())).await;

            assert_eq!(wire.first(), expected, "power {value}");
        }
    }

    /// Timed idle is a duty cycle, not a period: the standby half is whatever
    /// is left of ten minutes after the transmit half.
    #[tokio::test]
    async fn the_timed_idle_cycle_stays_at_ten_minutes() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::TimedRun, json!(300))).await;

        assert_eq!(wire.first(), "$S69,1,0,0,300,300,0");
    }

    /// A transmit period long enough to leave no standby is clamped: the radar
    /// is given a minute off rather than a zero it would have to interpret.
    #[tokio::test]
    async fn a_transmit_period_that_fills_the_cycle_still_leaves_a_minute() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::TimedRun, json!(580))).await;

        assert_eq!(wire.first(), "$S69,1,0,0,580,60,0");
    }

    // ----- Range -----

    /// Six nautical miles is wire index 9 in unit 0. The indices are not in
    /// range order, so nothing but the table can produce them.
    #[tokio::test]
    async fn a_nautical_range_is_sent_as_its_wire_index() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::Range, json!(11112))).await;

        assert_eq!(wire.first(), "$S62,9,0,0");
    }

    /// A metric range switches the unit as well as the index: 4 km is index 8
    /// in unit 1, where index 8 in unit 0 would have been 4 nm.
    #[tokio::test]
    async fn a_metric_range_switches_the_wire_unit() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::Range, json!(4000))).await;

        assert_eq!(wire.first(), "$S62,8,1,0");
    }

    /// Range B is the same command with the dual range id set. Getting this
    /// wrong points both ranges at one antenna half.
    #[tokio::test]
    async fn a_range_b_command_carries_the_dual_range_id() {
        let (mut command, info, wire) = nxt();
        command.dual_range_id = 1;

        set(&mut command, &info, cv(ControlId::Range, json!(11112))).await;

        assert_eq!(wire.first(), "$S62,9,0,1");
    }

    /// Changing the unit re-sends the range the radar is already on, read in
    /// the new unit's table: 6 nm becomes the nearest kilometre range rather
    /// than the same index in a different unit.
    #[tokio::test]
    async fn changing_the_unit_resends_the_current_range() {
        let (mut command, info, wire) = nxt();
        info.controls
            .set_value(&ControlId::Range, json!(11112))
            .expect("range");

        set(&mut command, &info, cv(ControlId::RangeUnits, json!(1))).await;

        assert_eq!(wire.first(), "$S62,11,1,0");
    }

    // ----- Gain, sea, rain -----

    /// Gain, sea and rain each put the dual range id in a different place, and
    /// each carries a fixed auto value the radar expects but mayara does not
    /// expose. These are the sentences that went out before any of this was
    /// tested; they are pinned so a re-ordering shows up here.
    #[tokio::test]
    async fn the_clutter_controls_each_order_their_arguments_differently() {
        let cases = [
            (ControlId::Gain, "$S63,0,80,0,80,0"),
            (ControlId::Sea, "$S64,0,80,50,0,0,0"),
            (ControlId::Rain, "$S65,0,80,0,0,0,0"),
        ];

        for (id, expected) in cases {
            let (mut command, info, wire) = nxt();

            set(&mut command, &info, cv(id, json!(80))).await;

            assert_eq!(wire.first(), expected, "{id:?}");
        }
    }

    /// Asking for auto keeps the number: the radar is told which mode to be in
    /// and what to fall back to, in one sentence.
    #[tokio::test]
    async fn an_auto_request_keeps_the_number_beside_the_flag() {
        let (mut command, info, wire) = nxt();
        let mut value = cv(ControlId::Gain, json!(55));
        value.auto = Some(true);

        set(&mut command, &info, value).await;

        assert_eq!(wire.first(), "$S63,1,55,0,80,0");
    }

    /// A GUI that flips auto on without touching the slider sends no value at
    /// all. The radar still needs one, so the value it is already on is used
    /// rather than a zero that would dim the picture.
    #[tokio::test]
    async fn an_auto_request_without_a_value_uses_the_value_the_radar_is_on() {
        let (mut command, info, wire) = nxt();
        info.controls
            .set_value(&ControlId::Gain, json!(42))
            .expect("gain");
        let mut value = ControlValue {
            auto: Some(true),
            ..cv(ControlId::Gain, json!(0))
        };
        value.value = None;

        set(&mut command, &info, value).await;

        assert_eq!(wire.first(), "$S63,1,42,0,80,0");
    }

    // ----- No-transmit sectors -----

    /// The GUI gives a sector as start and end; the radar wants start and
    /// width. A sector that straddles north has an end below its start, and
    /// the width has to come out positive.
    #[tokio::test]
    async fn a_sector_across_north_still_has_a_positive_width() {
        let (mut command, info, wire) = nxt();
        let mut value = cv(ControlId::NoTransmitSector1, json!(350));
        value.end_value = Some(10.);
        value.enabled = Some(true);

        set(&mut command, &info, value).await;

        // The second sector, never enabled, rides along without a width.
        assert_eq!(wire.first(), "$S77,0,350,20,0,0");
    }

    /// Both sectors travel in one sentence, so setting the second one carries
    /// the first one along, at the angles the radar last reported for it --
    /// read back out of the radians a control stores, or a sector reported at
    /// 100 degrees would go back as 1 and disappear.
    #[tokio::test]
    async fn setting_the_second_sector_carries_the_first_one_along() {
        let (mut command, info, wire) = nxt();
        info.controls
            .set_sector(&ControlId::NoTransmitSector1, 100., 130., Some(true))
            .expect("sector 1 as the radar reported it");

        let mut second = cv(ControlId::NoTransmitSector2, json!(200));
        second.end_value = Some(260.);
        second.enabled = Some(true);
        set(&mut command, &info, second).await;

        assert_eq!(wire.first(), "$S77,0,100,30,200,60");
    }

    /// A sector set on Range B carries the other sector along as Range B
    /// holds it, not Range A, which may not have caught up yet.
    #[tokio::test]
    async fn a_range_b_sector_carries_range_b_s_other_sector() {
        let (mut command, _, wire) = nxt();
        let (_, range_b, _) = nxt();
        range_b
            .controls
            .set_sector(&ControlId::NoTransmitSector1, 100., 130., Some(true))
            .expect("sector 1 as Range B holds it");
        command.dual_range_id = 1;

        let mut second = cv(ControlId::NoTransmitSector2, json!(200));
        second.end_value = Some(260.);
        second.enabled = Some(true);
        set(&mut command, &range_b, second).await;

        assert_eq!(wire.first(), "$S77,0,100,30,200,60");
    }

    /// The radar blanks a sector whenever it has a width, whatever the first
    /// field says: this one used to keep blanking after it was switched off.
    /// A disabled sector goes out as a chartplotter sends it, with start and
    /// width both 0.
    #[tokio::test]
    async fn a_disabled_sector_goes_out_without_a_width() {
        let (mut command, info, wire) = nxt();
        info.controls
            .set_sector(&ControlId::NoTransmitSector2, 200., 260., Some(true))
            .expect("sector 2 as the radar reported it");
        let mut value = cv(ControlId::NoTransmitSector1, json!(124));
        value.end_value = Some(268.);
        value.enabled = Some(false);

        set(&mut command, &info, value).await;

        assert_eq!(wire.first(), "$S77,0,0,0,200,60");
    }

    // ----- NXT features -----

    /// Target separation and bird mode are kept per range, so Range B's
    /// setting must not land on Range A.
    #[tokio::test]
    async fn a_range_b_target_separation_and_bird_mode_name_range_b() {
        let (mut command, info, wire) = nxt();
        command.dual_range_id = 1;

        set(
            &mut command,
            &info,
            cv(ControlId::TargetSeparation, json!(2)),
        )
        .await;
        set(&mut command, &info, cv(ControlId::BirdMode, json!(3))).await;

        let sent = wire.sentences();
        for expected in ["$SEE,2,1", "$SED,3,1"] {
            assert!(sent.iter().any(|s| s == expected), "{expected} in {sent:?}");
        }
    }

    // ----- Installation -----

    /// The alignment goes out in tenths of a degree, and the radar takes only
    /// 0..3599: a negative alignment is the same bearing counted the other
    /// way round.
    #[tokio::test]
    async fn bearing_alignment_goes_out_as_tenths_of_a_degree_within_one_turn() {
        let cases = [
            (json!(7), "$S81,70,0"),
            (json!(12.3), "$S81,123,0"),
            (json!(0), "$S81,0,0"),
            (json!(-5), "$S81,3550,0"),
            (json!(-0.1), "$S81,3599,0"),
            (json!(180), "$S81,1800,0"),
            (json!(-180), "$S81,1800,0"),
        ];

        for (value, expected) in cases {
            let (mut command, info, wire) = nxt();

            set(
                &mut command,
                &info,
                cv(ControlId::BearingAlignment, value.clone()),
            )
            .await;

            assert_eq!(wire.first(), expected, "{value}");
        }
    }

    /// Auto is channel 0 to the radar. A channel the user picks, 1 to 3, goes
    /// out as itself, also when the same request turns auto off.
    #[tokio::test]
    async fn transmit_channel_sends_auto_as_channel_zero() {
        let (mut command, info, wire) = nxt();
        let mut auto = cv(ControlId::TransmitChannel, json!(2));
        auto.auto = Some(true);
        let mut manual = cv(ControlId::TransmitChannel, json!(2));
        manual.auto = Some(false);

        set(
            &mut command,
            &info,
            cv(ControlId::TransmitChannel, json!(3)),
        )
        .await;
        set(&mut command, &info, auto).await;
        set(&mut command, &info, manual).await;

        let sent: Vec<String> = wire
            .sentences()
            .into_iter()
            .filter(|s| s.starts_with("$SEC"))
            .collect();
        assert_eq!(sent, ["$SEC,3", "$SEC,0", "$SEC,2"]);
    }

    // ----- Signal processing -----

    /// Main bang suppression is a percentage to the user and a byte to the
    /// radar.
    #[tokio::test]
    async fn main_bang_suppression_scales_percent_to_a_byte() {
        for (percent, expected) in [(0, 0), (50, 127), (100, 255)] {
            let (mut command, info, wire) = nxt();

            set(
                &mut command,
                &info,
                cv(ControlId::MainBangSuppression, json!(percent)),
            )
            .await;

            assert_eq!(wire.first(), format!("$S83,{expected},0"), "{percent}%");
        }
    }

    /// Noise reduction and interference rejection are the same command with a
    /// different feature number -- and interference rejection is switched on
    /// with a 2, not the 1 that every other flag uses.
    #[tokio::test]
    async fn the_two_signal_processing_features_differ_in_more_than_their_number() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::NoiseRejection, json!(1))).await;
        set(
            &mut command,
            &info,
            cv(ControlId::InterferenceRejection, json!(1)),
        )
        .await;
        set(&mut command, &info, cv(ControlId::NoiseRejection, json!(0))).await;
        set(
            &mut command,
            &info,
            cv(ControlId::InterferenceRejection, json!(0)),
        )
        .await;

        let sent: Vec<String> = wire
            .sentences()
            .into_iter()
            .filter(|s| s.starts_with("$S67"))
            .collect();
        assert_eq!(
            sent,
            [
                "$S67,0,3,1,0",
                "$S67,0,0,2,0",
                "$S67,0,3,0,0",
                "$S67,0,0,0,0"
            ]
        );
    }

    /// The Doppler control is one number to the user and two to the radar: on
    /// or off, and which of the two modes. A value outside the three the GUI
    /// offers turns it off rather than picking a mode at random.
    #[tokio::test]
    async fn doppler_splits_into_an_enable_and_a_mode() {
        let cases = [
            (0, "$SEF,0,0,0,1"),
            (1, "$SEF,1,0,0,1"),
            (2, "$SEF,1,1,0,1"),
            (7, "$SEF,0,0,0,1"),
        ];

        for (value, expected) in cases {
            let (mut command, info, wire) = nxt();

            set(&mut command, &info, cv(ControlId::Doppler, json!(value))).await;

            assert_eq!(wire.first(), expected, "doppler {value}");
        }
    }

    /// The level controls that take a screen argument, and the ones that take
    /// the dual range id. Both trail their value with a number that is not the
    /// value, which is exactly the kind of thing that gets swapped.
    #[tokio::test]
    async fn the_level_controls_send_their_trailing_argument() {
        let cases = [
            (ControlId::TargetSeparation, "$SEE,2,0"),
            (ControlId::BirdMode, "$SED,2,0"),
            (ControlId::NearStcCurve, "$S85,2,0"),
            (ControlId::MiddleStcCurve, "$S86,2,0"),
            (ControlId::FarStcCurve, "$S87,2,0"),
            (ControlId::StcRange, "$SD2,2,0"),
            (ControlId::ScanSpeed, "$S89,2,0"),
            (ControlId::AntiJamming, "$SE8,2"),
            (ControlId::AntennaHeight, "$S84,0,2,0"),
            (ControlId::Tune, "$S75,0,2,0"),
        ];

        for (id, expected) in cases {
            let (mut command, info, wire) = nxt();

            set(&mut command, &info, cv(id, json!(2))).await;

            assert_eq!(wire.first(), expected, "{id:?}");
        }
    }

    /// The STC curves and tuning are per-range; on Range B they have to say so.
    #[tokio::test]
    async fn the_per_range_controls_carry_the_dual_range_id() {
        let cases = [
            (ControlId::NearStcCurve, "$S85,2,1"),
            (ControlId::MiddleStcCurve, "$S86,2,1"),
            (ControlId::FarStcCurve, "$S87,2,1"),
            (ControlId::StcRange, "$SD2,2,1"),
            (ControlId::Tune, "$S75,0,2,1"),
        ];

        for (id, expected) in cases {
            let (mut command, info, wire) = nxt();
            command.dual_range_id = 1;

            set(&mut command, &info, cv(id, json!(2))).await;

            assert_eq!(wire.first(), expected, "{id:?}");
        }
    }

    // ----- Guard zones -----

    /// A guard zone is two sentences: where the fan is, then the mode that
    /// switches it on. The angles go out in the radar's 8192 spokes per turn,
    /// not degrees or radians.
    #[tokio::test]
    async fn an_enabled_guard_zone_sends_its_fan_and_then_its_mode() {
        let (mut command, info, wire) = nxt();
        info.controls.set_guard_zone(
            &ControlId::GuardZone1,
            &GuardZone {
                start_angle: 0.,
                end_angle: std::f64::consts::FRAC_PI_2,
                start_distance: 100.,
                end_distance: 500.,
                enabled: true,
            },
        );

        set(&mut command, &info, cv(ControlId::GuardZone1, json!(1))).await;

        assert_eq!(wire.sentences(), ["$S99,0,0,2048,100,500", "$S98,1,0,0"]);
    }

    /// Switching a zone off sends the mode alone: there is no fan to describe.
    #[tokio::test]
    async fn a_disabled_guard_zone_sends_only_its_mode() {
        let (mut command, info, wire) = nxt();
        info.controls.set_guard_zone(
            &ControlId::GuardZone2,
            &GuardZone {
                start_angle: 0.,
                end_angle: 1.,
                start_distance: 100.,
                end_distance: 500.,
                enabled: false,
            },
        );

        set(&mut command, &info, cv(ControlId::GuardZone2, json!(0))).await;

        assert_eq!(wire.sentences(), ["$S98,0,0,1"]);
    }

    /// A zone the radar was never told about is switched off, not left in
    /// whatever state the last zone put it in.
    #[tokio::test]
    async fn a_guard_zone_that_was_never_set_is_switched_off() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::GuardZone1, json!(1))).await;

        assert_eq!(wire.sentences(), ["$S98,0,0,0"]);
    }

    /// Angles arrive as radians and have to come out inside one turn: a full
    /// turn is spoke 0, and a negative angle counts back from the top.
    #[test]
    fn an_angle_is_wrapped_into_the_radars_spokes() {
        assert_eq!(radians_to_spokes(0.), 0);
        assert_eq!(radians_to_spokes(TAU), 0);
        assert_eq!(radians_to_spokes(TAU / 4.), 2048);
        assert_eq!(radians_to_spokes(-TAU / 4.), 6144);
        assert_eq!(radians_to_spokes(3. * TAU), 0);
    }

    // ----- What a set is chased with -----

    /// Every set is followed by a read-back, because the radar does not
    /// acknowledge a set and the GUI would otherwise show the value the user
    /// asked for rather than the one the radar took.
    #[tokio::test]
    async fn a_set_is_chased_with_a_read_back() {
        let (mut command, info, wire) = nxt();

        set(&mut command, &info, cv(ControlId::Gain, json!(80))).await;

        assert_eq!(wire.sentences(), ["$S63,0,80,0,80,0", "$R66"]);
    }

    /// A magnetron radar picks its own pulse width from the range, so its
    /// read-back asks for that too. A solid-state NXT has no pulse to report.
    #[tokio::test]
    async fn a_magnetron_radar_is_also_asked_for_its_pulse_width() {
        let (mut command, info, wire) = radar(RadarModel::DRS4DL);

        set(&mut command, &info, cv(ControlId::Gain, json!(80))).await;

        assert_eq!(wire.sentences(), ["$S63,0,80,0,80,0", "$R66", "$R68"]);
    }

    // ----- Startup -----

    /// What mayara asks a radar at startup. The list is the whole reason a
    /// freshly discovered radar arrives in the GUI with its controls filled
    /// in, and every entry is a control the model actually has: the
    /// NXT's noise rejection does nothing, so it is not asked about.
    #[tokio::test]
    async fn the_startup_queries_cover_the_models_controls() {
        let (mut command, _info, wire) = nxt();

        command.send_report_requests().await.unwrap();

        assert_eq!(
            wire.sentences(),
            [
                "$RE3",       // alive check
                "$R96",       // modules
                "$R8E,0",     // operating hours
                "$R8F,0",     // transmit hours
                "$R69",       // power status
                "$R62",       // range
                "$R63",       // gain
                "$R64",       // sea
                "$R65",       // rain
                "$R75",       // tune
                "$R89",       // scan speed
                "$R83,0,0",   // main bang size
                "$R84,0,0,0", // antenna height
                "$R81",       // bearing alignment
                "$REC",       // transmit channel
                "$R77",       // no-transmit sectors
                "$RE8",       // anti-jamming
                "$R7D",       // signal alarms
                "$R85",       // near STC curve
                "$R86",       // middle STC curve
                "$R87",       // far STC curve
                "$R67,0,0",   // interference rejection
                "$REE",       // target separation
                "$RED",       // bird mode
                "$REF",       // target analyzer
                // Range B, which answers only when asked by name
                "$R69,0,1",
                "$R62,0,0,1",
                "$R63,0,0,1",
                "$R64,0,0,0,1",
                "$R65,0,0,0,1",
                "$R75,0,0,1",
                "$R67,0,0,0,1",
                "$REE,0,1",
                "$RED,0,1",
            ]
        );
    }

    /// A radar without the NXT signal processing, transmit channel or antenna
    /// height is not asked about them: a query for a control it does not have
    /// draws an error reply.
    #[tokio::test]
    async fn a_radar_is_not_asked_about_controls_it_does_not_have() {
        let (mut command, _info, wire) = radar(RadarModel::DRS4DL);

        command.send_report_requests().await.unwrap();

        let sentences = wire.sentences();
        for absent in [
            "$REE",
            "$RED",
            "$REF",
            "$R67,0,3",
            "$R67,0,0",
            "$REC",
            "$R84,0,0,0",
        ] {
            assert!(
                !sentences.contains(&absent.to_string()),
                "{absent} went out anyway: {sentences:?}"
            );
        }
        assert!(sentences.contains(&"$R69".to_string()), "{sentences:?}");
    }
}
