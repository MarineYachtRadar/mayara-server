use chrono::{DateTime, Utc};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Deserializer, Serialize, de::Visitor, ser::Serializer};
use serde_json::{Number, Value};
use std::cell::RefCell;
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::From,
    fmt::{self, Display},
    str::FromStr,
    sync::{Arc, RwLock},
};
use strum::{EnumCount, EnumIter, EnumString, IntoStaticStr};
use thiserror::Error;
use utoipa::ToSchema;

use super::NAUTICAL_MILE;
use super::range::Range;
use super::units::Units;
use crate::Cli;
use crate::config::GuardZone;
use crate::stream::SignalKDelta;
use crate::{
    TargetMode,
    radar::{Power, RadarError, range::Ranges},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiVersion {
    V1 = 1,
    V3 = 3,
}

thread_local! {
    static API_VERSION: RefCell<ApiVersion> = RefCell::new(ApiVersion::V3);
}

pub fn set_api_version(version: ApiVersion) {
    API_VERSION.with(|v| {
        *v.borrow_mut() = version;
    });
}
pub fn get_api_version() -> ApiVersion {
    API_VERSION.with(|v| v.borrow().clone())
}

#[derive(
    Eq,
    PartialEq,
    PartialOrd,
    Hash,
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    ToPrimitive,
    EnumIter,
    EnumString,
    IntoStaticStr,
    EnumCount,
    ToSchema,
)]
#[repr(u8)]
#[strum(ascii_case_insensitive, serialize_all = "camelCase")]
// The order is the one in which we deem the representation is "right"
// when present as a straight list of controls. This is the same order
// as shown in the radar page for HALO on NOS MFDs.
pub enum ControlId {
    Power,
    WarmupTime,
    Range,
    Mode,
    // AllAuto,
    Gain,
    ColorGain,
    Sea,
    SeaState,
    // Stc,
    // StcCurve,
    Rain,
    // Scaling,
    Doppler,
    DopplerMode,
    // Client Only, not here: ColorPalette,
    // Client Only, not here: Orientation,
    // Client Only, not here: Position,
    // Client Only, not here: Symbology,
    ShowAis,

    // Target tracking
    GuardZone1,
    GuardZone2,
    DopplerAutoTrack,
    ArpaDetectMaxSpeed,
    ClearTargets,

    // Exclusion zones (stationary installations only)
    ExclusionZone1,
    ExclusionZone2,
    ExclusionZone3,
    ExclusionZone4,
    // Rectangular exclusion zones (stationary installations only)
    ExclusionRect1,
    ExclusionRect2,
    ExclusionRect3,
    ExclusionRect4,

    // Trails
    TargetTrails,
    TrailsMotion,
    DopplerTrailsOnly,
    ClearTrails,
    // TimedIdle,
    // TimedRun,
    DopplerSpeedThreshold,
    NoiseRejection,
    TargetBoost,
    TargetExpansion,
    InterferenceRejection,
    TargetSeparation,
    LocalInterferenceRejection,
    BirdMode,
    ScanSpeed,
    SideLobeSuppression,
    Tune,
    // TuneCoarse,
    // TuneFine,
    // ColorGain,
    // DisplayTiming,
    Ftc,
    // MainBangSize,
    RangeUnits,
    MainBangSuppression,
    SeaClutterCurve,
    DisplayTiming,
    NoTransmitSector1,
    NoTransmitSector2,
    NoTransmitSector3,
    NoTransmitSector4,
    AccentLight,
    AntennaHeight,
    AntennaForward,
    AntennaStarboard,
    BearingAlignment,
    // Orientation,
    RotationSpeed,
    MagnetronCurrent,
    SignalStrength,
    OperatingTime,
    TransmitTime,
    ModelName,
    FirmwareVersion,
    SerialNumber,
    Spokes,
    SpokeLength,
    SpokeProcessing,
    UserName,
    // Appended (not inserted above) to keep u8 discriminants of all existing
    // ControlIds stable for numeric API clients.
    TimedIdle,
    TimedRun,
    SupplyVoltage,
    DeviceTemperature,
    ScanAverageMode,
    ScanAverageSensitivity,
    ParkPosition,
    TransmitChannel,
    PulseWidth,
    NearStcCurve,
    MiddleStcCurve,
    FarStcCurve,
    StcRange,
    AntiJamming,
}

impl Display for ControlId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &'static str = self.into();

        write!(f, "{}", s)
    }
}

impl ControlId {
    pub fn from_u8(value: u8) -> ControlId {
        FromPrimitive::from_u8(value).unwrap()
    }

    pub fn parse_str(s: Cow<'_, str>) -> Result<ControlId, RadarError> {
        // Numeric discriminant encoded as string
        if let Ok(num) = s.parse::<u8>() {
            return match FromPrimitive::from_u8(num) {
                Some(ct) => Ok(ct),
                None => Err(RadarError::InvalidControlId(
                    "invalid ControlId discriminant".to_string(),
                )),
            };
        }

        // Case-insensitive name lookup (Strum)
        ControlId::from_str(&s)
            .map_err(|_| RadarError::InvalidControlId("invalid ControlId discriminant".to_string()))
    }

    pub fn get_category(&self) -> Category {
        match self {
            ControlId::Power
            | ControlId::BirdMode
            | ControlId::Range
            | ControlId::Mode
            | ControlId::Gain
            | ControlId::ColorGain
            | ControlId::Sea
            | ControlId::SeaState
            | ControlId::Rain
            | ControlId::Doppler
            | ControlId::DopplerMode => Category::Base,
            ControlId::ShowAis
            | ControlId::DopplerAutoTrack
            | ControlId::ArpaDetectMaxSpeed
            | ControlId::ClearTargets
            | ControlId::GuardZone1
            | ControlId::GuardZone2 => Category::Targets,
            ControlId::DopplerTrailsOnly
            | ControlId::TargetTrails
            | ControlId::ClearTrails
            | ControlId::TrailsMotion => Category::Trails,
            ControlId::AccentLight
            | ControlId::AntennaForward
            | ControlId::AntennaHeight
            | ControlId::AntennaStarboard
            | ControlId::BearingAlignment
            | ControlId::NoTransmitSector1
            | ControlId::NoTransmitSector2
            | ControlId::NoTransmitSector3
            | ControlId::NoTransmitSector4
            | ControlId::ExclusionZone1
            | ControlId::ExclusionZone2
            | ControlId::ExclusionZone3
            | ControlId::ExclusionZone4
            | ControlId::ExclusionRect1
            | ControlId::ExclusionRect2
            | ControlId::ExclusionRect3
            | ControlId::ExclusionRect4
            | ControlId::RangeUnits => Category::Installation,
            ControlId::UserName
            | ControlId::ModelName
            | ControlId::WarmupTime
            | ControlId::FirmwareVersion
            | ControlId::OperatingTime
            | ControlId::TransmitTime
            | ControlId::MagnetronCurrent
            | ControlId::RotationSpeed
            | ControlId::SerialNumber
            | ControlId::SignalStrength
            | ControlId::Spokes
            | ControlId::SpokeLength
            | ControlId::PulseWidth => Category::Info,
            ControlId::SupplyVoltage | ControlId::DeviceTemperature => Category::Info,
            ControlId::ScanAverageMode | ControlId::ScanAverageSensitivity => Category::Advanced,
            ControlId::ParkPosition => Category::Installation,
            ControlId::TransmitChannel => Category::Advanced,
            ControlId::NoiseRejection
            | ControlId::TargetBoost
            | ControlId::TargetExpansion
            | ControlId::InterferenceRejection
            | ControlId::TargetSeparation
            | ControlId::LocalInterferenceRejection
            | ControlId::ScanSpeed
            | ControlId::SideLobeSuppression
            | ControlId::Tune
            | ControlId::Ftc
            | ControlId::MainBangSuppression
            | ControlId::SeaClutterCurve
            | ControlId::DisplayTiming
            | ControlId::SpokeProcessing
            | ControlId::DopplerSpeedThreshold
            | ControlId::TimedIdle
            | ControlId::TimedRun
            | ControlId::NearStcCurve
            | ControlId::MiddleStcCurve
            | ControlId::FarStcCurve
            | ControlId::StcRange
            | ControlId::AntiJamming => Category::Advanced,
        }
    }

