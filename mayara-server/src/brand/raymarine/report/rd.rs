use serde::Deserialize;
use std::mem::size_of;
use std::time::{SystemTime, UNIX_EPOCH};

use mayara_core::protocol::raymarine::{
    decompress_rd_spoke, parse_rd_frame_header, parse_rd_status, RD_FRAME_HEADER_SIZE,
};

use crate::brand::raymarine::{hd_to_pixel_values, settings, RaymarineModel};
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::range::{Range, Ranges};
use crate::radar::spoke::to_protobuf_spoke;
use crate::radar::Status;
use mayara_core::controllers::{RaymarineController, RaymarineVariant};

use super::{RaymarineReportReceiver, ReceiverState};

// Spoke headers still parsed locally since they're per-spoke, not frame-level
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct SpokeHeader2 {
    field01: u32,
    _length: u32,
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

const SPOKE_HEADER_2_LENGTH: usize = size_of::<SpokeHeader2>();
const SPOKE_HEADER_1_LENGTH: usize = size_of::<SpokeHeader1>();
const SPOKE_DATA_LENGTH: usize = size_of::<SpokeHeader3>();

pub(crate) fn process_frame(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    let mut mark_full_rotation = false;

    if receiver.state != ReceiverState::StatusRequestReceived {
        log::trace!("{}: Skip scan: not all reports seen", receiver.key);
        return;
    }

    if data.len() < RD_FRAME_HEADER_SIZE + SPOKE_HEADER_1_LENGTH {
        log::warn!(
            "UDP data frame with even less than one spoke, len {} dropped",
            data.len()
        );
        return;
    }
    log::trace!("{}: Scandata {:02X?}", receiver.key, data);

    // Use core parsing for frame header
    let frame = match parse_rd_frame_header(data) {
        Ok(f) => f,
        Err(e) => {
            log::error!("{}: Failed to parse RD frame header: {}", receiver.key, e);
            return;
        }
    };
    log::trace!("{}: frame {:?}", receiver.key, frame);

    if frame.is_hd {
        log::warn!("{}: different radar type found (HD)", receiver.key);
        return;
    }

    if frame.nspokes == 0 || frame.nspokes > 360 {
        log::warn!("{}: Invalid spoke count {}", receiver.key, frame.nspokes);
        return;
    }

    let nspokes = frame.nspokes;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();
    let mut message = RadarMessage::new();

    let mut scanline = 0;
    let mut next_offset = RD_FRAME_HEADER_SIZE;

    while next_offset < data.len() - SPOKE_HEADER_1_LENGTH {
        let spoke_header_1 = &data[next_offset..next_offset + SPOKE_HEADER_1_LENGTH];
        log::trace!("{}: header3 {:?}", receiver.key, spoke_header_1);

        let spoke_header_1: SpokeHeader1 = match bincode::deserialize(spoke_header_1) {
            Ok(h) => h,
            Err(e) => {
                log::error!("{}: Failed to deserialize header3: {}", receiver.key, e);
                return;
            }
        };
        log::trace!("{}: header3 {:?}", receiver.key, spoke_header_1);

        if spoke_header_1.field01 != 0x00000001 || spoke_header_1.length != 0x00000028 {
            log::warn!("{}: header3 unknown {:02X?}", receiver.key, spoke_header_1);
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
                    receiver.key,
                    spoke_header_1
                );
                break;
            }
        };

        next_offset += SPOKE_HEADER_1_LENGTH;

        // Now check if the optional "Header2" marker is present
        let header2 = &data[next_offset..next_offset + SPOKE_HEADER_2_LENGTH];
        log::trace!("{}: header2 {:?}", receiver.key, header2);

        let header2: SpokeHeader2 = match bincode::deserialize(header2) {
            Ok(h) => h,
            Err(e) => {
                log::error!("{}: Failed to deserialize scan header: {}", receiver.key, e);
                return;
            }
        };
        log::trace!("{}: header2 {:?}", receiver.key, header2);

        if header2.field01 == 0x00000002 {
            next_offset += SPOKE_HEADER_2_LENGTH;
        }

        // Followed by the actual spoke data
        let header3 = &data[next_offset..next_offset + SPOKE_DATA_LENGTH];
        log::trace!("{}: SpokeData {:?}", receiver.key, header3);
        let header3: SpokeHeader3 = match bincode::deserialize(header3) {
            Ok(h) => h,
            Err(e) => {
                log::error!("{}: Failed to deserialize header: {}", receiver.key, e);
                return;
            }
        };
        log::trace!("{}: SpokeData {:?}", receiver.key, header3);
        if (header3.field01 & 0x7fffffff) != 0x00000003 || header3.length < header3.data_len + 8 {
            log::warn!(
                "{}: spoke_data header check failed {:02X?}",
                receiver.key,
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
        log::trace!("{}: Spoke {:?}", receiver.key, spoke);

        let angle = (spoke_header_1.azimuth as u16 + receiver.info.spokes_per_revolution / 2)
            % receiver.info.spokes_per_revolution;

        // Use core decompression function
        let unpacked = decompress_rd_spoke(spoke, hd_type, returns_per_line);
        log::trace!("process_spoke unpacked={}", unpacked.len());

        let mut spoke = to_protobuf_spoke(
            &receiver.info,
            receiver.range_meters * 4,
            angle,
            None,
            now,
            unpacked,
        );
        receiver
            .trails
            .update_trails(&mut spoke, &receiver.info.legend);
        message.spokes.push(spoke);

        next_offset += header3.length as usize - SPOKE_DATA_LENGTH;

        if angle < receiver.prev_azimuth {
            mark_full_rotation = true;
        }
        let spokes_per_revolution = receiver.info.spokes_per_revolution as u16;
        if receiver.prev_azimuth < spokes_per_revolution
            && ((receiver.prev_azimuth + 1) % spokes_per_revolution) != angle
        {
            receiver.statistics.missing_spokes +=
                (angle + spokes_per_revolution - receiver.prev_azimuth - 1) as usize
                    % spokes_per_revolution as usize;
            log::trace!(
                "{}: Spoke angle {} is not consecutive to previous angle {}, new missing spokes {}",
                receiver.key,
                angle,
                receiver.prev_azimuth,
                receiver.statistics.missing_spokes
            );
        }
        receiver.statistics.received_spokes += 1;
        receiver.prev_azimuth = angle;

        scanline += 1;
    }
    if scanline != nspokes {
        log::debug!(
            "{}: Scanline count mismatch, header {} vs actual {}",
            receiver.key,
            nspokes,
            scanline
        );
        receiver.statistics.broken_packets += 1;
    }

    if mark_full_rotation {
        let ms = receiver.info.full_rotation();
        receiver.trails.set_rotation_speed(ms);
        receiver.statistics.full_rotation(&receiver.key);
    }

    receiver.info.broadcast_radar_message(message);
}

pub(super) fn process_status_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state < ReceiverState::FixedRequestReceived {
        log::trace!("{}: Skip status: not all reports seen", receiver.key);
        return;
    }

    // Use core parsing
    let report = match parse_rd_status(data) {
        Ok(r) => r,
        Err(e) => {
            log::error!("{}: Failed to parse RD status: {}", receiver.key, e);
            return;
        }
    };
    log::info!("{}: status report {:?}", receiver.key, report);

    if receiver.state == ReceiverState::FixedRequestReceived {
        receiver.state = ReceiverState::StatusRequestReceived;
    }

    let status = match report.status {
        0x00 => Status::Standby,
        0x01 => Status::Transmit,
        0x02 => Status::Preparing,
        0x03 => Status::Off,
        _ => {
            log::warn!("{}: Unknown status {}", receiver.key, report.status);
            Status::Standby
        }
    };
    receiver.set_value("power", status as i32 as f32);

    if receiver.info.ranges.is_empty() {
        let mut ranges = Ranges::empty();

        for (i, &range) in report.ranges.iter().enumerate() {
            let meters = (range as f64 * 1.852f64) as i32; // Convert to nautical miles
            ranges.push(Range::new(meters, i));
        }
        // When we set ranges, the UI starts showing this radar, so this should be the
        // last thing we do -- eg. only do this once model and min/max info is known
        receiver.set_ranges(Ranges::new(ranges.all));
        receiver.radars.update(&receiver.info);
        log::info!(
            "{}: Ranges initialized: {}",
            receiver.key,
            receiver.info.ranges
        );
    }
    let range_index = if report.is_hd {
        data[296]
    } else {
        report.range_id
    } as usize;
    let range_meters = receiver.info.ranges.get_distance(range_index);
    receiver.range_meters = range_meters as u32;
    log::info!("{}: range_meters={}", receiver.key, range_meters);

    receiver.set_value("range", range_meters as f32);
    receiver.set_value_auto("gain", report.gain as f32, report.auto_gain as u8);

    receiver.set_value_auto("sea", report.sea, report.auto_sea);
    receiver.set_value_enabled("rain", report.rain, report.rain_enabled as u8);
    receiver.set_value_enabled("ftc", report.ftc, report.ftc_enabled as u8);
    receiver.set_value_auto("tune", report.tune, report.auto_tune as u8);
    receiver.set_value("targetExpansion", report.target_expansion);
    receiver.set_value("interferenceRejection", report.interference_rejection);
    receiver.set_value("bearingAlignment", report.bearing_offset);
    receiver.set_value("mainBangSuppression", report.mbs_enabled as u8);
    receiver.set_value_enabled("warmupTime", report.warmup_time, report.warmup_time);
    receiver.set_value("signalStrength", report.signal_strength);
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
        log::trace!("{}: Skip fixed report: no info report seen", receiver.key);
        return;
    }

    if data.len() < FIXED_REPORT_PREFIX + FIXED_REPORT_LENGTH {
        log::warn!(
            "{}: Invalid data length for fixed report: {}",
            receiver.key,
            data.len()
        );
        return;
    }
    log::trace!(
        "{}: ignoring fixed report prefix {:02X?}",
        receiver.key,
        &data[0..FIXED_REPORT_PREFIX]
    );
    let report = &data[FIXED_REPORT_PREFIX..FIXED_REPORT_PREFIX + FIXED_REPORT_LENGTH];
    log::trace!("{}: fixed report {:02X?}", receiver.key, report);
    let report: FixedReport = match bincode::deserialize(report) {
        Ok(h) => h,
        Err(e) => {
            log::error!("{}: Failed to deserialize header: {}", receiver.key, e);
            return;
        }
    };
    log::debug!("{}: fixed report {:02X?}", receiver.key, report);

    if receiver.state == ReceiverState::InfoRequestReceived {
        receiver.state = ReceiverState::FixedRequestReceived;
    }

    if receiver.model.is_some() {
        receiver.set_value("operatingHours", report.magnetron_time);
        receiver.set_value("magnetronCurrent", report.magnetron_current);
        receiver.set_value("signalStrength", report.signal_strength_value);
        receiver.set_value("displayTiming", report.display_timing);

        receiver.set_wire_range("gain", report.gain_min, report.gain_max);
        receiver.set_wire_range("sea", report.sea_min, report.sea_max);
        receiver.set_wire_range("rain", report.rain_min, report.rain_max);
        receiver.set_wire_range("ftc", report.ftc_min, report.ftc_max);
    }
}

