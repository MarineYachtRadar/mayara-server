use std::f64::consts::TAU;

use crate::{protos::RadarMessage::radar_message::Spoke, radar::SpokeBearing};

pub(crate) type GenericSpoke = Vec<u8>;

pub(crate) fn to_protobuf_spoke(
    spokes_per_revolution: u16,
    range: u32,
    angle: SpokeBearing,
    heading: Option<u16>,
    time: Option<u64>,
    generic_spoke: GenericSpoke,
) -> Spoke {
    log::trace!(
        "Spoke {}/{:?}/{} len {}",
        range,
        heading,
        angle,
        generic_spoke.len()
    );

    // `heading` is expected to already be in `spokes_per_revolution`-units
    // (each brand normalizes before calling add_spoke).
    let heading = if heading.is_some() {
        heading.map(|h| ((h + angle) % spokes_per_revolution) as u32)
    } else {
        let heading = crate::navdata::get_heading_true();
        heading.map(|h| {
            (((h * spokes_per_revolution as f64 / TAU) as u16 + angle)
                % spokes_per_revolution) as u32
        })
    };

    let mut spoke = Spoke::new();
    spoke.range = range;
    spoke.angle = angle as u32;
    spoke.bearing = heading;

    (spoke.lat, spoke.lon) = crate::navdata::get_position();
    spoke.time = time;
    spoke.data = generic_spoke;

    spoke
}
