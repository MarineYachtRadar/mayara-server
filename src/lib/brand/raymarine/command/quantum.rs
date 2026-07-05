use crate::radar::settings::{ControlId, ControlValue, SharedControls};
use crate::radar::{Power, RadarError};

use super::Command;

fn one_byte_command(cmd: &mut Vec<u8>, lead: &[u8], value: u8) {
    two_byte_command(cmd, lead, (value as u16) << 8);
}

/// Quantum `SetRadarMode` code for a power state (`10 00 28 00 <code>`),
/// wire-confirmed from issue #160 captures: standby 0, transmit 1, off 3.
/// Preparing/Fault are radar-reported states, not commands — treat them as
/// standby if ever passed in.
fn power_mode_code(power: Power) -> u8 {
    match power {
        Power::Transmit => 1,
        Power::Off => 3,
        _ => 0,
    }
}

fn two_byte_command(cmd: &mut Vec<u8>, lead: &[u8], value: u16) {
    two_value_command(cmd, lead, value, 0);
}

fn two_value_command(cmd: &mut Vec<u8>, lead: &[u8], value1: u16, value2: u16) {
    cmd.extend_from_slice(lead);
    cmd.extend_from_slice(&[0x28, 0x00]);
    cmd.extend_from_slice(&value1.to_le_bytes());
    cmd.extend_from_slice(&value2.to_le_bytes());
}

pub async fn set_control(
    command: &mut Command,
    cv: &ControlValue,
    value: f64,
    controls: &SharedControls,
) -> Result<(), RadarError> {
    let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
    let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
    let v = value as u8; // todo! use transform values

    let mut cmd = Vec::with_capacity(6);

    match cv.id {
        ControlId::Power => {
            let power = Power::from_value(&cv.as_value()?).unwrap_or(Power::Standby);
            // A sleeping radar ignores the mode command; an Axiom precedes a
            // power-on with a WOL burst, so do the same. Only when actually
            // waking it — never when we are putting it to sleep. Harmless when
            // the radar is already awake.
            if power != Power::Off {
                super::super::send_wake_burst(&command.info.nic_addr).await;
            }
            let code = power_mode_code(power);
            cmd.extend_from_slice(&[0x10, 0x00, 0x28, 0x00, code, 0x00, 0x00, 0x00]);
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
            one_byte_command(&mut cmd, &[0x01, 0x01], index);
        }
        ControlId::Gain => {
            one_byte_command(&mut cmd, &[0x01, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                // SetGainValue_t lead is 0x02 0x03 (value in wire byte 5).
                // Axiom v4.09.167 libSystemFunctions SetThresholdValue@0x1cb4bdc
                // sends id 02 03 28 00; radar_pi's 0x02 0x83 is not a valid id.
                one_byte_command(&mut cmd, &[0x02, 0x03], v);
            }
        }
        ControlId::ColorGain => {
            one_byte_command(&mut cmd, &[0x03, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x04, 0x03], v);
            }
        }
        ControlId::Sea => {
            one_byte_command(&mut cmd, &[0x05, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x06, 0x03], v);
            }
        }
        ControlId::Rain => {
            one_byte_command(&mut cmd, &[0x0b, 0x03], enabled);
            if enabled > 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x0c, 0x03], v);
            }
        }
        ControlId::TargetExpansion => {
            one_byte_command(&mut cmd, &[0x0f, 0x03], v);
        }
        ControlId::InterferenceRejection => {
            // SetInterferenceRejection_t has no channel byte: the level is wire
            // byte 4. Axiom v4.09.167 libSystemFunctions SetInterferenceRejection
            // @0x1cb50ac stores it at msg+12 (byte 4); IsValid@0x1cbbe10 bounds
            // byte 4 < 6. two_byte_command writes the value LE into byte 4
            // (one_byte_command would leave byte 4 = 0, i.e. always "off").
            two_byte_command(&mut cmd, &[0x11, 0x03], v as u16);
        }
        ControlId::Mode => {
            one_byte_command(&mut cmd, &[0x14, 0x03], v);
        }
        ControlId::BearingAlignment => {
            let deci_value = (value * 10.0) as i16;
            two_byte_command(&mut cmd, &[0x01, 0x04], deci_value as u16);
        }
        ControlId::MainBangSuppression => {
            one_byte_command(&mut cmd, &[0x0a, 0x04], v);
        }
        ControlId::NoTransmitSector1 | ControlId::NoTransmitSector2 => {
            let sector = if cv.id == ControlId::NoTransmitSector1 {
                0
            } else {
                1
            };
            let value_start: i16 = (value * 10.0) as i16;
            let control = controls.get(&cv.id).unwrap();
            let end_value = cv
                .end_as_f64()
                .unwrap_or(control.end_as_f64().unwrap_or(0.));
            let value_end: i16 = (end_value * 10.0) as i16;
            cmd = send_no_transmit_cmd(command, value_start, value_end, enabled, sector).await?;
        }
        ControlId::SeaClutterCurve => {
            one_byte_command(&mut cmd, &[0x12, 0x03], v - 1);
        }
        ControlId::Doppler => {
            one_byte_command(&mut cmd, &[0x17, 0x03], v * 3); // 0x00 or 0x03
        }

        // Non-hardware settings
        _ => return Err(RadarError::CannotSetControlId(cv.id)),
    };

    log::info!("{}: Send command {:02X?}", command.info.key(), cmd);
    command.send(&cmd).await?;

    Ok(())
}

