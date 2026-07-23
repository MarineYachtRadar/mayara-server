use serde::Deserialize;
use std::mem::size_of;

use crate::brand::raymarine::report::wire_to_legend;
use crate::brand::raymarine::{RaymarineModel, hd_to_pixel_values, settings};
use crate::radar::Power;
use crate::radar::range::{Range, Ranges};
use crate::radar::settings::ControlId;
use crate::radar::spoke::GenericSpoke;
use crate::util::decode_bin;

use super::{RaymarineReportReceiver, ReceiverState};

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
struct FrameHeader {
    field01: u32, // 0x00010003
    _zero_1: u32,
    fieldx_1: u32,     // 0x0000001c
    nspokes: u32,      // 0x00000008 - usually but changes
    _spoke_count: u32, // 0x00000000 in regular, counting in HD
    _zero_3: u32,
    fieldx_3: u32,  // 0x00000001
    _fieldx_4: u32, // 0 on an RD418D; on an RD418HD the first spoke's block length
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
struct SpokeHeader2 {
    field01: u32,
    length: u32, // total block length: 8 where only the two header words follow, 28 on an RD418D
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
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
#[repr(C, packed)]
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

    // No check on _fieldx_4: it is not a radar-type marker. An RD418D sends 0
    // there, an RD418HD the first spoke's block length — which reaches 0x400
    // for an incompressible 1024-sample spoke, so rejecting on it drops valid
    // frames. Unknown frame layouts fail the SpokeHeader1 match below instead.

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

