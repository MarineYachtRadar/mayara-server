use anyhow::bail;
use serde::Deserialize;
use std::mem::size_of;

use crate::brand::raymarine::command::Command;
use crate::brand::raymarine::report::{LookupDoppler, WireToLegendTable, wire_to_legend};
use crate::brand::raymarine::{RaymarineModel, hd_to_pixel_values, settings};
use crate::radar::range::{Range, Ranges};
use crate::radar::settings::ControlId;
use crate::radar::spoke::GenericSpoke;
use crate::radar::{Power, SpokeBearing};
use crate::util::decode_bin;

use super::{RaymarineReportReceiver, ReceiverState};

const QUANTUM_RADAR_RANGES: usize = 20;

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct FrameHeader {
    _type: u32, // 0x00280003
    _seq_num: u16,
    _something_1: u16,      // 0x0101
    scan_len: u16,          // 0x002b
    num_spokes: u16,        // 0x00fa
    _something_3: u16,      // 0x0008
    returns_per_range: u16, // number of radar returns per range from the status
    azimuth: u16,
    data_len: u16, // length of the rest of the data
}

const FRAME_HEADER_LENGTH: usize = size_of::<FrameHeader>();

pub(crate) fn process_frame(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state != ReceiverState::StatusRequestReceived {
        log::trace!("{}: Skip scan: not all reports seen", receiver.common.key);
        return;
    }

    if data.len() < FRAME_HEADER_LENGTH {
        log::warn!(
            "UDP data frame with even less than header, len {} dropped",
            data.len()
        );
        return;
    }
    let header = &data[..FRAME_HEADER_LENGTH];
    let header: FrameHeader = match decode_bin(header) {
        Ok(h) => h,
        Err(e) => {
            log::error!(
                "{}: Failed to deserialize header: {}",
                receiver.common.key,
                e
            );
            return;
        }
    };
    log::trace!("{}: FrameHeader {:?}", receiver.common.key, header);
    let nspokes = header.num_spokes;
    let returns_per_range = header.returns_per_range as u32;
    let returns_per_line = header.scan_len as u32;
    // Rotate image 180 degrees to get our "0 = up" view
    let azimuth = (header.azimuth + receiver.common.info.spokes_per_revolution / 2)
        % receiver.common.info.spokes_per_revolution as SpokeBearing;

    if nspokes != receiver.common.info.spokes_per_revolution {
        log::warn!(
            "{}: Invalid spokes per revolution {}",
            receiver.common.key,
            nspokes
        );
        return;
    }

    receiver.common.new_spoke_message();

    let next_offset = FRAME_HEADER_LENGTH;

    let data_len = header.data_len as usize;

    let spoke = &data[next_offset..next_offset + data_len];

    receiver.common.add_spoke(
        receiver.range_meters * returns_per_line / returns_per_range,
        azimuth,
        None,
        process_spoke(
            returns_per_line as usize,
            spoke,
            LookupDoppler::Doppler as usize,
            &receiver.wire_to_legend,
        ),
    );

    receiver.common.send_spoke_message();
}

