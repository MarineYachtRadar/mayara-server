use std::time::{SystemTime, UNIX_EPOCH};

use mayara_core::protocol::raymarine::{
    decompress_quantum_spoke, parse_quantum_frame_header, parse_quantum_status,
    QUANTUM_FRAME_HEADER_SIZE,
};

use crate::brand::raymarine::report::LookupDoppler;
use crate::brand::raymarine::{hd_to_pixel_values, settings, RaymarineModel};
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::range::{Range, Ranges};
use crate::radar::spoke::to_protobuf_spoke;
use crate::radar::{SpokeBearing, Status};
use mayara_core::controllers::{RaymarineController, RaymarineVariant};

use super::{RaymarineReportReceiver, ReceiverState};

pub(crate) fn process_frame(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.state != ReceiverState::StatusRequestReceived {
        log::trace!("{}: Skip scan: not all reports seen", receiver.key);
        return;
    }

    if data.len() < QUANTUM_FRAME_HEADER_SIZE {
        log::warn!(
            "UDP data frame with even less than header, len {} dropped",
            data.len()
        );
        return;
    }

    // Use core parsing for frame header
    let header = match parse_quantum_frame_header(data) {
        Ok(h) => h,
        Err(e) => {
            log::error!(
                "{}: Failed to parse Quantum frame header: {}",
                receiver.key,
                e
            );
            return;
        }
    };
    log::trace!("{}: FrameHeader {:?}", receiver.key, header);

    let nspokes = header.num_spokes;
    let returns_per_range = header.returns_per_range as u32;
    let returns_per_line = header.scan_len as u32;
    // Rotate image 180 degrees to get our "0 = up" view
    let azimuth = (header.azimuth + receiver.info.spokes_per_revolution / 2)
        % receiver.info.spokes_per_revolution as SpokeBearing;

    if nspokes != receiver.info.spokes_per_revolution {
        log::warn!(
            "{}: Invalid spokes per revolution {}",
            receiver.key,
            nspokes
        );
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();
    let mut message = RadarMessage::new();

    let next_offset = QUANTUM_FRAME_HEADER_SIZE;
    let data_len = header.data_len as usize;
    let spoke_data = &data[next_offset..next_offset + data_len];

    // Build doppler lookup table from pixel_to_blob for the core function
    let doppler = LookupDoppler::Doppler as usize;
    let doppler_lookup: [u8; 256] = core::array::from_fn(|i| receiver.pixel_to_blob[doppler][i]);

    // Use core decompression
    let unpacked = decompress_quantum_spoke(spoke_data, &doppler_lookup, returns_per_line as usize);

    let mut spoke = to_protobuf_spoke(
        &receiver.info,
        receiver.range_meters * returns_per_line / returns_per_range,
        azimuth,
        None,
        now,
        unpacked,
    );
    for p in &spoke.data {
        receiver.pixel_stats[*p as usize] += 1;
    }
    receiver
        .trails
        .update_trails(&mut spoke, &receiver.info.legend);
    message.spokes.push(spoke);

    receiver.info.broadcast_radar_message(message);

    if azimuth < receiver.prev_azimuth {
        log::info!("Pixel stats: {:?}", receiver.pixel_stats);
        receiver.pixel_stats = [0; 256];

        let ms = receiver.info.full_rotation();
        receiver.trails.set_rotation_speed(ms);
        receiver.statistics.full_rotation(&receiver.key);
    }
    receiver.prev_azimuth = azimuth;
}

pub(super) fn process_status_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_none() {
        return;
    }

    // Use core parsing
    let report = match parse_quantum_status(data) {
        Ok(r) => r,
        Err(e) => {
            log::error!("{}: Failed to parse Quantum status: {}", receiver.key, e);
            return;
        }
    };
    log::debug!("{}: Quantum report {:?}", receiver.key, report);

    // Update controls based on the report
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
        receiver.set_ranges(Ranges::new(ranges.all));
        receiver.radars.update(&receiver.info);
        log::info!(
            "{}: Ranges initialized: {}",
            receiver.key,
            receiver.info.ranges
        );
    }
    let range_meters = receiver
        .info
        .ranges
        .get_distance(report.range_index as usize);
    receiver.set_value("range", range_meters as f32);
    receiver.range_meters = range_meters as u32;
    receiver.state = ReceiverState::StatusRequestReceived;

    let mode = report.mode as usize;
    if mode <= 3 {
        receiver.set_value("mode", mode as f32);
        receiver.set_value_auto(
            "gain",
            report.controls[mode].gain as f32,
            report.controls[mode].gain_auto as u8,
        );
        receiver.set_value_auto(
            "colorGain",
            report.controls[mode].color_gain as f32,
            report.controls[mode].color_gain_auto as u8,
        );
        receiver.set_value_auto(
            "sea",
            report.controls[mode].sea as f32,
            report.controls[mode].sea_auto as u8,
        );
        receiver.set_value_enabled(
            "rain",
            report.controls[mode].rain as f32,
            report.controls[mode].rain_enabled as u8,
        );
    } else {
        log::warn!("{}: Unknown mode {}", receiver.key, report.mode);
    }
    receiver.set_value("targetExpansion", report.target_expansion as f32);
    receiver.set_value(
        "interferenceRejection",
        report.interference_rejection as f32,
    );
    receiver.set_value("bearingAlignment", report.bearing_offset as f32);
    receiver.set_value("mainBangSuppression", report.mbs_enabled as u8 as f32);
}

pub(super) fn process_info_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_some() {
        return;
    }

    if data.len() < 17 {
        log::warn!(
            "{}: Invalid data length for quantum info report: {}",
            receiver.key,
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
            log::info!(
                "{}: Detected model: {} with serial {}",
                receiver.key,
                model.name,
                serial_nr
            );
            receiver.info.serial_no = Some(serial_nr);
            let info2 = receiver.info.clone();
            settings::update_when_model_known(&mut receiver.info.controls, &model, &info2);
            receiver.info.set_pixel_values(hd_to_pixel_values(model.hd));
            receiver.info.set_doppler(model.doppler);
            receiver.radars.update(&receiver.info);

            // Create the unified controller if not in replay mode
            if !receiver.replay {
                log::debug!("{}: Starting unified controller (Quantum)", receiver.key);
                let controller = RaymarineController::new(
                    &receiver.key,
                    &receiver.info.send_command_addr.ip().to_string(),
                    receiver.info.send_command_addr.port(),
                    &receiver.info.report_addr.ip().to_string(),
                    receiver.info.report_addr.port(),
                    RaymarineVariant::Quantum,
                    model.doppler,
                );
                receiver.controller = Some(controller);
            } else {
                log::debug!("{}: No controller, replay mode", receiver.key);
            };
            receiver.base_model = Some(model.model.clone());
            receiver.model = Some(model);
            receiver.state = ReceiverState::InfoRequestReceived;
        }
        None => {
            log::error!("{}: Unknown model serial: {}", receiver.key, model_serial);
        }
    }
}