async fn send_no_transmit_cmd(
    command: &mut Command,
    value_start: i16,
    value_end: i16,
    enabled: u8,
    sector: u8,
) -> Result<Vec<u8>, RadarError> {
    let mut cmd = Vec::with_capacity(12);

    log::info!(
        "{}: send_no_transmit_cmd start={value_start} end={value_end} enabled={enabled} sector={sector}",
        command.info.key()
    );
    two_byte_command(
        &mut cmd,
        &[0x05, 0x04],
        sector as u16 + ((enabled as u16) << 8),
    );
    log::info!("{}: Send command1 {:02X?}", command.info.key(), cmd);

    command.send(&cmd).await?;
    cmd.clear();

    two_value_command(
        &mut cmd,
        &[0x03, 0x04],
        value_start as u16,
        value_end as u16,
    );
    cmd.extend_from_slice(&[sector]);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::{one_byte_command, power_mode_code, two_byte_command};
    use crate::radar::Power;

    // Wire-confirmed Quantum SetRadarMode codes (issue #160): standby 0,
    // transmit 1, off 3. Off must not collapse into standby.
    #[test]
    fn power_mode_codes_match_the_wire() {
        assert_eq!(power_mode_code(Power::Standby), 0);
        assert_eq!(power_mode_code(Power::Transmit), 1);
        assert_eq!(power_mode_code(Power::Off), 3);
    }

    // Channel commands (gain, sea, rain, range, ...) carry channel in wire byte 4
    // and the value in byte 5. `one_byte_command` must therefore place the value
    // in byte 5 (byte 4 = 0).
    #[test]
    fn one_byte_command_puts_value_in_byte5() {
        let mut cmd = Vec::new();
        one_byte_command(&mut cmd, &[0x05, 0x03], 0x42);
        assert_eq!(cmd, vec![0x05, 0x03, 0x28, 0x00, 0x00, 0x42, 0x00, 0x00]);
    }

    // SetInterferenceRejection_t has no channel byte: the level is wire byte 4.
    // `two_byte_command` writes the value little-endian so it lands in byte 4.
    #[test]
    fn two_byte_command_puts_value_in_byte4() {
        let mut cmd = Vec::new();
        two_byte_command(&mut cmd, &[0x11, 0x03], 0x05);
        assert_eq!(cmd, vec![0x11, 0x03, 0x28, 0x00, 0x05, 0x00, 0x00, 0x00]);
    }
}
