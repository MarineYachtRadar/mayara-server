//! Report stream of the emulated xHD radar (`239.254.2.0:50100`).
//!
//! A real xHD broadcasts its entire state as a stream of single-value reports,
//! and the plotter mirrors what it hears there: a setting the radar never
//! reports back is a setting the plotter shows as unavailable. The same set is
//! sent once a second, and again immediately after the plotter changes
//! something so its own commands are confirmed without a wait.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::garmin::capabilities::{GarminCapabilities, cap};
use crate::brand::garmin::protocol::*;
use crate::radar::settings::{ControlId, SharedControls};
use crate::radar::{Power, RadarError};

use super::convert::XHD_RANGES_M;
use super::{
    GAIN_MODE_AUTO, GAIN_MODE_MANUAL, SEA_MODE_AUTO, SEA_MODE_MANUAL, SEA_MODE_OFF, Shared,
    multicast_send, packet, packet_u8, packet_u16, packet_u32,
};

/// Interval between full state broadcasts.
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// What the emulated radar is, as opposed to what the source radar can do.
///
/// The MFD reduces these bits to a radar class, and draws anything below
/// class 2 with the legacy two-colour ramp however many bits per sample the
/// spokes carry. `COLOR_PALETTE` then picks which of the two remaining
/// palettes it uses. A GMR xHD sets all of them, so the bridge does too:
/// they describe the protocol being spoken, not a control anyone can reach.
/// See `research/garmin/feature-detection.md`.
const CLASS_CAPABILITIES: &[u32] = &[
    cap::RANGE_MODE,
    cap::QUICK_ADJUST_GAIN,
    cap::QUICK_ADJUST_SEA,
    cap::CLASS_6,
    cap::CLASS_7,
    cap::ENHANCED_PROTOCOL,
    cap::HIGH_CLASS,
    cap::COLOR_PALETTE,
];

// Reports a real xHD sends whose meaning is not documented anywhere, and for
// which mayara has no equivalent state. They are replayed with the values seen
// in the capture: leaving them out makes the plotter wait for a radar that
// never finishes starting up.
const MSG_SCAN_TYPE_B: u32 = 0x0912;
const MSG_SCAN_TYPE_C: u32 = 0x0913;
const MSG_ANTENNA_HEIGHT: u32 = 0x0928;
const MSG_ANTENNA_FORWARD: u32 = 0x0929;
const MSG_ANTENNA_STARBOARD: u32 = 0x092a;
const MSG_ANTENNA_POWER: u32 = 0x092b;
const MSG_TUNE_FINE: u32 = 0x0951;
const MSG_TUNE_COARSE: u32 = 0x0952;
const MSG_TUNE_MODE: u32 = 0x0953;
const MSG_TRIGGER_PERIOD: u32 = 0x0994;
const MSG_TRIGGER_DELAY: u32 = 0x0995;
const MSG_TRIGGER_PERIOD_B: u32 = 0x0996;
const MSG_STATUS_A: u32 = 0x099c;
const MSG_STATUS_B: u32 = 0x099d;
const MSG_STATUS_C: u32 = 0x099e;

/// Antenna position, in centimetres. An xHD reports where it is mounted; the
/// Signal K API models that as vessel configuration rather than radar state,
/// so plausible values are sent instead of nothing.
const ANTENNA_HEIGHT_CM: u16 = 350;
const ANTENNA_FORWARD_CM: u16 = 150;
const ANTENNA_STARBOARD_CM: u16 = 0;

// Values of the reports above, as the captured GMR xHD sends them. mayara has
// no equivalent state to derive any of these from, and they describe the
// transmitter rather than anything the source radar could contribute.
const ANTENNA_POWER: u16 = 0x2134;
const TRIGGER_PERIOD: u32 = 0x0000_09b4;
const TRIGGER_DELAY: u32 = 0x0000_1710;
const TRIGGER_PERIOD_B: u32 = 0x0000_16f8;
const TUNE_FINE: u8 = 2;
const TUNE_COARSE: u8 = 0;
const TUNE_MODE: u8 = 0;
const STATUS_A: u8 = 1;
const STATUS_B: u8 = 1;
const STATUS_C: u8 = 0;