    pub fn get_description(&self) -> &'static str {
        match self {
            ControlId::Power => "Radar operational state",
            ControlId::WarmupTime => {
                "How long the radar still needs to warm up before transmitting"
            }
            ControlId::Range => "Maximum distance the radar is looking at",
            ControlId::Mode => "Choice of radar mode tuning to certain conditions, or custom",
            ControlId::Gain => "How sensitive the radar is to returning echoes",
            ControlId::ColorGain => "Adjust the color curve relative to gain",
            ControlId::Sea => "Sea clutter suppression",
            ControlId::SeaState => "Sea state for sea clutter suppression",
            ControlId::Rain => "Rain clutter suppression",
            ControlId::Doppler => {
                "Targets coming towards or going away from own ship shown in different colors"
            }
            ControlId::DopplerMode => "For what type of targets Doppler is used",
            ControlId::ShowAis => "Show AIS vessels on the radar display",
            ControlId::DopplerAutoTrack => {
                "Convert all Doppler targets to ARPA targets automatically"
            }
            ControlId::ArpaDetectMaxSpeed => {
                "Maximum target speed: Normal (25kn), Medium (40kn), Fast (50kn)"
            }
            ControlId::ClearTargets => "Clear all ARPA targets",
            ControlId::GuardZone1 => "First guard zone for target detection",
            ControlId::GuardZone2 => "Second guard zone for target detection",
            ControlId::ExclusionZone1 => "First exclusion zone (stationary only)",
            ControlId::ExclusionZone2 => "Second exclusion zone (stationary only)",
            ControlId::ExclusionZone3 => "Third exclusion zone (stationary only)",
            ControlId::ExclusionZone4 => "Fourth exclusion zone (stationary only)",
            ControlId::ExclusionRect1 => "First rectangular exclusion zone (stationary only)",
            ControlId::ExclusionRect2 => "Second rectangular exclusion zone (stationary only)",
            ControlId::ExclusionRect3 => "Third rectangular exclusion zone (stationary only)",
            ControlId::ExclusionRect4 => "Fourth rectangular exclusion zone (stationary only)",
            ControlId::TargetTrails => "Whether target trails are shown",
            ControlId::TrailsMotion => "How target trails behave",
            ControlId::DopplerTrailsOnly => "Convert only Doppler targets to target trails",
            ControlId::ClearTrails => "Clear target trails",
            ControlId::DopplerSpeedThreshold => "Threshold speed above which Doppler is applied",
            ControlId::NoiseRejection => "Filters out noise",
            ControlId::TargetBoost => "Level of how much small targets are boosted",
            ControlId::TargetExpansion => "Increases target length for small targets",
            ControlId::InterferenceRejection => "Reduces interference from other radars",
            ControlId::TargetSeparation => "Makes separation between targets more prominent",
            ControlId::LocalInterferenceRejection => {
                "How much local interference rejection is applied"
            }
            ControlId::BirdMode => "Level of optimization for bird targets",
            ControlId::ScanSpeed => "Desired rotation speed of the radar antenna",
            ControlId::SideLobeSuppression => "Level of side lobe suppression",
            ControlId::Tune => "Method to finely tune the radar receiver",
            ControlId::Ftc => "FTC",
            ControlId::RangeUnits => "Which unit system to use for range values",
            ControlId::MainBangSuppression => "Main bang suppression",
            ControlId::SeaClutterCurve => "Sea clutter curve",
            ControlId::DisplayTiming => "Display timing",
            ControlId::NoTransmitSector1 => "First no-transmit sector",
            ControlId::NoTransmitSector2 => "Second no-transmit sector",
            ControlId::NoTransmitSector3 => "Third no-transmit sector",
            ControlId::NoTransmitSector4 => "Fourth no-transmit sector",
            ControlId::AccentLight => "Strength of the accent light",
            ControlId::AntennaForward => "Antenna offset forward of GPS position",
            ControlId::AntennaHeight => "Height of the antenna above waterline",
            ControlId::AntennaStarboard => "Antenna offset starboard of GPS position",
            ControlId::BearingAlignment => "Alignment of the antenna relative to the vessel's bow",
            ControlId::RotationSpeed => "How quickly the radar antenna rotates",
            ControlId::MagnetronCurrent => "The current supplied to the magnetron",
            ControlId::SignalStrength => "Signal strength of the radar",
            ControlId::OperatingTime => "How long the radar has been operating over its lifetime",
            ControlId::TransmitTime => "How long the radar has been transmitting over its lifetime",
            ControlId::ModelName => "Manufacturer model name of the radar",
            ControlId::FirmwareVersion => "Version of the radar firmware",
            ControlId::SerialNumber => "Manufacturer serial number of the radar",
            ControlId::Spokes => "How many spokes the radar transmitted last rotation",
            ControlId::SpokeLength => {
                "How long the spokes are that the radar transmitted last rotation"
            }
            ControlId::SpokeProcessing => "How to process spoke data for display",
            ControlId::TimedIdle => "Periodically switch between transmit and standby",
            ControlId::TimedRun => "How long the radar transmits during timed idle",
            ControlId::PulseWidth => "Current transmit pulse width selected by the radar",
            ControlId::UserName => "User defined name for the radar",
            ControlId::SupplyVoltage => "DC supply voltage at the radar",
            ControlId::DeviceTemperature => "Internal temperature of the radar",
            ControlId::ScanAverageMode => "Scan-to-scan averaging reduces noise by comparing successive sweeps",
            ControlId::ScanAverageSensitivity => "Threshold for scan averaging filter",
            ControlId::ParkPosition => "Antenna park position when entering standby",
            ControlId::TransmitChannel => "Transmit frequency channel selection (auto or manual)",
            ControlId::NearStcCurve => "Near-range STC suppression curve",
            ControlId::MiddleStcCurve => "Middle-range STC suppression curve",
            ControlId::FarStcCurve => "Far-range STC suppression curve",
            ControlId::StcRange => "Distance boundary between STC range bands",
            ControlId::AntiJamming => "Anti-jamming filter reduces interference from other radars on the same frequency",
        }
    }

    fn get_name(&self) -> &'static str {
        match self {
            ControlId::AccentLight => "Accent light",
            ControlId::AntennaForward => "Antenna forward",
            ControlId::AntennaHeight => "Antenna height",
            ControlId::AntennaStarboard => "Antenna starboard",
            ControlId::BearingAlignment => "Bearing alignment",
            // ControlId::ColorGain => "Color gain",
            ControlId::ShowAis => "Show AIS",
            ControlId::ClearTargets => "Clear targets",
            ControlId::ClearTrails => "Clear trails",
            ControlId::ColorGain => "Color gain",
            ControlId::DisplayTiming => "Display timing",
            ControlId::Doppler => "Doppler",
            ControlId::DopplerMode => "Doppler mode",
            ControlId::DopplerAutoTrack => "Doppler Auto Track",
            ControlId::ArpaDetectMaxSpeed => "ARPA Max Speed",
            ControlId::DopplerSpeedThreshold => "Doppler speed threshold",
            ControlId::DopplerTrailsOnly => "Doppler trails only",
            ControlId::FirmwareVersion => "Firmware version",
            ControlId::Ftc => "FTC",
            ControlId::Gain => "Gain",
            ControlId::GuardZone1 => "Guard zone",
            ControlId::GuardZone2 => "Guard zone (2)",
            ControlId::ExclusionZone1 => "Exclusion zone",
            ControlId::ExclusionZone2 => "Exclusion zone (2)",
            ControlId::ExclusionZone3 => "Exclusion zone (3)",
            ControlId::ExclusionZone4 => "Exclusion zone (4)",
            ControlId::ExclusionRect1 => "Exclusion rect",
            ControlId::ExclusionRect2 => "Exclusion rect (2)",
            ControlId::ExclusionRect3 => "Exclusion rect (3)",
            ControlId::ExclusionRect4 => "Exclusion rect (4)",
            ControlId::InterferenceRejection => "Interference rejection",
            ControlId::LocalInterferenceRejection => "Local interference rejection",
            ControlId::BirdMode => "Bird mode",
            // ControlId::MainBangSize => "Main bang size",
            ControlId::MagnetronCurrent => "Magnetron current",
            ControlId::MainBangSuppression => "Main bang suppression",
            ControlId::Mode => "Mode",
            ControlId::ModelName => "Model name",
            ControlId::NoTransmitSector1 => "No Transmit sector",
            ControlId::NoTransmitSector2 => "No Transmit sector (2)",
            ControlId::NoTransmitSector3 => "No Transmit sector (3)",
            ControlId::NoTransmitSector4 => "No Transmit sector (4)",
            ControlId::NoiseRejection => "Noise rejection",
            ControlId::OperatingTime => "Operating time",
            ControlId::TransmitTime => "Transmit time",
            // ControlId::Orientation => "Orientation",
            ControlId::Rain => "Rain clutter",
            ControlId::Range => "Range",
            ControlId::RotationSpeed => "Rotation speed",
            // ControlId::Scaling => "Scaling",
            ControlId::ScanSpeed => "Scan speed",
            ControlId::Sea => "Sea clutter",
            ControlId::SeaClutterCurve => "Sea clutter curve",
            ControlId::SeaState => "Sea state",
            ControlId::SerialNumber => "Serial Number",
            ControlId::SideLobeSuppression => "Side lobe suppression",
            // ControlId::Stc => "Sensitivity Time Control",
            // ControlId::StcCurve => "STC curve",
            ControlId::SignalStrength => "Signal strength",
            ControlId::Power => "Power",
            ControlId::TargetBoost => "Target boost",
            ControlId::TargetExpansion => "Target expansion",
            ControlId::TargetSeparation => "Target separation",
            ControlId::TargetTrails => "Target trails",
            ControlId::TimedIdle => "Timed idle",
            ControlId::TimedRun => "Timed idle transmit",
            // ControlId::TimedRun => "Timed run",
            ControlId::TrailsMotion => "Target trails motion",
            ControlId::Tune => "Tune",
            // ControlId::TuneFine => "Fine tune",
            ControlId::Spokes => "Spokes",
            ControlId::SpokeLength => "Spoke length",
            ControlId::SpokeProcessing => "Spoke Processing",
            ControlId::PulseWidth => "Pulse width",
            ControlId::RangeUnits => "Range Units",
            ControlId::UserName => "Custom name",
            ControlId::WarmupTime => "Warmup time",
            ControlId::SupplyVoltage => "Supply voltage",
            ControlId::DeviceTemperature => "Device temperature",
            ControlId::ScanAverageMode => "Scan average",
            ControlId::ScanAverageSensitivity => "Scan average sensitivity",
            ControlId::ParkPosition => "Park position",
            ControlId::TransmitChannel => "Transmit channel",
            ControlId::NearStcCurve => "Near STC curve",
            ControlId::MiddleStcCurve => "Middle STC curve",
            ControlId::FarStcCurve => "Far STC curve",
            ControlId::StcRange => "STC range",
            ControlId::AntiJamming => "Anti-jamming",
        }
    }

    pub(crate) fn get_destination(&self) -> ControlDestination {
        match self {
            ControlId::AccentLight => ControlDestination::Command,
            ControlId::AntennaHeight => ControlDestination::Command,
            ControlId::BearingAlignment => ControlDestination::Command,
            ControlId::BirdMode => ControlDestination::Command,
            ControlId::ClearTargets => ControlDestination::Target,
            ControlId::ClearTrails => ControlDestination::Trail,
            ControlId::ColorGain => ControlDestination::Command,
            ControlId::DisplayTiming => ControlDestination::Command,
            ControlId::Power => ControlDestination::Command,
            ControlId::WarmupTime => ControlDestination::ReadOnly,
            ControlId::Range => ControlDestination::Command,
            ControlId::Mode => ControlDestination::Command,
            ControlId::Gain => ControlDestination::Command,
            ControlId::ShowAis => ControlDestination::Internal,
            ControlId::GuardZone1 => ControlDestination::Target,
            ControlId::GuardZone2 => ControlDestination::Target,
            ControlId::ExclusionZone1 => ControlDestination::Target,
            ControlId::ExclusionZone2 => ControlDestination::Target,
            ControlId::ExclusionZone3 => ControlDestination::Target,
            ControlId::ExclusionZone4 => ControlDestination::Target,
            ControlId::ExclusionRect1 => ControlDestination::Target,
            ControlId::ExclusionRect2 => ControlDestination::Target,
            ControlId::ExclusionRect3 => ControlDestination::Target,
            ControlId::ExclusionRect4 => ControlDestination::Target,
            ControlId::Sea => ControlDestination::Command,
            ControlId::SeaState => ControlDestination::Command,
            ControlId::Rain => ControlDestination::Command,
            ControlId::Doppler => ControlDestination::Command,
            ControlId::DopplerMode => ControlDestination::Command,
            ControlId::DopplerAutoTrack => ControlDestination::Target,
            ControlId::ArpaDetectMaxSpeed => ControlDestination::Target,
            ControlId::DopplerSpeedThreshold => ControlDestination::Command,
            ControlId::TargetTrails => ControlDestination::Trail,
            ControlId::TrailsMotion => ControlDestination::Trail,
            ControlId::DopplerTrailsOnly => ControlDestination::Trail,
            ControlId::NoiseRejection => ControlDestination::Command,
            ControlId::TargetBoost => ControlDestination::Command,
            ControlId::TargetExpansion => ControlDestination::Command,
            ControlId::InterferenceRejection => ControlDestination::Command,
            ControlId::TargetSeparation => ControlDestination::Command,
            ControlId::LocalInterferenceRejection => ControlDestination::Command,
            ControlId::ScanSpeed => ControlDestination::Command,
            ControlId::SideLobeSuppression => ControlDestination::Command,
            ControlId::Tune => ControlDestination::Command,
            ControlId::Ftc => ControlDestination::Command,
            ControlId::MainBangSuppression => ControlDestination::Command,
            ControlId::SeaClutterCurve => ControlDestination::Command,
            ControlId::NoTransmitSector1 => ControlDestination::Command,
            ControlId::NoTransmitSector2 => ControlDestination::Command,
            ControlId::NoTransmitSector3 => ControlDestination::Command,
            ControlId::NoTransmitSector4 => ControlDestination::Command,
            ControlId::RotationSpeed => ControlDestination::Command,
            ControlId::MagnetronCurrent => ControlDestination::Command,
            ControlId::SignalStrength => ControlDestination::Command,
            ControlId::AntennaForward => ControlDestination::Command,
            ControlId::AntennaStarboard => ControlDestination::Command,
            ControlId::OperatingTime => ControlDestination::ReadOnly,
            ControlId::TransmitTime => ControlDestination::ReadOnly,
            ControlId::ModelName => ControlDestination::ReadOnly,
            ControlId::FirmwareVersion => ControlDestination::ReadOnly,
            ControlId::SerialNumber => ControlDestination::ReadOnly,
            ControlId::Spokes => ControlDestination::ReadOnly,
            ControlId::SpokeLength => ControlDestination::ReadOnly,
            ControlId::SpokeProcessing => ControlDestination::Internal,
            ControlId::PulseWidth => ControlDestination::ReadOnly,
            ControlId::TimedIdle => ControlDestination::Command,
            ControlId::TimedRun => ControlDestination::Command,
            ControlId::RangeUnits => ControlDestination::Command,
            ControlId::UserName => ControlDestination::Internal,
            ControlId::SupplyVoltage => ControlDestination::ReadOnly,
            ControlId::DeviceTemperature => ControlDestination::ReadOnly,
            ControlId::ScanAverageMode => ControlDestination::Command,
            ControlId::ScanAverageSensitivity => ControlDestination::Command,
            ControlId::ParkPosition => ControlDestination::Command,
            ControlId::TransmitChannel => ControlDestination::Command,
            ControlId::NearStcCurve => ControlDestination::Command,
            ControlId::MiddleStcCurve => ControlDestination::Command,
            ControlId::FarStcCurve => ControlDestination::Command,
            ControlId::StcRange => ControlDestination::Command,
            ControlId::AntiJamming => ControlDestination::Command,
        }
    }
}
///
/// Radars have settings. There are some common ones that every radar supports:
/// range, gain, sea clutter and rain clutter. Some others are less common, and
/// are usually expressed in slightly different ways.
/// For instance, a radar may have an interference rejection setting. Some will
/// have two possible values (off or on) whilst others may have multiple levels,
/// like off, low, medium and high.
///
/// To cater for this, we keep the state of these settings in generalized state
/// structures in Rust.
///
/// Per radar we keep a single Controls structure in memory that is
/// accessed from all threads that are working for that radar and any user
/// clients.
///
/// If you've got a reference to the controls object for a radar, you can
/// subscribe to any changes made to it.
///

