use std::collections::HashMap;

use crate::{
    Cli,
    radar::settings::{
        ControlBuilder, ControlId, HAS_AUTO_NOT_ADJUSTABLE, RANGE_UNITS_KILOYARDS,
        RANGE_UNITS_METRIC, RANGE_UNITS_NAMES, RANGE_UNITS_NAUTICAL, RANGE_UNITS_STATUTE,
        SharedControls, new_auto, new_auto_standby, new_list, new_numeric, new_sector, new_string,
    },
    radar::{
        FRAC_NM_2, FRAC_NM_4, FRAC_NM_8, FRAC_NM_16, NM, RadarInfo, range::Ranges, units::Units,
    },
    stream::SignalKDelta,
};

use super::protocol::{RadarModel, TX_CHANNEL_MAX, metric_to_kiloyards, nautical_to_statute};

/// `$NF5` reports supply rails in hundredths of a volt.
const SUPPLY_VOLTAGE_SCALE: f64 = 100.;

pub(crate) fn new(
    radar_id: String,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    args: &Cli,
) -> SharedControls {
    let mut controls = HashMap::new();

    // Controls present on all Furuno radars. Model-specific controls are added
    // later in update_when_model_known() based on the capability table.
    new_string(ControlId::UserName)
        .read_only(false)
        .build(&mut controls);

    // Gain/Sea/Rain are added in update_when_model_known() once the model's
    // auto-capability flags are known. Until the model is detected the default
    // is a plain numeric control without auto, re-created later.
    new_numeric(ControlId::Gain, 0., 100.).build(&mut controls);
    new_numeric(ControlId::Sea, 0., 100.).build(&mut controls);
    new_numeric(ControlId::Rain, 0., 100.).build(&mut controls);
    new_numeric(ControlId::OperatingTime, 0., 999999999.)
        .read_only(true)
        .wire_units(Units::Seconds)
        .build(&mut controls);
    new_numeric(ControlId::TransmitTime, 0., 999999999.)
        .read_only(true)
        .wire_units(Units::Seconds)
        .build(&mut controls);

    new_string(ControlId::SerialNumber).build(&mut controls);
    // The radar ranges in each of these; Mixed has no wire unit.
    new_list(ControlId::RangeUnits, &RANGE_UNITS_NAMES)
        .set_valid_values(vec![
            RANGE_UNITS_NAUTICAL,
            RANGE_UNITS_METRIC,
            RANGE_UNITS_STATUTE,
            RANGE_UNITS_KILOYARDS,
        ])
        .build(&mut controls);

    // The report receiver stands a transmit that was asked through mayara down itself
    new_auto_standby().build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

/// The curves each STC band takes, by wire value. A band without curves
/// does nothing on that radar, so it is not offered.
struct StcCurves {
    near: &'static [i32],
    middle: &'static [i32],
    far: &'static [i32],
}

/// The curves TimeZero offers.
const STC_CURVES: StcCurves = StcCurves {
    near: &[0, 1, 2, 3, 4],
    middle: &[0, 1, 2, 3],
    far: &[0, 1, 2],
};

/// The curves the NXT firmware applies, read from both the DRS4D-NXT and
/// the DRS6A/12A/25A-NXT images; it echoes any other value, then reports
/// the curve it kept. Its default far curve is 3.
const NXT_STC_CURVES: StcCurves = StcCurves {
    near: &[2, 3, 4, 5],
    middle: &[0, 1],
    far: &[1, 3],
};

/// The X-Class firmware applies only the near band; it echoes the middle
/// and far curves but registers nothing that acts on them.
const X_CLASS_STC_CURVES: StcCurves = StcCurves {
    near: &[2, 3, 4, 5],
    middle: &[],
    far: &[],
};

/// The classic DRS has no STC commands, and the DRS4DL only echoes them.
const NO_STC_CURVES: StcCurves = StcCurves {
    near: &[],
    middle: &[],
    far: &[],
};

/// The manual tune range TimeZero offers.
const TUNE_MAX: f64 = 2000.;
/// The manual tune range the classic DRS and X-Class firmware apply, around
/// a centre of 1475.
const TUNE_MAX_2950: f64 = 2950.;
/// The manual tune range the DRS4DL firmware applies.
const TUNE_MAX_4096: f64 = 4096.;

/// An STC band offering only `curves`, each named by its wire value.
fn stc_curve(control_id: ControlId, curves: &[i32]) -> ControlBuilder {
    let highest = *curves.last().expect("an offered STC band has curves");
    let names: Vec<String> = (0..=highest).map(|curve| curve.to_string()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    new_list(control_id, &names).set_valid_values(curves.to_vec())
}

/// The names of a rejection control's levels, which go out as their index.
fn rejection_levels(levels: u8) -> Option<&'static [&'static str]> {
    match levels {
        2 => Some(&["Off", "On"]),
        3 => Some(&["Off", "Low", "High"]),
        4 => Some(&["Off", "Low", "Medium", "High"]),
        _ => None,
    }
}

/// Per-model capability flags, derived from MaxSea.Radar.dll capability
/// table and corrected from each model's firmware.
#[allow(dead_code)]
struct Capabilities {
    auto_gain: bool,
    auto_sea: bool,
    auto_rain: bool,
    dual_range: bool,
    scan_speed: bool,
    sector_blanking: bool,
    main_bang_suppression: bool,
    rez_boost: bool,
    bird_mode: bool,
    target_analyzer: bool, // Doppler
    target_analyzer_rain: bool,
    noise_rejection_levels: u8,        // 0=none, 2=off/on, 3=off/low/high
    interference_rejection_levels: u8, // 0=none, 2=off/on, 3=off/low/high, 4=off/low/med/high
    stc: &'static StcCurves,
    anti_jamming: bool,
    tune: bool,
    tune_max: f64,
    antenna_height: bool,
    tx_channel: bool,
    watchman: bool,
    pulse_width: bool,
    sea_modes: &'static [&'static str], // auto sea modes, empty when none
    echo_average: bool,                 // scan-to-scan echo averaging
}

