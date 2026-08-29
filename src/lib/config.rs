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

pub(crate) fn get_project_dirs() -> ProjectDirs {
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
pub(crate) struct Radar {
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
pub(crate) struct Config {
    pub radars: HashMap<String, Radar>,
}

#[derive(Debug, Clone)]
pub(crate) struct Persistence {
    pub config: Config,
    timestamp: SystemTime,
    path: PathBuf,
}

const SETTINGS_FILE: &str = "settings.json";

/// The settings file to read and write, or `None` when the config directory
/// cannot be created. A radar works fine without one, so an unwritable
/// config directory -- a read-only mount, a host directory owned by another
/// user -- costs the user their remembered settings, never their radar.
fn settings_path(config_dir: &std::path::Path) -> Option<PathBuf> {
    if let Err(e) = fs::create_dir_all(config_dir) {
        warn!(
            "Cannot create settings directory '{}': {}",
            config_dir.display(),
            e
        );
        warn!("Radar names, guard zones and other settings will not be remembered");
        return None;
    }
    Some(config_dir.join(SETTINGS_FILE))
}

impl Persistence {
    /// A persistence that remembers nothing, for a run that has nowhere to
    /// write. An empty path is the marker every write path already checks.
    fn disabled() -> Self {
        Persistence {
            config: Config {
                radars: HashMap::new(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: PathBuf::new(),
        }
    }

    pub(crate) fn new() -> Self {
        if crate::replay::is_active() {
            debug!("persistence disabled in pcap replay mode");
            return Self::disabled();
        }

        let Some(settings_path) = settings_path(get_project_dirs().config_dir()) else {
            return Self::disabled();
        };

        let mut this = Persistence {
            config: Config {
                radars: HashMap::new(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: settings_path,
        };

        if !this.load() {
            warn!("Radar names, guard zones and other settings will not be remembered");
            return Self::disabled();
        }
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
            self.path.display()
        );
    }

    /// Returns whether the settings file can be used. A first run writes an
    /// empty one, which doubles as the check that this run can write at all:
    /// `create_dir_all` succeeds on a directory that already exists but
    /// belongs to another user, so the write is what finds that out.
    fn load(&mut self) -> bool {
        let file = match File::open(&self.path) {
            Err(e) => {
                warn!(
                    "no config '{}' yet; starting fresh: {}",
                    self.path.display(),
                    e
                );

                return self.save();
            }
            Ok(f) => f,
        };

        let reader = BufReader::new(file);

        match serde_json::from_reader(reader) {
            Ok(u) => {
                self.config = u;
                info!("Loaded config from '{}'", self.path.display());
            }
            Err(e) => {
                warn!(
                    "Config '{}' corrupted; starting fresh: {}",
                    self.path.display(),
                    e
                );
            }
        };

        self.timestamp = self.get_file_time();
        true
    }

    fn saver(&mut self) -> Result<(), Box<dyn Error>> {
        let file = File::create(&self.path)?;

        let mut writer = BufWriter::new(&file);

        serde_json::to_writer_pretty(writer.by_ref(), &self.config)?;
        writeln!(writer)?;
        writer.flush()?;

        info!("Written config file '{}'", self.path.display());
        self.timestamp = self.get_file_time();
        Ok(())
    }

    fn save(&mut self) -> bool {
        match self.saver() {
            Err(e) => {
                warn!("cannot store config '{}': {}", self.path.display(), e);
                false
            }
            Ok(()) => true,
        }
    }

    pub(crate) fn store(&mut self, radar_info: &RadarInfo) {
        if self.path.as_os_str().is_empty() {
            return; // Pcap replay mode — no persistence
        }
        let mut modified = false;

        let radar = self.config.radars.entry(radar_info.key()).or_default();

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
            let _ = self.save();
        }
    }

    /// Move settings saved under a radar's address-derived key across to
    /// the key it is known by now, once.
    ///
    /// The move matters as much as the copy: leaving the old entry behind
    /// would have every later start find it again, and would let a
    /// different radar that happens to take the same address inherit the
    /// settings of the one it replaced.
    fn take_legacy_entry(&mut self, key: &str, legacy: &str) -> bool {
        if legacy == key || self.config.radars.contains_key(key) {
            return false;
        }
        let Some(entry) = self.config.radars.remove(legacy) else {
            return false;
        };
        log::info!(
            "{}: adopting the settings saved under its previous key '{}'",
            key,
            legacy
        );
        self.config.radars.insert(key.to_string(), entry);
        if !self.path.as_os_str().is_empty() {
            let _ = self.save();
        }
        true
    }

    pub(crate) fn update_info_from_persistence(&mut self, info: &mut RadarInfo) {
        self.take_legacy_entry(&info.key(), &crate::radar::legacy_address_key(info));

        if let Some(p) = self.config.radars.get(&info.key()) {
            if let Some(model_name) = p.model_name.as_ref() {
                info.controls.set_model_name(model_name.clone());
            }
            info.controls.set_user_name(p.user_name.clone());
            info.controls.set_spoke_processing(p.spoke_processing);
            info.controls.set_range_units(p.range_units);
            if let Some(ranges) = &p.ranges
                && !ranges.is_empty()
            {
                info.set_ranges(Ranges::new_by_distance(ranges));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn persistence_with(key: &str, user_name: &str) -> Persistence {
        let mut radars = HashMap::new();
        radars.insert(
            key.to_string(),
            Radar {
                user_name: user_name.to_string(),
                ..Default::default()
            },
        );
        Persistence {
            config: Config { radars },
            ..Persistence::disabled()
        }
    }

    /// Settings follow a radar to its new key exactly once, and the old
    /// entry does not linger: leaving it would have every later start find
    /// it again, and would let a different radar that later takes the same
    /// address inherit the settings of the one it replaced.
    #[test]
    fn legacy_settings_move_to_the_stable_key() {
        let mut p = persistence_with("kod0102", "Bow Radar");

        assert!(p.take_legacy_entry("kod3456", "kod0102"));
        assert_eq!(
            p.config.radars.get("kod3456").map(|r| r.user_name.as_str()),
            Some("Bow Radar")
        );
        assert!(
            !p.config.radars.contains_key("kod0102"),
            "the old entry must not linger"
        );

        assert!(
            !p.take_legacy_entry("kod3456", "kod0102"),
            "a second start has nothing left to move"
        );
    }

    /// A radar that already has settings under its stable key keeps them:
    /// whatever is under an address key is older and must not win.
    #[test]
    fn an_existing_stable_entry_is_not_overwritten() {
        let mut p = persistence_with("kod0102", "Old Name");
        p.config.radars.insert(
            "kod3456".to_string(),
            Radar {
                user_name: "Current Name".to_string(),
                ..Default::default()
            },
        );

        assert!(!p.take_legacy_entry("kod3456", "kod0102"));
        assert_eq!(
            p.config.radars.get("kod3456").map(|r| r.user_name.as_str()),
            Some("Current Name")
        );
    }

    /// Brands that never had an identity key on their address as before,
    /// so there is nothing to move.
    #[test]
    fn a_radar_still_keyed_on_its_address_moves_nothing() {
        let mut p = persistence_with("gar0102", "Radar");
        assert!(!p.take_legacy_entry("gar0102", "gar0102"));
        assert!(p.config.radars.contains_key("gar0102"));
    }

    /// The usual case: the config directory does not exist yet on first run.
    #[test]
    fn a_missing_config_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("mayara");

        let path = settings_path(&config_dir).expect("first run must get a settings file");

        assert_eq!(path, config_dir.join(SETTINGS_FILE));
        assert!(config_dir.is_dir());
    }

    /// A radar must keep working when its settings cannot be stored: the
    /// config directory may be a read-only mount, or a host directory owned
    /// by another user. Blocked by a plain file standing where the directory
    /// should be, which no user -- root included -- can create through.
    #[test]
    fn an_unusable_config_directory_disables_persistence_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();

        assert!(settings_path(&blocker.join("mayara")).is_none());
    }

    fn persistence_at(path: PathBuf) -> Persistence {
        Persistence {
            path,
            ..Persistence::disabled()
        }
    }

    /// First run: no settings file yet, so one is written and kept.
    #[test]
    fn a_first_run_writes_the_settings_file_it_found_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);

        assert!(persistence_at(path.clone()).load());
        assert!(path.is_file());
    }

    /// An existing directory says nothing about being able to write in it --
    /// it may belong to another user -- so a settings file that cannot be
    /// created has to disable persistence too, or every later save retries a
    /// write that cannot succeed.
    #[test]
    fn a_settings_file_that_cannot_be_written_disables_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();

        assert!(!persistence_at(blocker.join(SETTINGS_FILE)).load());
    }
}
