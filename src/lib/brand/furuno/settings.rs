use std::collections::HashMap;

use crate::{
    Cli,
    radar::settings::{
        ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, new_auto, new_list, new_numeric,
        new_sector, new_string,
    },
    radar::{RadarInfo, range::Ranges, units::Units},
    stream::SignalKDelta,
};

use super::protocol::RadarModel;

pub fn new(
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
    new_list(ControlId::RangeUnits, &["Nautical", "Metric"]).build(&mut controls);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

/// Per-model capability flags, derived from MaxSea.Radar.dll capability table.
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
    noise_rejection: bool,
    interference_rejection_levels: u8, // 0=none, 2=off/on, 4=off/low/med/high
    tune: bool,
    antenna_height: bool,
    watchman: bool,
    pulse_width: bool,
}

fn capabilities(model: &RadarModel) -> Capabilities {
    match model {
        // NXT series: full feature set
        RadarModel::DRS4DNXT
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
            noise_rejection: true,
            interference_rejection_levels: 2,
            tune: true,
            antenna_height: true,
            watchman: true,
            pulse_width: false, // solid-state, no selectable pulse
        },
        // DRS6A X-Class: DRS + bird mode
        RadarModel::DRS6AXCLASS => Capabilities {
            auto_gain: true,
            auto_sea: true,
            auto_rain: true,
            dual_range: false,
            scan_speed: false,
            sector_blanking: false,
            main_bang_suppression: false,
            rez_boost: false,
            bird_mode: true,
            target_analyzer: false,
            target_analyzer_rain: false,
            noise_rejection: false,
            interference_rejection_levels: 0,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: true,
        },
        // DRS4DL: limited (no auto gain, no scan speed, no dual range)
        RadarModel::DRS4DL => Capabilities {
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
            noise_rejection: false,
            interference_rejection_levels: 2,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: true,
        },
        // Standard DRS
        RadarModel::DRS | RadarModel::DRS4W => Capabilities {
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
            noise_rejection: false,
            interference_rejection_levels: 0,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: false, // DRS4W firmware disables pulse width
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
            noise_rejection: false,
            interference_rejection_levels: 4,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: true,
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
            noise_rejection: true,
            interference_rejection_levels: 4,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: true,
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
            noise_rejection: false,
            interference_rejection_levels: 0,
            tune: true,
            antenna_height: false,
            watchman: false,
            pulse_width: false,
        },
    }
}

pub fn update_when_model_known(info: &mut RadarInfo, model: RadarModel, version: &str) {
    let model_name = model.to_string();
    log::debug!("update_when_model_known: {}", model_name);
    info.controls.set_model_name(model_name.to_string());

    if info.controls.user_name() == info.key() {
        let mut user_name = model_name.to_string();
        if let Some(serial_no) = info.serial_no.as_deref() {
            user_name.push(' ');
            user_name.push_str(&serial_no[serial_no.len().saturating_sub(4)..]);
        }
        info.controls.set_user_name(user_name);
    }

    let ranges = Ranges::new_by_distance(&get_ranges_by_model(&model));
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
        info.controls
            .update_definition(new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE));
    } else {
        info.controls.update_definition(new_numeric(ControlId::Gain, 0., 100.));
    }
    if cap.auto_sea {
        info.controls
            .update_definition(new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE));
    } else {
        info.controls.update_definition(new_numeric(ControlId::Sea, 0., 100.));
    }
    if cap.auto_rain {
        info.controls
            .update_definition(new_auto(ControlId::Rain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE));
    } else {
        info.controls.update_definition(new_numeric(ControlId::Rain, 0., 100.));
    }

    info.controls.add(new_string(ControlId::FirmwareVersion));
    info.controls
        .set_string(&ControlId::FirmwareVersion, version.to_string())
        .expect("FirmwareVersion");
    if cap.pulse_width {
        info.controls.add(new_string(ControlId::PulseWidth));
    }

    // STC (Sensitivity Time Control) — all models support this
    info.controls
        .add(new_list(ControlId::NearStcCurve, &["0", "1", "2", "3", "4"]));
    info.controls
        .add(new_list(ControlId::MiddleStcCurve, &["0", "1", "2", "3"]));
    info.controls
        .add(new_list(ControlId::FarStcCurve, &["0", "1", "2"]));
    // StcRange ($RD2) is not registered: DRS4D-NXT does not respond to $RD2.
    // Re-enable when a model that supports it is available for testing.

    info.controls
        .add(new_list(ControlId::AntiJamming, &["Off", "On"]));

    // Tuning
    if cap.tune {
        info.controls.add(new_auto(
            ControlId::Tune,
            0.,
            2000.,
            HAS_AUTO_NOT_ADJUSTABLE,
        ));
    }

    // Antenna controls (shared across ranges)
    if cap.sector_blanking {
        info.controls.add(
            new_sector(ControlId::NoTransmitSector1, -180., 180.)
                .wire_offset(-1.)
                .wire_units(Units::Degrees)
                .has_enabled(),
        );
        info.controls.add(
            new_sector(ControlId::NoTransmitSector2, -180., 180.)
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

    // Interference rejection (model-dependent levels)
    match cap.interference_rejection_levels {
        2 => {
            info.controls
                .add(new_list(ControlId::InterferenceRejection, &["Off", "On"]));
        }
        4 => {
            info.controls.add(new_list(
                ControlId::InterferenceRejection,
                &["Off", "Low", "Medium", "High"],
            ));
        }
        _ => {}
    }

    // NXT-specific per-range features
    if cap.noise_rejection {
        info.controls
            .add(new_list(ControlId::NoiseRejection, &["Off", "On"]));
    }
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
    }
    if cap.watchman {
        info.controls
            .add(new_list(ControlId::TimedIdle, &["Off", "On"]));
        info.controls
            .add(new_numeric(ControlId::TimedRun, 10., 120.).wire_units(Units::Seconds));
    }
    if cap.dual_range {
        info.dual_range = true;
    }
}