/// Auto sea modes, in the order of the sea command's mode field.
const SEA_MODES: &[&str] = &["Adaptive", "Coastal", "Channel"];
/// The classic DRS only tells Coastal from the rest; Channel acts as
/// Adaptive there.
const CLASSIC_SEA_MODES: &[&str] = &["Adaptive", "Coastal"];

fn capabilities(model: &RadarModel) -> Capabilities {
    match model {
        // NXT series: full feature set. Both NXT firmware images store a
        // noise rejection setting that nothing acts on.
        RadarModel::DRS2DNXT
        | RadarModel::DRS4DNXT
        | RadarModel::DRS6ANXT
        | RadarModel::DRS12ANXT
        | RadarModel::DRS25ANXT => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: true,
            scan_speed: true,
            sector_blanking: true,
            main_bang_suppression: true,
            rez_boost: true,
            bird_mode: true,
            target_analyzer: true,
            target_analyzer_rain: true,
            noise_rejection_levels: 0,
            interference_rejection_levels: 2,
            stc: &NXT_STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX,
            antenna_height: true,
            tx_channel: true,
            watchman: true,
            pulse_width: false, // solid-state, no selectable pulse
            sea_modes: SEA_MODES,
            echo_average: true,
        },
        // X-Class (DRS6A/12A/25A share one image): the NXT code base without
        // its solid-state features
        RadarModel::DRS6AXCLASS => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: true,
            sector_blanking: true,
            main_bang_suppression: true,
            rez_boost: false,
            bird_mode: true,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 0,
            interference_rejection_levels: 4,
            stc: &X_CLASS_STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX_2950,
            antenna_height: true,
            tx_channel: false,
            watchman: false,
            pulse_width: true,
            sea_modes: SEA_MODES,
            echo_average: false,
        },
        // DRS4DL / DRS4DL+: single range; scan speed is stored but drives
        // nothing, and there is no bird mode or antenna height
        RadarModel::DRS4DL => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: false,
            sector_blanking: true,
            main_bang_suppression: true,
            rez_boost: false,
            bird_mode: false,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 3,
            interference_rejection_levels: 3,
            stc: &NO_STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX_4096,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: true,
            sea_modes: SEA_MODES,
            echo_average: false,
        },
        // Classic NavNet 3D DRS, open and radome alike. Its antenna height
        // command takes a height class rather than metres, so it is not
        // offered; it has no STC or anti-jamming commands.
        RadarModel::DRS => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: true,
            sector_blanking: true,
            main_bang_suppression: true,
            rez_boost: false,
            bird_mode: true,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 0,
            interference_rejection_levels: 2,
            stc: &NO_STC_CURVES,
            anti_jamming: false,
            tune: true,
            tune_max: TUNE_MAX_2950,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: true,
            sea_modes: CLASSIC_SEA_MODES,
            echo_average: false,
        },
        RadarModel::DRS4W => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: false,
            sector_blanking: false,
            main_bang_suppression: false,
            rez_boost: false,
            bird_mode: false,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 0,
            interference_rejection_levels: 0,
            stc: &STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: false, // DRS4W firmware disables pulse width
            sea_modes: &[],
            echo_average: false,
        },
        // FAR series: commercial, 4-level IR, no NXT features
        RadarModel::FAR21x7 | RadarModel::FAR14x7 | RadarModel::FAR14x6 => Capabilities {
            auto_gain: false,
            auto_sea: false,
            auto_rain: false,
            dual_range: false,
            scan_speed: false,
            sector_blanking: false,
            main_bang_suppression: false,
            rez_boost: false,
            bird_mode: false,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 0,
            interference_rejection_levels: 4,
            stc: &STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: true,
            sea_modes: &[],
            echo_average: false,
        },
        // FAR-15x3 / FAR-3000: auto sea/rain, noise rejection
        RadarModel::FAR15x3 | RadarModel::FAR3000 => Capabilities {
            auto_gain: false,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: false,
            sector_blanking: false,
            main_bang_suppression: false,
            rez_boost: false,
            bird_mode: false,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 2,
            interference_rejection_levels: 4,
            stc: &STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: true,
            sea_modes: &[],
            echo_average: false,
        },
        // Unknown: basic capabilities
        RadarModel::Unknown => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: false,
            sector_blanking: false,
            main_bang_suppression: false,
            rez_boost: false,
            bird_mode: false,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection_levels: 0,
            interference_rejection_levels: 0,
            stc: &STC_CURVES,
            anti_jamming: true,
            tune: true,
            tune_max: TUNE_MAX,
            antenna_height: false,
            tx_channel: false,
            watchman: false,
            pulse_width: false,
            sea_modes: &[],
            echo_average: false,
        },
    }
}

