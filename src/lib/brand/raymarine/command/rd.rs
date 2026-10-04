use deku::DekuWrite;

use crate::radar::range::Ranges;
use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError};
use crate::util::encode;

use super::Command;

/// A 24-byte RD command that carries a level, at offset 20.
#[derive(DekuWrite, Debug, Default, PartialEq)]
#[deku(endian = "little")]
struct RdValueCommand {
    lead: [u8; 2],
    _head: [u8; 18],
    value: u8,
    _tail: [u8; 3],
}

/// A 24-byte RD command that carries an on/off flag, which sits four bytes
/// earlier in the frame than a level does.
#[derive(DekuWrite, Debug, Default, PartialEq)]
#[deku(endian = "little")]
struct RdOnOffCommand {
    lead: [u8; 2],
    _head: [u8; 14],
    on_off: u8,
    _tail: [u8; 7],
}

fn standard_command(cmd: &mut Vec<u8>, lead: &[u8], value: u8) {
    cmd.extend_from_slice(&encode(&RdValueCommand {
        lead: [lead[0], lead[1]],
        _head: [
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
        value,
        _tail: [0x00; 3],
    }));
}

fn on_off_command(cmd: &mut Vec<u8>, lead: &[u8], on_off: u8) {
    cmd.extend_from_slice(&encode(&RdOnOffCommand {
        lead: [lead[0], lead[1]],
        _head: [
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
        on_off,
        _tail: [0x00; 7],
    }));
}

/// Gain, sea, rain and FTC are sent in the raw range the radar advertises in
/// its fixed report; until that report has arrived, scale onto 0..255.
fn level_byte(controls: &SharedControls, control_id: &ControlId, value: f64) -> u8 {
    controls
        .wire_value(control_id, value)
        .map(|wire| wire.round() as u8)
        .unwrap_or_else(|| Command::scale_100_to_byte(value))
}

pub async fn set_control(
    command: &mut Command,
    cv: &ControlValue,
    value: f64,
    controls: &SharedControls,
) -> Result<(), RadarError> {
    for cmd in control_frames(&command.info.ranges, cv, value, controls)? {
        log::info!("{}: Send command {:02X?}", command.info.key(), cmd);
        command.send(&cmd).await?;
    }

    Ok(())
}

/// The datagrams that set `cv` on an RD radar, in the order they must be sent.
fn control_frames(
    ranges: &Ranges,
    cv: &ControlValue,
    value: f64,
    controls: &SharedControls,
) -> Result<Vec<Vec<u8>>, RadarError> {
    let deci_value = (value * 10.0) as i32;
    let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
    let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
    let v = Command::scale_100_to_byte(value); // todo! use transform values

    let mut frames = Vec::with_capacity(2);
    let mut cmd = Vec::with_capacity(24);

    match cv.id {
        ControlId::Power => {
            let value = match Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby) {
                Power::Transmit => 1,
                _ => 0,
            };
            cmd.extend_from_slice(&[0x01, 0x80, 0x01, 0x00, value, 0x00, 0x00, 0x00]);
        }

        ControlId::Range => {
            let value = value as i32;
            let index = if value < ranges.len() as i32 {
                value as u8
            } else {
                let mut i = 0;
                for r in ranges.all.iter() {
                    if r.distance() >= value {
                        break;
                    }
                    i += 1;
                }
                i
            };
            log::trace!("range {value} -> {index}");
            cmd.extend_from_slice(&[
                0x01, 0x81, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                index, // Range at offset 8 (0 - 1/8, 1 - 1/4, 2 - 1/2, 3 - 3/4, 4 - 1, 5 - 1.5, 6 - 3...)
                0x00, 0x00, 0x00,
            ]);
        }
        ControlId::BearingAlignment => {
            cmd.extend_from_slice(&[0x07, 0x82, 0x01, 0x00]);
            // to be consistent with the local bearing alignment of the pi
            // this bearing alignment works opposite to the one an a Lowrance display
            cmd.extend_from_slice(&(deci_value as u32).to_le_bytes());
        }

        ControlId::Gain => {
            on_off_command(&mut cmd, &[0x01, 0x83], auto);
            if auto == 0 {
                frames.push(std::mem::take(&mut cmd));
                standard_command(&mut cmd, &[0x01, 0x83], level_byte(controls, &cv.id, value));
            }
        }
        ControlId::Sea => {
            on_off_command(&mut cmd, &[0x02, 0x83], auto);
            if auto == 0 {
                frames.push(std::mem::take(&mut cmd));
                standard_command(&mut cmd, &[0x02, 0x83], level_byte(controls, &cv.id, value));
            }
        }
        ControlId::Rain => {
            on_off_command(&mut cmd, &[0x03, 0x83], enabled);
            if enabled == 1 {
                frames.push(std::mem::take(&mut cmd));
                standard_command(&mut cmd, &[0x03, 0x83], level_byte(controls, &cv.id, value));
            }
        }
        ControlId::Ftc => {
            on_off_command(&mut cmd, &[0x04, 0x83], enabled);
            if enabled == 1 {
                frames.push(std::mem::take(&mut cmd));
                standard_command(&mut cmd, &[0x04, 0x83], level_byte(controls, &cv.id, value));
            }
        }
        ControlId::MainBangSuppression => {
            on_off_command(&mut cmd, &[0x01, 0x82], value as u8);
        }
        ControlId::TargetExpansion => {
            let level = value as u8;
            cmd.extend_from_slice(&[
                0x06, 0x83, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                level, // Target expansion at offset 8: 0 - off, 1 - low, 2 - high
                0x00, 0x00, 0x00,
            ]);
        }
        ControlId::DisplayTiming => {
            cmd.extend_from_slice(&[
                0x02, 0x82, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                v, // Display timing value at offset 8
                0x00, 0x00, 0x00,
            ]);
        }
        ControlId::InterferenceRejection => {
            let level = value as u8;
            cmd.extend_from_slice(&[
                0x07, 0x83, 0x01, 0x00,
                level, // Interference rejection level at offset 4, 0 - off
                0x00, 0x00, 0x00,
            ]);
        }

        // Non-hardware settings
        _ => return Err(RadarError::CannotSetControlId(cv.id)),
    };

    frames.push(cmd);

    Ok(frames)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use serde_json::json;

    use super::{
        RdOnOffCommand, RdValueCommand, control_frames, level_byte, on_off_command,
        standard_command,
    };
    use crate::Cli;
    use crate::brand::raymarine::BaseModel;
    use crate::brand::raymarine::settings;
    use crate::radar::range::Ranges;
    use crate::radar::settings::{ControlId, ControlValue, SharedControls};
    use crate::util::encode;

    fn rd_controls() -> SharedControls {
        let args = Cli::parse_from(["mayara-server"]);
        let tx = tokio::sync::broadcast::Sender::new(1);
        settings::new("ray1234".to_string(), tx, &args, BaseModel::RD)
    }

    /// The level sent for gain, sea, rain and FTC sits in the range the radar
    /// advertised (gain 42..222 on a captured RD), so mid-scale is 132, not
    /// the 128 a plain 0..255 scaling gives. Regression for #729.
    #[test]
    fn level_is_sent_in_the_radar_advertised_range() {
        let controls = rd_controls();
        controls
            .map_wire_range(&ControlId::Gain, 42., 222.)
            .unwrap();

        assert_eq!(level_byte(&controls, &ControlId::Gain, 0.), 42);
        assert_eq!(level_byte(&controls, &ControlId::Gain, 50.), 132);
        assert_eq!(level_byte(&controls, &ControlId::Gain, 100.), 222);
    }

    /// Until the fixed report has advertised a range, levels scale onto 0..255.
    #[test]
    fn level_without_an_advertised_range_scales_onto_a_byte() {
        let controls = rd_controls();

        assert_eq!(level_byte(&controls, &ControlId::Gain, 50.), 128);
    }

    /// Nothing pinned the RD command frames before. Both are 24 bytes and
    /// differ only in where their payload sits, which is exactly the kind of
    /// thing that goes wrong unnoticed -- as it did while writing this.
    #[test]
    fn rd_command_templates_put_their_payload_where_the_radar_reads_it() {
        let mut cmd = Vec::new();
        standard_command(&mut cmd, &[0x01, 0x83], 0x42);
        assert_eq!(cmd.len(), 24);
        assert_eq!(cmd[0..2], [0x01, 0x83]);
        assert_eq!(cmd[20], 0x42, "a level sits at offset 20");
        assert_eq!(cmd[16], 0x00, "and not where an on/off flag goes");

        let mut cmd = Vec::new();
        on_off_command(&mut cmd, &[0x01, 0x83], 1);
        assert_eq!(cmd.len(), 24);
        assert_eq!(cmd[0..2], [0x01, 0x83]);
        assert_eq!(cmd[16], 0x01, "a flag sits at offset 16");
        assert_eq!(cmd[20], 0x00, "and not where a level goes");
    }

    #[test]
    fn rd_command_structs_are_both_24_bytes() {
        assert_eq!(encode(&RdValueCommand::default()).len(), 24);
        assert_eq!(encode(&RdOnOffCommand::default()).len(), 24);
    }

    fn frames(cv: ControlValue) -> Vec<Vec<u8>> {
        let value = cv.as_f64().unwrap_or(0.);
        control_frames(&Ranges::empty(), &cv, value, &rd_controls())
            .expect("the RD has a command for this control")
    }

    fn with_auto(id: ControlId, auto: bool) -> ControlValue {
        let mut cv = ControlValue::new(id, json!(50));
        cv.auto = Some(auto);
        cv
    }

    fn with_enabled(id: ControlId, enabled: bool) -> ControlValue {
        let mut cv = ControlValue::new(id, json!(50));
        cv.enabled = Some(enabled);
        cv
    }

    /// The auto flag sits at offset 16, behind a 1 at offset 8. Auto sends
    /// that frame alone; manual follows it with the level.
    #[test]
    fn gain_auto_sends_only_the_auto_flag() {
        let auto = frames(with_auto(ControlId::Gain, true));
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0][0..2], [0x01, 0x83]);
        assert_eq!(auto[0][8], 0x01);
        assert_eq!(auto[0][16], 0x01);

        let manual = frames(with_auto(ControlId::Gain, false));
        assert_eq!(manual.len(), 2);
        assert_eq!(manual[0][16], 0x00);
        assert_eq!(manual[1][20], 128);
    }

    /// Rain and FTC switch on and off with `enabled`, not `auto`: a GUI
    /// enable never carries auto, so keying on it sent rain "off" and FTC
    /// "on" whatever was asked. Regression for #729.
    #[test]
    fn rain_and_ftc_switch_on_enabled() {
        for (id, lead) in [(ControlId::Rain, 0x03), (ControlId::Ftc, 0x04)] {
            let on = frames(with_enabled(id, true));
            assert_eq!(on.len(), 2, "{id:?} on sends the flag, then the level");
            assert_eq!(on[0][0..2], [lead, 0x83]);
            assert_eq!(on[0][16], 0x01, "{id:?} on");
            assert_eq!(on[1][20], 128);

            let off = frames(with_enabled(id, false));
            assert_eq!(off.len(), 1, "{id:?} off sends only the flag");
            assert_eq!(off[0][16], 0x00, "{id:?} off");
        }
    }

    /// Wire-observed on an RD418D: a frame with the flag at offset 20 was
    /// ignored. RMRadar_pi's layout puts it at 16 behind a 1 at offset 8.
    #[test]
    fn main_bang_suppression_flag_sits_where_the_radar_reads_it() {
        let frames = frames(ControlValue::new(ControlId::MainBangSuppression, json!(0)));
        assert_eq!(
            frames,
            [vec![
                0x01, 0x82, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]]
        );
    }

    #[test]
    fn target_expansion_has_a_command() {
        let frames = frames(ControlValue::new(ControlId::TargetExpansion, json!(1)));
        assert_eq!(
            frames,
            [vec![
                0x06, 0x83, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            ]]
        );
    }

    /// The level is the list index, not scaled onto 0..255: level 1 used to
    /// go out as 3, and levels above that as values the radar rejects.
    #[test]
    fn interference_rejection_sends_the_level_itself() {
        let frames = frames(ControlValue::new(
            ControlId::InterferenceRejection,
            json!(2),
        ));
        assert_eq!(
            frames,
            [vec![0x07, 0x83, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00]]
        );
    }
}
