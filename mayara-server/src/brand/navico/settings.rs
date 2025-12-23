use std::collections::HashMap;

use mayara_core::Brand;

use crate::{
    control_factory,
    radar::{RadarInfo, NAUTICAL_MILE_F64},
    settings::{
        AutomaticValue, Control, ControlDestination, SharedControls, HAS_AUTO_NOT_ADJUSTABLE,
    },
    Session,
};

use super::Model;

pub fn new(session: Session, model: Option<&str>) -> SharedControls {
    let mut controls = HashMap::new();

    let mut control = Control::new_string("modelName");
    if model.is_some() {
        control.set_string(model.unwrap().to_string());
    }
    controls.insert("modelName".to_string(), control);

    // Power control - from mayara-core (single source of truth)
    controls.insert(
        "power".to_string(),
        control_factory::power_control_for_brand(Brand::Navico),
    );

    // From mayara-core (single source of truth)
    controls.insert(
        "antennaHeight".to_string(),
        control_factory::antenna_height_control_for_brand(Brand::Navico),
    );
    controls.insert(
        "bearingAlignment".to_string(),
        control_factory::bearing_alignment_control_for_brand(Brand::Navico),
    );
    controls.insert(
        "gain".to_string(),
        control_factory::gain_control_for_brand(Brand::Navico),
    );
    controls.insert(
        "interferenceRejection".to_string(),
        control_factory::interference_rejection_control(),
    );
    controls.insert(
        "localInterferenceRejection".to_string(),
        control_factory::local_interference_rejection_control(),
    );
    controls.insert(
        "rain".to_string(),
        control_factory::rain_control_for_brand(Brand::Navico),
    );
    controls.insert(
        "targetBoost".to_string(),
        control_factory::target_boost_control(),
    );
    controls.insert(
        "operatingHours".to_string(),
        control_factory::operating_hours_control(),
    );
    controls.insert(
        "transmitHours".to_string(),
        control_factory::transmit_hours_control(),
    );

    controls.insert(
        "rotationSpeed".to_string(),
        control_factory::rotation_speed_control_for_brand(Brand::Navico),
    );

    controls.insert(
        "firmwareVersion".to_string(),
        control_factory::firmware_version_control(),
    );
    controls.insert(
        "sidelobeSuppression".to_string(),
        control_factory::sidelobe_suppression_control_for_brand(Brand::Navico),
    );

    SharedControls::new(session, controls)
}

pub fn update_when_model_known(controls: &SharedControls, model: Model, radar_info: &RadarInfo) {
    controls.set_model_name(model.to_string());

    let mut control = control_factory::serial_number_control();
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert("serialNumber", control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Navico ... ' name.
    if controls.user_name().as_deref() == Some(radar_info.key().as_str()) {
        let mut user_name = model.to_string();
        if radar_info.serial_no.is_some() {
            let mut serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial.split_off(7));
        }
        if radar_info.which.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.which.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    let max_value = (match model {
        Model::Unknown => 96.,
        Model::BR24 => 24.,
        Model::Gen3 => 36.,
        Model::Gen4 => 48.,
        Model::HALO => 96.,
    }) * NAUTICAL_MILE_F64 as f32;
    let mut range_control = Control::new_numeric("range", 50., max_value)
        .unit("m")
        .wire_scale_factor(10. * max_value, false); // Radar sends and receives in decimeters
    range_control.set_valid_ranges(&radar_info.ranges);
    controls.insert("range", range_control);

    if model == Model::HALO {
        controls.insert(
            "mode",
            Control::new_list(
                "mode",
                &["Custom", "Harbor", "Offshore", "Buoy", "Weather", "Bird"],
            ),
        );
        controls.insert("accentLight", control_factory::accent_light_control());

        // No-transmit zones use core definitions for consistent metadata
        for (zone_idx, start_id, end_id) in super::BLANKING_SETS {
            let zone_number = (zone_idx + 1) as u8;
            controls.insert(
                start_id,
                control_factory::no_transmit_angle_control_for_brand(
                    start_id,
                    zone_number,
                    true,
                    Brand::Navico,
                ),
            );
            controls.insert(
                end_id,
                control_factory::no_transmit_angle_control_for_brand(
                    end_id,
                    zone_number,
                    false,
                    Brand::Navico,
                ),
            );
        }

        controls.insert("seaState", control_factory::sea_state_control());

        controls.insert(
            "sea",
            Control::new_auto(
                "sea",
                0.,
                100.,
                AutomaticValue {
                    has_auto: true,
                    has_auto_adjustable: true,
                    auto_adjust_min_value: -50.,
                    auto_adjust_max_value: 50.,
                },
            ),
        );
    } else {
        controls.insert(
            "sea",
            Control::new_auto("sea", 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(255., false),
        );
    }

    controls.insert(
        "scanSpeed",
        Control::new_list(
            "scanSpeed",
            if model == Model::HALO {
                &["Normal", "Medium", "Medium Plus", "Fast"]
            } else {
                &["Normal", "Medium", "Medium-High"]
            },
        ),
    );
    controls.insert(
        "targetExpansion",
        Control::new_list(
            "targetExpansion",
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "On"]
            },
        ),
    );
    controls.insert(
        "noiseRejection",
        Control::new_list(
            "noiseRejection",
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );
    if model.has_dual_range() {
        controls.insert(
            "targetSeparation",
            control_factory::target_separation_control(),
        );
    }
    if model.has_doppler() {
        controls.insert(
            "dopplerMode",
            Control::new_list("dopplerMode", &["Off", "Normal", "Approaching"]),
        );
        controls.insert(
            "dopplerAutoTrack",
            Control::new_list("dopplerAutoTrack", &["Off", "On"])
                .set_destination(ControlDestination::Data),
        );
        controls.insert(
            "dopplerSpeed",
            control_factory::doppler_speed_control_for_brand(Brand::Navico),
        );
        controls.insert(
            "dopplerTrailsOnly",
            Control::new_list("dopplerTrailsOnly", &["Off", "On"])
                .set_destination(ControlDestination::Data),
        );
    }

    controls.insert(
        "noiseRejection",
        Control::new_list(
            "noiseRejection",
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );

    log::debug!("update_when_model_known: {:?}", controls);
}
