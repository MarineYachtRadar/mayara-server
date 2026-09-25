use std::collections::HashMap;

use crate::{
    Cli,
    brand::raymarine::RaymarineModel,
    radar::Power,
    radar::RadarInfo,
    radar::settings::{
        ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_auto_standby, new_list,
        new_numeric, new_sector, new_string,
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
            // The scanner's "Heater hour count": lifetime magnetron heater time,
            // standby and transmit summed, as a u16 in tenths of an hour.
            // Confirmed against an E120 self-test screen showing 2792.3 h for a
            // wire value of 27923.
            new_numeric(ControlId::OperatingTime, 0., 6553.5)
                .read_only(true)
                .wire_scale_step(0.1)
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

    // The report receiver drops the heartbeat while the radar should stand down
    new_auto_standby().build(&mut controls);

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
        if let Some(dual) = radar_info.dual.as_ref() {
            user_name.push(' ');
            user_name.push_str(dual);
        }
        controls.set_user_name(user_name);
    }

    controls.add(
        new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).wire_scale_factor(255., false),
    );

    controls.add(new_list(ControlId::TargetExpansion, &["Off", "On"]));

    // Doppler belongs only to the radars that have it. The capability is not
    // known when `new()` runs at discovery — it arrives with the E-number in
    // the 0x280001 info report, which is what picks `model` here. A Q24C
    // reports features 0x00001900, Doppler bit clear, and must not be offered
    // a switch it cannot honour.
    if model.doppler {
        controls.add(new_list(ControlId::Doppler, &["Off", "On"]));
    }

    // Quantum accepts a full power-off (mode 3, wire-confirmed in issue #160)
    // in addition to the generic Standby/Transmit. Widen the Power control so
    // the Radar API exposes and accepts Off for these radars.
    if model.model == BaseModel::Quantum {
        controls.add_valid_value(&ControlId::Power, Power::Off as i32);
    }
}

/// This brand's controls for every model it knows, for the UI strings catalog
/// in [`crate::radar::ui_strings`].
#[cfg(test)]
pub(crate) fn controls_for_every_model(args: &Cli) -> Vec<SharedControls> {
    use strum::IntoEnumIterator;

    BaseModel::iter()
        .map(|base_model| {
            let info =
                crate::radar::ui_strings::radar_info(crate::Brand::Raymarine, args, |id, tx| {
                    new(id, tx, args, base_model)
                });
            let model = RaymarineModel {
                model: base_model,
                hd: false,
                max_spoke_len: 512,
                // The catalog is the union of every string a radar of this
                // brand can show, so claim every optional capability.
                doppler: true,
                name: "Test",
            };
            let mut controls = info.controls.clone();
            update_when_model_known(&mut controls, &model, &info);
            controls
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::time::Duration;

    /// A control must only exist for hardware that honours it. A Quantum
    /// without Doppler (a Q24C, features 0x00001900) previously got a Doppler
    /// switch anyway, and a PUT on it reached the wire. See #705.
    #[test]
    fn doppler_is_offered_only_to_radars_that_have_it() {
        let args = Cli::parse_from(["mayara-server"]);

        // Doppler is gated on the capability, not the family, because that is
        // what the model table records. No RD entry in the table sets it, so
        // an RD is only exercised for the absence.
        for (base_model, doppler, expected) in [
            (BaseModel::Quantum, true, true),
            (BaseModel::Quantum, false, false),
            (BaseModel::RD, false, false),
        ] {
            let info =
                crate::radar::ui_strings::radar_info(crate::Brand::Raymarine, &args, |id, tx| {
                    new(id, tx, &args, base_model)
                });
            assert!(
                info.controls.get(&ControlId::Doppler).is_none(),
                "{base_model} must not offer Doppler before its capability is known"
            );

            let model = RaymarineModel {
                model: base_model,
                hd: false,
                max_spoke_len: 512,
                doppler,
                name: "Test",
            };
            let mut controls = info.controls.clone();
            update_when_model_known(&mut controls, &model, &info);

            assert_eq!(
                controls.get(&ControlId::Doppler).is_some(),
                expected,
                "{base_model} with doppler={doppler} should{} offer Doppler",
                if expected { "" } else { " not" }
            );
        }
    }

    /// Both Raymarine families are held up by the same heartbeat, so both
    /// offer the control, enabled at its default.
    #[test]
    fn raymarine_offers_auto_standby_at_one_minute() {
        let args = Cli::parse_from(["mayara-server"]);
        for model in [BaseModel::Quantum, BaseModel::RD] {
            let tx = tokio::sync::broadcast::Sender::new(1);
            let controls = new("ray1234".to_string(), tx, &args, model);
            assert_eq!(controls.auto_standby(), Some(Duration::from_secs(60)));
        }
    }
}
