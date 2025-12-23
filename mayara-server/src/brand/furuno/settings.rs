use std::collections::HashMap;

use mayara_core::capabilities::controls::get_control_for_brand;
use mayara_core::{models, Brand};

use crate::{
    control_factory,
    radar::{range::Ranges, RadarInfo, NAUTICAL_MILE},
    settings::{Control, DataUpdate, SharedControls},
    Session,
};

use super::{RadarModel, FURUNO_SPOKES};

pub fn new(session: Session) -> SharedControls {
    let mut controls = HashMap::new();

    controls.insert(
        "userName".to_string(),
        Control::new_string("userName").read_only(false),
    );

    // Power control - from mayara-core (single source of truth)
    // Furuno: only standby (1) and transmit (2) are settable, send_always=true
    controls.insert(
        "power".to_string(),
        control_factory::power_control_for_brand(Brand::Furuno),
    );

    // Range control - valid values will be set when model is known from mayara-core
    let max_value = 120. * NAUTICAL_MILE as f32;
    let range_control = Control::new_numeric("range", 0., max_value).unit("m");
    // Note: valid_values will be populated from mayara-core when model is detected
    controls.insert("range".to_string(), range_control);

    // Gain, Sea, Rain controls with auto mode - from mayara-core (single source of truth)
    controls.insert(
        "gain".to_string(),
        control_factory::gain_control_for_brand(Brand::Furuno),
    );
    controls.insert(
        "sea".to_string(),
        control_factory::sea_control_for_brand(Brand::Furuno),
    );
    controls.insert(
        "rain".to_string(),
        control_factory::rain_control_for_brand(Brand::Furuno),
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
        control_factory::rotation_speed_control_for_brand(Brand::Furuno),
    );

    if log::log_enabled!(log::Level::Debug) {
        controls.insert(
            "spokes".to_string(),
            Control::new_numeric("spokes", 0., FURUNO_SPOKES as f32)
                .read_only(true)
                .unit("per rotation"),
        );
    }

    SharedControls::new(session, controls)
}

#[inline(never)]
pub fn update_when_model_known(info: &mut RadarInfo, model: RadarModel, version: &str) {
    let model_name = model.as_str();
    log::debug!("update_when_model_known: {}", model_name);
    info.controls.set_model_name(model_name.to_string());

    let mut control = control_factory::serial_number_control();
    if let Some(serial_number) = info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    info.controls.insert("serialNumber", control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Furuno ... ' name.
    if info.controls.user_name().as_deref() == Some(info.key().as_str()) {
        let mut user_name = model_name.to_string();
        if info.serial_no.is_some() {
            let serial = info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial);
        }
        info.controls.set_user_name(user_name);
    }

    // Get ranges from mayara-core model database (the single source of truth)
    let ranges = get_ranges_from_core(model_name);
    log::info!(
        "{}: model {} supports ranges {}",
        info.key(),
        model_name,
        ranges
    );
    // Update the RadarInfo ranges - this is used by the command handler
    info.ranges = ranges.clone();
    info.controls
        .set_valid_ranges("range", &ranges)
        .expect("Set valid values");
    // Notify data receiver of ranges - may fail if data receiver not yet started
    // (which is fine, it will use info.ranges when it starts)
    if let Err(e) = info
        .controls
        .get_data_update_tx()
        .send(DataUpdate::Ranges(ranges))
    {
        log::debug!(
            "{}: Ranges update not sent (data receiver not ready): {}",
            info.key(),
            e
        );
    }

    // Only set firmware version if we actually know it (from TCP $N96 response)
    // The locator passes "unknown" since it only has UDP beacon info
    if version != "unknown" {
        info.controls.insert(
            "firmwareVersion",
            control_factory::firmware_version_control(),
        );
        info.controls
            .set_string("firmwareVersion", version.to_string())
            .expect("FirmwareVersion");
    }

    // Add no-transmit zone controls (for radars that support them)
    // Uses core definitions for consistent metadata across server and WASM
    info.controls.insert(
        "noTransmitStart1",
        control_factory::no_transmit_angle_control_for_brand(
            "noTransmitStart1",
            1,
            true,
            Brand::Furuno,
        ),
    );
    info.controls.insert(
        "noTransmitEnd1",
        control_factory::no_transmit_angle_control_for_brand(
            "noTransmitEnd1",
            1,
            false,
            Brand::Furuno,
        ),
    );
    info.controls.insert(
        "noTransmitStart2",
        control_factory::no_transmit_angle_control_for_brand(
            "noTransmitStart2",
            2,
            true,
            Brand::Furuno,
        ),
    );
    info.controls.insert(
        "noTransmitEnd2",
        control_factory::no_transmit_angle_control_for_brand(
            "noTransmitEnd2",
            2,
            false,
            Brand::Furuno,
        ),
    );

    // Dynamically add extended controls from mayara-core based on model capabilities
    if let Some(model_info) = models::get_model(Brand::Furuno, model_name) {
        log::info!(
            "{}: Adding {} extended controls from model capabilities",
            info.key(),
            model_info.controls.len()
        );

        for control_id in model_info.controls {
            // Skip controls that are already added (like noTransmitZones which maps to Start/End controls)
            if *control_id == "noTransmitZones" {
                continue;
            }

            // Get control definition from mayara-core
            if let Some(core_def) = get_control_for_brand(control_id, Brand::Furuno) {
                log::info!(
                    "{}: Building control '{}' (type: {:?})",
                    info.key(),
                    control_id,
                    core_def.control_type
                );
                let control = control_factory::build_control(&core_def);
                log::info!(
                    "{}: Adding extended control '{}' from core definition",
                    info.key(),
                    control_id
                );
                info.controls.insert(control_id, control);
            } else {
                log::warn!(
                    "{}: Control '{}' listed in model capabilities but not found in mayara-core",
                    info.key(),
                    control_id
                );
            }
        }
    }
}

/// Get ranges from mayara-core model database.
/// This is the single source of truth for radar capabilities.
fn get_ranges_from_core(model_name: &str) -> Ranges {
    match models::get_model(Brand::Furuno, model_name) {
        Some(model_info) => {
            let ranges: Vec<i32> = model_info.range_table.iter().map(|&r| r as i32).collect();
            log::debug!(
                "Model {} found in mayara-core with {} ranges: {:?}",
                model_name,
                ranges.len(),
                ranges
            );
            Ranges::new_by_distance(&ranges)
        }
        None => {
            // Model not in mayara-core database
            log::warn!(
                "Model '{}' not found in mayara-core database! \
                Please open an issue at https://github.com/MaYaRa-MARINE/mayara/issues \
                to request support for this radar model. Include your radar model and \
                be willing to help with testing.",
                model_name
            );
            // Return empty ranges - radar won't work properly until model is added to core
            Ranges::new_by_distance(&[])
        }
    }
}