/// Scan type: one range, as opposed to the dual-range modes.
const SCAN_TYPE_SINGLE: u8 = 1;
const SCAN_TYPE_B: u8 = 0;
const SCAN_TYPE_C: u8 = 0;

/// Range mode: single range.
const RANGE_MODE_SINGLE: u8 = 0;

/// Automatic frequency control, which the emulated radar leaves on and offers
/// no way to change.
const AFC_MODE_AUTO: u8 = 1;

/// Interference rejection and crosstalk suppression happen on the source
/// radar, if at all, so the emulated one reports both switched off.
const DITHER_OFF: u8 = 0;
const NOISE_BLANKER_OFF: u8 = 0;

/// Spokes per revolution as an xHD counts them for this report, which is not
/// the 1440 it sends (`0x0960` in the capture; see
/// `research/garmin/discovery-handshake.md`).
const SPOKE_TOTAL: u32 = 2400;

/// Supply voltage in decivolts: 12.0 V.
const INPUT_VOLTAGE_DV: u16 = 120;

/// Everything the bridge does not emulate reports as off or centred.
const OFF: u8 = 0;
const NO_BEARING_OFFSET: u32 = 0;
const NO_TIME: u16 = 0;

/// Milliseconds until the scanner state changes again. The emulated radar
/// never warms up or spins down on a timer, so its state is always settled.
const NO_STATE_CHANGE_PENDING: u32 = 0;

/// The range B fields still have to carry something in single-range mode;
/// 1/2 NM is what the captured radar reports.
const RANGE_B_M: u32 = 926;

pub(super) async fn run(
    local_addr: Ipv4Addr,
    shared: Arc<Shared>,
    mut echo_rx: mpsc::Receiver<Vec<u8>>,
    subsys: &mut SubsystemHandle,
) -> Result<(), RadarError> {
    let socket = match multicast_send(&REPORT_ADDRESS, local_addr) {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("Garmin xHD status: cannot open socket: {e}");
            return Ok(());
        }
    };

    let send = async |report: &[u8]| {
        if let Err(e) = socket.send(report).await {
            log::warn!("Garmin xHD status: send failed: {e}");
        }
    };

    loop {
        for report in reports(&shared) {
            send(&report).await;
        }

        let next_report = tokio::time::Instant::now() + REPORT_INTERVAL;
        loop {
            tokio::select! {
                biased;
                _ = subsys.on_shutdown_requested() => return Ok(()),
                // A command acknowledgement has to go out on this socket, and
                // it has to go out now: the plotter shows nothing until the
                // radar confirms what it was told.
                Some(echo) = echo_rx.recv() => send(&echo).await,
                _ = tokio::time::sleep_until(next_report) => break,
            }
        }
    }
}

