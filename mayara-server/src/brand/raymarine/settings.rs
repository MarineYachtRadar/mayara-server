use std::collections::HashMap;

use mayara_core::Brand;

use crate::{
    brand::raymarine::RaymarineModel,
    control_factory,
    radar::{RadarInfo, NAUTICAL_MILE_F64},
    settings::{Control, SharedControls},
    Session,
};

use super::BaseModel;

pub fn new(session: Session, model: BaseModel) -> SharedControls {
    let mut controls = HashMap::new();

    let mut control = Control::new_string("userName");
    control.set_string(model.to_string());
    controls.insert("userName".to_string(), control.read_only(false));

    let mut control = Control::new_string("modelName");
    control.set_string(model.to_string());
    controls.insert("modelName".to_string(), control);

    // From mayara-core (single source of truth)
    controls.insert(
        "bearingAlignment".to_string(),
        control_factory::bearing_alignment_control_for_brand(Brand::Raymarine),
    );
    controls.insert(
        "gain".to_string(),
        control_factory::gain_control_for_brand(Brand::Raymarine),
    );
    controls.insert(
        "interferenceRejection".to_string(),
        control_factory::interference_rejection_control(),
    );
    controls.insert(
        "rain".to_string(),
        control_factory::rain_control_for_brand(Brand::Raymarine),
    );

    let mut control = Control::new_numeric("ftc", 0., 100.).wire_scale_factor(100., false);
    if model == BaseModel::RD {
        control = control.has_enabled();
    }
    controls.insert("ftc".to_string(), control);

    controls.insert(
        "rotationSpeed".to_string(),
        control_factory::rotation_speed_control_for_brand(Brand::Raymarine),
    );
    controls.insert(
        "operatingHours".to_string(),
        control_factory::operating_hours_control(),
    );
    controls.insert(
        "mainBangSuppression".to_string(),
        control_factory::main_bang_suppression_control(),
    );

    match model {
        BaseModel::Quantum => {
            controls.insert(
                "mode".to_string(),
                Control::new_list("mode", &["Harbor", "Coastal", "Offshore", "Weather"]),
            );
            controls.insert(
                "targetExpansion".to_string(),
                control_factory::target_expansion_control(),
            );
            controls.insert(
                "colorGain".to_string(),
                control_factory::color_gain_control_for_brand(Brand::Raymarine),
            );
        }
        BaseModel::RD => {
            controls.insert(
                "magnetronCurrent".to_string(),
                Control::new_numeric("magnetronCurrent", 0., 65535.).read_only(true),
            );
            controls.insert(
                "displayTiming".to_string(),
                Control::new_numeric("displayTiming", 0., 255.).read_only(true),
            );
            controls.insert(
                "signalStrength".to_string(),
                Control::new_numeric("signalStrength", 0., 255.).read_only(true),
            );
            controls.insert(
                "warmupTime".to_string(),
                Control::new_numeric("warmupTime", 0., 255.)
                    .has_enabled()
                    .read_only(true),
            );
            controls.insert(
                "tune".to_string(),
                control_factory::tune_control_for_brand(Brand::Raymarine).read_only(true),
            );
        }
    }
    SharedControls::new(session, controls)
}

pub fn update_when_model_known(
    controls: &mut SharedControls,
    model: &RaymarineModel,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.name.to_string());

    let mut control = control_factory::serial_number_control();
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert("serialNumber", control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Raymarine ... ' name.
    if controls.user_name().as_deref() == Some(radar_info.key().as_str()) {
        let mut user_name = model.name.to_string();
        if radar_info.serial_no.is_some() {
            let serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial);
        }
        if radar_info.which.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.which.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    let max_value = 36. * NAUTICAL_MILE_F64 as f32;
    let range_control = Control::new_numeric("range", 50., max_value).unit("m");
    controls.insert("range", range_control);

    controls.insert(
        "sea",
        control_factory::sea_control_for_brand(Brand::Raymarine),
    );
    controls.insert(
        "targetExpansion",
        control_factory::target_expansion_control(),
    );
}
