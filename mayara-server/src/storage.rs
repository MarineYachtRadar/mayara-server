//! Local storage for SignalK applicationData API compatibility.
//!
//! This module implements the same API as SignalK's applicationData storage,
//! allowing the GUI to persist settings regardless of whether it's running
//! against SignalK or standalone Mayara.
//!
//! Storage path: `~/.local/share/mayara/applicationData/{appid}/{version}/{key}.json`

use log::{debug, error, info, warn};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::config::get_project_dirs;

/// Key for applicationData storage: (appid, version, key)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppDataKey {
    pub appid: String,
    pub version: String,
    pub key: String,
}

impl AppDataKey {
    pub fn new(appid: &str, version: &str, key: &str) -> Self {
        Self {
            appid: appid.to_string(),
            version: version.to_string(),
            key: key.to_string(),
        }
    }

    /// Get the file path for this key
    fn file_path(&self, base_dir: &PathBuf) -> PathBuf {
        let mut path = base_dir.clone();
        path.push(&self.appid);
        path.push(&self.version);
        // Sanitize key for filesystem (replace / with __)
        let safe_key = self.key.replace("/", "__");
        path.push(format!("{}.json", safe_key));
        path
    }

    /// Get the directory path for this app/version
    fn dir_path(&self, base_dir: &PathBuf) -> PathBuf {
        let mut path = base_dir.clone();
        path.push(&self.appid);
        path.push(&self.version);
        path
    }
}

/// Local storage backend for applicationData API
pub struct LocalStorage {
    base_dir: PathBuf,
    /// In-memory cache of loaded values
    cache: HashMap<AppDataKey, Value>,
}

impl LocalStorage {
    /// Create a new LocalStorage instance
    pub fn new() -> Self {
        let project_dirs = get_project_dirs();
        let mut base_dir = project_dirs.data_dir().to_owned();
        base_dir.push("applicationData");

        // Ensure base directory exists
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create applicationData directory: {}", e);
        } else {
            debug!("applicationData directory: {}", base_dir.display());
        }