/// The full state of the emulated radar, one value per packet.
fn reports(shared: &Shared) -> Vec<Vec<u8>> {
    let power = shared
        .control(ControlId::Power)
        .and_then(|v| Power::from_value(&serde_json::json!(v as i64)).ok())
        .unwrap_or(Power::Standby);
    let transmitting = power == Power::Transmit;
    let scanner_state = match power {
        Power::Transmit => STATE_TRANSMIT,
        Power::Preparing => STATE_WARMING_UP,
        // A radar that is off or faulted cannot be told apart from one in
        // standby over this protocol; the picture stopping says the rest.
        _ => STATE_STANDBY,
    } as u8;

    let gain_auto = shared.control_auto(ControlId::Gain);
    let gain = percent_to_wire(shared.control(ControlId::Gain));

    let sea_auto = shared.control_auto(ControlId::Sea);
    let sea = percent_to_wire(shared.control(ControlId::Sea));
    let sea_mode = if sea_auto {
        SEA_MODE_AUTO
    } else if sea > 0 {
        SEA_MODE_MANUAL
    } else {
        SEA_MODE_OFF
    };

    // Rain filtering is on or off on an xHD, with no automatic mode; a radar
    // set to filter nothing is one with rain filtering switched off.
    let rain = percent_to_wire(shared.control(ControlId::Rain));
    let rain_mode = u8::from(rain > 0);

    vec![
        packet_u8(MSG_SCANNER_STATE, scanner_state),
        packet_u32(MSG_STATE_CHANGE, NO_STATE_CHANGE_PENDING),
        packet_u8(MSG_TRANSMIT_MODE, u8::from(transmitting)),
        packet_u8(MSG_TRANSMIT_MODE_CURRENT, u8::from(transmitting)),
        packet_u8(MSG_SCAN_TYPE, SCAN_TYPE_SINGLE),
        packet_u8(MSG_SCAN_TYPE_B, SCAN_TYPE_B),
        packet_u8(MSG_SCAN_TYPE_C, SCAN_TYPE_C),
        packet_u8(MSG_RANGE_MODE, RANGE_MODE_SINGLE),
        packet_u32(MSG_RANGE_A, shared.range_m()),
        packet_u32(MSG_RANGE_B, RANGE_B_M),
        packet_u8(
            MSG_RANGE_A_GAIN_MODE,
            if gain_auto {
                GAIN_MODE_AUTO
            } else {
                GAIN_MODE_MANUAL
            },
        ),
        packet_u16(MSG_RANGE_A_GAIN, gain),
        packet_u8(MSG_RANGE_A_SEA_MODE, sea_mode),
        packet_u16(MSG_RANGE_A_SEA_GAIN, sea),
        packet_u8(MSG_RANGE_A_SEA_STATE, OFF),
        packet_u8(MSG_RANGE_A_RAIN_MODE, rain_mode),
        packet_u16(MSG_RANGE_A_RAIN_GAIN, rain),
        packet_u8(MSG_DITHER_MODE, DITHER_OFF),
        packet_u8(MSG_NOISE_BLANKER, NOISE_BLANKER_OFF),
        // The source radar applies its own bearing alignment before it hands
        // over a spoke, so the emulated radar is aligned by definition.
        packet_u32(MSG_BEARING_ALIGNMENT, NO_BEARING_OFFSET),
        packet_u8(MSG_NO_TX_ZONE_1_MODE, OFF),
        packet_u8(MSG_SENTRY_MODE, OFF),
        packet_u16(MSG_SENTRY_STANDBY_TIME, NO_TIME),
        packet_u16(MSG_SENTRY_TRANSMIT_TIME, NO_TIME),
        packet_u8(MSG_AFC_MODE, AFC_MODE_AUTO),
        packet_u8(MSG_RPM_MODE, OFF),
        packet_u16(MSG_ANTENNA_HEIGHT, ANTENNA_HEIGHT_CM),
        packet_u16(MSG_ANTENNA_FORWARD, ANTENNA_FORWARD_CM),
        packet_u16(MSG_ANTENNA_STARBOARD, ANTENNA_STARBOARD_CM),
        packet_u16(MSG_ANTENNA_POWER, ANTENNA_POWER),
        packet_u32(MSG_TRIGGER_PERIOD, TRIGGER_PERIOD),
        packet_u32(MSG_TRIGGER_DELAY, TRIGGER_DELAY),
        packet_u32(MSG_TRIGGER_PERIOD_B, TRIGGER_PERIOD_B),
        packet_u8(MSG_TUNE_FINE, TUNE_FINE),
        packet_u8(MSG_TUNE_COARSE, TUNE_COARSE),
        packet_u8(MSG_TUNE_MODE, TUNE_MODE),
        packet_u8(MSG_STATUS_A, STATUS_A),
        packet_u8(MSG_STATUS_B, STATUS_B),
        packet_u8(MSG_STATUS_C, STATUS_C),
        packet_u32(MSG_MAX_RANGE, *XHD_RANGES_M.last().unwrap()),
        packet_u32(MSG_SPOKE_TOTAL, SPOKE_TOTAL),
        packet_u16(MSG_INPUT_VOLTAGE, INPUT_VOLTAGE_DV),
        capability_report(&shared.controls),
        range_table(),
    ]
}

