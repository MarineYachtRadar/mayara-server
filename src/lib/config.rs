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

use crate::Brand;
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

/// Where the answer to the telemetry question lives, so the user is asked
/// once and never again. A run that cannot store settings cannot remember an
/// answer either, and so never asks and never reports.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct TelemetryConfig {
    /// Random id for this install, created when consent is first given so
    /// repeat runs are not counted as separate installs. Never derived from
    /// anything about the boat, the radar or the network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
    /// `None` until the user has answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct Config {
    pub radars: HashMap<String, Radar>,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

/// What the server may do about telemetry, and whether the GUI should put the
/// question to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Settings are not being stored, so an answer could not be remembered.
    /// Never ask, never report.
    Unavailable,
    /// The user has not answered yet.
    Unasked,
    Granted,
    Denied,
}

#[derive(Debug, Clone)]
pub(crate) struct Persistence {
    pub config: Config,
    timestamp: SystemTime,
    path: PathBuf,
    /// The settings file this run wanted but cannot write. `None` when
    /// settings are stored, and when a replay run wants none.
    unwritable: Option<PathBuf>,
    /// Set when an answer to the telemetry question could not be written.
    /// The stored answer no longer matches what the user asked for, so this
    /// run neither reports nor asks again.
    consent_unwritable: bool,
}

const SETTINGS_FILE: &str = "settings.json";

/// A radar this install has seen before, remembered across restarts.
///
/// Discovery is the same for everyone, but the advice is not: someone whose
/// radar worked last week has a different problem from someone setting one up
/// for the first time, and the page can only say so if it knows.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownRadar {
    pub brand: Option<Brand>,
    /// The name the user gave it, or the one the radar reported.
    pub name: String,
    /// Model as last detected, e.g. `HALO24`.
    pub model: Option<String>,
}

/// What this run does with radar settings. Reported by the status endpoint
/// and, when settings are going nowhere, sent to every connecting client as
/// a Signal K notification.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsStorage {
    /// Settings are read from and written to this file.
    Stored(PathBuf),
    /// This file cannot be written, so nothing the user sets up is
    /// remembered. Named, so they know which permissions to fix.
    Unwritable(PathBuf),
    /// A replay run keeps nothing on purpose.
    NotWanted,
}