#[derive(Debug, Serialize)]
pub struct Controls {
    #[serde(skip)]
    replay: bool,

    #[serde(flatten)]
    controls: HashMap<ControlId, Control>,

    #[serde(skip)]
    radar_id: String, // A copy of the radar's key() value
    #[serde(skip)]
    all_ranges: Ranges, // All supported ranges, used for filtering by RangeUnits
    #[serde(skip)]
    default_range_units: i32, // Default range units (0=Metric, 1=Nautical, 2=Mixed) when no control exists
    #[serde(skip)]
    all_clients_tx: tokio::sync::broadcast::Sender<ControlValue>,
    #[serde(skip)]
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    #[serde(skip)]
    control_update_tx: tokio::sync::broadcast::Sender<ControlUpdate>,
}

impl Controls {
    fn insert(&mut self, control_id: ControlId, value: Control) {
        let v = Control {
            item: ControlDefinition {
                is_read_only: self.replay || value.item.is_read_only,
                ..value.item
            },
            ..value
        };

        self.controls.insert(control_id, v);
    }

    pub(self) fn new_base(
        radar_id: String,
        sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
        args: &Cli,
        mut controls: HashMap<ControlId, Control>,
    ) -> Self {
        // Add _mandatory_ controls
        if !controls.contains_key(&ControlId::ModelName) {
            new_string(ControlId::ModelName)
                .read_only(true)
                .build(&mut controls);
        }

        // Note: valid range values are set per-model in update_when_model_known()
        let max_value = 120. * NAUTICAL_MILE as f64;
        new_numeric(ControlId::Range, 0., max_value)
            .wire_units(Units::Meters)
            .build(&mut controls);

        new_numeric(ControlId::Spokes, 0., 9999.)
            .read_only(true)
            .build(&mut controls);
        new_numeric(ControlId::SpokeLength, 0., 9999.)
            .read_only(true)
            .build(&mut controls);

        if args.replay {
            controls.iter_mut().for_each(|(_k, v)| {
                if v.item.control_id.get_destination() != ControlDestination::Target {
                    v.item.is_read_only = true;
                }
            });
        }

        // Add controls that are not radar dependent

        if !controls.contains_key(&ControlId::UserName) {
            new_string(ControlId::UserName)
                .read_only(false)
                .build(&mut controls);
        }

        new_list(
            ControlId::SpokeProcessing,
            &["Clean", "Fill", "Reduce", "Smooth"],
        )
        .build(&mut controls);

        if args.targets != TargetMode::None {
            new_map(
                ControlId::TargetTrails,
                HashMap::from([
                    (0, "Off".to_string()),
                    (1, "15s".to_string()),
                    (2, "30s".to_string()),
                    (3, "1 min".to_string()),
                    (4, "3 min".to_string()),
                    (5, "5 min".to_string()),
                    (6, "10 min".to_string()),
                ]),
            )
            .build(&mut controls);

            new_map(
                ControlId::TrailsMotion,
                HashMap::from([(0, "Relative".to_string()), (1, "True".to_string())]),
            )
            .build(&mut controls);

            new_button(ControlId::ClearTrails).build(&mut controls);

            new_list(ControlId::ArpaDetectMaxSpeed, &["Normal", "Medium", "Fast"])
                .build(&mut controls);

            new_button(ControlId::ClearTargets).build(&mut controls);
        }

        // Add ShowAis control when pass_ais is enabled
        if args.pass_ais {
            new_list(ControlId::ShowAis, &["Off", "On"])
                .set_value(1.) // Default to "On"
                .build(&mut controls);
        }

        let (all_clients_tx, _) = tokio::sync::broadcast::channel(32);
        let (control_update_tx, _) = tokio::sync::broadcast::channel(32);

        Controls {
            replay: args.replay,
            controls,
            radar_id,
            all_ranges: Ranges::empty(),
            default_range_units: 0, // Nautical (default) - can be overridden per brand
            all_clients_tx,
            sk_client_tx,
            control_update_tx,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SharedControls {
    #[serde(flatten, with = "arc_rwlock_serde")]
    controls: Arc<RwLock<Controls>>,
}

mod arc_rwlock_serde {
    use serde::de::Deserializer;
    use serde::ser::Serializer;
    use serde::{Deserialize, Serialize};
    use std::sync::{Arc, RwLock};

    pub fn serialize<S, T>(val: &Arc<RwLock<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        T::serialize(&*val.read().unwrap(), s)
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D, T>(d: D) -> Result<Arc<RwLock<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Arc::new(RwLock::new(T::deserialize(d)?)))
    }
}

impl SharedControls {
    // Create a new set of controls, for a radar.
    // There is only one set that is shared amongst the various threads and
    // structs, hence the word Shared.
    pub(crate) fn new(
        radar_id: String,
        sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
        args: &Cli,
        mut controls: HashMap<ControlId, Control>,
    ) -> SharedControls {
        // All radars must have the same Status control
        new_list(
            ControlId::Power,
            &["Off", "Standby", "Transmit", "Preparing"],
        )
        //.send_always()
        .set_valid_values([1, 2].to_vec())
        .build(&mut controls); // Only allow setting to Standby (index 1) and Transmit (index 2)

        // Guard zones - generic controls for all radars
        new_zone(ControlId::GuardZone1, -180., 180., 100000.)
            .wire_units(Units::Degrees)
            .build(&mut controls);
        new_zone(ControlId::GuardZone2, -180., 180., 100000.)
            .wire_units(Units::Degrees)
            .build(&mut controls);

        // Exclusion zones - only for stationary installations
        if args.stationary {
            new_zone(ControlId::ExclusionZone1, -180., 180., 100000.)
                .wire_units(Units::Degrees)
                .build(&mut controls);
            new_zone(ControlId::ExclusionZone2, -180., 180., 100000.)
                .wire_units(Units::Degrees)
                .build(&mut controls);
            new_zone(ControlId::ExclusionZone3, -180., 180., 100000.)
                .wire_units(Units::Degrees)
                .build(&mut controls);
            new_zone(ControlId::ExclusionZone4, -180., 180., 100000.)
                .wire_units(Units::Degrees)
                .build(&mut controls);

            // Rectangular exclusion zones - only for stationary installations
            new_rect(ControlId::ExclusionRect1, 100000.)
                .wire_units(Units::Meters)
                .build(&mut controls);
            new_rect(ControlId::ExclusionRect2, 100000.)
                .wire_units(Units::Meters)
                .build(&mut controls);
            new_rect(ControlId::ExclusionRect3, 100000.)
                .wire_units(Units::Meters)
                .build(&mut controls);
            new_rect(ControlId::ExclusionRect4, 100000.)
                .wire_units(Units::Meters)
                .build(&mut controls);
        }

        SharedControls {
            controls: Arc::new(RwLock::new(Controls::new_base(
                radar_id,
                sk_client_tx,
                args,
                controls,
            ))),
        }
    }

    pub(crate) fn add(&mut self, control_builder: ControlBuilder) {
        let (id, control) = control_builder.take();
        self.controls.write().unwrap().controls.insert(id, control);
    }

    /// Replace a control's definition (auto flags, min/max, etc.) while
    /// preserving any runtime state (value, auto, enabled, …) that has
    /// already been set. Falls back to a plain `add` when the control
    /// does not yet exist.
    pub(crate) fn update_definition(&mut self, control_builder: ControlBuilder) {
        let (id, new_control) = control_builder.take();
        let mut locked = self.controls.write().unwrap();
        if let Some(existing) = locked.controls.get_mut(&id) {
            let was_read_only = existing.item.is_read_only;
            existing.item = new_control.item;
            existing.item.is_read_only |= was_read_only;
        } else {
            locked.controls.insert(id, new_control);
        }
    }

    fn get_command_tx(&self) -> tokio::sync::broadcast::Sender<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.clone()
    }

    pub fn get_controls(&self) -> HashMap<ControlId, Control> {
        let locked = self.controls.read().unwrap();

        locked.controls.clone()
    }

    pub(crate) fn new_client_subscription(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        let locked = self.controls.read().unwrap();

        locked.all_clients_tx.subscribe()
    }

    fn normalize_value(value: &Value, control: &Control) -> Result<Value, RadarError> {
        match value {
            // Primitive types are returned unchanged
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),

            Value::String(s) => {
                if let Ok(num) = s.parse::<f64>() {
                    // `serde_json::Number` can hold both ints and floats.
                    // We create a Number from the parsed f64.
                    let n = match Number::from_f64(num) {
                        Some(n) => n,
                        None => {
                            return Err(RadarError::CannotSetControlIdValue(
                                control.item.control_id,
                                value.clone(),
                            ));
                        }
                    };
                    return Ok(Value::Number(n));
                }

                if let Some(descriptions) = &control.item.descriptions {
                    if let Some(idx) = descriptions
                        .iter()
                        .position(|(_, d)| d.eq_ignore_ascii_case(&s))
                    {
                        return Ok(Value::Number(Number::from(idx)));
                    }
                }

                // 3. If no match, keep the string as it is.
                Ok(value.clone())
            }

            // For arrays we recurse into each element
            Value::Array(_) | Value::Object(_) => {
                return Err(RadarError::CannotSetControlIdValue(
                    control.item.control_id,
                    value.clone(),
                ));
            }
        }
    }

    //
    // Convert to number, possibly to SI if the control_value contains a user unit.
    // If the control is an Enum style, also convert to number.
    // If it is a string or button, leave it alone.
    //
    fn convert_to_wire_number(
        cv_units: Option<Units>,
        cv_value: Option<Value>,
        control: &Control,
    ) -> Result<(Option<Units>, Option<Value>), RadarError> {
        if control.item.data_type == ControlDataType::Button
            || control.item.data_type == ControlDataType::String
        {
            return Ok((None, cv_value));
        }

        if let Some(mut value) = cv_value {
            // convert boolean and enums into numbers
            value = Self::normalize_value(&value, control)?;

            let mut value_float = match value.as_f64() {
                None => {
                    return Err(RadarError::NotNumeric(control.item.control_id, value));
                }
                Some(v) => v,
            };

            let mut units: Units = Units::None;
            if let Some(cv_units) = cv_units {
                (units, value_float) = cv_units.to_si(value_float);
            }
            if let Some(wire_units) = control.item.wire_units {
                value_float = wire_units.from_si(value_float);
                units = wire_units;
            }
            let value_number = serde_json::Number::from_f64(value_float);
            if value_number.is_none() {
                return Err(RadarError::NotNumeric(control.item.control_id, value));
            }
            let units = match units {
                Units::None => None,
                v => Some(v),
            };
            let value = Some(Value::Number(value_number.unwrap()));
            return Ok((units, value));
        }

        Ok((cv_units, cv_value))
    }