        let spoke_header_1: SpokeHeader1 = match decode_bin(spoke_header_1) {
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

        let header2: SpokeHeader2 = match decode_bin(header2) {
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
            // The type-2 sub-block is length-prefixed: an RD418D sends
            // 28-byte blocks with extra data after the two header words
            // (issue #419); a fixed 8-byte skip misreads everything after.
            let block_len = header2.length as usize;
            if block_len < SPOKE_HEADER_2_LENGTH || next_offset + block_len > data.len() {
                log::warn!(
                    "{}: implausible type-2 spoke block length {}",
                    receiver.common.key,
                    block_len
                );
                break;
            }
            next_offset += block_len;
        }

        // Followed by the actual spoke data
        let header3 = &data[next_offset..next_offset + SPOKE_DATA_LENGTH];
        log::trace!("{}: SpokeData {:?}", receiver.common.key, header3);
        let header3: SpokeHeader3 = match decode_bin(header3) {
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
        next_offset += SPOKE_DATA_LENGTH;

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
#[repr(C, packed)]
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

/// Offset of the power-state byte in the HD (0x018801) status report. The
/// D/analog (0x010001) report carries it at `StatusReport::status` (180),
/// where the HD variant holds a constant 1. Wire-observed on an E92142
/// RD418HD: byte 124 flips 0 -> 1 as the MFD switches it to transmit.
const HD_STATUS_POWER_OFFSET: usize = 124;

/// Offset of the range-index byte in the HD (0x018801) status report.
const HD_STATUS_RANGE_OFFSET: usize = 296;

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
    let report: StatusReport = match decode_bin(report) {
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

    // The HD report reads bytes beyond STATUS_REPORT_LENGTH; a truncated
    // datagram must not panic the receiver.
    if hd && data.len() <= HD_STATUS_RANGE_OFFSET {
        log::warn!(
            "{}: Invalid data length for HD status report: {}",
            receiver.common.key,
            data.len()
        );
        return;
    }

    let power_byte = if hd {
        data[HD_STATUS_POWER_OFFSET]
    } else {
        report.status
    };
    let status = match power_byte {
        0x00 => Power::Standby,
        0x01 => Power::Transmit,
        0x02 => Power::Preparing,
        0x03 => Power::Off,
        _ => {
            log::warn!("{}: Unknown status {}", receiver.common.key, power_byte);
            Power::Standby // Default to Standby if unknown
        }
    };
    receiver
        .common
        .set_value(&ControlId::Power, status as i32 as f64);

    if receiver.common.info.ranges.is_empty() {
        let mut ranges = Ranges::empty();
        let report_ranges = report.ranges; // copy for alignment

        for (i, &raw) in report_ranges.iter().enumerate() {
            let meters = (raw as f64 * 1.852f64) as i32; // Convert to nautical miles

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
    let range_index = if hd {
        data[HD_STATUS_RANGE_OFFSET]
    } else {
        report.range_id
    } as usize;
    let range_meters = receiver.common.info.ranges.get_distance(range_index);
    receiver.range_meters = range_meters as u32;
    log::debug!("{}: range_meters={}", receiver.common.key, range_meters);

    receiver
        .common
        .set_value(&ControlId::Range, range_meters as f64);

    // The remaining StatusReport offsets only hold data in the D/analog
    // (0x010001) layout. In the HD (0x018801) report they land on constant
    // zeros and a bogus bearing offset (wire-observed on an E92142), so
    // leave those controls untouched until their HD offsets are mapped.
    if hd {
        return;
    }

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
#[repr(C, packed)]
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
    let report: FixedReport = match decode_bin(report) {
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

/// The info report's model field holds the E-number, on some radars with the
/// serial number appended without a separator (an RD418D sends
/// "E921300530192"). Try the whole field first, then the 6-character
/// E-number prefix.
fn model_from_serial(model_serial: &str) -> Option<RaymarineModel> {
    RaymarineModel::try_into(model_serial)
        .or_else(|| model_serial.get(..6).and_then(RaymarineModel::try_into))
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

    let model_field = &data[20..];
    let model_field = &model_field[..model_field
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(model_field.len())];
    let model_serial = String::from_utf8_lossy(model_field).to_string();

    let model = match model_from_serial(&model_serial) {
        Some(model) => model,
        None => {
            if model_serial.parse::<u64>().is_ok() {
                RaymarineModel::new_eseries()
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
    apply_model(receiver, model, Some(serial_nr));
    receiver.state = ReceiverState::InfoRequestReceived;
}

/// Store the identified model and serial number and update the controls,
/// legend and pixel geometry accordingly. The caller advances the receiver
/// state, which differs per info-report flavour.
fn apply_model(
    receiver: &mut RaymarineReportReceiver,
    model: RaymarineModel,
    serial_nr: Option<String>,
) {
    log::info!(
        "{}: Detected model {} with serialnr {}",
        receiver.common.key,
        model.name,
        serial_nr.as_deref().unwrap_or("<unknown>")
    );
    if let Some(serial_nr) = serial_nr {
        receiver
            .common
            .set_string(&ControlId::SerialNumber, serial_nr.clone());
        receiver.common.info.serial_no = Some(serial_nr);
    }
    // spokes_per_revolution keeps the locator's RD_SPOKES_PER_REVOLUTION:
    // an RD418D sends azimuths 0..2047 even though its spokes carry only
    // 512 samples (issue #419) — spoke count and sample count are distinct.
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
}

/// Marker introducing an identity block in the HD 0x018701 report.
const HD_INFO_BLOCK_MARKER: [u8; 6] = [0xff, 0x01, 0x50, 0x50, 0x50, 0x41];

/// The identity block is a grid of 16-byte string slots after the marker.
const HD_INFO_SLOT_LEN: usize = 16;
/// Slot offset (relative to the marker) of the 7-digit serial number.
const HD_INFO_SERIAL_SLOT: usize = 16;
/// Slot offset (relative to the marker) of the model + unit number string.
const HD_INFO_MODEL_SLOT: usize = 32;

/// Read a Pascal-style string from a 16-byte identity slot:
/// a length byte followed by ASCII characters, padded with 0xff.
fn hd_info_string(slot: &[u8]) -> Option<String> {
    let len = *slot.first()? as usize;
    if len == 0 || len >= HD_INFO_SLOT_LEN {
        return None;
    }
    let bytes = slot.get(1..1 + len)?;
    if !bytes.iter().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// HD-generation radomes (e.g. an E92142 RD418HD) never send the 0x010006
/// info or 0x010002 fixed reports; their ~1 Hz 0x018701 report carries the
/// unit identity instead. It holds one identity block per subassembly, each
/// starting with `HD_INFO_BLOCK_MARKER` followed by 16-byte string slots:
/// the board serial ("9137606") and the model with the unit number appended
/// ("E921421220193" = E92142 + unit, the same concatenation style as the
/// RD418D's 0x010006 model field). Parse the first block and treat it as
/// info + fixed combined, so the next status report completes the state
/// machine and spokes flow.
pub(super) fn process_hd_info_report(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    if receiver.model.is_some() {
        return;
    }

    let Some(marker) = data
        .windows(HD_INFO_BLOCK_MARKER.len())
        .position(|w| w == HD_INFO_BLOCK_MARKER)
    else {
        log::warn!(
            "{}: HD info report without identity block, len {}",
            receiver.common.key,
            data.len()
        );
        return;
    };

    let slot = |offset: usize| data.get(marker + offset..marker + offset + HD_INFO_SLOT_LEN);
    let serial_nr = slot(HD_INFO_SERIAL_SLOT).and_then(hd_info_string);
    let model_serial = slot(HD_INFO_MODEL_SLOT).and_then(hd_info_string);

    let Some(model_serial) = model_serial else {
        log::error!(
            "{}: HD info report identity block without model string: {:02X?}",
            receiver.common.key,
            &data[marker..data.len().min(marker + 64)]
        );
        return;
    };

    let Some(model) = model_from_serial(&model_serial) else {
        log::error!(
            "{}: Unknown model serial: {}",
            receiver.common.key,
            model_serial
        );
        return;
    };

    apply_model(receiver, model, serial_nr);
    // No fixed report will ever come; this report covers both.
    receiver.state = ReceiverState::FixedRequestReceived;
}

#[cfg(test)]
mod tests {
    use super::{hd_info_string, model_from_serial};

    #[test]
    fn model_from_concatenated_serial() {
        // RD418D wire data (issue #419): E-number with the serial appended.
        assert_eq!(model_from_serial("E921300530192").unwrap().name, "RD418D");
        // RD418HD wire data: model + unit number from the 0x018701 report.
        assert_eq!(model_from_serial("E921421220193").unwrap().name, "RD418HD");
        // NUL-padded fields arrive here already truncated and match exactly.
        assert_eq!(model_from_serial("E92130").unwrap().name, "RD418D");
        assert_eq!(model_from_serial("E70498").unwrap().name, "Quantum Q24D");
        // Numeric E-series fields are no model; the caller falls back.
        assert!(model_from_serial("1234567").is_none());
        assert!(model_from_serial("E9999").is_none());
    }

    #[test]
    fn hd_info_slot_strings() {
        // Real identity slots from an E92142 RD418HD 0x018701 report:
        // Pascal length byte, ASCII characters, 0xff padding.
        const SERIAL_SLOT: [u8; 16] = [
            0x07, 0x39, 0x31, 0x33, 0x37, 0x36, 0x30, 0x36, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff,
        ];
        const MODEL_SLOT: [u8; 16] = [
            0x0d, 0x45, 0x39, 0x32, 0x31, 0x34, 0x32, 0x31, 0x32, 0x32, 0x30, 0x31, 0x39, 0x33,
            0xff, 0xff,
        ];
        assert_eq!(hd_info_string(&SERIAL_SLOT).as_deref(), Some("9137606"));
        assert_eq!(
            hd_info_string(&MODEL_SLOT).as_deref(),
            Some("E921421220193")
        );

        // Length byte out of range or non-printable content is rejected.
        assert_eq!(hd_info_string(&[0x00; 16]), None);
        assert_eq!(hd_info_string(&[0xff; 16]), None);
        let mut bad = SERIAL_SLOT;
        bad[1] = 0xff;
        assert_eq!(hd_info_string(&bad), None);
    }
}
