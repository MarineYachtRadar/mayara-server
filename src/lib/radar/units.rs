use serde_string_enum::{DeserializeLabeledStringEnum, SerializeLabeledStringEnum};
use std::f64::consts::{PI, TAU};
use utoipa::ToSchema;

#[derive(
    Copy,
    PartialEq,
    SerializeLabeledStringEnum,
    DeserializeLabeledStringEnum,
    Clone,
    Debug,
    ToSchema,
)]
pub enum Units {
    #[string = ""]
    None,
    #[string = "m"]
    Meters,
    #[string = "km"]
    KiloMeters,
    #[string = "nm"]
    NauticalMiles,
    #[string = "m/s"]
    MetersPerSecond,
    #[string = "kn"]
    Knots,
    #[string = "deg"]
    Degrees,
    #[string = "rad"]
    Radians,
    #[string = "rad/s"]
    RadiansPerSecond,
    #[string = "rpm"]
    RotationsPerMinute,
    #[string = "s"]
    Seconds,
    #[string = "min"]
    Minutes,
    #[string = "h"]
    Hours,
    #[string = "V"]
    Volts,
    #[string = "A"]
    Amps,
    #[string = "°C"]
    Celsius,
    #[string = "K"]
    Kelvin,
}

impl Units {
    pub(crate) fn to_si(self, value: f64) -> (Units, f64) {
        // Celsius→Kelvin is an affine transform (offset, not just scale).
        if self == Units::Celsius {
            return (Units::Kelvin, value + 273.15);
        }
        let (units, factor) = match self {
            Units::Degrees => (Units::Radians, PI / 180.),
            Units::Hours => (Units::Seconds, 3600.),
            Units::Volts => (Units::Volts, 1.),
            Units::Amps => (Units::Amps, 1.),
            Units::Kelvin => (Units::Kelvin, 1.),
            Units::Minutes => (Units::Seconds, 60.),
            Units::KiloMeters => (Units::Meters, 1000.),
            Units::Knots => (Units::MetersPerSecond, 1852. / 3600.),
            Units::Meters => (Units::Meters, 1.),
            Units::MetersPerSecond => (Units::MetersPerSecond, 1.),
            Units::NauticalMiles => (Units::Meters, 1852.),
            Units::None => unreachable!("Units::None"),
            Units::Radians => (Units::Radians, 1.),
            Units::RadiansPerSecond => (Units::RadiansPerSecond, 1.),
            Units::RotationsPerMinute => (Units::RotationsPerMinute, TAU / 60.),
            Units::Seconds => (Units::Seconds, 1.),
            Units::Celsius => unreachable!(), // handled above
        };
        (units, value * factor)
    }

    pub(crate) fn from_si(self, value: f64) -> f64 {
        // Kelvin→Celsius is an affine transform (offset, not just scale).
        if self == Units::Celsius {
            return value - 273.15;
        }
        let factor = match self {
            Units::Degrees => 180. / PI,
            Units::Hours => 1. / 3600.,
            Units::Volts => 1.,
            Units::Amps => 1.,
            Units::Kelvin => 1.,
            Units::Minutes => 1. / 60.,
            Units::KiloMeters => 0.001,
            Units::Knots => 3600. / 1852.,
            Units::Meters => 1.,
            Units::MetersPerSecond => 1.,
            Units::NauticalMiles => 1. / 1852.,
            Units::None => unreachable!("Units::None"),
            Units::Radians => 1.,
            Units::RadiansPerSecond => 1.,
            Units::RotationsPerMinute => 60. / TAU,
            Units::Seconds => 1.,
            Units::Celsius => unreachable!(), // handled above
        };

        value * factor
    }
}