    fn convert_f64_to_wire(
        cv_units: Option<Units>,
        cv_value: Option<f64>,
        control: &Control,
    ) -> Result<Option<f64>, RadarError> {
        let value_as_value = cv_value
            .and_then(|v| Number::from_f64(v))
            .map(Value::Number);
        let (_, result) = Self::convert_to_wire_number(cv_units, value_as_value, control)?;
        Ok(result.and_then(|v| v.as_f64()))
    }

    // process_client_request()
    //
    // In theory this could be from anywhere that somebody holds a SharedControls reference,
    // but in practice only called from the websocket request handler in web.rs.
    // The end user has sent a control update and we need to process this.
    //
    // Some controls are handled internally, some in the data handler for a radar and the
    // rest are settings that need to be sent to the radar.
    //
    pub fn process_client_request(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        match self.get(&control_value.id) {
            Some(c) => {
                let cv_orig = control_value.clone();
                let (units, value) =
                    Self::convert_to_wire_number(control_value.units, control_value.value, &c)?;
                let end_value =
                    Self::convert_f64_to_wire(control_value.units, control_value.end_value, &c)?;
                let auto_value =
                    Self::convert_f64_to_wire(control_value.units, control_value.auto_value, &c)?;
                let cv = ControlValue {
                    units,
                    value,
                    auto_value,
                    end_value,
                    ..control_value
                };
                log::info!(
                    "Client request to update {:?} to {:?} wire {:?}",
                    ControlValue::from(&c, None),
                    cv_orig,
                    cv
                );
                // Handle zone controls specially - they have multiple values and are stored directly
                // This applies to both Internal and Target destinations (guard zones are Target)
                if c.item.data_type == ControlDataType::Zone {
                    let start_angle = cv.value.as_ref().and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let end_angle = cv.end_value.unwrap_or(0.0);
                    let start_distance = cv.start_distance.unwrap_or(0.0);
                    let end_distance = cv.end_distance.unwrap_or(0.0);
                    self.set_zone(
                        &cv.id,
                        start_angle,
                        end_angle,
                        start_distance,
                        end_distance,
                        cv.enabled,
                    )
                    .map_err(|e| RadarError::ControlError(e))?;
                    // Broadcast the update to the radar so blob detector gets updated
                    return self.send_to_command_handler(cv, reply_tx);
                }

                // Handle rect controls specially - they have corner-based coordinates
                if c.item.data_type == ControlDataType::Rect {
                    let x1 = cv.x1.unwrap_or(0.0);
                    let y1 = cv.y1.unwrap_or(0.0);
                    let x2 = cv.x2.unwrap_or(0.0);
                    let y2 = cv.y2.unwrap_or(0.0);
                    let width = cv.width.unwrap_or(0.0);
                    self.set_rect(&cv.id, x1, y1, x2, y2, width, cv.enabled)
                        .map_err(|e| RadarError::ControlError(e))?;
                    // Broadcast the update to the radar so exclusion mask gets updated
                    return self.send_to_command_handler(cv, reply_tx);
                }

                match cv.id.get_destination() {
                    ControlDestination::Internal => {
                        if let Some(value) = cv.value {
                            self.set_value(&cv.id, value)
                                .map(|_| ())
                                .map_err(|e| RadarError::ControlError(e))
                        } else {
                            Err(RadarError::CannotSetControlId(cv.id))
                        }
                    }
                    ControlDestination::Command
                    | ControlDestination::Target
                    | ControlDestination::Trail => self.send_to_command_handler(cv, reply_tx),
                    ControlDestination::ReadOnly => Err(RadarError::CannotSetControlId(cv.id)),
                }
            }
            None => Err(RadarError::CannotSetControlId(control_value.id)),
        }
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.subscribe()
    }

    pub fn get_radar_control_values(&self) -> Vec<RadarControlValue> {
        let locked = self.controls.read().unwrap();

        locked
            .controls
            .iter()
            .filter(|(_, c)| c.item.data_type != ControlDataType::Button)
            .map(|(_, c)| RadarControlValue::new(&locked.radar_id, c, None))
            .collect()
    }

    pub async fn send_all_controls(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let controls: Vec<Control> = {
            let locked = self.controls.read().unwrap();

            locked
                .controls
                .clone()
                .into_values()
                .filter(|c| c.item.data_type != ControlDataType::Button)
                .collect()
        };

        for c in controls {
            self.send_reply_to_client(reply_tx.clone(), &c, None)
                .await?;
        }
        Ok(())
    }

    pub fn send_to_command_handler(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let control_update = ControlUpdate {
            control_value,
            reply_tx,
        };
        self.get_command_tx()
            .send(control_update)
            .map(|_| ())
            .map_err(|_| RadarError::Shutdown)
    }

    fn send_to_all_clients(&self, control: &Control) {
        if control.item.control_id == ControlId::RangeUnits {
            self.update_valid_ranges();
        }
        let control_value = ControlValue::from(control, None);
        let locked = self.controls.read().unwrap();
        match locked.all_clients_tx.send(control_value.clone()) {
            Err(_e) => {}
            Ok(cnt) => {
                log::trace!(
                    "Sent control value {} to {} JSON clients",
                    control.item().control_id,
                    cnt
                );
            }
        }
        let radar_control_value = RadarControlValue::new(&locked.radar_id, control, None);

        log::debug!("Sending {:?} to SignalK", radar_control_value);
        let mut sk_delta = SignalKDelta::new();
        if control.item.control_id == ControlId::RangeUnits {
            let range_control = locked
                .controls
                .get(&ControlId::Range)
                .expect("Range should always be set");
            sk_delta.add_meta_for_control(&locked.radar_id, &range_control);
            log::debug!("meta: {:?}", sk_delta);
        }
        sk_delta.add_updates(vec![radar_control_value]);
        match locked.sk_client_tx.send(sk_delta.build().unwrap()) {
            Err(_e) => {}
            Ok(cnt) => {
                log::debug!(
                    "Sent control value {} to {} SK clients",
                    control.item().control_id,
                    cnt
                );
            }
        }
    }

