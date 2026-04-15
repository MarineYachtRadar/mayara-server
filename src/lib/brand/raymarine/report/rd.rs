use serde::Deserialize;
use std::mem::size_of;

use crate::brand::raymarine::report::wire_to_legend;
use crate::brand::raymarine::{RaymarineModel, hd_to_pixel_values, settings};
use crate::radar::Power;
use crate::radar::range::{Range, Ranges};
use crate::radar::settings::ControlId;
use crate::radar::spoke::GenericSpoke;

use super::{RaymarineReportReceiver, ReceiverState};

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]

struct FrameHeader {
    field01: u32, // 0x00010003
    _zero_1: u32,
    fieldx_1: u32,     // 0x0000001c
    nspokes: u32,      // 0x00000008 - usually but changes
    _spoke_count: u32, // 0x00000000 in regular, counting in HD
    _zero_3: u32,
    fieldx_3: u32, // 0x00000001
    fieldx_4: u32, // 0x00000000 or 0xffffffff in regular, 0x400 in HD
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct SpokeHeader2 {
    field01: u32,
    _length: u32, // ..
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct SpokeHeader1 {
    field01: u32, // 0x00000001
    length: u32,  // 0x00000028
    azimuth: u32,
    fieldx_2: u32, // 0x00000001 - 0x03 - HD
    fieldx_3: u32, // 0x00000002
    fieldx_4: u32, // 0x00000001 - 0x03 - HD
    fieldx_5: u32, // 0x00000001 - 0x00 - HD
    fieldx_6: u32, // 0x000001f4 - 0x00 - HD
    _zero_1: u32,
    fieldx_7: u32, // 0x00000001
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct SpokeHeader3 {
    field01: u32, // 0x00000003
    length: u32,
    data_len: u32,
}

const FRAME_HEADER_LENGTH: usize = size_of::<FrameHeader>();
const SPOKE_HEADER_2_LENGTH: usize = size_of::<SpokeHeader2>();
const SPOKE_HEADER_1_LENGTH: usize = size_of::<SpokeHeader1>();
const SPOKE_DATA_LENGTH: usize = size_of::<SpokeHeader3>();

pub(crate) fn process_frame(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state != ReceiverState::StatusRequestReceived {
        log::trace!("{}: Skip scan: not all reports seen", receiver.common.key);
        return;
    }

    if data.len() < FRAME_HEADER_LENGTH + SPOKE_HEADER_1_LENGTH {
        log::warn!(
            "UDP data frame with even less than one spoke, len {} dropped",
            data.len()
        );
        return;
    }
    log::trace!("{}: Scandata {:02X?}", receiver.common.key, data);

    let header = &data[..FRAME_HEADER_LENGTH];
    log::trace!("{}: header1 {:?}", receiver.common.key, header);
    let header: FrameHeader = match bincode::deserialize(header) {
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
    log::trace!("{}: header1 {:?}", receiver.common.key, header);
    let nspokes = header.nspokes;

    if header.field01 != 0x00010003
        || header.fieldx_1 != 0x0000001c
        || header.fieldx_3 != 0x00000001
    {
        log::warn!(
            "{}: Packet header1 mismatch {:02X?}",
            receiver.common.key,
            header
        );
        return;
    }

    if header.fieldx_4 == 0x400 {
        log::warn!("{}: different radar type found", receiver.common.key);
        return;
    }

    if nspokes == 0 || nspokes > 360 {
        log::warn!("{}: Invalid spoke count {}", receiver.common.key, nspokes);
        return;
    }

    receiver.common.new_spoke_message();

    let mut scanline = 0;
    let mut next_offset = FRAME_HEADER_LENGTH;

    while next_offset < data.len() - SPOKE_HEADER_1_LENGTH {
        let spoke_header_1 = &data[next_offset..next_offset + SPOKE_HEADER_1_LENGTH];
        log::trace!("{}: header3 {:?}", receiver.common.key, spoke_header_1);

        let spoke_header_1: SpokeHeader1 = match bincode::deserialize(spoke_header_1) {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "{}: Failed to deserialize header3: {}",
                    receiver.common.key,
                    e
                );
                return;
            }
        };
        log::trace!("{}: header3 {:?}", receiver.common.key, spoke_header_1);

        if spoke_header_1.field01 != 0x00000001 || spoke_header_1.length != 0x00000028 {
            log::warn!(
                "{}: header3 unknown {:02X?}",
                receiver.common.key,
                spoke_header_1
            );
            break;
        }

        let (hd_type, returns_per_line) = match (
            spoke_header_1.fieldx_2,
            spoke_header_1.fieldx_3,
            spoke_header_1.fieldx_4,
            spoke_header_1.fieldx_5,
            spoke_header_1.fieldx_6,
            spoke_header_1.fieldx_7,
        ) {
            (1, 2, 1, 1, 0x01f4, 1) => (false, 512),
            (3, 2, 3, 1, 0, 1) => (true, 1024),
            _ => {
                log::debug!(
                    "{}: process_frame header unknown {:02X?}",
                    receiver.common.key,
                    spoke_header_1
                );
                break;
            }
        };

        next_offset += SPOKE_HEADER_1_LENGTH;

        // Now check if the optional "Header2" marker is present
        let header2 = &data[next_offset..next_offset + SPOKE_HEADER_2_LENGTH];
        log::trace!("{}: header2 {:?}", receiver.common.key, header2);

        let header2: SpokeHeader2 = match bincode::deserialize(header2) {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "{}: Failed to deserialize scan header: {}",
                    receiver.common.key,
                    e
                );
                return;
            }
        };
        log::trace!("{}: header2 {:?}", receiver.common.key, header2);

        if header2.field01 == 0x00000002 {
            next_offset += SPOKE_HEADER_2_LENGTH;
        }

        // Followed by the actual spoke data
        let header3 = &data[next_offset..next_offset + SPOKE_DATA_LENGTH];
        log::trace!("{}: SpokeData {:?}", receiver.common.key, header3);
        let header3: SpokeHeader3 = match bincode::deserialize(header3) {
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
        log::trace!("{}: SpokeData {:?}", receiver.common.key, header3);
        if (header3.field01 & 0x7fffffff) != 0x00000003 || header3.length < header3.data_len + 8 {
            log::warn!(
                "{}: spoke_data header check failed {:02X?}",
                receiver.common.key,
                header3
            );
            break;
        }
        next_offset = next_offset + SPOKE_DATA_LENGTH;

        let mut data_len = header3.data_len as usize;
        if next_offset + data_len > data.len() {
            data_len = data.len() - next_offset;
        }
        let spoke = &data[next_offset..next_offset + data_len];
        log::trace!("{}: Spoke {:?}", receiver.common.key, spoke);

        let angle = (spoke_header_1.azimuth as u16
            + receiver.common.info.spokes_per_revolution / 2)
            % receiver.common.info.spokes_per_revolution;

        receiver.common.add_spoke(
            receiver.range_meters * 4,
            angle,
            None,
            process_spoke(hd_type, returns_per_line, spoke, data_len),
        );

        next_offset += header3.length as usize - SPOKE_DATA_LENGTH;

        scanline += 1;
    }
    if scanline != nspokes {
        log::debug!(
            "{}: Scanline count mismatch, header {} vs actual {}",
            receiver.common.key,
            nspokes,
            scanline
        );
    }

    receiver.common.send_spoke_message();
}

fn process_spoke(
    hd_type: bool,
    returns_per_line: usize,
    spoke: &[u8],
    data_len: usize,
) -> GenericSpoke {
    let mut unpacked_data: Vec<u8> = Vec::with_capacity(10240);
    let mut src_offset: usize = 0;
    while src_offset < data_len {
        if hd_type {
            if spoke[src_offset] != 0x5c {
                unpacked_data.push(spoke[src_offset] >> 1);
                src_offset += 1;
            } else {
                let count = spoke[src_offset + 1] as usize; // number to be filled
                let value = spoke[src_offset + 2]; // data to be filled
                for _ in 0..count {
                    unpacked_data.push(value >> 1);
                }
                src_offset += 3;
            }
        } else {
            // not HDtype, extract nibbles and blow up values by 8 so they match HD legend
            let value = spoke[src_offset];
            if value != 0x5c {
                unpacked_data.push((value & 0x0f) << 3);
                unpacked_data.push((value & 0xf0) >> 1);
                src_offset += 1;
            } else {
                let count = spoke[src_offset + 1] as usize; // number to be filled
                let value = spoke[src_offset + 2]; // data to be filled
                for _ in 0..count {
                    unpacked_data.push((value & 0x0f) << 3);
                    unpacked_data.push((value & 0xf0) >> 1);
                }
                src_offset += 3;
            }
        }
    }
    log::trace!("process_spoke unpacked={}", unpacked_data.len());
    unpacked_data.truncate(returns_per_line);

    unpacked_data
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct StatusReport {
    field01: u32,          // 0x010001  // 0-3
    ranges: [u32; 11],     // 4 - 47
    _fieldx_1a: [u32; 10], // 48 - 97
    _fieldx_1b: [u32; 10], // 98 - 137
    _fieldx_1c: [u32; 13], // 138 - 169

    status: u8, // 2 - warmup, 1 - transmit, 0 - standby, 6 - shutting down (warmup time - countdown), 3 - shutdown  // 180
    _fieldx_2: [u8; 3], // 181
    warmup_time: u8, // 184
    signal_strength: u8, // number of bars   // 185

    _fieldx_3: [u8; 7], // 186
    range_id: u8,       // 193
    _fieldx_4: [u8; 2], // 194
    auto_gain: u8,      // 196
    _fieldx_5: [u8; 3], // 197
    gain: u32,          // 200
    auto_sea: u8,       // 0 - disabled; 1 - harbour, 2 - offshore, 3 - coastal   // 204
    _fieldx_6: [u8; 3], // 205
    sea: u8,            // 208
    rain_enabled: u8,   // 209
    _fieldx_7: [u8; 3], // 210
    rain: u8,           // 213
    ftc_enabled: u8,    // 214
    _fieldx_8: [u8; 3], // 215
    ftc: u8,            // 218
    auto_tune: u8,
    _fieldx_9: [u8; 3],
    tune: u8,
    bearing_offset: i16, // degrees * 10; left - negative, right - positive
    interference_rejection: u8,
    _fieldx_10: [u8; 3],
    target_expansion: u8,
    _fieldx_11: [u8; 13],
    mbs_enabled: u8, // Main Bang Suppression enabled if 1
}

const STATUS_REPORT_LENGTH: usize = size_of::<StatusReport>();

pub(super) fn process_status_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state < ReceiverState::FixedRequestReceived {
        log::trace!("{}: Skip status: not all reports seen", receiver.common.key);
        return;
    }

    if data.len() < STATUS_REPORT_LENGTH {
        log::warn!(
            "{}: Invalid data length for quantum info report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }
    let report = &data[..STATUS_REPORT_LENGTH];
    log::info!("{}: status report {:02X?}", receiver.common.key, report);
    let report: StatusReport = match bincode::deserialize(report) {
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
    log::info!("{}: status report {:02X?}", receiver.common.key, report);

    if report.field01 != 0x010001 && report.field01 != 0x018801 {
        log::error!(
            "{}: Packet header1 mismatch {:02X?}",
            receiver.common.key,
            report
        );
        return;
    }

    if receiver.state == ReceiverState::FixedRequestReceived {
        receiver.state = ReceiverState::StatusRequestReceived;
    }

    let hd = report.field01 == 0x00018801;

    let status = match report.status {
        0x00 => Power::Standby,
        0x01 => Power::Transmit,
        0x02 => Power::Preparing,
        0x03 => Power::Off,
        _ => {
            log::warn!("{}: Unknown status {}", receiver.common.key, report.status);
            Power::Standby // Default to Standby if unknown
        }
    };
    receiver
        .common
        .set_value(&ControlId::Power, status as i32 as f64);

    if receiver.common.info.ranges.is_empty() {
        let mut ranges = Ranges::empty();
        let report_ranges = report.ranges; // copy for alignment

        for i in 0..report_ranges.len() {
            let meters = (report_ranges[i] as f64 * 1.852f64) as i32; // Convert to nautical miles

            ranges.push(Range::new(meters, i));
        }
        // When we set ranges, the UI starts showing this radar, so this should be the
        // last thing we do -- eg. only do this once model and min/max info is known
        receiver.set_ranges(Ranges::new(ranges.all));
        log::info!(
            "{}: Ranges initialized: {}",
            receiver.common.key,
            receiver.common.info.ranges
        );
    }
    let range_index = if hd { data[296] } else { report.range_id } as usize;
    let range_meters = receiver.common.info.ranges.get_distance(range_index);
    receiver.range_meters = range_meters as u32;
    log::debug!("{}: range_meters={}", receiver.common.key, range_meters);

    receiver
        .common
        .set_value(&ControlId::Range, range_meters as f64);
    receiver
        .common
        .set_value_auto(&ControlId::Gain, report.gain as f64, report.auto_gain);

    receiver
        .common
        .set_value_auto(&ControlId::Sea, report.sea, report.auto_sea);
    receiver
        .common
        .set_value_enabled(&ControlId::Rain, report.rain, report.rain_enabled);
    receiver
        .common
        .set_value_enabled(&ControlId::Ftc, report.ftc, report.ftc_enabled);
    receiver
        .common
        .set_value_auto(&ControlId::Tune, report.tune, report.auto_tune);
    receiver
        .common
        .set_value(&ControlId::TargetExpansion, report.target_expansion);
    receiver.common.set_value(
        &ControlId::InterferenceRejection,
        report.interference_rejection,
    );
    receiver
        .common
        .set_value(&ControlId::BearingAlignment, report.bearing_offset);
    receiver
        .common
        .set_value(&ControlId::MainBangSuppression, report.mbs_enabled);
    receiver.common.set_value_enabled(
        &ControlId::WarmupTime,
        report.warmup_time,
        report.warmup_time,
    );
    receiver
        .common
        .set_value(&ControlId::SignalStrength, report.signal_strength);
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct FixedReport {
    magnetron_time: u16,
    _fieldx_2: [u8; 6],
    magnetron_current: u8,
    _fieldx_3: [u8; 11],
    _rotation_time: u16, // We ignore rotation time in the report, we count our own rotation time

    _fieldx_4: [u8; 13],
    _fieldx_41: u8,
    _fieldx_5: [u8; 2],
    _fieldx_42: [u8; 3],
    _fieldx_43: [u8; 3], // 3 bytes (fine-tune values for SP, MP, LP)
    _fieldx_6: [u8; 6],
    display_timing: u8,
    _fieldx_7: [u8; 12],
    _fieldx_71: u8,
    _fieldx_8: [u8; 12],
    gain_min: u8,
    gain_max: u8,
    sea_min: u8,
    sea_max: u8,
    rain_min: u8,
    rain_max: u8,
    ftc_min: u8,
    ftc_max: u8,
    _fieldx_81: u8,
    _fieldx_82: u8,
    _fieldx_83: u8,
    _fieldx_84: u8,
    signal_strength_value: u8,
    _fieldx_9: [u8; 2],
}

const FIXED_REPORT_LENGTH: usize = size_of::<FixedReport>();
const FIXED_REPORT_PREFIX: usize = 217;

pub(super) fn process_fixed_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state < ReceiverState::InfoRequestReceived {
        log::trace!(
            "{}: Skip fixed report: no info report seen",
            receiver.common.key
        );
        return;
    }

    if data.len() < FIXED_REPORT_PREFIX + FIXED_REPORT_LENGTH {
        log::warn!(
            "{}: Invalid data length for fixed report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }
    log::trace!(
        "{}: ignoring fixed report prefix {:02X?}",
        receiver.common.key,
        &data[0..FIXED_REPORT_PREFIX]
    );
    let report = &data[FIXED_REPORT_PREFIX..FIXED_REPORT_PREFIX + FIXED_REPORT_LENGTH];
    log::trace!("{}: fixed report {:02X?}", receiver.common.key, report);
    let report: FixedReport = match bincode::deserialize(report) {
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
    log::debug!("{}: fixed report {:02X?}", receiver.common.key, report);

    if receiver.state == ReceiverState::InfoRequestReceived {
        receiver.state = ReceiverState::FixedRequestReceived;
    }

    if receiver.model.is_some() {
        receiver
            .common
            .set_value(&ControlId::TransmitTime, report.magnetron_time);
        receiver
            .common
            .set_value(&ControlId::MagnetronCurrent, report.magnetron_current);
        receiver
            .common
            .set_value(&ControlId::SignalStrength, report.signal_strength_value);
        receiver
            .common
            .set_value(&ControlId::DisplayTiming, report.display_timing);

        receiver
            .common
            .set_wire_range(&ControlId::Gain, report.gain_min, report.gain_max);
        receiver
            .common
            .set_wire_range(&ControlId::Sea, report.sea_min, report.sea_max);
        receiver
            .common
            .set_wire_range(&ControlId::Rain, report.rain_min, report.rain_max);
        receiver
            .common
            .set_wire_range(&ControlId::Ftc, report.ftc_min, report.ftc_max);
    }
}

pub(super) fn process_info_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_some() {
        return;
    }

    if data.len() < 27 {
        log::warn!(
            "{}: Invalid data length for RD info report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }
    let serial_nr = &data[4..11];
    let serial_nr = String::from_utf8_lossy(serial_nr)
        .trim_end_matches('\0')
        .to_string();

    let model_serial = &data[20..27];
    let model_serial = String::from_utf8_lossy(model_serial)
        .trim_end_matches('\0')
        .to_string();

    let model = match RaymarineModel::try_into(&model_serial) {
        Some(model) => model,
        None => {
            if model_serial.parse::<u64>().is_ok() {
                let model = RaymarineModel::new_eseries();
                model
            } else {
                log::error!(
                    "{}: Unknown model serial: {}",
                    receiver.common.key,
                    model_serial
                );
                log::error!("{}: report {:02X?}", receiver.common.key, data);
                return;
            }
        }
    };
    log::info!(
        "{}: Detected model {} with serialnr {}",
        receiver.common.key,
        model.name,
        serial_nr
    );
    receiver
        .common
        .set_string(&ControlId::SerialNumber, serial_nr.clone());
    receiver.common.info.serial_no = Some(serial_nr);
    receiver.common.info.spokes_per_revolution = model.max_spoke_len as u16;
    receiver.common.info.max_spoke_len = model.max_spoke_len as u16;
    let info2 = receiver.common.info.clone();
    settings::update_when_model_known(&mut receiver.common.info.controls, &model, &info2);
    receiver
        .common
        .info
        .set_pixel_values(hd_to_pixel_values(model.hd));

    receiver.common.info.set_doppler(model.doppler);
    receiver.wire_to_legend = wire_to_legend(&receiver.common.info.get_legend());
    receiver.common.update();
    receiver.model = Some(model);
    receiver.state = ReceiverState::InfoRequestReceived;
}
