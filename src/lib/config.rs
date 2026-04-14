use directories::ProjectDirs;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::radar::RadarInfo;
use crate::radar::range::Ranges;
use crate::radar::settings::ControlId;

pub fn get_project_dirs() -> ProjectDirs {
    directories::ProjectDirs::from("net", "verruijt", "mayara")
        .expect("Cannot find project directories")
}

pub(crate) fn default_range_units() -> i32 {
    0 // Nautical (default)
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct GuardZone {
    pub start_angle: f64,    // Start angle in radians (SI)
    pub end_angle: f64,      // End angle in radians (SI)
    pub start_distance: f64, // Inner distance in meters
    pub end_distance: f64,   // Outer distance in meters
    pub enabled: bool,
}

/// Exclusion zone for stationary radar installations.
/// Areas within exclusion zones are displayed differently and targets are not tracked.
pub type ExclusionZone = GuardZone;

/// Rectangular exclusion zone for stationary radar installations.
/// Defined by two corners (x1,y1) and (x2,y2) that form one edge, plus a width.
/// Coordinates are in meters from radar position (x=east, y=north).
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct ExclusionRect {
    pub x1: f64,    // First corner X (meters from radar, positive = east)
    pub y1: f64,    // First corner Y (meters from radar, positive = north)
    pub x2: f64,    // Second corner X (defines one edge with first corner)
    pub y2: f64,    // Second corner Y (defines one edge with first corner)
    pub width: f64, // Perpendicular width of rectangle (always positive)
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Radar {
    pub id: usize,
    pub user_name: String,
    #[serde(default)]
    pub spoke_processing: i32, // 0 = Clean, 1 = Fill, 2 = Reduce, 3 = Smooth
    #[serde(default = "default_range_units")]
    pub range_units: i32, // 0 = Nautical, 1 = Metric, 2 = Mixed

    // Data that is computed and not immediately known when starting
    pub model_name: Option<String>, // Descriptive model name (4G, HALO)
    pub ranges: Option<Vec<i32>>,   // Detected ranges

    // Guard zones
    #[serde(default)]
    pub guard_zone_1: Option<GuardZone>,
    #[serde(default)]
    pub guard_zone_2: Option<GuardZone>,

    // Exclusion zones (stationary installations only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_zone_1: Option<ExclusionZone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_zone_2: Option<ExclusionZone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_zone_3: Option<ExclusionZone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_zone_4: Option<ExclusionZone>,

    // Rectangular exclusion zones (stationary installations only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_rect_1: Option<ExclusionRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_rect_2: Option<ExclusionRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_rect_3: Option<ExclusionRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_rect_4: Option<ExclusionRect>,

    // ARPA/Target tracking settings
    #[serde(default)]
    pub arpa_max_speed: i32, // 0 = Normal (25kn), 1 = Medium (40kn), 2 = Fast (50kn)
    #[serde(default)]
    pub doppler_auto_track: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Config {
    pub radars: HashMap<String, Radar>,
}

#[derive(Debug, Clone)]
pub(crate) struct Persistence {
    pub config: Config,
    timestamp: SystemTime,
    path: PathBuf,
}

impl Persistence {
    pub fn new() -> Self {
        if crate::replay::is_active() {
            debug!("persistence disabled in pcap replay mode");
            return Persistence {
                config: Config {
                    radars: HashMap::new(),
                },
                timestamp: SystemTime::UNIX_EPOCH,
                path: PathBuf::new(),
            };
        }

        let project_dirs = get_project_dirs();
        let mut settings_path = project_dirs.config_dir().to_owned();
        fs::create_dir_all(&settings_path).expect("Cannot create settings directory");
        settings_path.push("settings.json");

        let mut this = Persistence {
            config: Config {
                radars: HashMap::new(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: settings_path,
        };

        this.load();
        debug!("persistence loaded: {:?}", this);

        this
    }

    fn get_file_time(&self) -> SystemTime {
        let metadata = fs::metadata(&self.path);

        match metadata {
            Ok(data) => {
                if let Ok(time) = data.modified() {
                    return time;
                }
            }
            Err(e) => {
                error!("{e}");
            }
        }

        panic!(
            "Cannot check file modification of '{}' on this platform",
            &self.path.display()
        );
    }

    fn load(&mut self) {
        let file = match File::open(&self.path) {
            Err(e) => {
                warn!(
                    "no config '{}' yet; starting fresh: {}",
                    &self.path.display(),
                    e
                );

                self.save();
                return;
            }
            Ok(f) => f,
        };

        let reader = BufReader::new(file);

        match serde_json::from_reader(reader) {
            Ok(u) => {
                self.config = u;
                info!("Loaded config from '{}'", &self.path.display());
            }
            Err(e) => {
                warn!(
                    "Config '{}' corrupted; starting fresh: {}",
                    &self.path.display(),
                    e
                );
            }
        };

        self.timestamp = self.get_file_time();
    }

    fn saver(&mut self) -> Result<(), Box<dyn Error>> {
        let file = File::create(&self.path)?;

        let mut writer = BufWriter::new(&file);

        serde_json::to_writer_pretty(writer.by_ref(), &self.config)?;
        write!(writer, "\n")?;
        writer.flush()?;

        info!("Written config file '{}'", &self.path.display());
        self.timestamp = self.get_file_time();
        Ok(())
    }

    fn save(&mut self) {
        match self.saver() {
            Err(e) => {
                warn!("cannot store config '{}': {}", &self.path.display(), e);
                return;
            }
            Ok(_) => {}
        };
    }

    pub fn store(&mut self, radar_info: &RadarInfo) {
        if self.path.as_os_str().is_empty() {
            return; // Pcap replay mode — no persistence
        }
        let mut modified = false;

        let radar = self
            .config
            .radars
            .entry(radar_info.key())
            .or_insert(Radar::default());

        let user_name = radar_info.controls.user_name();
        if radar.user_name != user_name {
            radar.user_name = user_name;
            modified = true;
        }
        let spoke_processing = radar_info.controls.spoke_processing();
        if radar.spoke_processing != spoke_processing {
            radar.spoke_processing = spoke_processing;
            modified = true;
        }
        let range_units = radar_info.controls.range_units();
        if radar.range_units != range_units {
            radar.range_units = range_units;
            modified = true;
        }
        let ranges = Some(radar_info.ranges.all.iter().map(|r| r.distance()).collect());
        if radar.ranges != ranges {
            radar.ranges = ranges;
            modified = true;
        }

        let model_name = radar_info.controls.model_name();
        if radar.model_name != model_name {
            radar.model_name = model_name;
            modified = true;
        }

        let guard_zone_1 = radar_info.controls.guard_zone(&ControlId::GuardZone1);
        if radar.guard_zone_1 != guard_zone_1 {
            radar.guard_zone_1 = guard_zone_1;
            modified = true;
        }

        let guard_zone_2 = radar_info.controls.guard_zone(&ControlId::GuardZone2);
        if radar.guard_zone_2 != guard_zone_2 {
            radar.guard_zone_2 = guard_zone_2;
            modified = true;
        }

        // Only persist enabled exclusion zones to keep settings.json clean
        let exclusion_zone_1 = radar_info
            .controls
            .exclusion_zone(&ControlId::ExclusionZone1)
            .filter(|z| z.enabled);
        if radar.exclusion_zone_1 != exclusion_zone_1 {
            radar.exclusion_zone_1 = exclusion_zone_1;
            modified = true;
        }

        let exclusion_zone_2 = radar_info
            .controls
            .exclusion_zone(&ControlId::ExclusionZone2)
            .filter(|z| z.enabled);
        if radar.exclusion_zone_2 != exclusion_zone_2 {
            radar.exclusion_zone_2 = exclusion_zone_2;
            modified = true;
        }

        let exclusion_zone_3 = radar_info
            .controls
            .exclusion_zone(&ControlId::ExclusionZone3)
            .filter(|z| z.enabled);
        if radar.exclusion_zone_3 != exclusion_zone_3 {
            radar.exclusion_zone_3 = exclusion_zone_3;
            modified = true;
        }

        let exclusion_zone_4 = radar_info
            .controls
            .exclusion_zone(&ControlId::ExclusionZone4)
            .filter(|z| z.enabled);
        if radar.exclusion_zone_4 != exclusion_zone_4 {
            radar.exclusion_zone_4 = exclusion_zone_4;
            modified = true;
        }

        // Only persist enabled exclusion rects to keep settings.json clean
        let exclusion_rect_1 = radar_info
            .controls
            .exclusion_rect(&ControlId::ExclusionRect1)
            .filter(|r| r.enabled);
        if radar.exclusion_rect_1 != exclusion_rect_1 {
            radar.exclusion_rect_1 = exclusion_rect_1;
            modified = true;
        }

        let exclusion_rect_2 = radar_info
            .controls
            .exclusion_rect(&ControlId::ExclusionRect2)
            .filter(|r| r.enabled);
        if radar.exclusion_rect_2 != exclusion_rect_2 {
            radar.exclusion_rect_2 = exclusion_rect_2;
            modified = true;
        }

        let exclusion_rect_3 = radar_info
            .controls
            .exclusion_rect(&ControlId::ExclusionRect3)
            .filter(|r| r.enabled);
        if radar.exclusion_rect_3 != exclusion_rect_3 {
            radar.exclusion_rect_3 = exclusion_rect_3;
            modified = true;
        }

        let exclusion_rect_4 = radar_info
            .controls
            .exclusion_rect(&ControlId::ExclusionRect4)
            .filter(|r| r.enabled);
        if radar.exclusion_rect_4 != exclusion_rect_4 {
            radar.exclusion_rect_4 = exclusion_rect_4;
            modified = true;
        }

        let arpa_max_speed = radar_info.controls.arpa_detect_max_speed();
        if radar.arpa_max_speed != arpa_max_speed {
            radar.arpa_max_speed = arpa_max_speed;
            modified = true;
        }

        let doppler_auto_track = radar_info.controls.doppler_auto_track();
        if radar.doppler_auto_track != doppler_auto_track {
            radar.doppler_auto_track = doppler_auto_track;
            modified = true;
        }

        if modified {
            self.save();
        }
    }

    pub fn update_info_from_persistence(&self, info: &mut RadarInfo) {
        if let Some(p) = self.config.radars.get(&info.key()) {
            if p.model_name.is_some() {
                info.controls
                    .set_model_name(p.model_name.as_ref().unwrap().clone());
            }
            info.controls.set_user_name(p.user_name.clone());
            info.controls.set_spoke_processing(p.spoke_processing);
            info.controls.set_range_units(p.range_units);
            if let Some(ranges) = &p.ranges {
                if ranges.len() > 0 {
                    info.set_ranges(Ranges::new_by_distance(ranges));
                }
            }
            if let Some(zone) = &p.guard_zone_1 {
                info.controls.set_guard_zone(&ControlId::GuardZone1, zone);
            }
            if let Some(zone) = &p.guard_zone_2 {
                info.controls.set_guard_zone(&ControlId::GuardZone2, zone);
            }
            if let Some(zone) = &p.exclusion_zone_1 {
                info.controls
                    .set_exclusion_zone(&ControlId::ExclusionZone1, zone);
            }
            if let Some(zone) = &p.exclusion_zone_2 {
                info.controls
                    .set_exclusion_zone(&ControlId::ExclusionZone2, zone);
            }
            if let Some(zone) = &p.exclusion_zone_3 {
                info.controls
                    .set_exclusion_zone(&ControlId::ExclusionZone3, zone);
            }
            if let Some(zone) = &p.exclusion_zone_4 {
                info.controls
                    .set_exclusion_zone(&ControlId::ExclusionZone4, zone);
            }
            if let Some(rect) = &p.exclusion_rect_1 {
                info.controls
                    .set_exclusion_rect(&ControlId::ExclusionRect1, rect);
            }
            if let Some(rect) = &p.exclusion_rect_2 {
                info.controls
                    .set_exclusion_rect(&ControlId::ExclusionRect2, rect);
            }
            if let Some(rect) = &p.exclusion_rect_3 {
                info.controls
                    .set_exclusion_rect(&ControlId::ExclusionRect3, rect);
            }
            if let Some(rect) = &p.exclusion_rect_4 {
                info.controls
                    .set_exclusion_rect(&ControlId::ExclusionRect4, rect);
            }
            info.controls.set_arpa_max_speed(p.arpa_max_speed);
            info.controls.set_doppler_auto_track(p.doppler_auto_track);
        }
    }
}