    pub async fn send_reply_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        control: &Control,
        error: Option<String>,
    ) -> Result<(), RadarError> {
        let control_value = ControlValue::from(control, error);

        log::debug!(
            "Sending reply {:?} to requesting JSON client",
            &control_value,
        );

        reply_tx
            .send(control_value)
            .await
            .map_err(|_| RadarError::Shutdown)
    }

    pub async fn send_error_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        cv: &ControlValue,
        e: &RadarError,
    ) -> Result<(), RadarError> {
        if let Some(control) = self.get(&cv.id) {
            self.send_reply_to_client(reply_tx, &control, Some(e.to_string()))
                .await?;
            log::warn!("User tried to set invalid {}: {}", cv.id, e);
            Ok(())
        } else {
            Err(RadarError::CannotSetControlId(cv.id))
        }
    }

    // ******* GET & SET METHODS

    pub fn insert(&self, control_id: ControlId, value: Control) {
        let mut locked = self.controls.write().unwrap();

        locked.insert(control_id, value);
    }

    pub fn get(&self, control_id: &ControlId) -> Option<Control> {
        let locked = self.controls.read().unwrap();

        locked.controls.get(control_id).cloned()
    }

    pub fn get_by_id(&self, control_id: &str) -> Option<Control> {
        match ControlId::parse_str(Cow::Borrowed(control_id)) {
            Ok(cv) => self.get(&cv),
            Err(_) => None,
        }
    }

    pub fn get_control_keys(&self) -> Vec<&'static str> {
        let locked = self.controls.read().unwrap();

        locked.controls.iter().map(|(k, _)| k.into()).collect()
    }

    pub fn contains_key(&self, control_id: &ControlId) -> bool {
        let locked = self.controls.read().unwrap();

        locked.controls.contains_key(control_id)
    }

    pub fn set_refresh(&self, control_id: &ControlId) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.needs_refresh = true;
        }
    }

    pub fn set_allowed(&self, control_id: &ControlId, read_only: bool) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.set_allowed(read_only);
        }
    }

    pub fn set_value_auto_enabled<T>(
        &self,
        control_id: &ControlId,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError>
    where
        f64: From<T>,
    {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                control
                    .set(value.into(), None, auto, enabled)?
                    .map(|_| control.clone())
            } else {
                return Err(ControlError::NotSupported(*control_id));
            }
        };

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_wire_range(
        &self,
        control_id: &ControlId,
        min: f64,
        max: f64,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control.set_wire_range(min, max)?.map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    //
    // Set a control from a wire value, so apply all transformations
    // to convert it to a user visible value
    //
    pub fn set(
        &self,
        control_id: &ControlId,
        value: f64,
        auto: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set(value, None, auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_auto_state(&self, control_id: &ControlId, auto: bool) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.set_auto(auto);
        } else {
            return Err(ControlError::NotSupported(*control_id));
        };
        Ok(())
    }

    pub fn set_value_auto(
        &self,
        control_id: &ControlId,
        auto: bool,
        value: f64,
    ) -> Result<Option<()>, ControlError> {
        self.set(control_id, value, Some(auto))
    }

    pub fn set_value_with_many_auto(
        &self,
        control_id: &ControlId,
        value: f64,
        auto_value: f64,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                let auto = control.auto;
                Ok(control
                    .set(value, Some(auto_value), auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(
        &self,
        control_id: &ControlId,
        value: String,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                if control.item().data_type == ControlDataType::String {
                    Ok(control.set_string(value).map(|_| control.clone()))
                } else {
                    let i = value
                        .parse::<i32>()
                        .map_err(|_| ControlError::Invalid(control_id.clone(), value))?;
                    Ok(control
                        .set(i as f64, None, None, None)?
                        .map(|_| control.clone()))
                }
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    pub fn set_value(
        &self,
        control_id: &ControlId,
        value: Value,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                if control.item().data_type == ControlDataType::String {
                    match value {
                        Value::String(s) => Ok(control.set_string(s).map(|_| control.clone())),
                        _ => Err(ControlError::Invalid(*control_id, format!("{:?}", value))),
                    }
                } else {
                    let n = match value.clone() {
                        Value::String(s) => s.parse::<i32>().map(|i| i as f64).map_err(|_| {
                            ControlError::Invalid(control_id.clone(), format!("{:?}", value))
                        }),
                        Value::Bool(b) => Ok(b as i32 as f64),
                        Value::Number(n) => n.as_f64().ok_or_else(|| {
                            ControlError::Invalid(control_id.clone(), format!("{:?}", value))
                        }),
                        _ => Err(ControlError::Invalid(
                            control_id.clone(),
                            format!("{:?}", value),
                        )),
                    }?;
                    Ok(control
                        .set(n, None, None, None)?
                        .map(|_| control.clone()))
                }
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);

            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    /// Set a sector control with start (value) and end (end_value) angles
    pub fn set_sector(
        &self,
        control_id: &ControlId,
        start: f64,
        end: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set_sector(start, end, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Set a zone control with start/end angles and start/end distances
    pub fn set_zone(
        &self,
        control_id: &ControlId,
        start_angle: f64,
        end_angle: f64,
        start_distance: f64,
        end_distance: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set_zone(
                        start_angle,
                        end_angle,
                        start_distance,
                        end_distance,
                        enabled,
                    )?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Set a rectangular exclusion zone with corner-based coordinates
    pub fn set_rect(
        &self,
        control_id: &ControlId,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        width: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set_rect(x1, y1, x2, y2, width, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)]
    fn get_description(control: &Control) -> Option<String> {
        if let (Some(value), Some(descriptions)) = (control.value, &control.item().descriptions) {
            let value = value as i32;
            if value >= 0 && value < (descriptions.len() as i32) {
                return descriptions.get(&value).cloned();
            }
        }
        return None;
    }

    pub fn set_user_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut(&ControlId::UserName).unwrap();
        control.set_string(name);
    }

    pub fn user_name(&self) -> String {
        self.get(&ControlId::UserName)
            .and_then(|c| c.description)
            .unwrap()
    }

    pub fn set_spoke_processing(&self, value: i32) {
        let mut locked = self.controls.write().unwrap();
        let control = locked
            .controls
            .get_mut(&ControlId::SpokeProcessing)
            .unwrap();
        let _ = control.set(value as f64, None, None, None);
    }

    pub fn spoke_processing(&self) -> i32 {
        self.get(&ControlId::SpokeProcessing)
            .and_then(|c| c.value)
            .map(|v| v as i32)
            .unwrap_or(0)
    }

    /// Returns the current ARPA max speed setting: 0 = Normal (25kn), 1 = Medium (40kn), 2 = Fast (50kn)
    pub fn doppler_auto_track(&self) -> bool {
        self.get(&ControlId::DopplerAutoTrack)
            .and_then(|c| c.value)
            .map(|v| v != 0.0)
            .unwrap_or(false)
    }

    pub fn arpa_detect_max_speed(&self) -> i32 {
        self.get(&ControlId::ArpaDetectMaxSpeed)
            .and_then(|c| c.value)
            .map(|v| v as i32)
            .unwrap_or(0)
    }

    pub fn set_arpa_max_speed(&self, value: i32) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(&ControlId::ArpaDetectMaxSpeed) {
            let _ = control.set(value as f64, None, None, None);
        }
    }

    pub fn set_doppler_auto_track(&self, value: bool) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(&ControlId::DopplerAutoTrack) {
            let _ = control.set(if value { 1.0 } else { 0.0 }, None, None, None);
        }
    }

    pub fn set_range_units(&self, value: i32) {
        let mut locked = self.controls.write().unwrap();
        match locked.controls.get_mut(&ControlId::RangeUnits) {
            Some(control) => {
                let _ = control.set(value as f64, None, None, None);
            }
            None => {
                if value != locked.default_range_units {
                    panic!("Cannot set range_units on a radar that has no range units");
                }
            }
        }
    }

    pub fn range_units(&self) -> i32 {
        let locked = self.controls.read().unwrap();
        locked
            .controls
            .get(&ControlId::RangeUnits)
            .and_then(|c| c.value)
            .map(|v| v as i32)
            .unwrap_or(locked.default_range_units)
    }

    pub fn set_model_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut(&ControlId::ModelName).unwrap();
        control.set_string(name.clone());
    }

    pub fn model_name(&self) -> Option<String> {
        self.get(&ControlId::ModelName).and_then(|c| c.description)
    }

    pub fn guard_zone(&self, control_id: &ControlId) -> Option<GuardZone> {
        self.get(control_id).and_then(|c| {
            Some(GuardZone {
                start_angle: c.value?,
                end_angle: c.end_value?,
                start_distance: c.start_distance?,
                end_distance: c.end_distance?,
                enabled: c.enabled.unwrap_or(false),
            })
        })
    }

    pub fn set_guard_zone(&self, control_id: &ControlId, zone: &GuardZone) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.value = Some(zone.start_angle);
            control.end_value = Some(zone.end_angle);
            control.start_distance = Some(zone.start_distance);
            control.end_distance = Some(zone.end_distance);
            control.enabled = Some(zone.enabled);
            control.timestamp = Some(Utc::now());
        }
    }

    pub fn exclusion_zone(&self, control_id: &ControlId) -> Option<crate::config::ExclusionZone> {
        self.get(control_id).and_then(|c| {
            Some(crate::config::ExclusionZone {
                start_angle: c.value?,
                end_angle: c.end_value?,
                start_distance: c.start_distance?,
                end_distance: c.end_distance?,
                enabled: c.enabled.unwrap_or(false),
            })
        })
    }

    pub fn set_exclusion_zone(&self, control_id: &ControlId, zone: &crate::config::ExclusionZone) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.value = Some(zone.start_angle);
            control.end_value = Some(zone.end_angle);
            control.start_distance = Some(zone.start_distance);
            control.end_distance = Some(zone.end_distance);
            control.enabled = Some(zone.enabled);
            control.timestamp = Some(Utc::now());
        }
    }

    pub fn exclusion_rect(&self, control_id: &ControlId) -> Option<crate::config::ExclusionRect> {
        self.get(control_id).and_then(|c| {
            Some(crate::config::ExclusionRect {
                x1: c.x1?,
                y1: c.y1?,
                x2: c.x2?,
                y2: c.y2?,
                width: c.width?,
                enabled: c.enabled.unwrap_or(false),
            })
        })
    }

    pub fn set_exclusion_rect(&self, control_id: &ControlId, rect: &crate::config::ExclusionRect) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.x1 = Some(rect.x1);
            control.y1 = Some(rect.y1);
            control.x2 = Some(rect.x2);
            control.y2 = Some(rect.y2);
            control.width = Some(rect.width);
            control.enabled = Some(rect.enabled);
            control.timestamp = Some(Utc::now());
        }
    }

    pub(crate) fn set_valid_ranges(&self, ranges: &Ranges) {
        self.controls.write().unwrap().all_ranges = ranges.clone();

        self.update_valid_ranges();
    }

    pub(crate) fn update_valid_ranges(&self) {
        // read this before taking the lock as it also locks
        let range_units = self.range_units();

        let mut locked = self.controls.write().unwrap();
        let ranges = &locked.all_ranges;

        let filtered_ranges = match range_units {
            0 => &ranges.nautical, // Nautical (default)
            1 => &ranges.metric,   // Metric
            _ => &ranges.mixed,    // Mixed
        }
        .clone(); // to avoid borrow mut problem

        locked
            .controls
            .get_mut(&ControlId::Range)
            .expect("Range is mandatory control")
            .set_valid_ranges(&filtered_ranges);
    }

    pub(crate) fn get_status(&self) -> Option<Power> {
        let locked = self.controls.read().unwrap();
        if let Some(control) = locked.controls.get(&ControlId::Power) {
            return control
                .value()
                .map(|v| Power::from_value(&v).ok())
                .flatten();
        }

        None
    }
}

#[derive(Clone, Debug)]
pub struct ControlUpdate {
    pub reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    pub control_value: ControlValue,
}

// This is what we send back and forth internally between (web) clients and radar managers for v1
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    pub id: ControlId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing)]
    pub units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    // Rectangular exclusion zone fields (corner-based, meters from radar position)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x2: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y2: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Timestamp when the control value was last changed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl ControlValue {
    pub(crate) fn new(id: ControlId, value: Value) -> Self {
        ControlValue {
            id,
            value: Some(value),
            units: None,
            auto: None,
            auto_value: None,
            end_value: None,
            start_distance: None,
            end_distance: None,
            enabled: None,
            allowed: None,
            error: None,
            x1: None,
            y1: None,
            x2: None,
            y2: None,
            width: None,
            timestamp: None,
        }
    }

    pub fn from_request(id: ControlId, b: BareControlValue) -> Self {
        ControlValue {
            id,
            value: b.value,
            units: b.units,
            auto: b.auto,
            auto_value: b.auto_value,
            end_value: b.end_value,
            start_distance: b.start_distance,
            end_distance: b.end_distance,
            enabled: b.enabled,
            allowed: b.allowed,
            error: b.error,
            x1: b.x1,
            y1: b.y1,
            x2: b.x2,
            y2: b.y2,
            width: b.width,
            timestamp: b.timestamp,
        }
    }

    pub fn from(control: &Control, error: Option<String>) -> Self {
        ControlValue {
            id: control.item().control_id,
            value: control.value(),
            units: control.item().units.clone(),
            auto: control.auto,
            auto_value: control.auto_value(),
            end_value: control.end_value(),
            start_distance: control.start_distance(),
            end_distance: control.end_distance(),
            enabled: control.enabled,
            allowed: control.allowed,
            error,
            x1: control.x1,
            y1: control.y1,
            x2: control.x2,
            y2: control.y2,
            width: control.width,
            timestamp: control.timestamp,
        }
    }

    pub fn as_value(&self) -> Result<Value, RadarError> {
        match &self.value {
            None => Err(RadarError::CannotSetControlId(self.id)),
            Some(v) => Ok(v.clone()),
        }
    }

    pub fn as_bool(&self) -> Result<bool, RadarError> {
        self.as_i64().map(|n| n != 0)
    }

    pub fn as_i32(&self) -> Result<i32, RadarError> {
        self.as_i64()?
            .try_into()
            .map_err(|_| RadarError::CannotSetControlId(self.id))
    }

    pub fn as_i64(&self) -> Result<i64, RadarError> {
        let value = self.as_value()?;
        match value {
            Value::String(s) => {
                // TODO enum style
                s.parse::<i64>().map_err(|_| RadarError::EnumerationFailed)
            }
            Value::Bool(b) => Ok(if b { 1 } else { 0 }),
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .ok_or(RadarError::EnumerationFailed),
            _ => Err(RadarError::EnumerationFailed),
        }
        .map_err(|_| RadarError::CannotSetControlId(self.id))
    }

    pub fn as_f64(&self) -> Result<f64, RadarError> {
        if let Some(value) = &self.value {
            match value {
                Value::String(s) => {
                    // TODO enum style
                    s.parse::<f64>().map_err(|_| RadarError::EnumerationFailed)
                }
                Value::Bool(b) => Ok(if *b { 1. } else { 0. }),
                Value::Number(n) => n.as_f64().ok_or(RadarError::EnumerationFailed),
                _ => Err(RadarError::EnumerationFailed),
            }
            .map_err(|_| RadarError::CannotSetControlId(self.id))
        } else {
            Err(RadarError::CannotSetControlId(self.id))
        }
    }

    pub fn auto_as_f64(&self) -> Result<f64, RadarError> {
        self.auto_value
            .ok_or(RadarError::CannotSetControlId(self.id))
    }

    pub fn end_as_f64(&self) -> Result<f64, RadarError> {
        self.end_value
            .ok_or(RadarError::CannotSetControlId(self.id))
    }
}