/// Range table for DRS-NXT series (in meters)
/// Ranges: 1/16, 1/8, 1/4, 1/2, 3/4, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32, 36, 48 NM
static RANGE_TABLE_DRS_NXT: &[i32] = &[
    116,   // 1/16 NM
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    59264, // 32 NM
    66672, // 36 NM
    88896, // 48 NM
];

/// Extended range table for DRS12A/DRS25A-NXT (adds 64, 72, 96 NM)
static RANGE_TABLE_DRS_NXT_EXTENDED: &[i32] = &[
    116,    // 1/16 NM
    231,    // 1/8 NM
    463,    // 1/4 NM
    926,    // 1/2 NM
    1389,   // 3/4 NM
    1852,   // 1 NM
    2778,   // 1.5 NM
    3704,   // 2 NM
    5556,   // 3 NM
    7408,   // 4 NM
    11112,  // 6 NM
    14816,  // 8 NM
    22224,  // 12 NM
    29632,  // 16 NM
    44448,  // 24 NM
    59264,  // 32 NM
    66672,  // 36 NM
    88896,  // 48 NM
    118528, // 64 NM
    133344, // 72 NM
    177792, // 96 NM
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
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
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
    116,   // 1/16 NM
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    59264, // 32 NM
    66672, // 36 NM
];

/// Range table for FAR series commercial radars (different range increments)
static RANGE_TABLE_FAR: &[i32] = &[
    231,    // 1/8 NM
    463,    // 1/4 NM
    926,    // 1/2 NM
    1389,   // 3/4 NM
    1852,   // 1 NM
    2778,   // 1.5 NM
    3704,   // 2 NM
    5556,   // 3 NM
    7408,   // 4 NM
    11112,  // 6 NM
    14816,  // 8 NM
    22224,  // 12 NM
    29632,  // 16 NM
    44448,  // 24 NM
    59264,  // 32 NM
    88896,  // 48 NM
    177792, // 96 NM
];

/// Get the combined NM + km range table for a specific model.
/// Both unit modes are included so the Ranges struct can auto-classify
/// them into nautical/metric lists for RangeUnits filtering.
fn get_ranges_by_model(model: &RadarModel) -> Vec<i32> {
    let (nm_table, km_table): (&[i32], &[i32]) = match model {
        // DRS-NXT series with extended ranges
        RadarModel::DRS12ANXT | RadarModel::DRS25ANXT => (
            RANGE_TABLE_DRS_NXT_EXTENDED,
            RANGE_TABLE_DRS_NXT_EXTENDED_KM,
        ),

        // DRS-NXT series (standard)
        RadarModel::DRS4DNXT | RadarModel::DRS6ANXT => {
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
        RadarModel::Unknown
        | RadarModel::DRS
        | RadarModel::DRS4DL
        | RadarModel::DRS6AXCLASS => (RANGE_TABLE_DRS, RANGE_TABLE_DRS_KM),
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
    ranges
}