pub(super) fn process_info_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_some() {
        return;
    }

    if data.len() < 27 {
        log::warn!(
            "{}: Invalid data length for RD info report: {}",
            receiver.key,
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
                log::error!("{}: Unknown model serial: {}", receiver.key, model_serial);
                log::error!("{}: report {:02X?}", receiver.key, data);
                return;
            }
        }
    };
    log::info!(
        "{}: Detected model {} with serialnr {}",
        receiver.key,
        model.name,
        serial_nr
    );
    receiver.set_string("serialNumber", serial_nr.clone());
    receiver.info.serial_no = Some(serial_nr);
    receiver.info.spokes_per_revolution = model.max_spoke_len as u16;
    receiver.info.max_spoke_len = model.max_spoke_len as u16;
    let info2 = receiver.info.clone();
    settings::update_when_model_known(&mut receiver.info.controls, &model, &info2);
    receiver.info.set_pixel_values(hd_to_pixel_values(model.hd));

    receiver.info.set_doppler(model.doppler);
    receiver.radars.update(&receiver.info);

    // Create the unified controller if not in replay mode
    if !receiver.replay {
        log::debug!("{}: Starting unified controller (RD)", receiver.key);
        let controller = RaymarineController::new(
            &receiver.key,
            &receiver.info.send_command_addr.ip().to_string(),
            receiver.info.send_command_addr.port(),
            &receiver.info.report_addr.ip().to_string(),
            receiver.info.report_addr.port(),
            RaymarineVariant::RD,
            model.doppler,
        );
        receiver.controller = Some(controller);
    } else {
        log::debug!("{}: No controller, replay mode", receiver.key);
    }
    receiver.base_model = Some(model.model.clone());
    receiver.model = Some(model);
    receiver.state = ReceiverState::InfoRequestReceived;
}