/// What a run can do with the settings file it found.
enum Loaded {
    /// Settings are read and written normally.
    Writable,
    /// Existing settings were read but cannot be changed -- a file that
    /// belongs to someone else.
    ReadOnly,
    /// Nothing could be read and nothing can be written.
    Unusable,
}

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
    /// A persistence that remembers nothing. An empty path is the marker
    /// every write path already checks; `unwritable` names the file this run
    /// failed to write, and stays `None` when it never wanted one.
    fn disabled(unwritable: Option<PathBuf>) -> Self {
        Persistence {
            config: Config {
                radars: HashMap::new(),
                telemetry: TelemetryConfig::default(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: PathBuf::new(),
            unwritable,
            consent_unwritable: false,
        }
    }

    /// Every radar in the settings file, whether or not it is on the air now.
    pub(crate) fn known_radars(&self) -> Vec<KnownRadar> {
        let mut known: Vec<KnownRadar> = self
            .config
            .radars
            .iter()
            .map(|(key, radar)| KnownRadar {
                brand: Brand::from_key(key),
                name: if radar.user_name.is_empty() {
                    key.clone()
                } else {
                    radar.user_name.clone()
                },
                model: radar.model_name.clone(),
            })
            .collect();
        // A map's order is nobody's idea of an order; the page shows these to
        // a person.
        known.sort_by(|a, b| a.name.cmp(&b.name));
        known
    }

    pub(crate) fn storage(&self) -> SettingsStorage {
        match &self.unwritable {
            Some(path) => SettingsStorage::Unwritable(path.clone()),
            None if self.path.as_os_str().is_empty() => SettingsStorage::NotWanted,
            None => SettingsStorage::Stored(self.path.clone()),
        }
    }

    pub(crate) fn new() -> Self {
        if crate::replay::is_active() {
            debug!("persistence disabled in pcap replay mode");
            return Self::disabled(None);
        }

        let config_dir = get_project_dirs().config_dir().to_owned();
        let Some(settings_path) = settings_path(&config_dir) else {
            return Self::disabled(Some(config_dir.join(SETTINGS_FILE)));
        };

        let mut this = Persistence {
            config: Config {
                radars: HashMap::new(),
                telemetry: TelemetryConfig::default(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: settings_path,
            unwritable: None,
            consent_unwritable: false,
        };

        match this.load() {
            Loaded::Writable => {
                debug!("persistence loaded: {:?}", this);
                this
            }
            Loaded::ReadOnly => {
                warn!("Radar names, guard zones and other settings will not be remembered");
                this.read_only()
            }
            Loaded::Unusable => {
                warn!("Radar names, guard zones and other settings will not be remembered");
                Self::disabled(Some(this.path))
            }
        }
    }

    /// Keep the settings that were read, write no more of them. A file owned
    /// by another user still describes this radar correctly -- the user keeps
    /// their zones and names for this run -- it just cannot take changes.
    fn read_only(self) -> Self {
        Persistence {
            config: self.config,
            timestamp: self.timestamp,
            unwritable: Some(self.path),
            path: PathBuf::new(),
            consent_unwritable: self.consent_unwritable,
        }
    }

    /// Whether the settings file will take a change. Existing settings can be
    /// readable and still belong to another user, which is what a container
    /// mount produces after the uid it runs as changes; opening for writing
    /// is the only way to find out, and it leaves the contents alone.
    fn is_writable(&self) -> bool {
        match fs::OpenOptions::new().write(true).open(&self.path) {
            Ok(_) => true,
            Err(e) => {
                warn!("cannot write config '{}': {}", self.path.display(), e);
                false
            }
        }
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

    /// What this run can do with the settings file. A first run writes an
    /// empty one, which doubles as the check that this run can write at all:
    /// `create_dir_all` succeeds on a directory that already exists but
    /// belongs to another user, so the write is what finds that out. An
    /// existing file gets the same question asked of it directly.
    fn load(&mut self) -> Loaded {
        let file = match File::open(&self.path) {
            Err(e) => {
                warn!(
                    "no config '{}' yet; starting fresh: {}",
                    self.path.display(),
                    e
                );

                return if self.save() {
                    Loaded::Writable
                } else {
                    Loaded::Unusable
                };
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

        if self.is_writable() {
            Loaded::Writable
        } else {
            Loaded::ReadOnly
        }
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

    /// Whether this run can remember anything at all.
    fn is_storing(&self) -> bool {
        matches!(self.storage(), SettingsStorage::Stored(_))
    }

    pub(crate) fn consent(&self) -> Consent {
        if !self.is_storing() || self.consent_unwritable {
            return Consent::Unavailable;
        }
        match self.config.telemetry.consent {
            None => Consent::Unasked,
            Some(true) => Consent::Granted,
            Some(false) => Consent::Denied,
        }
    }

    /// Record the user's answer. Withdrawing consent drops the install id, so
    /// a later yes is a new install rather than the old one resurfacing.
    ///
    /// An answer that cannot be written is not an answer: the file still
    /// holds the previous one, which a restart would restore -- turning a
    /// withdrawn consent back into a granted one behind the user's back. So a
    /// failed write puts this run in the same place as having no settings
    /// file at all: nothing is reported, and nothing more is asked.
    pub(crate) fn set_consent(&mut self, granted: bool) -> Consent {
        if !self.is_storing() || self.consent_unwritable {
            return Consent::Unavailable;
        }

        let previous = self.config.telemetry.clone();
        self.config.telemetry.consent = Some(granted);
        if !granted {
            self.config.telemetry.install_id = None;
        }

        if !self.save() {
            self.config.telemetry = previous;
            self.consent_unwritable = true;
            return Consent::Unavailable;
        }

        self.consent()
    }

    /// The id this install reports under, created on first use. Only ever
    /// called once reporting is allowed, so creating one here is not a
    /// decision about whether to report -- that has already been made.
    pub(crate) fn ensure_install_id(&mut self) -> Option<String> {
        if self.consent() == Consent::Unavailable {
            return None;
        }

        if self.config.telemetry.install_id.is_none() {
            self.config.telemetry.install_id = Some(uuid::Uuid::new_v4().to_string());
            // An id that cannot be stored would be a fresh one on every
            // start, counting one install many times over.
            if !self.save() {
                self.config.telemetry.install_id = None;
                return None;
            }
        }
        self.config.telemetry.install_id.clone()
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
            config: Config {
                radars,
                ..Config::default()
            },
            ..Persistence::disabled(None)
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

    /// The discovery page names the radars this install has had working, so
    /// someone whose radar worked last week is told something different from
    /// someone setting one up for the first time. A radar the user renamed is
    /// named the way they named it.
    #[test]
    fn known_radars_are_listed_by_the_name_the_user_would_recognise() {
        let mut p = persistence_with("nav1034A", "Bow Radar");
        p.config.radars.insert(
            "fur6424A".to_string(),
            Radar {
                user_name: String::new(),
                model_name: Some("DRS4D-NXT".to_string()),
                ..Default::default()
            },
        );

        let known = p.known_radars();

        assert_eq!(known.len(), 2);
        // Sorted, so the page does not reshuffle them between visits.
        assert_eq!(known[0].name, "Bow Radar");
        assert_eq!(known[0].brand, Some(Brand::Navico));
        // No name of its own falls back to the key, which at least identifies it.
        assert_eq!(known[1].name, "fur6424A");
        assert_eq!(known[1].brand, Some(Brand::Furuno));
        assert_eq!(known[1].model.as_deref(), Some("DRS4D-NXT"));
    }

    /// A first run has nothing to say about radars seen before.
    #[test]
    fn a_run_with_no_stored_radars_knows_of_none() {
        assert!(Persistence::disabled(None).known_radars().is_empty());
    }

    /// The question is only ever put to a user whose answer can be kept.
    #[test]
    fn a_run_that_stores_nothing_is_never_asked() {
        let mut p = Persistence::disabled(None);

        assert_eq!(p.consent(), Consent::Unavailable);
        assert_eq!(p.set_consent(true), Consent::Unavailable);
        assert_eq!(p.ensure_install_id(), None);
    }

    /// The answer and the id outlive the run that produced them, which is the
    /// whole reason the question is asked only once.
    #[test]
    fn consent_and_install_id_are_written_to_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut p = persistence_at(path.clone());
        assert_eq!(p.consent(), Consent::Unasked);
        assert_eq!(p.set_consent(true), Consent::Granted);

        let id = p.ensure_install_id().expect("consent means an id");
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_eq!(p.ensure_install_id(), Some(id.clone()), "id is stable");

        let saved: Config = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(saved.telemetry.consent, Some(true));
        assert_eq!(saved.telemetry.install_id, Some(id));
    }

    /// An answer that cannot be written is worse than no answer: the file
    /// still holds the previous one, so a restart would restore it -- turning
    /// a withdrawn consent back into a granted one. The run must then behave
    /// as if it could store nothing at all.
    #[test]
    fn an_answer_that_cannot_be_written_is_not_acknowledged() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = persistence_at(dir.path().join("settings.json"));
        p.set_consent(true);
        assert_eq!(p.consent(), Consent::Granted);

        // Take the file away from under it: the parent is now a plain file,
        // which no user can write through.
        let blocked = dir.path().join("blocker");
        fs::write(&blocked, b"not a directory").unwrap();
        p.path = blocked.join("settings.json");

        assert_eq!(p.set_consent(false), Consent::Unavailable);
        assert_eq!(
            p.consent(),
            Consent::Unavailable,
            "a run that cannot record the answer must not keep reporting under the old one"
        );
        assert_eq!(p.ensure_install_id(), None);
    }

    /// Saying no drops the id, so a later yes counts as a new install rather
    /// than resurrecting the old one.
    #[test]
    fn declining_forgets_the_install_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = persistence_at(dir.path().join("settings.json"));

        p.set_consent(true);
        let first = p.ensure_install_id().unwrap();
        assert_eq!(p.set_consent(false), Consent::Denied);
        assert_eq!(p.config.telemetry.install_id, None);

        p.set_consent(true);
        assert_ne!(p.ensure_install_id().unwrap(), first);
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

    /// The status endpoint and the connect-time warning both read this, and
    /// they must tell apart "cannot write" from "a replay run wants none" --
    /// only the first is worth warning a user about.
    #[test]
    fn storage_tells_a_failed_run_from_one_that_wants_nothing() {
        let path = PathBuf::from("/config/mayara/settings.json");

        assert_eq!(
            persistence_at(path.clone()).storage(),
            SettingsStorage::Stored(path.clone())
        );
        assert_eq!(
            Persistence::disabled(Some(path.clone())).storage(),
            SettingsStorage::Unwritable(path)
        );
        assert_eq!(
            Persistence::disabled(None).storage(),
            SettingsStorage::NotWanted
        );
    }

    fn persistence_at(path: PathBuf) -> Persistence {
        Persistence {
            path,
            ..Persistence::disabled(None)
        }
    }

    /// First run: no settings file yet, so one is written and kept.
    #[test]
    fn a_first_run_writes_the_settings_file_it_found_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);

        assert!(matches!(
            persistence_at(path.clone()).load(),
            Loaded::Writable
        ));
        assert!(path.is_file());
    }

    /// A settings file that opens for reading may still belong to another
    /// user -- exactly what a container mount produces once the uid it runs
    /// as changes. Opening it for writing is the only way to find out.
    /// A directory standing in for the file is refused by every user, root
    /// included, which no chmod can claim.
    #[test]
    fn a_settings_file_that_rejects_writes_is_not_storage() {
        let dir = tempfile::tempdir().unwrap();

        let writable = dir.path().join("settings.json");
        fs::write(&writable, b"{}").unwrap();
        assert!(persistence_at(writable).is_writable());

        let refuses_writes = dir.path().join("unwritable.json");
        fs::create_dir(&refuses_writes).unwrap();
        assert!(!persistence_at(refuses_writes).is_writable());
    }

    /// Settings that cannot be updated are still worth having: the user keeps
    /// their radar names and zones for this run, and is told they will not
    /// survive it.
    #[test]
    fn settings_that_cannot_be_updated_are_still_read() {
        let path = PathBuf::from("/config/mayara/settings.json");
        let mut p = persistence_with("nav1034A", "Bow Radar");
        p.path = path.clone();

        let p = p.read_only();

        assert_eq!(
            p.config
                .radars
                .get("nav1034A")
                .map(|r| r.user_name.as_str()),
            Some("Bow Radar"),
            "what was read stays available for this run"
        );
        assert_eq!(p.storage(), SettingsStorage::Unwritable(path));
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

        assert!(matches!(
            persistence_at(blocker.join(SETTINGS_FILE)).load(),
            Loaded::Unusable
        ));
    }
}