/// A control value in percent as the enhanced protocol carries it.
fn percent_to_wire(percent: Option<f64>) -> u16 {
    (percent.unwrap_or(0.0).clamp(0.0, 100.0) * GAIN_SCALE as f64) as u16
}

/// The `0x09B1` capability bitmap of the emulated radar, as five 64-bit words.
fn capability_report(controls: &SharedControls) -> Vec<u8> {
    packet(MSG_CAPABILITY, &emulated_capabilities(controls).to_body())
}

/// What the emulated radar claims it can do: the class bits, plus one bit per
/// control the bridge really translates onto the source radar.
///
/// A display decides from this which controls to offer, so a bit claimed for a
/// control the source radar does not have is a knob on the plotter that does
/// nothing. The other way round is just as wrong: the MFD tests a control bit
/// together with the group bit of the block it belongs to, so a gain the
/// bridge forwards but never claims `GAIN_GROUP` for is a knob the plotter
/// will not send.
///
/// Notably absent is the Range B block: the bridge presents a single range
/// whatever the source radar can do. The GMR xHD in
/// `radar-recordings/garmin/garmin_xhd.pcap` claims dual range even though it
/// has none — the MFD appears to set that whole word unconditionally — and a
/// display that believes it offers a second range no spoke ever arrives on.
///
/// `QUICK_ADJUST_PANEL` is left clear for a different reason: which control
/// each of its three slots adjusts is not known, and a slot guessed wrong is a
/// slider that moves the wrong setting.
fn emulated_capabilities(controls: &SharedControls) -> GarminCapabilities {
    let mut bits = CLASS_CAPABILITIES.to_vec();

    if controls.contains_key(&ControlId::Power) {
        bits.push(cap::TRANSMIT_MODE);
    }
    if controls.contains_key(&ControlId::Range) {
        bits.push(cap::RANGE_A);
    }
    if let Some(gain) = controls.get(&ControlId::Gain) {
        bits.extend([cap::GAIN_GROUP, cap::RANGE_A_GAIN]);
        // The gain mode command is the one that switches to automatic; a
        // radar that only knows one mode has no use for it.
        if gain.has_auto() {
            bits.push(cap::RANGE_A_GAIN_MODE);
        }
    }
    // Sea clutter mode carries off and manual as well as automatic, so it is
    // claimed whether or not the source radar has an automatic mode.
    if controls.contains_key(&ControlId::Sea) {
        bits.extend([cap::SEA_GROUP, cap::RANGE_A_SEA_MODE, cap::RANGE_A_SEA_GAIN]);
    }
    if controls.contains_key(&ControlId::Rain) {
        bits.extend([
            cap::RAIN_GROUP,
            cap::RANGE_A_RAIN_MODE,
            cap::RANGE_A_RAIN_GAIN,
        ]);
    }

    GarminCapabilities::from_bits(&bits)
}

