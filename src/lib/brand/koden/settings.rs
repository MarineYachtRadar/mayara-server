use std::collections::HashMap;
use std::f64::consts::PI;

use super::protocol::KODEN_ANGLE_SCALE;
use crate::{
    Cli,
    radar::settings::{
        ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list, new_numeric,
        new_sector, new_string,
    },
    stream::SignalKDelta,
};

pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::UserName).build(&mut controls);
    new_string(ControlId::ModelName)
        .read_only(true)
        .build(&mut controls);
    new_string(ControlId::SerialNumber)
        .read_only(true)
        .build(&mut controls);

    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_numeric(ControlId::Sea, 0., 100.).build(&mut controls);
    new_numeric(ControlId::Rain, 0., 100.).build(&mut controls);
    new_list(ControlId::SeaState, &["Manual", "Auto", "Harbor"]).build(&mut controls);
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    new_list(ControlId::TargetExpansion, &["Off", "On"]).build(&mut controls);
    new_list(ControlId::ScanSpeed, &["Normal", "Fast"]).build(&mut controls);
    new_auto(ControlId::Tune, 0., 255., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_numeric(ControlId::TuneFine, 0., 15.).build(&mut controls);
    new_list(ControlId::PulseWidth, &["Short", "Long"]).build(&mut controls);

    new_numeric(ControlId::DisplayTiming, 0., 124.).build(&mut controls);

    new_sector(ControlId::NoTransmitSector1, -PI, PI)
        .wire_scale_factor(KODEN_ANGLE_SCALE * 180. / PI, true)
        .wire_offset(-1.)
        .build(&mut controls);

    new_numeric(ControlId::ParkPosition, -PI, PI)
        .wire_scale_factor(KODEN_ANGLE_SCALE * 180. / PI, false)
        .wire_offset(-1.)
        .build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}
