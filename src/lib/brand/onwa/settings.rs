use std::collections::HashMap;

use crate::{
    Cli,
    radar::{
        settings::{ControlId, SharedControls, new_list, new_numeric, new_sector, new_string},
        units::Units,
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
    new_string(ControlId::FirmwareVersion)
        .read_only(true)
        .build(&mut controls);

    // Counted by Mayara: the radar does not report its warm-up
    new_numeric(ControlId::WarmupTime, 0., 255.)
        .has_enabled()
        .read_only(true)
        .build(&mut controls);

    new_numeric(ControlId::Gain, 0., 100.).build(&mut controls);
    new_numeric(ControlId::Sea, 0., 100.).build(&mut controls);
    new_numeric(ControlId::Rain, 0., 100.).build(&mut controls);
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    // The radar's "echo stretch"
    new_list(ControlId::TargetExpansion, &["Off", "Low", "High"]).build(&mut controls);
    new_list(ControlId::NoiseRejection, &["Off", "On"]).build(&mut controls);
    // The radar's "sweep timing"
    new_numeric(ControlId::DisplayTiming, 0., 100.).build(&mut controls);

    // The radar's "dead sector": tenths of a degree on the wire
    new_sector(ControlId::NoTransmitSector1, -180., 180.)
        .wire_scale_factor(10., true)
        .wire_offset(-1.)
        .wire_units(Units::Degrees)
        .has_enabled()
        .build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

/// This brand's controls for every model it knows, for the UI strings catalog
/// in [`crate::radar::ui_strings`].
#[cfg(test)]
pub(crate) fn controls_for_every_model(args: &Cli) -> Vec<SharedControls> {
    vec![new(
        "onw1234".to_string(),
        tokio::sync::broadcast::Sender::new(1),
        args,
    )]
}
