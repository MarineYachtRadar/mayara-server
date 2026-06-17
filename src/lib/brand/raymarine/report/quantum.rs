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
#[repr(C, packed)]
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

    // Defend the slice against truncated/malformed UDP payloads — both
    // `data_len` and the packet length are wire-supplied. A short packet
    // (or a packet with a wire-inflated `data_len`) would otherwise panic
    // the receiver task.
    let payload_remaining = data.len().saturating_sub(next_offset);
    if data_len > payload_remaining {
        log::warn!(
            "{}: Quantum frame data_len {} exceeds remaining payload {}; dropped",
            receiver.common.key,
            data_len,
            payload_remaining
        );
        return;
    }
    let spoke = &data[next_offset..next_offset + data_len];

    let doppler_row = if receiver.doppler {
        LookupDoppler::Doppler
    } else {
        LookupDoppler::Normal
    } as usize;

    receiver.common.add_spoke(
        receiver.range_meters * returns_per_line / returns_per_range,
        azimuth,
        None,
        process_spoke(
            returns_per_line as usize,
            spoke,
            doppler_row,
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
            // RLE marker is a 3-byte tuple `(0x5c, count, pixel)`. Bail
            // out cleanly on a truncated marker instead of panicking —
            // the spoke buffer comes straight from a UDP payload.
            if src_offset + 3 > spoke.len() {
                log::warn!(
                    "truncated Quantum RLE marker at offset {} (spoke len {}); spoke dropped",
                    src_offset,
                    spoke.len()
                );
                break;
            }
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
#[repr(C, packed)]
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
#[repr(C, packed)]
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
// 0x04 = "transmitting but no display consuming the image" — observed on
// healthy Quantum 2 captures when the operator is on a non-radar MFD page.
// The antenna parks itself after a ~30 s grace, but mayara has no display
// concept, so we treat it as a normal Transmit state.
// See research/raymarine/radar-error-reporting.md (Pelagia capture analysis).
const STATUS_TRANSMIT_DISPLAY_INACTIVE: u8 = 0x04;
const STATUS_FAULT_SELF_TEST: u8 = 0x0a;

/// Map a Quantum status-report byte to a `Power` state. The self-test fault
/// (observed on a faulty unit asked to transmit) maps to `Power::Fault` so
/// the GUI and Signal K consumers can distinguish it from a routine standby;
/// other out-of-range values fall back to Standby with a warning.
/// `key` is only used for the log line on unknown values.
fn status_to_power(status: u8, key: &str) -> Power {
    match status {
        STATUS_STANDBY => Power::Standby,
        STATUS_TRANSMIT | STATUS_TRANSMIT_DISPLAY_INACTIVE => Power::Transmit,
        STATUS_PREPARING => Power::Preparing,
        STATUS_OFF => Power::Off,
        STATUS_FAULT_SELF_TEST => Power::Fault,
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
    let in_fault = status == Power::Fault;
    if in_fault != receiver.self_test_fault {
        if in_fault {
            log::warn!(
                "{}: radar reported FAULT status {:#x} (self-test failure); no image will appear until it clears",
                receiver.common.key,
                STATUS_FAULT_SELF_TEST
            );
        } else {
            log::info!("{}: self-test fault cleared", receiver.common.key);
        }
        receiver.common.info.controls.send_radar_error_notification(
            STATUS_FAULT_SELF_TEST as u32,
            Some("Self-test failure"),
            in_fault,
        );
        receiver.self_test_fault = in_fault;
    }
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
    receiver.doppler = doppler != 0;
    receiver
        .common
        .set_value(&ControlId::Doppler, doppler as f64);
}

// ---------------------------------------------------------------------------
// SelfTestResults — wire id `0a 08 28 00` (LE u32 `0x0028080a`).
//
// The radar pushes this unsolicited on the report multicast channel; no
// request is needed. Payload is `SELF_TEST_ITEM_COUNT` bytes after the
// 4-byte id. Item layout was confirmed by cross-referencing the Pelagia
// Quantum 2 capture against the Axiom MFD's Radar diagnostics → Self-test
// results screen (recording IMG_1045.MOV):
//
// * Bytes  0..12 — 13 enum items shown by the MFD (Pass / Fail / Not Tested
//   / Did not run). Wire encoding: `0x01` = Pass; `0xff` = Did not run (or
//   still pending — flips to `0x01` as warm-up completes); any other value
//   is treated conservatively as a failure (the exact Fail wire byte is
//   unconfirmed without a faulting-unit capture).
// * Bytes 13..15 — 3 internal items the Axiom MFD does not display. Byte 13
//   was observed transitioning `0xff`→`0x01` during warm-up on the healthy
//   Pelagia unit, so these are enum-style results, just hidden from the
//   user. Logged but never alarmed.
// * Bytes 16..23 — 8 boolean items shown by the MFD. Wire encoding: `0x00`
//   = False; non-zero = True. Item index 22 ("Main enable initial state")
//   is steady False on the healthy Pelagia unit, so it's treated as
//   informational (no alarm) — the other 7 should be True and raise an
//   alarm if False.
//
// See research/raymarine/radar-error-reporting.md for the full table.
// ---------------------------------------------------------------------------

pub(crate) const SELF_TEST_RESULTS_ID: u32 = 0x0028080a;
pub(crate) const SELF_TEST_ITEM_COUNT: usize = 24;

const SELF_TEST_PASS: u8 = 0x01;
const SELF_TEST_PENDING: u8 = 0xff;

/// High-byte tag for per-item self-test SK notifications. The full code is
/// `SELF_TEST_ERROR_CODE_BASE | item_index`, which yields a distinct path
/// per item (`notifications.radar.<id>.error.0xa00`..`0xa17`) and stays
/// out of the way of the Layer-1 self-test fault code (`0x0a`) raised from
/// the status byte.
const SELF_TEST_ERROR_CODE_BASE: u32 = 0x0a00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelfTestKind {
    /// Pass / Fail / Did not run enum. Alarm if value is not Pass or
    /// Did-not-run/Pending.
    Enum,
    /// Boolean (True/False) that should be True on a healthy radar; alarm
    /// if False.
    Bool,
    /// Boolean that is expected to vary even on a healthy radar
    /// (informational, never alarmed).
    BoolInfo,
    /// Wire byte present in the message but not surfaced by the Axiom MFD.
    /// Logged on change but never alarmed.
    Reserved,
}

const SELF_TEST_LAYOUT: [(SelfTestKind, &str); SELF_TEST_ITEM_COUNT] = [
    // Bytes 0..12 — 13 enum items shown by the Axiom MFD, in display order.
    (SelfTestKind::Enum, "Power-up self Test Gpio"),
    (SelfTestKind::Enum, "Power-up self Test AD9963"),
    (SelfTestKind::Enum, "Power-up self Test ADF4159"),
    (SelfTestKind::Enum, "Power-up self Test DspBlock"),
    (SelfTestKind::Enum, "Power-up self Test Fpga"),
    (SelfTestKind::Enum, "Power-up self Test I2C"),
    (SelfTestKind::Enum, "Power-up self Test Uart"),
    (SelfTestKind::Enum, "Power-up self Test WiFi"),
    (SelfTestKind::Enum, "Runtime self Test WiFi Spi"),
    (SelfTestKind::Enum, "Runtime self Test AD9963 Data Lines"),
    (SelfTestKind::Enum, "Runtime self Test Psu Comms"),
    (SelfTestKind::Enum, "Runtime self Test Limiter Detect"),
    (SelfTestKind::Enum, "Runtime self Test Power Detect"),
    // Bytes 13..15 — internal results not shown by the MFD.
    (SelfTestKind::Reserved, "(internal self-test 13)"),
    (SelfTestKind::Reserved, "(internal self-test 14)"),
    (SelfTestKind::Reserved, "(internal self-test 15)"),
    // Bytes 16..23 — 8 boolean items shown by the Axiom MFD, in display order.
    (SelfTestKind::Bool, "Power in good"),
    (SelfTestKind::Bool, "Standby current good"),
    (SelfTestKind::Bool, "Main board connected"),
    (SelfTestKind::Bool, "Main EEPROM connection good"),
    (SelfTestKind::Bool, "Main supply on voltages good"),
    (SelfTestKind::Bool, "Main supply on current good"),
    // Steady False on the healthy Pelagia unit — treat as informational.
    (SelfTestKind::BoolInfo, "Main enable initial state"),
    (SelfTestKind::Bool, "POST run"),
];

fn item_is_failing(kind: SelfTestKind, value: u8) -> bool {
    match kind {
        SelfTestKind::Enum => !matches!(value, SELF_TEST_PASS | SELF_TEST_PENDING),
        SelfTestKind::Bool => value == 0,
        SelfTestKind::BoolInfo | SelfTestKind::Reserved => false,
    }
}

fn format_value(kind: SelfTestKind, value: u8) -> String {
    match (kind, value) {
        (SelfTestKind::Enum, SELF_TEST_PASS) => "Pass".to_string(),
        (SelfTestKind::Enum, SELF_TEST_PENDING) => "Did not run".to_string(),
        (SelfTestKind::Enum, v) => format!("Fail(0x{v:02x})"),
        (SelfTestKind::Bool | SelfTestKind::BoolInfo, 0) => "False".to_string(),
        (SelfTestKind::Bool | SelfTestKind::BoolInfo, _) => "True".to_string(),
        (SelfTestKind::Reserved, v) => format!("0x{v:02x}"),
    }
}

pub(super) fn process_self_test_results(receiver: &mut RaymarineReportReceiver, data: &[u8]) {
    let body = match data.get(4..4 + SELF_TEST_ITEM_COUNT) {
        Some(b) => b,
        None => {
            log::warn!(
                "{}: SelfTestResults too short: len={}",
                receiver.common.key,
                data.len()
            );
            return;
        }
    };
    let mut items = [0u8; SELF_TEST_ITEM_COUNT];
    items.copy_from_slice(body);

    let prev = receiver.self_test_results;
    let key = receiver.common.key.clone();
    for (i, &(kind, label)) in SELF_TEST_LAYOUT.iter().enumerate() {
        let curr = items[i];
        let prev_val = prev.map(|p| p[i]);

        if prev_val != Some(curr) {
            match prev_val {
                Some(p) => log::info!(
                    "{}: SelfTest [{}] {}: {} (was {})",
                    key,
                    i,
                    label,
                    format_value(kind, curr),
                    format_value(kind, p)
                ),
                None => log::info!(
                    "{}: SelfTest [{}] {}: {}",
                    key,
                    i,
                    label,
                    format_value(kind, curr)
                ),
            }
        }

        let was_failing = prev_val.is_some_and(|v| item_is_failing(kind, v));
        let now_failing = item_is_failing(kind, curr);
        if was_failing != now_failing {
            receiver.common.info.controls.send_radar_error_notification(
                SELF_TEST_ERROR_CODE_BASE | (i as u32),
                Some(label),
                now_failing,
            );
        }
    }

    receiver.self_test_results = Some(items);
}

#[cfg(test)]
mod tests {
    use super::{
        SELF_TEST_ITEM_COUNT, SELF_TEST_LAYOUT, SelfTestKind, item_is_failing, process_spoke,
        status_to_power,
    };
    use crate::radar::{BYTE_LOOKUP_LENGTH, Power};

    #[test]
    fn self_test_layout_has_one_label_per_wire_byte() {
        assert_eq!(SELF_TEST_LAYOUT.len(), SELF_TEST_ITEM_COUNT);
    }

    #[test]
    fn self_test_failure_classifier_silent_on_pelagia_capture() {
        // Pelagia Quantum 2, "running" snapshot, 24 wire bytes:
        //   01 01 01 01 01 01 01 01 01 ff 01 ff ff 01 01 ff 01 01 01 01 01 01 00 01
        // None of the items should fire an alarm on this healthy radar.
        let pelagia_running: [u8; SELF_TEST_ITEM_COUNT] = [
            0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0xff, 0x01, 0xff, 0xff, 0x01,
            0x01, 0xff, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x01,
        ];
        for (i, &(kind, _)) in SELF_TEST_LAYOUT.iter().enumerate() {
            assert!(
                !item_is_failing(kind, pelagia_running[i]),
                "item {} ({:?} = 0x{:02x}) should not flag as failing on a healthy radar",
                i,
                kind,
                pelagia_running[i]
            );
        }
    }

    #[test]
    fn self_test_failure_classifier_flags_fail_and_false_states() {
        // Enum: any byte outside {Pass=0x01, Pending=0xff} is conservatively
        // treated as a failure (the exact wire byte for Fail is unconfirmed).
        assert!(!item_is_failing(SelfTestKind::Enum, 0x01));
        assert!(!item_is_failing(SelfTestKind::Enum, 0xff));
        assert!(item_is_failing(SelfTestKind::Enum, 0x00));
        assert!(item_is_failing(SelfTestKind::Enum, 0x02));

        // Bool: 0 = False = alarm; any non-zero = True = silent.
        assert!(item_is_failing(SelfTestKind::Bool, 0x00));
        assert!(!item_is_failing(SelfTestKind::Bool, 0x01));
        assert!(!item_is_failing(SelfTestKind::Bool, 0xff));

        // BoolInfo and Reserved never alarm.
        assert!(!item_is_failing(SelfTestKind::BoolInfo, 0x00));
        assert!(!item_is_failing(SelfTestKind::Reserved, 0x00));
    }

    use crate::brand::raymarine::report::{LOOKUP_DOPPLER_LENGTH, WireToLegendTable};

    #[test]
    fn known_power_states() {
        assert_eq!(status_to_power(0x00, "k"), Power::Standby);
        assert_eq!(status_to_power(0x01, "k"), Power::Transmit);
        assert_eq!(status_to_power(0x02, "k"), Power::Preparing);
        assert_eq!(status_to_power(0x03, "k"), Power::Off);
    }

    #[test]
    fn transmit_display_inactive_maps_to_transmit() {
        // 0x04 is a wire-confirmed "transmit-but-no-display-consuming" state
        // (Pelagia Q2 healthy capture, 361 occurrences). Mayara has no display
        // concept, so it folds onto the normal Transmit state.
        assert_eq!(status_to_power(0x04, "k"), Power::Transmit);
    }

    #[test]
    fn self_test_fault_maps_to_fault_state() {
        // 0x0a is the documented self-test fault; surface it on the Power
        // control so consumers can distinguish it from a routine standby.
        assert_eq!(status_to_power(0x0a, "k"), Power::Fault);
    }

    #[test]
    fn unknown_status_falls_back_to_standby() {
        // Anything outside the documented set is treated as standby (with a
        // warning) rather than mis-claimed as a fault.
        assert_eq!(status_to_power(0xff, "k"), Power::Standby);
    }

    /// Identity legend so the unpacked bytes equal the wire bytes — keeps the
    /// process_spoke assertions readable.
    fn identity_lookup() -> WireToLegendTable {
        let mut lookup: WireToLegendTable = [[0u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
        for row in lookup.iter_mut() {
            for (j, slot) in row.iter_mut().enumerate() {
                *slot = j as u8;
            }
        }
        lookup
    }

    #[test]
    fn process_spoke_well_formed_rle_expands_correctly() {
        // `0x5c 0x05 0x42` is "fill 5 with pixel 0x42" — followed by one
        // plain pixel `0x11` for good measure.
        let lookup = identity_lookup();
        let spoke = [0x5c, 0x05, 0x42, 0x11];

        let out = process_spoke(
            /*returns_per_line=*/ 32, &spoke, /*doppler=*/ 1, &lookup,
        );

        assert_eq!(out, vec![0x42, 0x42, 0x42, 0x42, 0x42, 0x11]);
    }

    #[test]
    fn process_spoke_truncated_rle_does_not_panic() {
        // RLE marker `0x5c` followed by only the count byte — the value
        // byte is missing. Before the fix this panicked at `spoke[src+2]`.
        let lookup = identity_lookup();
        let spoke = [0x55, 0x5c, 0x03];

        // Should return what it already decoded (the leading `0x55`) and
        // log a warning about the truncated marker.
        let out = process_spoke(
            /*returns_per_line=*/ 32, &spoke, /*doppler=*/ 1, &lookup,
        );
        assert_eq!(out, vec![0x55]);
    }

    #[test]
    fn process_spoke_marker_at_very_end_does_not_panic() {
        // The marker itself is the last byte — `src_offset + 1` would
        // already be out of bounds before reading the count.
        let lookup = identity_lookup();
        let spoke = [0x12, 0x34, 0x5c];

        let out = process_spoke(
            /*returns_per_line=*/ 32, &spoke, /*doppler=*/ 1, &lookup,
        );
        assert_eq!(out, vec![0x12, 0x34]);
    }

    #[test]
    fn process_spoke_normal_row_skips_doppler_substitution() {
        // Build a table where Normal is identity but Doppler remaps
        // 0xFE/0xFF to sentinel marker values. With the Normal row
        // selected, 0xFE/0xFF must come out unchanged — exercising the
        // row-selection branch that was previously hard-coded to
        // Doppler regardless of the radar's actual mode.
        let mut lookup: WireToLegendTable = [[0u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
        for row in lookup.iter_mut() {
            for (j, slot) in row.iter_mut().enumerate() {
                *slot = j as u8;
            }
        }
        lookup[1][0xFE] = 0xAA;
        lookup[1][0xFF] = 0xBB;

        let spoke = [0xFEu8, 0xFF, 0x42];

        let normal = process_spoke(
            /*returns_per_line=*/ 32, &spoke, /*doppler=*/ 0, &lookup,
        );
        assert_eq!(normal, vec![0xFE, 0xFF, 0x42]);

        let doppler = process_spoke(
            /*returns_per_line=*/ 32, &spoke, /*doppler=*/ 1, &lookup,
        );
        assert_eq!(doppler, vec![0xAA, 0xBB, 0x42]);
    }
}
