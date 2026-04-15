use std::collections::HashMap;

use super::EMULATOR_RANGES;
use crate::Cli;
use crate::radar::settings::{
    ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list, new_numeric, new_string,
};
use crate::radar::units::Units;
use crate::stream::SignalKDelta;

pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
) -> SharedControls {
    let mut controls = HashMap::new();

    // User name (required)
    new_string(ControlId::UserName).build(&mut controls);
    controls
        .get_mut(&ControlId::UserName)
        .unwrap()
        .set_string("Emulator HALO".to_string());

    // Model name
    new_string(ControlId::ModelName).build(&mut controls);
    controls
        .get_mut(&ControlId::ModelName)
        .unwrap()
        .set_string("Emulator HALO".to_string());

    // Serial number
    new_string(ControlId::SerialNumber).build(&mut controls);
    controls
        .get_mut(&ControlId::SerialNumber)
        .unwrap()
        .set_string("EMU00001".to_string());

    // Firmware version
    new_string(ControlId::FirmwareVersion).build(&mut controls);
    controls
        .get_mut(&ControlId::FirmwareVersion)
        .unwrap()
        .set_string("1.0.0 (emulator)".to_string());

    // Range - use the emulator ranges
    let max_range = *EMULATOR_RANGES.last().unwrap() as f64;
    new_numeric(ControlId::Range, 0., max_range)
        .wire_units(Units::Meters)
        .build(&mut controls);

    // Gain with auto
    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);

    // Sea clutter with auto
    new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);

    // Rain clutter
    new_numeric(ControlId::Rain, 0., 100.)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);

    // Interference rejection
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);

    // Target boost
    new_list(ControlId::TargetBoost, &["Off", "Low", "High"]).build(&mut controls);

    // Target expansion
    new_list(
        ControlId::TargetExpansion,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);

    // Scan speed
    new_list(
        ControlId::ScanSpeed,
        &["Normal", "Medium", "Medium Plus", "Fast"],
    )
    .build(&mut controls);

    // Side lobe suppression with auto
    new_auto(
        ControlId::SideLobeSuppression,
        0.,
        100.,
        HAS_AUTO_NOT_ADJUSTABLE,
    )
    .wire_scale_factor(2.55, false)
    .build(&mut controls);

    // Noise rejection
    new_list(ControlId::NoiseRejection, &["Off", "Low", "Medium", "High"]).build(&mut controls);

    // Target separation
    new_list(
        ControlId::TargetSeparation,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);

    // Doppler
    new_list(ControlId::Doppler, &["Off", "Normal", "Approaching"]).build(&mut controls);

    // Doppler speed threshold
    new_numeric(ControlId::DopplerSpeedThreshold, 0., 15.94)
        .wire_scale_step(0.01)
        .wire_units(Units::MetersPerSecond)
        .build(&mut controls);

    // Antenna height
    new_numeric(ControlId::AntennaHeight, 0., 60.)
        .wire_scale_factor(1000., false)
        .wire_scale_step(0.1)
        .wire_units(Units::Meters)
        .build(&mut controls);

    // Bearing alignment
    new_numeric(ControlId::BearingAlignment, -180., 180.)
        .wire_scale_factor(10., true)
        .wire_offset(-1.)
        .wire_units(Units::Degrees)
        .build(&mut controls);

    // Operating time (read-only)
    new_numeric(ControlId::TransmitTime, 0., 999999.)
        .read_only(true)
        .wire_units(Units::Hours)
        .build(&mut controls);

    // Range units
    new_list(ControlId::RangeUnits, &["Nautical", "Metric", "Mixed"]).build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}
