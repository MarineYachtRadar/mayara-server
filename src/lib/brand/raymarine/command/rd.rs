use deku::DekuWrite;

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

pub async fn set_control(
    command: &mut Command,
    cv: &ControlValue,
    value: f64,
    _controls: &SharedControls, // Not used now, but useful if controls depend on other controls
) -> Result<(), RadarError> {
    let deci_value = (value * 10.0) as i32;
    let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
    let _enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
    let v = Command::scale_100_to_byte(value); // todo! use transform values

    let mut cmd = Vec::with_capacity(6);

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
            let ranges = &command.info.ranges;
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
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x01, 0x83], v);
            }
        }
        ControlId::Sea => {
            on_off_command(&mut cmd, &[0x02, 0x83], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x02, 0x83], v);
            }
        }
        ControlId::Rain => {
            on_off_command(&mut cmd, &[0x03, 0x83], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x03, 0x83], v);
            }
        }
        ControlId::Ftc => {
            let on_off = 1 - auto; // Ftc is really an on/off switch, so invert auto
            on_off_command(&mut cmd, &[0x04, 0x83], on_off);
            if on_off == 1 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x04, 0x83], v);
            }
        }
        ControlId::MainBangSuppression => {
            standard_command(&mut cmd, &[0x01, 0x82], value as u8);
        }
        ControlId::DisplayTiming => {
            cmd.extend_from_slice(&[
                0x02, 0x82, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                v, // Display timing value at offset 8
                0x00, 0x00, 0x00,
            ]);
        }
        ControlId::InterferenceRejection => {
            cmd.extend_from_slice(&[
                0x07, 0x83, 0x01, 0x00,
                v, // Interference rejection at offset 4, 0 - off, 1 - normal, 2 - high
                0x00, 0x00, 0x00,
            ]);
        }

        // Non-hardware settings
        _ => return Err(RadarError::CannotSetControlId(cv.id)),
    };

    log::info!("{}: Send command {:02X?}", command.info.key(), cmd);
    command.send(&cmd).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RdOnOffCommand, RdValueCommand, on_off_command, standard_command};
    use crate::util::encode;

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
}