fn process_spoke(
    returns_per_line: usize,
    spoke: &[u8],
    doppler: usize,
    wire_to_legend: &WireToLegendTable,
) -> GenericSpoke {
    let mut unpacked_data: Vec<u8> = Vec::with_capacity(1024);
    let mut src_offset: usize = 0;
    while src_offset < spoke.len() {
        if spoke[src_offset] != 0x5c {
            let pixel = spoke[src_offset] as usize;
            unpacked_data.push(wire_to_legend[doppler][pixel]);
            src_offset += 1;
        } else {
            let count = spoke[src_offset + 1] as usize; // number to be filled
            let pixel = spoke[src_offset + 2] as usize; // data to be filled
            let value = wire_to_legend[doppler][pixel];
            for _ in 0..count {
                unpacked_data.push(value);
            }
            src_offset += 3; // Marker byte, count, value
        }
    }
    unpacked_data.truncate(returns_per_line);

    unpacked_data
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct ControlsPerMode {
    gain_auto: u8,       // @ 0
    gain: u8,            // @ 1
    color_gain_auto: u8, // @ 2
    color_gain: u8,      // @ 3
    sea_auto: u8,        // @ 4
    sea: u8,             // @ 5
    rain_enabled: u8,    // @ 6
    rain: u8,            // @ 7
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct StatusReport {
    _id: [u8; 4],                        // @0 0x280002
    status: u8,                          // @4 0 - standby ; 1 - transmitting
    _something_1: [u8; 9],               // @5
    bearing_offset: [u8; 2],             // @14
    _something_2: u8,                    // @16
    interference_rejection: u8,          // @17
    _something_3: [u8; 2],               // @18
    range_index: u8,                     // @20
    mode: u8,                            // @21 harbor - 0, coastal - 1, offshore - 2, weather - 3
    controls: [ControlsPerMode; 4],      // @22 controls indexed by mode
    target_expansion: u8,                // @54
    sea_clutter_curve: u8,               // @55
    _something_10: [u8; 3],              // @56
    mbs_enabled: u8,                     // @59
    _something_11: [u32; 18],            // @60
    blank_start_1: [u8; 2],              // @132
    blank_end_1: [u8; 2],                // @134
    blank_enabled_1: u8,                 // @136
    _pad_1: [u8; 3],                     // @137
    blank_start_2: [u8; 2],              // @140
    blank_end_2: [u8; 2],                // @142
    blank_enabled_2: u8,                 // @144
    _pad_2: [u8; 3],                     // @145
    ranges: [u32; QUANTUM_RADAR_RANGES], // @148
    _something_12: [u8; 32],             // @228
}

const STATUS_REPORT_LENGTH: usize = size_of::<StatusReport>();

impl StatusReport {
    fn transmute(receiver: &RaymarineReportReceiver, data: &[u8]) -> Result<Self, anyhow::Error> {
        if data.len() < STATUS_REPORT_LENGTH {
            bail!(
                "{}: Invalid data length for fixed report: {}",
                receiver.common.key,
                data.len()
            );
        }
        let report = &data[0..STATUS_REPORT_LENGTH];
        let report: StatusReport = match decode_bin(report) {
            Ok(h) => h,
            Err(e) => {
                bail!(
                    "{}: Failed to deserialize header: {}",
                    receiver.common.key,
                    e
                );
            }
        };
        Ok(report)
    }
}

// Status byte values in the Quantum 0x280002 status report.
const STATUS_STANDBY: u8 = 0x00;
const STATUS_TRANSMIT: u8 = 0x01;
const STATUS_PREPARING: u8 = 0x02;
const STATUS_OFF: u8 = 0x03;
const STATUS_FAULT_SELF_TEST: u8 = 0x0a;

/// Map a Quantum status-report byte to a `Power` state, logging anything
/// outside the normal set. The self-test fault is surfaced distinctly rather
/// than masked as "unknown" (observed on a faulty unit asked to transmit),
/// though it still maps to Standby since the Power control has no fault state.
/// `key` is only used for the log line.
fn status_to_power(status: u8, key: &str) -> Power {
    match status {
        STATUS_STANDBY => Power::Standby,
        STATUS_TRANSMIT => Power::Transmit,
        STATUS_PREPARING => Power::Preparing,
        STATUS_OFF => Power::Off,
        STATUS_FAULT_SELF_TEST => {
            log::warn!(
                "{key}: radar reported FAULT status {STATUS_FAULT_SELF_TEST:#x} (self-test failure); no image will appear until it clears"
            );
            Power::Standby
        }
        other => {
            log::warn!("{key}: unknown status 0x{other:02x}");
            Power::Standby
        }
    }
}

pub(super) fn process_status_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_none() {
        return;
    }

    let report = match StatusReport::transmute(receiver, data) {
        Ok(r) => r,
        Err(_) => return,
    };
    log::debug!("{}: Quantum report {:?}", receiver.common.key, report);

    // Update controls based on the report
    let status = status_to_power(report.status, &receiver.common.key);
    receiver
        .common
        .set_value(&ControlId::Power, status as i32 as f64);

    if receiver.common.info.ranges.is_empty() {
        let mut ranges = Ranges::empty();

        // Can't use rust's iter() over report.ranges as it complains about packed data alignment
        for i in 0..QUANTUM_RADAR_RANGES {
            let range = report.ranges[i];
            let meters = (range as f64 * 1.852f64) as i32; // Convert to nautical miles

            ranges.push(Range::new(meters, i));
        }
        receiver.set_ranges(Ranges::new(ranges.all));
        log::info!(
            "{}: Ranges initialized: {}",
            receiver.common.key,
            receiver.common.info.ranges
        );
    }
    let range_meters = receiver
        .common
        .info
        .ranges
        .get_distance(report.range_index as usize);
    receiver
        .common
        .set_value(&ControlId::Range, range_meters as f64);
    receiver.range_meters = range_meters as u32;
    receiver.state = ReceiverState::StatusRequestReceived;

    let mode = report.mode as usize;
    if mode <= 3 {
        receiver.common.set_value(&ControlId::Mode, mode as f64);
        receiver.common.set_value_auto(
            &ControlId::Gain,
            report.controls[mode].gain as f64,
            report.controls[mode].gain_auto,
        );
        receiver.common.set_value_auto(
            &ControlId::ColorGain,
            report.controls[mode].color_gain as f64,
            report.controls[mode].color_gain_auto,
        );
        receiver.common.set_value_auto(
            &ControlId::Sea,
            report.controls[mode].sea as f64,
            report.controls[mode].sea_auto,
        );
        receiver.common.set_value_enabled(
            &ControlId::Rain,
            report.controls[mode].rain as f64,
            report.controls[mode].rain_enabled,
        );
    } else {
        log::warn!("{}: Unknown mode {}", receiver.common.key, report.mode);
    }
    receiver.common.set_value(
        &ControlId::SeaClutterCurve,
        (report.sea_clutter_curve + 1) as f64,
    );
    receiver
        .common
        .set_value(&ControlId::TargetExpansion, report.target_expansion as f64);
    receiver.common.set_value(
        &ControlId::InterferenceRejection,
        report.interference_rejection as f64,
    );
    receiver.common.set_value(
        &ControlId::BearingAlignment,
        i16::from_le_bytes(report.bearing_offset) as f64,
    );
    receiver
        .common
        .set_value(&ControlId::MainBangSuppression, report.mbs_enabled as f64);

    receiver.common.set_sector(
        &ControlId::NoTransmitSector1,
        u16::from_le_bytes(report.blank_start_1) as f64,
        u16::from_le_bytes(report.blank_end_1) as f64,
        Some(report.blank_enabled_1 > 0),
    );
    receiver.common.set_sector(
        &ControlId::NoTransmitSector2,
        u16::from_le_bytes(report.blank_start_2) as f64,
        u16::from_le_bytes(report.blank_end_2) as f64,
        Some(report.blank_enabled_2 > 0),
    );
}

pub(super) fn process_info_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_some() {
        return;
    }

    if data.len() < 17 {
        log::warn!(
            "{}: Invalid data length for quantum info report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }
    let serial_nr = &data[10..17];
    let serial_nr = String::from_utf8_lossy(serial_nr)
        .trim_end_matches('\0')
        .to_string();

    let model_serial = &data[4..10];
    let model_serial = String::from_utf8_lossy(model_serial)
        .trim_end_matches('\0')
        .to_string();

    match RaymarineModel::try_into(&model_serial) {
        Some(model) => {
            log::debug!(
                "{}: Detected model: {} with serial {}",
                receiver.common.key,
                model.name,
                serial_nr
            );
            receiver.common.info.serial_no = Some(serial_nr);
            let info2 = receiver.common.info.clone();
            settings::update_when_model_known(&mut receiver.common.info.controls, &model, &info2);
            receiver
                .common
                .info
                .set_pixel_values(hd_to_pixel_values(model.hd));
            receiver.common.info.set_doppler(model.doppler);
            receiver.wire_to_legend = wire_to_legend(&receiver.common.info.get_legend());
            receiver.common.update();

            // The command_sender was primed at construction with the
            // BaseModel known from discovery so the heartbeat loop could
            // start immediately (issue #228). The model-specific BaseModel
            // (Quantum vs RD) doesn't change between then and now, so we
            // keep the existing sender rather than recreating it.
            if receiver.command_sender.is_none() && !receiver.common.replay {
                log::debug!("{}: Starting command sender", receiver.common.key);
                receiver.command_sender =
                    Some(Command::new(receiver.common.info.clone(), model.model));
            }
            receiver.model = Some(model);
            receiver.state = ReceiverState::InfoRequestReceived;
        }
        None => {
            log::error!(
                "{}: Unknown model serial: {}",
                receiver.common.key,
                model_serial
            );
        }
    }
}

pub(super) fn process_doppler_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    // The doppler status byte is at offset 4, so we need at least 5 bytes.
    if data.len() < 5 {
        log::warn!(
            "{}: Invalid data length for quantum doppler report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }

    let doppler = match data[4] {
        0x00 => 0,
        0x03 => 1,
        _ => {
            log::warn!("{}: Unknown doppler status {:?}", receiver.common.key, data);
            0
        }
    };

    log::trace!("{}: Doppler {} -> {doppler}", receiver.common.key, data[4]);
    receiver
        .common
        .set_value(&ControlId::Doppler, doppler as f64);
}

#[cfg(test)]
mod tests {
    use super::status_to_power;
    use crate::radar::Power;

    #[test]
    fn known_power_states() {
        assert_eq!(status_to_power(0x00, "k"), Power::Standby);
        assert_eq!(status_to_power(0x01, "k"), Power::Transmit);
        assert_eq!(status_to_power(0x02, "k"), Power::Preparing);
        assert_eq!(status_to_power(0x03, "k"), Power::Off);
    }

    #[test]
    fn fault_and_unknown_fall_back_to_standby() {
        // 0x0a is the self-test fault; other out-of-range values are unknown.
        // Both map to Standby (the Power control has no fault state) but are
        // logged distinctly.
        assert_eq!(status_to_power(0x0a, "k"), Power::Standby);
        assert_eq!(status_to_power(0xff, "k"), Power::Standby);
    }
}