/// Client-to-server message to set a radar control value via WebSocket stream.
///
/// Used in the `/signalk/v1/stream` WebSocket to send control commands.
/// The path identifies which radar and control to modify.
#[derive(Deserialize, Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(example = json!({
    "path": "radars.nav1034A.controls.gain",
    "value": 50
}))]
pub struct RadarControlValue {
    #[serde(skip)]
    #[schema(ignore)]
    pub radar_id: Option<String>,
    #[serde(skip)]
    #[schema(ignore)]
    pub control_id: Option<ControlId>,
    /// Full path to the control: "radars.{radarId}.controls.{controlId}"
    #[schema(example = "radars.nav1034A.controls.gain")]
    pub path: String,
    /// The value to set (type depends on control: number, boolean, or string)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Value, example = json!(50))]
    pub value: Option<serde_json::Value>,
    /// Units for the value (e.g., "degrees", "meters")
    #[serde(skip_serializing)]
    #[schema(ignore)]
    pub units: Option<Units>,
    /// Enable/disable automatic mode for controls that support it
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = false)]
    pub auto: Option<bool>,
    /// Current auto-computed value (read-only, server-to-client only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_value: Option<f64>,
    /// End angle for guard zones (degrees)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 90.0)]
    pub end_value: Option<f64>,
    /// Start distance for guard zones (meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 100.0)]
    pub start_distance: Option<f64>,
    /// End distance for guard zones (meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 500.0)]
    pub end_distance: Option<f64>,
    /// Enable/disable the control (for guard zones)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = true)]
    pub enabled: Option<bool>,
    /// Whether the control is currently allowed (read-only)
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    /// Error message if the control command failed (read-only)
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// First corner X for rectangular exclusion zones (meters from radar, positive = east)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 100.0)]
    pub x1: Option<f64>,
    /// First corner Y for rectangular exclusion zones (meters from radar, positive = north)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 200.0)]
    pub y1: Option<f64>,
    /// Second corner X for rectangular exclusion zones (meters from radar, positive = east)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 300.0)]
    pub x2: Option<f64>,
    /// Second corner Y for rectangular exclusion zones (meters from radar, positive = north)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 200.0)]
    pub y2: Option<f64>,
    /// Width of rectangular exclusion zone (perpendicular to edge, meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 50.0)]
    pub width: Option<f64>,
    /// Timestamp when the control value was last changed (ISO 8601 format)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only = true, example = "2025-01-15T10:30:00Z")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl RadarControlValue {
    pub fn new(radar: &str, control: &Control, error: Option<String>) -> Self {
        RadarControlValue {
            path: format!("radars.{}.controls.{}", radar, control.item().control_id),
            radar_id: Some(radar.to_string()),
            control_id: Some(control.item().control_id),
            value: control.value(),
            units: control.item().units.clone(),
            auto: control.auto,
            auto_value: control.auto_value(),
            end_value: control.end_value(),
            start_distance: control.start_distance(),
            end_distance: control.end_distance(),
            enabled: control.enabled,
            allowed: control.allowed,
            error,
            x1: control.x1,
            y1: control.y1,
            x2: control.x2,
            y2: control.y2,
            width: control.width,
            timestamp: control.timestamp,
        }
    }

    pub fn parse_path(&mut self) -> Option<&str> {
        let mut path = self.path.as_str();
        if path.starts_with("radars.") {
            path = &path["radars.".len()..];
        }
        if let Some(r) = path.split('.').last() {
            self.control_id = ControlId::try_from(r).ok();
            if self.control_id.is_some() {
                self.radar_id = Some(r.to_string());
                return Some(r);
            }
        }

        None
    }
}

impl From<RadarControlValue> for ControlValue {
    fn from(rcv: RadarControlValue) -> Self {
        ControlValue {
            id: rcv.control_id.unwrap(),
            value: rcv.value,
            units: rcv.units,
            auto: rcv.auto,
            auto_value: rcv.auto_value,
            end_value: rcv.end_value,
            start_distance: rcv.start_distance,
            end_distance: rcv.end_distance,
            enabled: rcv.enabled,
            allowed: rcv.allowed,
            error: rcv.error,
            x1: rcv.x1,
            y1: rcv.y1,
            x2: rcv.x2,
            y2: rcv.y2,
            width: rcv.width,
            timestamp: rcv.timestamp,
        }
    }
}

/// Radar control value used by the Signal K REST API
///
/// Different control types use different fields:
/// - Simple controls: `value` (numeric or string)
/// - Auto-capable controls: `value`, `auto`, possibly `autoValue`
/// - sectors: `value` (angle start), `endValue` (angle end)
/// - zones: `value` (angle start), `endValue` (angle end), `startDistance`, `endDistance`, `enabled`
///
/// Numeric controls may send `units` when not in SI and control definition
/// has units specified.
///
#[derive(Deserialize, Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ControlValue, example = json!({
    "value": 50,
    "auto": false,
    "allowed": true
}))]
pub struct BareControlValue {
    /// The control value (numeric for most controls, string for some modes)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = json!(50))]
    pub value: Option<serde_json::Value>,
    /// Units for the value (internal use)
    #[serde(skip_serializing)]
    pub units: Option<Units>,
    /// Whether automatic mode is enabled (for Gain, Sea, Rain, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = false)]
    pub auto: Option<bool>,
    /// Adjustment of the auto algorithm when auto=true
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = -5.0)]
    pub auto_value: Option<f64>,
    /// End angle for sector and zone (radians)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 0.78593)]
    pub end_value: Option<f64>,
    /// Inner radius for zones (meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 100.0)]
    pub start_distance: Option<f64>,
    /// Outer radius for zones (meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 500.0)]
    pub end_distance: Option<f64>,
    /// Whether the user has enabled the control
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = true)]
    pub enabled: Option<bool>,
    /// Whether changing this control is currently allowed (read-only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only = true, example = true)]
    pub allowed: Option<bool>,
    /// Error message if the control change failed (read-only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only = true)]
    pub error: Option<String>,
    /// First corner X for rectangular exclusion zones (meters from radar, positive = east)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 100.0)]
    pub x1: Option<f64>,
    /// First corner Y for rectangular exclusion zones (meters from radar, positive = north)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 200.0)]
    pub y1: Option<f64>,
    /// Second corner X for rectangular exclusion zones (meters from radar, positive = east)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 300.0)]
    pub x2: Option<f64>,
    /// Second corner Y for rectangular exclusion zones (meters from radar, positive = north)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 200.0)]
    pub y2: Option<f64>,
    /// Width of rectangular exclusion zone (perpendicular to edge, meters)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 50.0)]
    pub width: Option<f64>,
    /// Timestamp when the control value was last changed (ISO 8601 format)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only = true, example = "2025-01-15T10:30:00Z")]
    pub timestamp: Option<DateTime<Utc>>,
}

impl BareControlValue {
    pub fn new_error(error: String) -> Self {
        BareControlValue {
            value: None,
            units: None,
            auto: None,
            auto_value: None,
            end_value: None,
            start_distance: None,
            end_distance: None,
            enabled: None,
            allowed: None,
            error: Some(error),
            x1: None,
            y1: None,
            x2: None,
            y2: None,
            width: None,
            timestamp: None,
        }
    }
}
impl From<RadarControlValue> for BareControlValue {
    fn from(rcv: RadarControlValue) -> Self {
        BareControlValue {
            value: rcv.value,
            units: rcv.units,
            auto: rcv.auto,
            auto_value: rcv.auto_value,
            end_value: rcv.end_value,
            start_distance: rcv.start_distance,
            end_distance: rcv.end_distance,
            enabled: rcv.enabled,
            allowed: rcv.allowed,
            error: rcv.error,
            x1: rcv.x1,
            y1: rcv.y1,
            x2: rcv.x2,
            y2: rcv.y2,
            width: rcv.width,
            timestamp: rcv.timestamp,
        }
    }
}

impl From<ControlValue> for BareControlValue {
    fn from(cv: ControlValue) -> Self {
        BareControlValue {
            value: cv.value,
            units: cv.units,
            auto: cv.auto,
            auto_value: cv.auto_value,
            end_value: cv.end_value,
            start_distance: cv.start_distance,
            end_distance: cv.end_distance,
            enabled: cv.enabled,
            allowed: cv.allowed,
            error: cv.error,
            x1: cv.x1,
            y1: cv.y1,
            x2: cv.x2,
            y2: cv.y2,
            width: cv.width,
            timestamp: cv.timestamp,
        }
    }
}

pub(crate) struct ControlBuilder {
    control: Control,
    frozen: bool,
}

impl ControlBuilder {
    pub(crate) fn read_only(mut self, is_read_only: bool) -> Self {
        self.control.item.is_read_only = is_read_only;

        self
    }

    pub(crate) fn wire_scale_step(mut self, step: f64) -> Self {
        if self.frozen {
            panic!("{} already frozen", self.control.item.control_id);
        }
        self.control.item.step_value = Some(step);
        if self.control.item.wire_scale_factor.is_none() {
            self.control.item.wire_scale_factor = self.control.item.step_value.map(|s| 1. / s);
        }

        self
    }

    pub(crate) fn wire_scale_factor(mut self, wire_scale_factor: f64, with_step: bool) -> Self {
        if self.frozen {
            panic!("{} already frozen", self.control.item.control_id);
        }
        self.control.item.wire_scale_factor = Some(wire_scale_factor);
        if with_step {
            self.control.item.step_value = self.control.item.wire_scale_factor.map(|f| 1. / f);
        }

        self
    }

    pub(crate) fn wire_offset(mut self, wire_offset: f64) -> Self {
        if self.frozen {
            panic!("{} already frozen", self.control.item.control_id);
        }
        self.control.item.wire_offset = Some(wire_offset);

        self
    }

    pub(crate) fn wire_units(mut self, units: Units) -> Self {
        if self.frozen {
            panic!("{} already frozen", self.control.item.control_id);
        }
        self.control.item.wire_units = Some(units);
        self.control.item.units = Some(units.to_si(0.).0);
        if let Some(value) = self.control.item.min_value {
            self.control.item.min_value = Some(units.to_si(value).1);
        }
        if let Some(value) = self.control.item.max_value {
            self.control.item.max_value = Some(units.to_si(value).1);
        }
        if let Some(value) = self.control.item.step_value {
            self.control.item.step_value = Some(units.to_si(value).1);
        }
        if let Some(value) = self.control.item.default_value {
            self.control.item.default_value = Some(units.to_si(value).1);
        }
        self.frozen = true;

        self
    }

    #[allow(dead_code)]
    pub(crate) fn send_always(mut self) -> Self {
        self.control.item.is_send_always = true;

        self
    }

    pub(crate) fn has_enabled(mut self) -> Self {
        self.control.item.has_enabled = true;

        self
    }

    pub(crate) fn set_valid_values(mut self, valid_values: Vec<i32>) -> Self {
        if self.frozen {
            panic!("{} already frozen", self.control.item.control_id);
        }
        self.control.set_valid_values(valid_values);

        self
    }

    pub(crate) fn set_value(mut self, value: f64) -> Self {
        self.control.value = Some(value);
        self
    }

    pub(crate) fn build(self, controls: &mut HashMap<ControlId, Control>) {
        controls.insert(self.control.item.control_id, self.control);
    }

    pub(crate) fn take(self) -> (ControlId, Control) {
        (self.control.item.control_id, self.control)
    }
}