        Self {
            base_dir,
            cache: HashMap::new(),
        }
    }

    /// Get a value from storage
    pub fn get(&mut self, key: &AppDataKey) -> Option<Value> {
        // Check cache first
        if let Some(value) = self.cache.get(key) {
            return Some(value.clone());
        }

        // Try to load from file
        let path = key.file_path(&self.base_dir);
        if !path.exists() {
            debug!("applicationData key not found: {:?}", key);
            return None;
        }

        match std::fs::File::open(&path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                match serde_json::from_reader(reader) {
                    Ok(value) => {
                        debug!("Loaded applicationData: {:?}", key);
                        self.cache.insert(key.clone(), value);
                        self.cache.get(key).cloned()
                    }
                    Err(e) => {
                        warn!("Failed to parse applicationData {}: {}", path.display(), e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Failed to open applicationData {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Store a value in storage
    pub fn put(&mut self, key: &AppDataKey, value: Value) -> Result<(), String> {
        let dir_path = key.dir_path(&self.base_dir);
        let file_path = key.file_path(&self.base_dir);

        // Ensure directory exists
        if let Err(e) = fs::create_dir_all(&dir_path) {
            let msg = format!("Failed to create directory {}: {}", dir_path.display(), e);
            error!("{}", msg);
            return Err(msg);
        }

        // Write to file
        match std::fs::File::create(&file_path) {
            Ok(file) => {
                let mut writer = BufWriter::new(file);
                if let Err(e) = serde_json::to_writer_pretty(&mut writer, &value) {
                    let msg = format!("Failed to write applicationData: {}", e);
                    error!("{}", msg);
                    return Err(msg);
                }
                if let Err(e) = writer.write_all(b"\n") {
                    warn!("Failed to write trailing newline: {}", e);
                }
                if let Err(e) = writer.flush() {
                    warn!("Failed to flush applicationData file: {}", e);
                }

                info!(
                    "Stored applicationData: {:?} -> {}",
                    key,
                    file_path.display()
                );
                self.cache.insert(key.clone(), value);
                Ok(())
            }
            Err(e) => {
                let msg = format!("Failed to create file {}: {}", file_path.display(), e);
                error!("{}", msg);
                Err(msg)
            }
        }
    }

    /// Delete a value from storage
    pub fn delete(&mut self, key: &AppDataKey) -> Result<(), String> {
        let file_path = key.file_path(&self.base_dir);

        // Remove from cache
        self.cache.remove(key);

        // Delete file if it exists
        if file_path.exists() {
            if let Err(e) = fs::remove_file(&file_path) {
                let msg = format!("Failed to delete {}: {}", file_path.display(), e);
                warn!("{}", msg);
                return Err(msg);
            }
            info!("Deleted applicationData: {:?}", key);
        }

        Ok(())
    }

    /// List all keys for a given appid and version
    pub fn list_keys(&self, appid: &str, version: &str) -> Vec<String> {
        let mut dir_path = self.base_dir.clone();
        dir_path.push(appid);
        dir_path.push(version);

        if !dir_path.exists() {
            return Vec::new();
        }

        let mut keys = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".json") {
                        // Convert filename back to key (reverse the sanitization)
                        let key = name.trim_end_matches(".json").replace("__", "/");
                        keys.push(key);
                    }
                }
            }
        }
        keys
    }
}

/// Shared storage for use across handlers
pub type SharedStorage = Arc<RwLock<LocalStorage>>;

/// Create a new shared storage instance
pub fn create_shared_storage() -> SharedStorage {
    Arc::new(RwLock::new(LocalStorage::new()))
}

/// Installation settings for a single radar
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationSettings {
    pub auto_acquire: Option<bool>,
    pub bearing_alignment: Option<i32>,
    pub antenna_height: Option<i32>,
}

/// Full application data structure (matches WASM SignalK plugin format)
/// Structure: { "radars": { "radar-id": { "bearingAlignment": ..., ... } } }
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct AppDataRadars {
    pub radars: Option<std::collections::HashMap<String, InstallationSettings>>,
}

/// Load installation settings for a radar directly from disk.
/// Uses same path as WASM SignalK plugin: @mayara/signalk-radar/1.0.0
/// This is used by report receivers to restore write-only settings on startup.
pub fn load_installation_settings(radar_id: &str) -> Option<InstallationSettings> {
    let project_dirs = get_project_dirs();
    let mut path = project_dirs.data_dir().to_owned();
    path.push("applicationData");
    path.push("@mayara");
    path.push("signalk-radar");
    path.push("1.0.0.json");

    info!(
        "Loading installation settings for {} from {}",
        radar_id,
        path.display()
    );

    if !path.exists() {
        info!("No installation settings file found at {}", path.display());
        return None;
    }

    match std::fs::File::open(&path) {
        Ok(file) => {
            let reader = std::io::BufReader::new(file);
            match serde_json::from_reader::<_, AppDataRadars>(reader) {
                Ok(data) => {
                    if let Some(radars) = data.radars {
                        if let Some(settings) = radars.get(radar_id) {
                            info!(
                                "Loaded installation settings for {}: {:?}",
                                radar_id, settings
                            );
                            return Some(settings.clone());
                        }
                    }
                    debug!(
                        "No installation settings for radar {} in {}",
                        radar_id,
                        path.display()
                    );
                    None
                }
                Err(e) => {
                    warn!(
                        "Failed to parse installation settings {}: {}",
                        path.display(),
                        e
                    );
                    None
                }
            }
        }
        Err(e) => {
            warn!(
                "Failed to open installation settings {}: {}",
                path.display(),
                e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn create_test_storage() -> (LocalStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage {
            base_dir: temp_dir.path().to_path_buf(),
            cache: HashMap::new(),
        };
        (storage, temp_dir)
    }

    #[test]
    fn test_put_and_get() {
        let (mut storage, _temp) = create_test_storage();
        let key = AppDataKey::new("mayara", "1.0", "guardZones");
        let value = json!({"zone1": {"enabled": true}});

        // Put value
        assert!(storage.put(&key, value.clone()).is_ok());

        // Get value (from cache)
        let retrieved = storage.get(&key);
        assert_eq!(retrieved, Some(value.clone()));

        // Clear cache and get again (from file)
        storage.cache.clear();
        let retrieved = storage.get(&key);
        assert_eq!(retrieved, Some(value));
    }

    #[test]
    fn test_delete() {
        let (mut storage, _temp) = create_test_storage();
        let key = AppDataKey::new("mayara", "1.0", "test");
        let value = json!({"test": true});

        storage.put(&key, value).unwrap();
        assert!(storage.get(&key).is_some());

        storage.delete(&key).unwrap();
        assert!(storage.get(&key).is_none());
    }

    #[test]
    fn test_key_with_slashes() {
        let (mut storage, _temp) = create_test_storage();
        let key = AppDataKey::new("mayara", "1.0", "settings/display/colors");
        let value = json!({"theme": "dark"});

        assert!(storage.put(&key, value.clone()).is_ok());
        let retrieved = storage.get(&key);
        assert_eq!(retrieved, Some(value));
    }

    #[test]
    fn test_list_keys() {
        let (mut storage, _temp) = create_test_storage();

        storage
            .put(&AppDataKey::new("mayara", "1.0", "key1"), json!(1))
            .unwrap();
        storage
            .put(&AppDataKey::new("mayara", "1.0", "key2"), json!(2))
            .unwrap();
        storage
            .put(&AppDataKey::new("other", "1.0", "key3"), json!(3))
            .unwrap();

        let keys = storage.list_keys("mayara", "1.0");
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
    }
}