/// The `0x09B2` range table: a version/length header, an entry count, and the
/// ranges the plotter may pick from.
fn range_table() -> Vec<u8> {
    let count = XHD_RANGES_M.len() as u32;
    let body_len = 8 + count * 4;

    let mut body = Vec::with_capacity(body_len as usize);
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&(body_len as u16).to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    for &meters in XHD_RANGES_M {
        body.extend_from_slice(&meters.to_le_bytes());
    }
    packet(MSG_RANGE_TABLE, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brand::garmin::range_table;
    use crate::output::garmin_xhd::tests::{controls, controls_with};

    /// The payload of one report, by message id.
    fn report_of(reports: &[Vec<u8>], message_id: u32) -> Vec<u8> {
        reports
            .iter()
            .find(|r| u32::from_le_bytes(r[0..4].try_into().unwrap()) == message_id)
            .unwrap_or_else(|| panic!("no report 0x{message_id:04x}"))[GMN_HEADER_LEN..]
            .to_vec()
    }

    #[test]
    fn standby_and_transmit_are_reported_as_the_plotter_expects() {
        let (shared, _echo_rx) = Shared::new(controls());

        shared
            .controls
            .set(&ControlId::Power, Power::Standby as u32 as f64, None)
            .unwrap();
        let reports = super::reports(&shared);
        assert_eq!(
            report_of(&reports, MSG_SCANNER_STATE),
            [STATE_STANDBY as u8]
        );
        assert_eq!(report_of(&reports, MSG_TRANSMIT_MODE), [0]);

        shared
            .controls
            .set(&ControlId::Power, Power::Transmit as u32 as f64, None)
            .unwrap();
        let reports = super::reports(&shared);
        assert_eq!(
            report_of(&reports, MSG_SCANNER_STATE),
            [STATE_TRANSMIT as u8]
        );
        assert_eq!(report_of(&reports, MSG_TRANSMIT_MODE), [1]);
        assert_eq!(report_of(&reports, MSG_TRANSMIT_MODE_CURRENT), [1]);
    }

    #[test]
    fn sea_clutter_off_manual_and_auto_map_onto_the_three_modes() {
        let (shared, _echo_rx) = Shared::new(controls());

        shared
            .controls
            .set(&ControlId::Sea, 0., Some(false))
            .unwrap();
        assert_eq!(
            report_of(&super::reports(&shared), MSG_RANGE_A_SEA_MODE),
            [SEA_MODE_OFF]
        );

        shared
            .controls
            .set(&ControlId::Sea, 30., Some(false))
            .unwrap();
        let reports = super::reports(&shared);
        assert_eq!(report_of(&reports, MSG_RANGE_A_SEA_MODE), [SEA_MODE_MANUAL]);
        assert_eq!(
            report_of(&reports, MSG_RANGE_A_SEA_GAIN),
            3000u16.to_le_bytes()
        );

        shared
            .controls
            .set(&ControlId::Sea, 30., Some(true))
            .unwrap();
        assert_eq!(
            report_of(&super::reports(&shared), MSG_RANGE_A_SEA_MODE),
            [SEA_MODE_AUTO]
        );
    }

    #[test]
    fn rain_filtering_is_off_when_the_radar_filters_nothing() {
        let (shared, _echo_rx) = Shared::new(controls());

        shared.controls.set(&ControlId::Rain, 0., None).unwrap();
        assert_eq!(
            report_of(&super::reports(&shared), MSG_RANGE_A_RAIN_MODE),
            [0]
        );

        shared.controls.set(&ControlId::Rain, 40., None).unwrap();
        let reports = super::reports(&shared);
        assert_eq!(report_of(&reports, MSG_RANGE_A_RAIN_MODE), [1]);
        assert_eq!(
            report_of(&reports, MSG_RANGE_A_RAIN_GAIN),
            4000u16.to_le_bytes()
        );
    }

    #[test]
    fn gain_mode_follows_the_control() {
        let (shared, _echo_rx) = Shared::new(controls());

        shared
            .controls
            .set(&ControlId::Gain, 75., Some(false))
            .unwrap();
        let reports = super::reports(&shared);
        assert_eq!(
            report_of(&reports, MSG_RANGE_A_GAIN_MODE),
            [GAIN_MODE_MANUAL]
        );
        assert_eq!(report_of(&reports, MSG_RANGE_A_GAIN), 7500u16.to_le_bytes());

        shared
            .controls
            .set(&ControlId::Gain, 75., Some(true))
            .unwrap();
        assert_eq!(
            report_of(&super::reports(&shared), MSG_RANGE_A_GAIN_MODE),
            [GAIN_MODE_AUTO]
        );
    }

    /// The capability bitmap a display receives, parsed back out of the
    /// broadcast rather than read off the value that went into it.
    fn claimed(controls: SharedControls) -> GarminCapabilities {
        let (shared, _echo_rx) = Shared::new(controls);
        let broadcast = report_of(&super::reports(&shared), MSG_CAPABILITY);
        GarminCapabilities::parse(&broadcast).expect("valid capabilities")
    }

    #[test]
    fn capabilities_claimed_are_the_ones_the_bridge_honours() {
        let reported = claimed(controls());

        for bit in [
            cap::TRANSMIT_MODE,
            cap::RANGE_A,
            cap::GAIN_GROUP,
            cap::RANGE_A_GAIN,
            cap::RANGE_A_GAIN_MODE,
            cap::SEA_GROUP,
            cap::RANGE_A_SEA_MODE,
            cap::RANGE_A_SEA_GAIN,
            cap::RAIN_GROUP,
            cap::RANGE_A_RAIN_MODE,
            cap::RANGE_A_RAIN_GAIN,
        ] {
            assert!(reported.has(bit), "capability 0x{bit:02x} was lost");
        }

        // A single-range radar, whatever the source radar can do. Claiming
        // otherwise makes a display offer a range no spoke ever arrives on.
        assert!(!reported.has_dual_range());
        for bit in cap::RANGE_B..=cap::RANGE_B_SEA_GAIN {
            assert!(!reported.has(bit), "Range B bit 0x{bit:02x} claimed");
        }

        // Nor anything else the bridge cannot act on.
        for bit in [
            cap::SENTRY_MODE,
            cap::NO_TX_ZONE_1_MODE,
            cap::RPM_MODE,
            cap::FRONT_OF_BOAT,
            cap::PARK_POSITION,
            cap::DOPPLER_RANGE_A,
            cap::QUICK_ADJUST_PANEL,
        ] {
            assert!(!reported.has(bit), "capability 0x{bit:02x} claimed");
        }
    }

    #[test]
    fn controls_the_source_radar_lacks_are_not_claimed() {
        // A radar with a range and a gain, and nothing else to adjust.
        let reported = claimed(controls_with(&[ControlId::Range, ControlId::Gain], &[]));

        assert!(reported.has(cap::RANGE_A));
        assert!(reported.has(cap::GAIN_GROUP));
        assert!(reported.has(cap::RANGE_A_GAIN));

        // A gain that cannot be switched to automatic has no gain mode.
        assert!(!reported.has(cap::RANGE_A_GAIN_MODE));

        // Sea and rain go entirely, group bit and all: a group bit on its own
        // would leave the plotter a menu with nothing behind it.
        for bit in [
            cap::SEA_GROUP,
            cap::RANGE_A_SEA_MODE,
            cap::RANGE_A_SEA_GAIN,
            cap::RAIN_GROUP,
            cap::RANGE_A_RAIN_MODE,
            cap::RANGE_A_RAIN_GAIN,
        ] {
            assert!(!reported.has(bit), "capability 0x{bit:02x} claimed");
        }
    }

    #[test]
    fn the_radar_class_is_claimed_whatever_the_source_radar_offers() {
        // Below class 2 the plotter draws the picture with the legacy
        // two-colour ramp, however many controls the source radar has.
        let reported = claimed(controls_with(&[], &[]));
        for bit in CLASS_CAPABILITIES {
            assert!(reported.has(*bit), "class bit 0x{bit:02x} was lost");
        }
    }

    #[test]
    fn range_table_parses_back_to_the_ranges_offered() {
        let packet = super::range_table();
        let parsed = range_table::parse(&packet[GMN_HEADER_LEN..]).expect("valid range table");
        let distances: Vec<u32> = parsed.all.iter().map(|r| r.distance() as u32).collect();
        assert_eq!(distances, XHD_RANGES_M);
    }

    #[test]
    fn percent_maps_onto_the_wire_scale() {
        assert_eq!(percent_to_wire(None), 0);
        assert_eq!(percent_to_wire(Some(0.0)), 0);
        assert_eq!(percent_to_wire(Some(50.0)), 5000);
        assert_eq!(percent_to_wire(Some(100.0)), 10000);
        assert_eq!(percent_to_wire(Some(150.0)), 10000);
        assert_eq!(percent_to_wire(Some(-10.0)), 0);
    }
}