pub(crate) fn new_numeric(control_id: ControlId, min_value: f64, max_value: f64) -> ControlBuilder {
    let min_value = Some(min_value);
    let max_value = Some(max_value);
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Number,
        min_value,
        None,
        false,
        min_value,
        max_value,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_auto(
    control_id: ControlId,
    min_value: f64,
    max_value: f64,
    automatic: AutomaticValue,
) -> ControlBuilder {
    let min_value = Some(min_value);
    let max_value = Some(max_value);
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Number,
        min_value,
        Some(automatic),
        false,
        min_value,
        max_value,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_list(control_id: ControlId, descriptions: &[&str]) -> ControlBuilder {
    let description_count = ((descriptions.len() as i32) - 1) as f64;
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Enum,
        Some(0.),
        None,
        false,
        Some(0.),
        Some(description_count),
        None,
        None,
        None,
        None,
        Some(
            descriptions
                .into_iter()
                .enumerate()
                .map(|(i, n)| (i as i32, n.to_string()))
                .collect(),
        ),
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_map(control_id: ControlId, descriptions: HashMap<i32, String>) -> ControlBuilder {
    let description_count = ((descriptions.len() as i32) - 1) as f64;
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Enum,
        Some(0.),
        None,
        false,
        Some(0.),
        Some(description_count),
        None,
        None,
        None,
        None,
        Some(descriptions),
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_string(control_id: ControlId) -> ControlBuilder {
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::String,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_button(control_id: ControlId) -> ControlBuilder {
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Button,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

pub(crate) fn new_sector(control_id: ControlId, min_value: f64, max_value: f64) -> ControlBuilder {
    let min_value = Some(min_value);
    let max_value = Some(max_value);
    let control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Sector,
        min_value,
        None,
        true, // Sectors always have enabled
        min_value,
        max_value,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    ControlBuilder {
        control,
        frozen: false,
    }
}

/// Create a zone control with start/end angles and start/end distances.
/// Angles are in the specified range (typically -180..180 degrees).
/// Distances are in meters (0..max_distance).
pub(crate) fn new_zone(
    control_id: ControlId,
    min_angle: f64,
    max_angle: f64,
    max_distance: f64,
) -> ControlBuilder {
    let min_value = Some(min_angle);
    let max_value = Some(max_angle);
    let mut control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Zone,
        Some(0.),
        None,
        true, // Zones always have enabled
        min_value,
        max_value,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    control.item.max_distance = Some(max_distance);
    control.value = Some(0.);
    control.end_value = Some(0.);
    control.start_distance = Some(0.);
    control.end_distance = Some(0.);
    ControlBuilder {
        control,
        frozen: false,
    }
}

/// Create a rectangular exclusion zone control with north/south/east/west bounds.
/// All distances are in meters from the radar position.
pub(crate) fn new_rect(control_id: ControlId, max_distance: f64) -> ControlBuilder {
    let min_value = Some(0.);
    let max_value = Some(max_distance);
    let mut control = Control::new(ControlDefinition::new(
        control_id,
        ControlDataType::Rect,
        Some(0.),
        None,
        true, // Rects always have enabled
        min_value,
        max_value,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        false,
    ));
    control.item.max_distance = Some(max_distance);
    control.x1 = Some(0.);
    control.y1 = Some(0.);
    control.x2 = Some(0.);
    control.y2 = Some(0.);
    control.width = Some(0.);
    ControlBuilder {
        control,
        frozen: false,
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    #[serde(flatten)]
    item: ControlDefinition,
    #[serde(skip)]
    pub value: Option<f64>,
    #[serde(skip)]
    pub auto_value: Option<f64>,
    #[serde(skip)]
    pub end_value: Option<f64>,
    #[serde(skip)]
    pub start_distance: Option<f64>,
    #[serde(skip)]
    pub end_distance: Option<f64>,
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(skip)]
    pub auto: Option<bool>,
    #[serde(skip)]
    pub enabled: Option<bool>,
    #[serde(skip)]
    pub allowed: Option<bool>,
    #[serde(skip)]
    pub needs_refresh: bool, // True when it has been changed and client needs to know value (again)
    // Rectangular exclusion zone fields (corner-based, meters from radar position)
    #[serde(skip)]
    pub x1: Option<f64>,
    #[serde(skip)]
    pub y1: Option<f64>,
    #[serde(skip)]
    pub x2: Option<f64>,
    #[serde(skip)]
    pub y2: Option<f64>,
    #[serde(skip)]
    pub width: Option<f64>,
    /// Timestamp when the control value was last changed
    #[serde(skip)]
    pub timestamp: Option<DateTime<Utc>>,
}

impl Control {
    fn new(item: ControlDefinition) -> Self {
        let value = item.default_value.clone();
        let timestamp = if value.is_some() {
            Some(Utc::now())
        } else {
            None
        };
        Control {
            item,
            value,
            auto_value: None,
            end_value: None,
            start_distance: None,
            end_distance: None,
            auto: None,
            enabled: None,
            description: None,
            allowed: None,
            needs_refresh: false,
            x1: None,
            y1: None,
            x2: None,
            y2: None,
            width: None,
            timestamp,
        }
    }

    /// Read-only access to the definition of the control
    pub fn item(&self) -> &ControlDefinition {
        &self.item
    }

    pub(crate) fn set_valid_values(&mut self, values: Vec<i32>) {
        self.item.valid_values = Some(values);
    }

    pub(crate) fn set_valid_ranges(&mut self, ranges: &Vec<Range>) {
        let mut values = Vec::new();
        let mut descriptions = HashMap::new();
        for range in ranges.iter() {
            values.push(range.distance());
            descriptions.insert(range.distance() as i32, format!("{}", range));
        }

        self.item.min_value = Some(values[0] as f64);
        self.item.max_value = Some(values[values.len() - 1] as f64);
        self.item.step_value = None;
        self.item.valid_values = Some(values);
        self.item.descriptions = Some(descriptions);
    }

    pub fn set_allowed(&mut self, allowed: bool) {
        if self.allowed != Some(allowed) {
            self.allowed = Some(allowed);
            self.needs_refresh = true;
        }
    }

    fn to_number(&self, v: f64) -> Value {
        if let Some(n) = {
            if v == v as i32 as f64 {
                Number::from_i128(v as i128)
            } else {
                Number::from_f64(v as f64)
            }
        } {
            Value::Number(n)
        } else {
            panic!("Cannot represent {:?} as number", self);
        }
    }

    fn to_f64(&self, v: f64) -> Option<f64> {
        Some(v)
    }

    pub fn value(&self) -> Option<Value> {
        if self.item.data_type == ControlDataType::String {
            return self.description.clone().map(|v| Value::String(v));
        }

        self.value.map(|v| self.to_number(v))
    }

    pub fn auto_value(&self) -> Option<f64> {
        if self.item.data_type == ControlDataType::String {
            return None;
        }

        self.auto_value.and_then(|v| self.to_f64(v))
    }

    pub fn end_value(&self) -> Option<f64> {
        if self.item.data_type != ControlDataType::Sector
            && self.item.data_type != ControlDataType::Zone
        {
            return None;
        }

        self.end_value.and_then(|v| self.to_f64(v))
    }

    pub fn start_distance(&self) -> Option<f64> {
        if self.item.data_type != ControlDataType::Zone {
            return None;
        }

        self.start_distance
    }

    pub fn end_distance(&self) -> Option<f64> {
        if self.item.data_type != ControlDataType::Zone {
            return None;
        }

        self.end_distance
    }

    pub(crate) fn auto_as_f64(&self) -> Option<f64> {
        self.auto_value
    }

    pub(crate) fn end_as_f64(&self) -> Option<f64> {
        self.end_value
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        self.value
    }

    pub(crate) fn as_u16(&self) -> Option<u16> {
        match self.value {
            None => None,
            Some(n) => n.to_u16(),
        }
    }

    fn set_auto(&mut self, auto: bool) {
        self.needs_refresh = self.auto != Some(auto);
        log::debug!(
            "Setting {} auto {} changed: {}",
            self.item.control_id,
            auto,
            self.needs_refresh
        );
        self.auto = Some(auto);
    }

    ///
    /// Set a control from a wire value, so apply all transformations
    /// to convert it to a user visible value
    ///
    /// Set the control to a (maybe new) value + auto state
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub fn set(
        &mut self,
        mut value: f64,
        mut auto_value: Option<f64>,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        // SCALE MAPPING
        log::trace!(
            "{}: set(value={},auto_value={:?},auto={:?},enabled={:?}) with item {:?}",
            self.item.name,
            value,
            auto_value,
            auto,
            enabled,
            self.item
        );

        if let Some(wire_offset) = self.item.wire_offset {
            if wire_offset > 0.0 {
                value -= wire_offset;
            }
        }

        if let Some(wire_scale_factor) = self.item.wire_scale_factor {
            // One of the reasons we use f64 is because Navico wire format for some things is
            // tenths of degrees. To make things uniform we map these to a float with .1 precision.

            log::debug!(
                "{} map value {} scale_factor {}",
                self.item.control_id,
                value,
                wire_scale_factor
            );
            value = value / wire_scale_factor;

            auto_value = auto_value.map(|v| v / wire_scale_factor);
            log::debug!("{} map value to scaled {}", self.item.control_id, value);
        }

        let wire_value = value;
        let mut value = self
            .item
            .wire_units
            .map(|u| u.to_si(value).1)
            .unwrap_or(value);
        log::debug!(
            "value {} {} -> {} {}",
            wire_value,
            self.item.wire_units.unwrap_or(Units::None),
            value,
            self.item.units.unwrap_or(Units::None)
        );

        // RANGE MAPPING
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && value > max_value
                && value <= 2. * max_value
            {
                // debug!("{} value {} -> ", self.item.control_id, value);
                value -= 2. * max_value;
                // debug!("{} ..... {}", self.item.control_id, value);
            }

            if value < min_value {
                return Err(ControlError::TooLow(self.item.control_id, value, min_value));
            }
            if value > max_value {
                return Err(ControlError::TooHigh(
                    self.item.control_id,
                    value,
                    max_value,
                ));
            }
        }

        let step = self.item.step_value.unwrap_or(1.0);
        match step {
            0.1 => {
                value = (value * 10.) as i32 as f64 / 10.;
                auto_value = auto_value.map(|value| (value * 10.) as i32 as f64 / 10.);
            }
            1.0 => {
                value = value as i32 as f64;
                auto_value = auto_value.map(|value| value as i32 as f64);
            }
            _ => {
                value = (value / step).round() * step;
                auto_value = auto_value.map(|value| (value / step).round() * step);
            }
        }
        log::trace!("{} map value to rounded {}", self.item.control_id, value);

        if let Some(av) = auto_value {
            let si = self.item.wire_units.map(|u| u.to_si(av).1).unwrap_or(av);
            log::trace!(
                "auto value {} {} -> {} {}",
                av,
                self.item.wire_units.unwrap_or(Units::None),
                si,
                self.item.units.unwrap_or(Units::None)
            );
            auto_value = Some(si);
        }

        if auto.is_some() && self.item.automatic.is_none() {
            Err(ControlError::NoAuto(self.item.control_id))
        } else if self.value != Some(value)
            || self.auto_value != auto_value
            || self.auto != auto
            || self.enabled != enabled
        {
            self.value = Some(value);
            self.auto_value = auto_value;
            self.auto = auto;
            self.enabled = enabled;
            self.needs_refresh = false;
            self.timestamp = Some(Utc::now());

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Set a sector control with start and end values
    ///
    /// The start value is stored in `value` and end value in `end_value`.
    /// Both values are in radians (SI units for angles).
    pub fn set_sector(
        &mut self,
        mut start: f64,
        mut end: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        if self.item.data_type != ControlDataType::Sector {
            return Err(ControlError::NotSupported(self.item.control_id));
        }

        // Apply wire offset
        if let Some(wire_offset) = self.item.wire_offset {
            if wire_offset > 0.0 {
                start -= wire_offset;
                end -= wire_offset;
            }
        }

        // Apply wire scale factor
        if let Some(wire_scale_factor) = self.item.wire_scale_factor {
            start = start / wire_scale_factor;
            end = end / wire_scale_factor;
        }

        // Convert to SI units
        start = self
            .item
            .wire_units
            .map(|u| u.to_si(start).1)
            .unwrap_or(start);
        end = self.item.wire_units.map(|u| u.to_si(end).1).unwrap_or(end);

        // Range validation for start
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && start > max_value
                && start <= 2. * max_value
            {
                start -= 2. * max_value;
            }
            if start < min_value {
                return Err(ControlError::TooLow(self.item.control_id, start, min_value));
            }
            if start > max_value {
                return Err(ControlError::TooHigh(
                    self.item.control_id,
                    start,
                    max_value,
                ));
            }

            // Range validation for end
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && end > max_value
                && end <= 2. * max_value
            {
                end -= 2. * max_value;
            }
            if end < min_value {
                return Err(ControlError::TooLow(self.item.control_id, end, min_value));
            }
            if end > max_value {
                return Err(ControlError::TooHigh(self.item.control_id, end, max_value));
            }
        }

        // Apply step rounding
        let step = self.item.step_value.unwrap_or(1.0);
        match step {
            0.1 => {
                start = (start * 10.) as i32 as f64 / 10.;
                end = (end * 10.) as i32 as f64 / 10.;
            }
            1.0 => {
                start = start as i32 as f64;
                end = end as i32 as f64;
            }
            _ => {
                start = (start / step).round() * step;
                end = (end / step).round() * step;
            }
        }

        if self.value != Some(start) || self.end_value != Some(end) || self.enabled != enabled {
            self.value = Some(start);
            self.end_value = Some(end);
            self.enabled = enabled;
            self.needs_refresh = false;
            self.timestamp = Some(Utc::now());

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Set a zone control with start/end angles and start/end distances
    ///
    /// Angles are in wire units (degrees) and will be converted to SI (radians).
    /// Distances are always in meters.
    pub fn set_zone(
        &mut self,
        mut start_angle: f64,
        mut end_angle: f64,
        start_distance: f64,
        end_distance: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        if self.item.data_type != ControlDataType::Zone {
            return Err(ControlError::NotSupported(self.item.control_id));
        }

        // Convert angles to SI units (radians)
        start_angle = self
            .item
            .wire_units
            .map(|u| u.to_si(start_angle).1)
            .unwrap_or(start_angle);
        end_angle = self
            .item
            .wire_units
            .map(|u| u.to_si(end_angle).1)
            .unwrap_or(end_angle);

        let changed = self.value != Some(start_angle)
            || self.end_value != Some(end_angle)
            || self.start_distance != Some(start_distance)
            || self.end_distance != Some(end_distance)
            || self.enabled != enabled;

        if changed {
            self.value = Some(start_angle);
            self.end_value = Some(end_angle);
            self.start_distance = Some(start_distance);
            self.end_distance = Some(end_distance);
            self.enabled = enabled;
            self.needs_refresh = false;
            self.timestamp = Some(Utc::now());

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Set a rectangular exclusion zone with corner-based coordinates.
    /// x1,y1 and x2,y2 define one edge, width is perpendicular.
    pub fn set_rect(
        &mut self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        width: f64,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        if self.item.data_type != ControlDataType::Rect {
            return Err(ControlError::NotSupported(self.item.control_id));
        }

        let changed = self.x1 != Some(x1)
            || self.y1 != Some(y1)
            || self.x2 != Some(x2)
            || self.y2 != Some(y2)
            || self.width != Some(width)
            || self.enabled != enabled;

        if changed {
            self.x1 = Some(x1);
            self.y1 = Some(y1);
            self.x2 = Some(x2);
            self.y2 = Some(y2);
            self.width = Some(width);
            self.enabled = enabled;
            self.needs_refresh = false;
            self.timestamp = Some(Utc::now());

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(&mut self, value: String) -> Option<()> {
        let value = Some(value);
        if &self.description != &value {
            self.description = value;
            self.needs_refresh = false;
            self.timestamp = Some(Utc::now());
            log::trace!("Set {} to {:?}", self.item.control_id, self.description);
            Some(())
        } else if self.needs_refresh {
            self.needs_refresh = false;
            Some(())
        } else {
            None
        }
    }

    /// Set the control's wire offset and scale
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub(crate) fn set_wire_range(
        &mut self,
        min: f64,
        max: f64,
    ) -> Result<Option<()>, ControlError> {
        let max = Some(max - min);
        let min = if min != 0.0 { Some(min) } else { None };

        if min != self.item.wire_offset || max != self.item.wire_scale_factor {
            log::debug!(
                "{}: new wire offset {:?} and scale {:?}",
                self.item.name,
                min,
                max,
            );
            self.item.wire_offset = min;
            self.item.wire_scale_factor = max;
        }
        Ok(None)
    }

    /// Look up the wire value (index) for an enum value by its string value or label
    /// Returns None if no match found or if not an enum control
    #[allow(dead_code)]
    pub(crate) fn enum_value_to_index(&self, value_str: &str) -> Option<usize> {
        if let Some(descriptions) = &self.item.descriptions {
            for (idx, label) in descriptions.iter() {
                if label.eq_ignore_ascii_case(value_str) {
                    return Some(*idx as usize);
                }
            }
        }
        None
    }

    /// Look up the string value for an enum by its index
    /// Returns the core definition's value if available, otherwise the label
    #[allow(dead_code)]
    pub(crate) fn index_to_enum_value(&self, index: usize) -> Option<String> {
        if let Some(descriptions) = &self.item.descriptions {
            return descriptions.get(&(index as i32)).cloned();
        }
        None
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticValue {
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_auto: bool,
    //#[serde(skip)]
    //pub(crate) auto_values: i32,
    //#[serde(skip)]
    //pub(crate) auto_descriptions: Option<Vec<String>>,
    pub(crate) has_auto_adjustable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auto_adjust_min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auto_adjust_max_value: Option<f64>,
}

pub(crate) const HAS_AUTO_NOT_ADJUSTABLE: AutomaticValue = AutomaticValue {
    has_auto: true,
    has_auto_adjustable: false,
    auto_adjust_min_value: None,
    auto_adjust_max_value: None,
};

#[derive(Clone, Debug, Serialize, PartialEq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ControlDataType {
    Number,
    Enum,
    String,
    Button,
    Sector,
    Zone,
    Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlDestination {
    ReadOnly,
    Internal,
    Command,
    Trail,
    Target,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    pub(crate) id: u8,
    #[serde(skip)]
    pub control_id: ControlId,
    name: &'static str,
    description: &'static str,
    category: Category,
    pub(crate) data_type: ControlDataType,
    //#[serde(skip)]
    //has_off: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    automatic: Option<AutomaticValue>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_enabled: bool,
    #[serde(skip)]
    default_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_value: Option<f64>,
    #[serde(skip)]
    wire_scale_factor: Option<f64>,
    #[serde(skip)]
    wire_offset: Option<f64>,
    #[serde(skip)]
    pub(crate) wire_units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptions: Option<HashMap<i32, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) valid_values: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "is_false")]
    is_read_only: bool,
    #[serde(skip)]
    is_send_always: bool, // Whether the controlvalue is sent out to client in all state messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_distance: Option<f64>, // For Zone controls: maximum distance in meters
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {
    fn new(
        control_id: ControlId,
        data_type: ControlDataType,
        default_value: Option<f64>,
        automatic: Option<AutomaticValue>,
        has_enabled: bool,
        min_value: Option<f64>,
        max_value: Option<f64>,
        step_value: Option<f64>,
        wire_scale_factor: Option<f64>,
        wire_offset: Option<f64>,
        wire_units: Option<Units>,
        descriptions: Option<HashMap<i32, String>>,
        valid_values: Option<Vec<i32>>,
        is_read_only: bool,
        is_send_always: bool,
    ) -> Self {
        let step_value =
            if data_type == ControlDataType::Number || data_type == ControlDataType::Enum {
                step_value.or(Some(1.0))
            } else {
                step_value
            };

        let units = wire_units.map(|u| u.to_si(0.0).0);

        ControlDefinition {
            id: control_id as u8,
            control_id,
            name: control_id.get_name(),
            description: control_id.get_description(),
            category: control_id.get_category(),
            data_type,
            automatic,
            has_enabled,
            default_value,
            min_value,
            max_value,
            step_value,
            wire_scale_factor,
            wire_offset,
            units,
            wire_units,
            descriptions,
            valid_values,
            is_read_only,
            is_send_always,
            max_distance: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Base,
    Targets,
    Trails,
    Advanced,
    Installation,
    Info,
}

#[derive(Error, Debug)]
pub enum ControlError {
    #[error("Control {0} not supported on this radar")]
    NotSupported(ControlId),
    #[error("Control {0} value {1} is lower than minimum value {2}")]
    TooLow(ControlId, f64, f64),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(ControlId, f64, f64),
    #[error("Control {0} value {1} is not a legal value")]
    Invalid(ControlId, String),
    #[error("Control {0} does not support Auto")]
    NoAuto(ControlId),
    #[error("Control {0} value '{1}' requires true heading input")]
    NoHeading(ControlId, &'static str),
    #[error("Control {0} value '{1}' requires a GNSS position")]
    NoPosition(ControlId, &'static str),
}

impl<'de> Deserialize<'de> for ControlId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ControlIdVisitor)
    }
}

struct ControlIdVisitor;

impl<'de> Visitor<'de> for ControlIdVisitor {
    type Value = ControlId;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string containing a ControlId name or numeric discriminant")
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Borrowed(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Borrowed(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Owned(v))
    }
}

fn parse_control_id<E>(s: Cow<'_, str>) -> Result<ControlId, E>
where
    E: serde::de::Error,
{
    // Numeric discriminant encoded as string
    if let Ok(num) = s.parse::<u8>() {
        return match FromPrimitive::from_u8(num) {
            Some(ct) => Ok(ct),
            None => Err(E::custom("invalid ControlId discriminant")),
        };
    }

    // Case-insensitive name lookup (Strum)
    ControlId::from_str(&s).map_err(|_| E::custom("invalid ControlId name"))
}

impl Serialize for ControlId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match get_api_version() {
            ApiVersion::V3 => {
                // camelCase string, zero allocation
                let name: &'static str = (*self).into();
                log::debug!("Serializing V3 ControlId {:?} as string {}", self, name);

                serializer.serialize_str(name)
            }
            ApiVersion::V1 => {
                // numeric discriminant rendered as string
                // stack-only formatting

                let mut buf = itoa::Buffer::new();
                let s = buf.format(*self as u8);

                log::debug!("Serializing V1 ControlId {:?} as number {}", self, s);
                serializer.serialize_str(s)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use clap::Parser;

    use super::*;

    #[test]
    fn serialize_control_value() {
        // Check that the ControlValue serializes correctly
        let ct = ControlId::Gain;
        let cts = ct.to_string();
        assert_eq!(cts, "gain".to_string());
        println!("ControlId as string: {}", cts);
        match ControlId::parse_str(Cow::Owned(cts)) {
            Ok(c) => {
                assert_eq!(c, ct);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check with optional fields and V3 ID
        let json = r#"{"id":"gain","value":"49","auto":true,"enabled":false}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlId::Gain);
                assert_eq!(cv.value, Some(Value::String("49".to_string())));
                assert_eq!(cv.auto, Some(true));
                assert_eq!(cv.enabled, Some(false));
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check without optional fields and with v1 ID
        let json = r#"{"id":"4","value":49}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlId::Gain);
                assert_eq!(
                    cv.value,
                    Some(Value::Number(Number::from_i128(49).unwrap()))
                );
                assert_eq!(cv.auto, None);
                assert_eq!(cv.enabled, None);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check with illegal negative v1 ID
        let json = r#"{"id":"-1","value":"49"}"#;

        assert!(serde_json::from_str::<ControlValue>(&json).is_err());
    }

    #[test]
    fn control_range_values() {
        let args = Cli::parse_from(["my_program"]);
        let tx = tokio::sync::broadcast::Sender::new(1);
        let controls = SharedControls::new("nav1234".to_string(), tx, &args, HashMap::new());

        assert!(controls.set(&ControlId::TargetTrails, 0., None).is_ok());
        assert_eq!(
            controls.set(&ControlId::TargetTrails, 6., None).unwrap(),
            Some(())
        );
        assert!(controls.set(&ControlId::TargetTrails, 7., None).is_err());
        assert!(controls.set(&ControlId::TargetTrails, -1., None).is_err());
        assert!(controls.set(&ControlId::TargetTrails, 0.3, None).is_ok());

        assert!(
            controls
                .set_value(&ControlId::TargetTrails, Value::String("3".to_string()))
                .is_ok()
        );
        assert_eq!(
            controls.get(&ControlId::TargetTrails).unwrap().value,
            Some(3.)
        );
    }
}
