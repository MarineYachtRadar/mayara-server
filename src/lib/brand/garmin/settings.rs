use std::collections::HashMap;
use std::f64::consts::PI;

use super::GarminRadarType;
use super::capabilities::GarminCapabilities;
use crate::{
    Cli,
    radar::{
        settings::{
            ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list, new_numeric,
            new_sector, new_string,
        },
        units::Units,
    },
    stream::SignalKDelta,
};

/// Build the control set for a Garmin radar.
///
/// Controls are gated by `capabilities`: only the features the radar
/// reports as supported (via `0x09B1` for enhanced-protocol radars or the synthesized legacy
/// bitmap for HD) are exposed. The `radar_type` is still passed so that
/// HD-only and enhanced-only quirks (e.g. the no-transmit sector layout)
/// can pick the right handler.
/// Build the control set for a Garmin radar.
///
/// `is_range_b`: when true, only per-range controls (gain, sea, rain,
/// range, doppler) are registered — antenna-level controls (bearing
/// alignment, scan speed, interference rejection, no-TX zones, sentry,
/// target expansion, telemetry) are omitted because they live on Range A.
pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
    radar_type: GarminRadarType,
    capabilities: &GarminCapabilities,
    is_range_b: bool,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::ModelName).build(&mut controls);
    new_string(ControlId::UserName).build(&mut controls);

    // --- Per-range controls (both A and B) ---

    // Gain: mode/level
    if capabilities.has(super::capabilities::cap::RANGE_A_GAIN) {
        new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    }

    // Rain clutter
    if capabilities.has(super::capabilities::cap::RANGE_A_RAIN_GAIN) {
        new_numeric(ControlId::Rain, 0., 100.).build(&mut controls);
    }

    // Sea clutter
    if capabilities.has(super::capabilities::cap::RANGE_A_SEA_GAIN) {
        new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    }

    // MotionScope / Doppler — Fantom-class radars only.
    if capabilities.has_motionscope() {
        new_list(ControlId::Doppler, &["Off", "Normal", "Approaching"]).build(&mut controls);
    }

    // Pulse expansion (xHD2+): expands pulse width for better small-target visibility.
    if capabilities.has_pulse_expansion() {
        new_list(ControlId::TargetExpansion, &["Off", "On"]).build(&mut controls);
    }

    // Target size mode (xHD2/Fantom): controls target rendering size.
    if capabilities.has_target_size_mode() {
        new_list(ControlId::TargetBoost, &["Off", "On"]).build(&mut controls);
    }

    // Scan average (xHD3/Fantom Pro): scan-to-scan averaging for noise reduction.
    // Mode accepts 0–5 on the wire; sensitivity is 0–10000 (percent × 100).
    if capabilities.has_scan_average() {
        new_list(
            ControlId::ScanAverageMode,
            &["Off", "Low", "Medium", "Medium High", "High", "Very High"],
        )
        .build(&mut controls);
        new_numeric(ControlId::ScanAverageSensitivity, 0., 100.)
            .wire_scale_factor(100., false)
            .build(&mut controls);
    }

    // --- Antenna-level / shared controls (Range A only) ---

    if !is_range_b {
        new_numeric(ControlId::WarmupTime, 0., 255.)
            .has_enabled()
            .read_only(true)
            .build(&mut controls);

        if capabilities.has(super::capabilities::cap::FRONT_OF_BOAT) {
            new_numeric(ControlId::BearingAlignment, -PI, PI)
                .wire_scale_factor(180. / PI, false)
                .build(&mut controls);
        }

        if capabilities.has(super::capabilities::cap::PARK_POSITION) {
            new_numeric(ControlId::ParkPosition, -PI, PI)
                .wire_scale_factor(180. / PI, false)
                .build(&mut controls);
        }

        // AFC tune: auto/manual mode. The trigger (0x092f) is sent
        // when transitioning from auto to manual.
        if capabilities.has_afc() {
            new_auto(ControlId::Tune, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
        }

        if capabilities.has_transmit_channel_select() {
            new_auto(ControlId::TransmitChannel, 1., 4., HAS_AUTO_NOT_ADJUSTABLE)
                .build(&mut controls);
        }

        if capabilities.has(super::capabilities::cap::DITHER_MODE)
            || capabilities.has(super::capabilities::cap::NOISE_BLANKER_MODE)
        {
            new_numeric(ControlId::InterferenceRejection, 0., 100.).build(&mut controls);
        }

        if capabilities.has(super::capabilities::cap::RPM_MODE) {
            new_numeric(ControlId::ScanSpeed, 0., 10.).build(&mut controls);
        }

        if radar_type == GarminRadarType::HD {
            new_list(ControlId::TargetExpansion, &["Off", "On"]).build(&mut controls);
        }

        if capabilities.has_no_tx_zone_1() && radar_type == GarminRadarType::XHD {
            new_sector(ControlId::NoTransmitSector1, -PI, PI)
                .wire_scale_factor(180. / PI, true)
                .build(&mut controls);
        }

        if capabilities.has_no_tx_zone_2() && radar_type == GarminRadarType::XHD {
            new_sector(ControlId::NoTransmitSector2, -PI, PI)
                .wire_scale_factor(180. / PI, true)
                .build(&mut controls);
        }

        // Read-only telemetry
        if radar_type == GarminRadarType::XHD {
            new_numeric(ControlId::OperatingTime, 0., u32::MAX as f64)
                .read_only(true)
                .wire_units(Units::Seconds)
                .build(&mut controls);
            new_numeric(ControlId::TransmitTime, 0., u32::MAX as f64)
                .read_only(true)
                .wire_units(Units::Seconds)
                .build(&mut controls);
            new_numeric(ControlId::MagnetronCurrent, 0., u16::MAX as f64)
                .read_only(true)
                .wire_scale_factor(1000., false)
                .wire_units(Units::Amps)
                .build(&mut controls);
            new_numeric(ControlId::SupplyVoltage, 0., u16::MAX as f64)
                .read_only(true)
                .wire_scale_factor(10., false)
                .wire_units(Units::Volts)
                .build(&mut controls);
            new_numeric(ControlId::DeviceTemperature, 0., u16::MAX as f64)
                .read_only(true)
                .wire_scale_factor(100., false)
                .wire_units(Units::Celsius)
                .build(&mut controls);
        }

        if capabilities.has_sentry_mode() && radar_type == GarminRadarType::XHD {
            new_list(ControlId::TimedIdle, &["Off", "On"]).build(&mut controls);
            new_numeric(ControlId::TimedRun, 1., 3600.)
                .wire_units(Units::Seconds)
                .build(&mut controls);
        }
    }

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}