pub(crate) fn update_when_model_known(info: &mut RadarInfo, model: RadarModel, version: &str) {
    let model_name = model.to_string();
    log::debug!("update_when_model_known: {}", model_name);
    info.controls.set_model_name(model_name.to_string());

    if info.controls.user_name() == info.key() {
        let mut user_name = model_name.to_string();
        // Same discriminator as the key, so two units reporting an all-zero
        // serial are told apart by name as well as by key.
        if let Some(discriminator) = info.discriminator() {
            user_name.push(' ');
            user_name.push_str(discriminator);
        }
        if let Some(dual) = info.dual.as_deref() {
            user_name.push(' ');
            user_name.push_str(dual);
        }
        info.controls.set_user_name(user_name);
    }

    let ranges = ranges_by_model(&model);
    log::info!(
        "{}: model {} supports ranges {}",
        info.key(),
        model_name,
        ranges
    );
    info.set_ranges(ranges);

    let cap = capabilities(&model);

    // Update Gain / Sea / Rain definitions with auto flags matching the
    // detected model's capabilities, preserving any runtime state (value,
    // auto) already set. The defaults installed in new() were plain numeric
    // controls because capabilities aren't known at discovery time.
    if cap.auto_gain {
        info.controls.update_definition(new_auto(
            ControlId::Gain,
            0.,
            100.,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    } else {
        info.controls
            .update_definition(new_numeric(ControlId::Gain, 0., 100.));
    }
    if cap.auto_sea {
        info.controls.update_definition(new_auto(
            ControlId::Sea,
            0.,
            100.,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    } else {
        info.controls
            .update_definition(new_numeric(ControlId::Sea, 0., 100.));
    }
    if cap.auto_rain {
        info.controls.update_definition(new_auto(
            ControlId::Rain,
            0.,
            100.,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    } else {
        info.controls
            .update_definition(new_numeric(ControlId::Rain, 0., 100.));
    }

    info.controls.add(new_string(ControlId::FirmwareVersion));
    info.controls
        .set_string(&ControlId::FirmwareVersion, version.to_string())
        .expect("FirmwareVersion");
    if cap.pulse_width {
        info.controls.add(new_string(ControlId::PulseWidth));
    }

    // STC (Sensitivity Time Control), for the bands the model applies
    for (control_id, curves) in [
        (ControlId::NearStcCurve, cap.stc.near),
        (ControlId::MiddleStcCurve, cap.stc.middle),
        (ControlId::FarStcCurve, cap.stc.far),
    ] {
        if !curves.is_empty() {
            info.controls.add(stc_curve(control_id, curves));
        }
    }
    // StcRange ($RD2) is not registered: DRS4D-NXT does not respond to $RD2.
    // Re-enable when a model that supports it is available for testing.

    if cap.anti_jamming {
        info.controls
            .add(new_list(ControlId::AntiJamming, &["Off", "On"]));
    }

    // Tuning
    if cap.tune {
        info.controls.add(new_auto(
            ControlId::Tune,
            0.,
            cap.tune_max,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    }

    // Antenna controls (shared across ranges)
    if cap.sector_blanking {
        info.controls.add(
            new_sector(ControlId::NoTransmitSector1, -180., 180.)
                .wire_scale_step(1.)
                .wire_offset(-1.)
                .wire_units(Units::Degrees)
                .has_enabled(),
        );
        info.controls.add(
            new_sector(ControlId::NoTransmitSector2, -180., 180.)
                .wire_scale_step(1.)
                .wire_offset(-1.)
                .wire_units(Units::Degrees)
                .has_enabled(),
        );
    }
    if cap.scan_speed {
        info.controls
            .add(new_list(ControlId::ScanSpeed, &["Normal", "Fast", "Auto"]));
    }
    if cap.main_bang_suppression {
        info.controls
            .add(new_numeric(ControlId::MainBangSuppression, 0., 100.));
    }
    if cap.antenna_height {
        info.controls
            .add(new_numeric(ControlId::AntennaHeight, 0., 30.).wire_units(Units::Meters));
    }
    // The radar turns its own spokes by the alignment, so every model takes it.
    info.controls.add(
        new_numeric(ControlId::BearingAlignment, -180., 180.)
            .wire_scale_factor(10., true)
            .wire_offset(-1.)
            .wire_units(Units::Degrees),
    );
    if cap.tx_channel {
        info.controls.add(new_auto(
            ControlId::TransmitChannel,
            1.,
            TX_CHANNEL_MAX as f64,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    }

    // Interference and noise rejection (model-dependent levels)
    for (control_id, levels) in [
        (
            ControlId::InterferenceRejection,
            cap.interference_rejection_levels,
        ),
        (ControlId::NoiseRejection, cap.noise_rejection_levels),
    ] {
        if let Some(names) = rejection_levels(levels) {
            info.controls.add(new_list(control_id, names));
        }
    }

    // NXT-specific per-range features
    if cap.rez_boost {
        info.controls.add(new_list(
            ControlId::TargetSeparation,
            &["Off", "Low", "Medium", "High"],
        ));
    }
    if cap.bird_mode {
        info.controls.add(new_list(
            ControlId::BirdMode,
            &["Off", "Low", "Medium", "High"],
        ));
    }
    if cap.target_analyzer {
        let doppler_options = if cap.target_analyzer_rain {
            &["Off", "Target", "Rain"] as &[&str]
        } else {
            &["Off", "Target"]
        };
        info.controls
            .add(new_list(ControlId::Doppler, doppler_options));
        // NXT radars encode rain as a distinct class on the wire when
        // Target Analyzer is active. Reserve a legend slot for it.
        info.set_has_rain_class(true);
        // Each Doppler band carries a 4-bit intensity on the wire (16 levels).
        // Reserve 16 legend slots per direction so the UI can render an
        // intensity gradient instead of a single flat color.
        info.set_doppler_levels(16);
    }
    if !cap.sea_modes.is_empty() {
        info.controls
            .add(new_list(ControlId::SeaState, cap.sea_modes));
    }
    if cap.echo_average {
        info.controls.add(new_list(
            ControlId::ScanAverageMode,
            &["Off", "Low", "Medium", "High"],
        ));
    }
    if cap.watchman {
        info.controls
            .add(new_list(ControlId::TimedIdle, &["Off", "On"]));
        info.controls
            .add(new_numeric(ControlId::TimedRun, 10., 120.).wire_units(Units::Seconds));
    }
    if model.is_nxt() {
        info.controls
            .add(new_list(ControlId::EchoFormat, &["IMO", "Tile"]));
        for control_id in [ControlId::Supply12Voltage, ControlId::Supply5Voltage] {
            info.controls.add(
                new_numeric(control_id, 0., u16::MAX as f64)
                    .read_only(true)
                    .wire_scale_factor(SUPPLY_VOLTAGE_SCALE, true)
                    .wire_units(Units::Volts),
            );
        }
    }
    if cap.dual_range {
        info.dual_range = true;
    }
}

/// Range table for DRS-NXT series (in meters)
/// Ranges: 1/16, 1/8, 1/4, 1/2, 3/4, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32, 36, 48 NM
static RANGE_TABLE_DRS_NXT: &[i32] = &[
    FRAC_NM_16, // 1/16 NM
    FRAC_NM_8,  // 1/8 NM
    FRAC_NM_4,  // 1/4 NM
    FRAC_NM_2,  // 1/2 NM
    NM * 3 / 4, // 3/4 NM
    NM,         // 1 NM
    NM * 3 / 2, // 1.5 NM
    NM * 2,     // 2 NM
    NM * 3,     // 3 NM
    NM * 4,     // 4 NM
    NM * 6,     // 6 NM
    NM * 8,     // 8 NM
    NM * 12,    // 12 NM
    NM * 16,    // 16 NM
    NM * 24,    // 24 NM
    NM * 32,    // 32 NM
    NM * 36,    // 36 NM
    NM * 48,    // 48 NM
];

/// Extended range table for DRS12A/DRS25A-NXT (adds 64, 72, 96 NM)
static RANGE_TABLE_DRS_NXT_EXTENDED: &[i32] = &[
    FRAC_NM_16, // 1/16 NM
    FRAC_NM_8,  // 1/8 NM
    FRAC_NM_4,  // 1/4 NM
    FRAC_NM_2,  // 1/2 NM
    NM * 3 / 4, // 3/4 NM
    NM,         // 1 NM
    NM * 3 / 2, // 1.5 NM
    NM * 2,     // 2 NM
    NM * 3,     // 3 NM
    NM * 4,     // 4 NM
    NM * 6,     // 6 NM
    NM * 8,     // 8 NM
    NM * 12,    // 12 NM
    NM * 16,    // 16 NM
    NM * 24,    // 24 NM
    NM * 32,    // 32 NM
    NM * 36,    // 36 NM
    NM * 48,    // 48 NM
    NM * 64,    // 64 NM
    NM * 72,    // 72 NM
    NM * 96,    // 96 NM
];

/// Range table for DRS-NXT series in km mode (in meters)
/// Ranges: 0.125, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32, 36, 48, 64 km
/// Note: 0.0625 km is NOT available in km mode for DRS4D-NXT
static RANGE_TABLE_DRS_NXT_KM: &[i32] = &[
    125,   // 0.125 km
    250,   // 0.25 km
    500,   // 0.5 km
    750,   // 0.75 km
    1000,  // 1 km
    1500,  // 1.5 km
    2000,  // 2 km
    3000,  // 3 km
    4000,  // 4 km
    6000,  // 6 km
    8000,  // 8 km
    12000, // 12 km
    16000, // 16 km
    24000, // 24 km
    32000, // 32 km
    36000, // 36 km
    48000, // 48 km
    64000, // 64 km
];

/// Extended km range table for DRS12A/DRS25A-NXT (adds 72, 96 km)
static RANGE_TABLE_DRS_NXT_EXTENDED_KM: &[i32] = &[
    125,   // 0.125 km
    250,   // 0.25 km
    500,   // 0.5 km
    750,   // 0.75 km
    1000,  // 1 km
    1500,  // 1.5 km
    2000,  // 2 km
    3000,  // 3 km
    4000,  // 4 km
    6000,  // 6 km
    8000,  // 8 km
    12000, // 12 km
    16000, // 16 km
    24000, // 24 km
    32000, // 32 km
    36000, // 36 km
    48000, // 48 km
    64000, // 64 km
    72000, // 72 km
    96000, // 96 km
];

/// Range table for standard DRS series in km mode (up to 96 km)
static RANGE_TABLE_DRS_KM: &[i32] = &[
    125,   // 0.125 km
    250,   // 0.25 km
    500,   // 0.5 km
    750,   // 0.75 km
    1000,  // 1 km
    1500,  // 1.5 km
    2000,  // 2 km
    3000,  // 3 km
    4000,  // 4 km
    6000,  // 6 km
    8000,  // 8 km
    12000, // 12 km
    16000, // 16 km
    24000, // 24 km
    32000, // 32 km
    36000, // 36 km
    48000, // 48 km
    64000, // 64 km
    72000, // 72 km
    96000, // 96 km
];

/// Range table for FAR series in km mode
/// Missing: 0.0625km, 36km, 64km, 72km (same gaps as NM mode)
static RANGE_TABLE_FAR_KM: &[i32] = &[
    125,   // 0.125 km
    250,   // 0.25 km
    500,   // 0.5 km
    750,   // 0.75 km
    1000,  // 1 km
    1500,  // 1.5 km
    2000,  // 2 km
    3000,  // 3 km
    4000,  // 4 km
    6000,  // 6 km
    8000,  // 8 km
    12000, // 12 km
    16000, // 16 km
    24000, // 24 km
    32000, // 32 km
    48000, // 48 km
    96000, // 96 km
];

/// Range table for DRS4W WiFi radar (1/8–24 NM).
/// Matches the 14 ranges exposed by the Furuno DRS4W official app.
/// Wire index 21 (1/16 NM) is not supported on DRS4W; 32/36 NM are
/// out of reach for this 4 kW WiFi radar.
static RANGE_TABLE_DRS4W: &[i32] = &[
    FRAC_NM_8,  // 1/8 NM
    FRAC_NM_4,  // 1/4 NM
    FRAC_NM_2,  // 1/2 NM
    NM * 3 / 4, // 3/4 NM
    NM,         // 1 NM
    NM * 3 / 2, // 1.5 NM
    NM * 2,     // 2 NM
    NM * 3,     // 3 NM
    NM * 4,     // 4 NM
    NM * 6,     // 6 NM
    NM * 8,     // 8 NM
    NM * 12,    // 12 NM
    NM * 16,    // 16 NM
    NM * 24,    // 24 NM
];

/// Range table for DRS4W WiFi radar in km mode (0.125–24 km).
static RANGE_TABLE_DRS4W_KM: &[i32] = &[
    125,   // 0.125 km
    250,   // 0.25 km
    500,   // 0.5 km
    750,   // 0.75 km
    1000,  // 1 km
    1500,  // 1.5 km
    2000,  // 2 km
    3000,  // 3 km
    4000,  // 4 km
    6000,  // 6 km
    8000,  // 8 km
    12000, // 12 km
    16000, // 16 km
    24000, // 24 km
];

/// Range table for standard DRS series (non-NXT, up to 36 NM)
static RANGE_TABLE_DRS: &[i32] = &[
    FRAC_NM_16, // 1/16 NM
    FRAC_NM_8,  // 1/8 NM
    FRAC_NM_4,  // 1/4 NM
    FRAC_NM_2,  // 1/2 NM
    NM * 3 / 4, // 3/4 NM
    NM,         // 1 NM
    NM * 3 / 2, // 1.5 NM
    NM * 2,     // 2 NM
    NM * 3,     // 3 NM
    NM * 4,     // 4 NM
    NM * 6,     // 6 NM
    NM * 8,     // 8 NM
    NM * 12,    // 12 NM
    NM * 16,    // 16 NM
    NM * 24,    // 24 NM
    NM * 32,    // 32 NM
    NM * 36,    // 36 NM
];

/// Range table for FAR series commercial radars (different range increments)
static RANGE_TABLE_FAR: &[i32] = &[
    FRAC_NM_8,  // 1/8 NM
    FRAC_NM_4,  // 1/4 NM
    FRAC_NM_2,  // 1/2 NM
    NM * 3 / 4, // 3/4 NM
    NM,         // 1 NM
    NM * 3 / 2, // 1.5 NM
    NM * 2,     // 2 NM
    NM * 3,     // 3 NM
    NM * 4,     // 4 NM
    NM * 6,     // 6 NM
    NM * 8,     // 8 NM
    NM * 12,    // 12 NM
    NM * 16,    // 16 NM
    NM * 24,    // 24 NM
    NM * 32,    // 32 NM
    NM * 48,    // 48 NM
    NM * 96,    // 96 NM
];

/// The ranges of a specific model in every unit it ranges in. The NM and km
/// tables are classified into nautical and metric by their metres; statute
/// miles count the NM steps and kiloyards the km ones, at metres that cannot
/// be told apart from the others, so those are listed by unit.
fn ranges_by_model(model: &RadarModel) -> Ranges {
    let (nm_table, km_table): (&[i32], &[i32]) = match model {
        // DRS-NXT series with extended ranges
        RadarModel::DRS12ANXT | RadarModel::DRS25ANXT => (
            RANGE_TABLE_DRS_NXT_EXTENDED,
            RANGE_TABLE_DRS_NXT_EXTENDED_KM,
        ),

        // DRS-NXT series (standard)
        RadarModel::DRS2DNXT | RadarModel::DRS4DNXT | RadarModel::DRS6ANXT => {
            (RANGE_TABLE_DRS_NXT, RANGE_TABLE_DRS_NXT_KM)
        }

        // FAR series (commercial radars)
        RadarModel::FAR21x7
        | RadarModel::FAR3000
        | RadarModel::FAR15x3
        | RadarModel::FAR14x6
        | RadarModel::FAR14x7 => (RANGE_TABLE_FAR, RANGE_TABLE_FAR_KM),

        // DRS4W WiFi radar: 1/8–24 NM (matches official DRS4W app)
        RadarModel::DRS4W => (RANGE_TABLE_DRS4W, RANGE_TABLE_DRS4W_KM),

        // Standard DRS series and unknown models
        RadarModel::Unknown | RadarModel::DRS | RadarModel::DRS4DL | RadarModel::DRS6AXCLASS => {
            (RANGE_TABLE_DRS, RANGE_TABLE_DRS_KM)
        }
    };

    let mut ranges: Vec<i32> = Vec::with_capacity(nm_table.len() + km_table.len());
    ranges.extend_from_slice(nm_table);
    ranges.extend_from_slice(km_table);
    log::debug!(
        "Model {} supports {} NM + {} km ranges",
        model,
        nm_table.len(),
        km_table.len(),
    );
    let statute: Vec<i32> = nm_table.iter().copied().map(nautical_to_statute).collect();
    let kiloyards: Vec<i32> = km_table.iter().copied().map(metric_to_kiloyards).collect();
    Ranges::new_by_distance(&ranges).with_statute_and_kiloyards(&statute, &kiloyards)
}

/// This brand's controls for every model it knows, for the UI strings catalog
/// in [`crate::radar::ui_strings`].
#[cfg(test)]
pub(crate) fn controls_for_every_model(args: &Cli) -> Vec<SharedControls> {
    use strum::IntoEnumIterator;

    RadarModel::iter()
        .map(|model| {
            let mut info =
                crate::radar::ui_strings::radar_info(crate::Brand::Furuno, args, |id, tx| {
                    new(id, tx, args)
                });
            update_when_model_known(&mut info, model, "1.00");
            info.controls
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brand::furuno::protocol::{PIXEL_VALUES, SPOKE_LEN, SPOKES};
    use crate::radar::SharedRadars;
    use clap::Parser;
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::time::Duration;

    /// The controls a radar of `model` gets once its model is known.
    fn controls_of(model: RadarModel) -> SharedControls {
        let args = Cli::parse_from(["mayara-server"]);
        let mut info =
            crate::radar::ui_strings::radar_info(crate::Brand::Furuno, &args, |id, tx| {
                new(id, tx, &args)
            });
        update_when_model_known(&mut info, model, "1.00");
        info.controls
    }

    /// The NXT firmware applies only some STC curves and quietly puts the
    /// old one back for the rest, so only those are offered. Its noise
    /// rejection is stored but drives nothing, so it is not offered at all.
    /// The two NXT firmware images read agree on both.
    #[test]
    fn a_drs_nxt_offers_only_what_its_firmware_applies() {
        for model in [
            RadarModel::DRS2DNXT,
            RadarModel::DRS4DNXT,
            RadarModel::DRS6ANXT,
            RadarModel::DRS12ANXT,
            RadarModel::DRS25ANXT,
        ] {
            let controls = controls_of(model);
            let curves = |id| controls.get(&id).unwrap().item().valid_values.clone();

            assert_eq!(curves(ControlId::NearStcCurve), Some(vec![2, 3, 4, 5]));
            assert_eq!(curves(ControlId::MiddleStcCurve), Some(vec![0, 1]));
            assert_eq!(curves(ControlId::FarStcCurve), Some(vec![1, 3]));
            assert!(!controls.contains_key(&ControlId::NoiseRejection));
            let levels = |id| controls.get(&id).unwrap().item().max_value;
            assert_eq!(levels(ControlId::SeaState), Some(2.), "three modes");
            assert_eq!(levels(ControlId::ScanAverageMode), Some(3.), "four levels");
        }

        let other = controls_of(RadarModel::FAR15x3);
        let curves = other
            .get(&ControlId::FarStcCurve)
            .unwrap()
            .item()
            .valid_values
            .clone();
        assert_eq!(curves, Some(vec![0, 1, 2]));
        assert!(other.contains_key(&ControlId::NoiseRejection));
    }

    /// What each radar before the NXT offers, from its firmware: a control
    /// whose command the radar lacks or ignores is not offered, and one it
    /// applies is.
    #[test]
    fn a_drs_before_the_nxt_offers_what_its_firmware_applies() {
        let has = |controls: &SharedControls, id| controls.contains_key(&id);
        let levels = |controls: &SharedControls, id| {
            controls
                .get(&id)
                .map(|c| c.item().max_value.unwrap() as i32 + 1)
        };
        let highest =
            |controls: &SharedControls, id| controls.get(&id).and_then(|c| c.item().max_value);
        let stc = [
            ControlId::NearStcCurve,
            ControlId::MiddleStcCurve,
            ControlId::FarStcCurve,
        ];
        let blanking = [
            ControlId::NoTransmitSector1,
            ControlId::NoTransmitSector2,
            ControlId::MainBangSuppression,
        ];

        // Classic NavNet 3D DRS: no STC or anti-jamming command at all
        let classic = controls_of(RadarModel::DRS);
        assert!(stc.iter().all(|id| !has(&classic, *id)));
        assert!(!has(&classic, ControlId::AntiJamming));
        assert!(blanking.iter().all(|id| has(&classic, *id)));
        assert!(has(&classic, ControlId::ScanSpeed));
        assert!(has(&classic, ControlId::BirdMode));
        assert!(!has(&classic, ControlId::AntennaHeight));
        assert_eq!(levels(&classic, ControlId::InterferenceRejection), Some(2));
        assert_eq!(highest(&classic, ControlId::Tune), Some(2950.));

        // DRS4DL: STC is only echoed; three levels of each rejection
        let dl = controls_of(RadarModel::DRS4DL);
        assert!(stc.iter().all(|id| !has(&dl, *id)));
        assert!(blanking.iter().all(|id| has(&dl, *id)));
        assert!(!has(&dl, ControlId::ScanSpeed));
        let gain = serde_json::to_value(dl.get(&ControlId::Gain).unwrap().item()).unwrap();
        assert_eq!(gain["hasAuto"], true, "{gain}");
        assert_eq!(levels(&dl, ControlId::InterferenceRejection), Some(3));
        assert_eq!(levels(&dl, ControlId::NoiseRejection), Some(3));
        assert_eq!(highest(&dl, ControlId::Tune), Some(4096.));

        // X-Class: only the near STC band does anything
        let x_class = controls_of(RadarModel::DRS6AXCLASS);
        assert_eq!(
            x_class
                .get(&ControlId::NearStcCurve)
                .unwrap()
                .item()
                .valid_values,
            Some(vec![2, 3, 4, 5])
        );
        assert!(!has(&x_class, ControlId::MiddleStcCurve));
        assert!(!has(&x_class, ControlId::FarStcCurve));
        assert!(blanking.iter().all(|id| has(&x_class, *id)));
        assert!(has(&x_class, ControlId::ScanSpeed));
        assert!(has(&x_class, ControlId::AntennaHeight));
        assert_eq!(levels(&x_class, ControlId::InterferenceRejection), Some(4));
        assert!(!has(&x_class, ControlId::NoiseRejection));

        // Auto sea mode: Channel acts as Adaptive on the classic DRS. Echo
        // averaging is stored by all three but applied by none.
        assert_eq!(levels(&classic, ControlId::SeaState), Some(2));
        for controls in [&dl, &x_class] {
            assert_eq!(levels(controls, ControlId::SeaState), Some(3));
        }
        for controls in [&classic, &dl, &x_class] {
            assert!(!has(controls, ControlId::ScanAverageMode));
        }
        assert!(!has(&controls_of(RadarModel::DRS4W), ControlId::SeaState));
    }

    /// A sector control holds radians, and rounds to the step it was built
    /// with. Without a step of its own it would round to whole radians, and
    /// a sector the radar reported at 100 degrees would be kept as 57.
    #[test]
    fn a_sector_keeps_the_degree_the_radar_reported() {
        let radars = SharedRadars::new();
        let args = Cli::parse_from(["mayara-server"]);
        let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 10000);
        let mut info = RadarInfo::new(
            &radars,
            &args,
            crate::Brand::Furuno,
            Some("TEST0001"),
            None,
            None,
            PIXEL_VALUES,
            SPOKES,
            SPOKE_LEN,
            addr,
            Ipv4Addr::new(10, 0, 0, 1),
            addr,
            addr,
            addr,
            |id, tx| new(id, tx, &args),
            true,
            true,
        );
        info.controls.set_user_name(info.key());
        update_when_model_known(&mut info, RadarModel::DRS4DNXT, "1.00");

        info.controls
            .set_sector(&ControlId::NoTransmitSector1, 100., 130., Some(true))
            .expect("the sector the radar reported");

        let sector = info.controls.get(&ControlId::NoTransmitSector1).unwrap();
        assert_eq!(sector.value.unwrap().to_degrees().round(), 100.);
        assert_eq!(sector.end_value.unwrap().to_degrees().round(), 130.);
    }

    /// A control is seeded with its default before the unit conversion, so
    /// the seed has to be converted too: a sector no radar has reported yet
    /// still reads as the -180 degrees it was declared with.
    #[test]
    fn an_unreported_sector_reads_as_the_degrees_it_was_declared_with() {
        let mut controls = HashMap::new();
        new_sector(ControlId::NoTransmitSector1, -180., 180.)
            .wire_scale_step(1.)
            .wire_offset(-1.)
            .wire_units(Units::Degrees)
            .has_enabled()
            .build(&mut controls);

        let sector = &controls[&ControlId::NoTransmitSector1];
        assert_eq!(sector.value.unwrap().to_degrees().round(), -180.);
    }

    /// Every Furuno radar is told its alignment.
    #[test]
    fn every_model_has_a_bearing_alignment() {
        let args = Cli::parse_from(["mayara-server"]);

        for controls in controls_for_every_model(&args) {
            assert!(
                controls.contains_key(&ControlId::BearingAlignment),
                "{:?}",
                controls.model_name()
            );
        }
    }

    /// Only the solid-state NXT lets the user pick the channel it transmits
    /// on.
    #[test]
    fn only_the_nxt_has_a_transmit_channel() {
        use strum::IntoEnumIterator;
        let args = Cli::parse_from(["mayara-server"]);

        for (model, controls) in RadarModel::iter().zip(controls_for_every_model(&args)) {
            assert_eq!(
                controls.contains_key(&ControlId::TransmitChannel),
                model.is_nxt(),
                "{model}"
            );
        }
    }

    /// The receiver sends a Standby for a transmit that was ours while
    /// standing down, so the control is offered, enabled at its default.
    #[test]
    fn furuno_offers_auto_standby_at_one_minute() {
        let args = Cli::parse_from(["mayara-server"]);
        let tx = tokio::sync::broadcast::Sender::new(1);
        let controls = new("fur1234".to_string(), tx, &args);
        assert_eq!(controls.auto_standby(), Some(Duration::from_secs(60)));
    }

    /// In statute miles a Furuno offers the nautical mile steps, and in
    /// kiloyards the kilometre ones, without the 1/16 kilometres lack. The
    /// ranges shown are the numbers on the radar's own range key.
    #[test]
    fn a_furuno_ranges_in_statute_miles_and_kiloyards() {
        let controls = controls_of(RadarModel::DRS4DNXT);
        let offered = |units: i32| {
            controls
                .set_value(&ControlId::RangeUnits, units.into())
                .unwrap();
            let item = controls.get(&ControlId::Range).unwrap().item().clone();
            let descriptions = item.descriptions.unwrap();
            item.valid_values
                .unwrap()
                .iter()
                .map(|v| (*v, descriptions[v].clone()))
                .collect::<Vec<_>>()
        };

        let statute = offered(RANGE_UNITS_STATUTE);
        assert_eq!(statute.len(), RANGE_TABLE_DRS_NXT.len());
        assert_eq!(statute[0], (100, "1/16 mi".to_string()));
        assert_eq!(statute[7], (3219, "2 mi".to_string()));
        assert_eq!(statute.last().unwrap().1, "48 mi");

        let kiloyards = offered(RANGE_UNITS_KILOYARDS);
        assert_eq!(kiloyards.len(), RANGE_TABLE_DRS_NXT_KM.len());
        assert_eq!(kiloyards[0], (114, "0.125 kyd".to_string()));
        assert_eq!(kiloyards[6], (1829, "2 kyd".to_string()));

        let nautical = offered(RANGE_UNITS_NAUTICAL);
        assert_eq!(nautical.len(), RANGE_TABLE_DRS_NXT.len());
        assert_eq!(nautical[7], (3704, "2 nm".to_string()));
    }

    /// Mixed has no wire unit, so a Furuno does not offer it.
    #[test]
    fn a_furuno_offers_every_range_unit_but_mixed() {
        let args = Cli::parse_from(["mayara-server"]);
        let tx = tokio::sync::broadcast::Sender::new(1);
        let controls = new("fur1234".to_string(), tx, &args);
        let item = controls.get(&ControlId::RangeUnits).unwrap().item().clone();

        assert_eq!(item.valid_values, Some(vec![0, 1, 3, 4]));
        assert_eq!(item.descriptions.unwrap()[&3], "Statute");
    }
}
