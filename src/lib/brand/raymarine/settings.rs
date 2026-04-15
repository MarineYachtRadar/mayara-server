use std::collections::HashMap;

use crate::{
    Cli,
    brand::raymarine::RaymarineModel,
    radar::RadarInfo,
    radar::settings::{
        ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list, new_numeric,
        new_sector, new_string,
    },
    radar::units::Units,
    stream::SignalKDelta,
};

use super::BaseModel;

pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
    model: BaseModel,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::UserName).build(&mut controls);
    controls
        .get_mut(&ControlId::UserName)
        .unwrap()
        .set_string(model.to_string());

    new_string(ControlId::ModelName).build(&mut controls);
    controls
        .get_mut(&ControlId::ModelName)
        .unwrap()
        .set_string(model.to_string());

    new_numeric(ControlId::BearingAlignment, -180., 180.)
        .wire_scale_factor(10., true)
        .wire_offset(-1.)
        .wire_units(Units::Degrees)
        .build(&mut controls);
    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Level 1", "Level 2", "Level 3", "Level 4", "Level 5"],
    )
    .build(&mut controls);

    new_numeric(ControlId::Rain, 0., 100.)
        .has_enabled()
        .build(&mut controls);

    match model {
        BaseModel::Quantum => {
            new_list(
                ControlId::Mode,
                &["Harbor", "Coastal", "Offshore", "Weather"],
            )
            .build(&mut controls);
            new_list(ControlId::Doppler, &["Off", "On"]).build(&mut controls);
            new_list(ControlId::TargetExpansion, &["Off", "On"]).build(&mut controls);
            new_auto(ControlId::ColorGain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
            new_list(ControlId::MainBangSuppression, &["Off", "On"]).build(&mut controls);
            new_sector(ControlId::NoTransmitSector1, 0., 359.)
                .wire_scale_step(0.1)
                .has_enabled()
                .wire_units(Units::Degrees)
                .build(&mut controls);
            new_sector(ControlId::NoTransmitSector2, 0., 359.)
                .wire_scale_step(0.1)
                .has_enabled()
                .wire_units(Units::Degrees)
                .build(&mut controls);
            new_numeric(ControlId::SeaClutterCurve, 1., 2.).build(&mut controls);
        }
        BaseModel::RD => {
            new_numeric(ControlId::TransmitTime, 0., 65535.)
                .read_only(true)
                .wire_units(Units::Hours)
                .build(&mut controls);
            new_numeric(ControlId::MagnetronCurrent, 0., 65535.)
                .read_only(true)
                .build(&mut controls);
            new_numeric(ControlId::DisplayTiming, 0., 255.)
                .read_only(true)
                .build(&mut controls);
            new_numeric(ControlId::SignalStrength, 0., 255.)
                .read_only(true)
                .build(&mut controls);
            new_numeric(ControlId::WarmupTime, 0., 255.)
                .has_enabled()
                .read_only(true)
                .build(&mut controls);
            new_auto(ControlId::Tune, 0., 255., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(255., false)
                .read_only(true)
                .build(&mut controls);

            let mut builder = new_numeric(ControlId::Ftc, 0., 100.).wire_scale_factor(100., false);
            if model == BaseModel::RD {
                builder = builder.has_enabled();
            }
            builder.build(&mut controls);
            new_list(ControlId::MainBangSuppression, &["Off", "On"]).build(&mut controls);
        }
    }
    new_string(ControlId::SerialNumber).build(&mut controls);

    // Raymarine is nautical-only - no RangeUnits control, default is already 0 (Nautical)
    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

pub(crate) fn update_when_model_known(
    controls: &mut SharedControls,
    model: &RaymarineModel,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.name.to_string());

    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        controls
            .set_string(&ControlId::SerialNumber, serial_number.to_string())
            .expect("SerialNumber");
    }

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Raymarine ... ' name.
    if controls.user_name() == radar_info.key() {
        let mut user_name = model.name.to_string();
        if radar_info.serial_no.is_some() {
            let serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial);
        }
        if radar_info.dual.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.dual.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    controls.add(
        new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).wire_scale_factor(255., false),
    );

    controls.add(new_list(ControlId::TargetExpansion, &["Off", "On"]));
}
