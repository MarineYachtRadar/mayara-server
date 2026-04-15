use std::collections::HashMap;

use strum::VariantNames;

use super::{HaloMode, Model};
use crate::{
    Cli,
    radar::RadarInfo,
    radar::settings::{
        AutomaticValue, ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list,
        new_numeric, new_sector, new_string,
    },
    radar::units::Units,
    stream::SignalKDelta,
};

pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
    model: Option<&str>,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::ModelName).build(&mut controls);
    if model.is_some() {
        controls
            .get_mut(&ControlId::ModelName)
            .unwrap()
            .set_string(model.unwrap().to_string());
    }

    new_numeric(ControlId::AntennaForward, -100., 100.)
        .wire_scale_factor(1000., true)
        .wire_units(Units::Meters)
        .build(&mut controls);
    new_numeric(ControlId::AntennaHeight, 0., 60.)
        .wire_scale_factor(1000., false)
        .wire_scale_step(0.1) // Allow control in decimeters
        .wire_units(Units::Meters)
        .build(&mut controls);
    new_numeric(ControlId::AntennaStarboard, -100., 100.)
        .wire_scale_factor(1000., true)
        .wire_units(Units::Meters)
        .build(&mut controls);
    new_numeric(ControlId::BearingAlignment, -180., 180.)
        .wire_scale_factor(10., true)
        .wire_offset(-1.)
        .wire_units(Units::Degrees)
        .build(&mut controls);
    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    new_list(
        ControlId::LocalInterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    new_numeric(ControlId::Rain, 0., 100.)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);
    new_list(ControlId::TargetBoost, &["Off", "Low", "High"]).build(&mut controls);
    new_numeric(ControlId::TransmitTime, 0., 999999.)
        .read_only(true)
        .wire_units(Units::Hours)
        .build(&mut controls);
    new_string(ControlId::FirmwareVersion).build(&mut controls);
    new_auto(
        ControlId::SideLobeSuppression,
        0.,
        100.,
        HAS_AUTO_NOT_ADJUSTABLE,
    )
    .wire_scale_factor(2.55, false)
    .build(&mut controls);
    new_string(ControlId::SerialNumber).build(&mut controls);

    // Navico supports all three range unit modes
    // 0 = Nautical (default), 1 = Metric, 2 = Mixed
    new_list(ControlId::RangeUnits, &["Nautical", "Metric", "Mixed"]).build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

pub(crate) fn update_when_model_known(
    controls: &mut SharedControls,
    model: Model,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.to_string());

    // Override the wire_units for Range
    controls
        .set_wire_range(&ControlId::Range, 0.0, 10.0)
        .expect("Range is mandatory");

    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        controls
            .set_string(&&ControlId::SerialNumber, serial_number.to_string())
            .unwrap();
    }
    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Navico ... ' name.
    if controls.user_name() == radar_info.key() {
        let mut user_name = model.to_string();
        if radar_info.serial_no.is_some() {
            let mut serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial.split_off(7));
        }
        if radar_info.dual.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.dual.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    if model.is_halo() {
        controls.add(new_list(ControlId::Mode, HaloMode::VARIANTS));
        controls.add(new_list(
            ControlId::AccentLight,
            &["Off", "Low", "Medium", "High"],
        ));

        for (_, sector) in super::BLANKING_SECTORS {
            controls.add(
                new_sector(sector, -180., 180.)
                    .wire_scale_factor(10., true)
                    .wire_offset(-1.)
                    .wire_units(Units::Degrees)
                    .has_enabled(),
            );
        }

        controls.add(new_list(
            ControlId::SeaState,
            &["Calm", "Moderate", "Rough"],
        ));

        controls.add(new_auto(
            ControlId::Sea,
            0.,
            100.,
            AutomaticValue {
                has_auto: true,
                has_auto_adjustable: true,
                auto_adjust_min_value: Some(-50.),
                auto_adjust_max_value: Some(50.),
            },
        ));
    } else {
        controls.add(
            new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(2.55, false),
        );
    }

    controls.add(new_list(
        ControlId::ScanSpeed,
        if model.is_halo() {
            &["Normal", "Medium", "Medium Plus", "Fast"]
        } else {
            &["Normal", "Medium", "Medium-High"]
        },
    ));
    controls.add(new_list(
        ControlId::TargetExpansion,
        if model.is_halo() {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "On"]
        },
    ));
    controls.add(new_list(
        ControlId::NoiseRejection,
        if model.is_halo() {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "Low", "High"]
        },
    ));
    if model.is_halo() || model == Model::Gen4 {
        controls.add(new_list(
            ControlId::TargetSeparation,
            &["Off", "Low", "Medium", "High"],
        ));
    }
    if model.is_halo() {
        controls.add(new_list(
            ControlId::Doppler,
            &["Off", "Normal", "Approaching"],
        ));
        controls.add(new_list(ControlId::DopplerAutoTrack, &["Off", "On"]));
        controls.add(
            new_numeric(ControlId::DopplerSpeedThreshold, 0., 15.94)
                .wire_scale_step(0.01)
                .wire_units(Units::MetersPerSecond),
        );
        controls.add(new_list(ControlId::DopplerTrailsOnly, &["Off", "On"]));
    }

    controls.add(new_list(
        ControlId::NoiseRejection,
        if model.is_halo() {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "Low", "High"]
        },
    ));

    log::debug!("update_when_model_known: {:?}", controls);
}

/// Refine controls based on 0xC409 TLV capabilities. Called after
/// `update_when_model_known` for HALO models when the capability
/// advertisement arrives from the radar.
pub(crate) fn update_from_capabilities(
    controls: &mut SharedControls,
    caps: &super::capabilities::NavicoCapabilities,
) {
    controls.set_valid_values_bitmask(&ControlId::ScanSpeed, caps.scan_speed_mask);
    controls.set_valid_values_bitmask(&ControlId::TargetExpansion, caps.target_boost_mask);
    controls.set_valid_values_bitmask(&ControlId::NoiseRejection, caps.noise_reject_mask);
    controls.set_valid_values_bitmask(&ControlId::TargetSeparation, caps.beam_sharpening_mask);
    controls.set_valid_values_bitmask(&ControlId::InterferenceRejection, caps.interference_reject_mask);
    controls.set_valid_values_bitmask(&ControlId::LocalInterferenceRejection, caps.local_interference_mask);

    log::debug!("update_from_capabilities: refined controls from TLV");
}
