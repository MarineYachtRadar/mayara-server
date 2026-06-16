//! Recording file manager.
//!
//! Handles listing, metadata extraction, and deletion of recordings.

use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::get_project_dirs;

use super::file_format::{FOOTER_SIZE, MrrFooter, MrrHeader};

/// Get the recordings directory path
pub fn recordings_dir() -> PathBuf {
    let project_dirs = get_project_dirs();
    let mut path = project_dirs.data_dir().to_owned();
    path.push("recordings");
    path
}

/// Information about a recording file
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub filename: String,
    #[serde(skip_serializing)]
    pub path: PathBuf,
    pub size: u64,
    pub duration_ms: u64,
    pub frame_count: u32,
    pub start_time_ms: u64,
    pub modified_ms: u64,
    pub radar_brand: u32,
    pub spokes_per_rev: u32,
    pub max_spoke_len: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdirectory: Option<String>,
}

/// Directory information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryInfo {
    pub name: String,
    pub recording_count: usize,
    pub total_size: u64,
}

fn is_valid_name(name: &str) -> bool {
    !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

/// Manager for recording files
pub struct RecordingManager {
    base_dir: PathBuf,
}

impl RecordingManager {
    pub fn new() -> Self {
        let base_dir = recordings_dir();

        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create recordings directory: {}", e);
        } else {
            debug!("Recordings directory: {}", base_dir.display());
        }

        Self { base_dir }
    }

    #[cfg(test)]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create recordings directory: {}", e);
        }
        Self { base_dir }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn list_recordings(&self, subdirectory: Option<&str>) -> Vec<RecordingInfo> {
        if let Some(sub) = subdirectory
            && !is_valid_name(sub)
        {
            return Vec::new();
        }

        let dir = match subdirectory {
            Some(sub) => self.base_dir.join(sub),
            None => self.base_dir.clone(),
        };

        if !dir.exists() {
            return Vec::new();
        }

        let mut recordings = Vec::new();

        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(ext) = path.extension()
                    && ext == "mrr"
                    && let Some(info) = self.get_recording_info(&path, subdirectory)
                {
                    recordings.push(info);
                }
            }
        }

        recordings.sort_by_key(|r| std::cmp::Reverse(r.modified_ms));
        recordings
    }

    pub fn list_directories(&self) -> Vec<DirectoryInfo> {
        let mut dirs = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    let recordings = self.list_recordings(Some(name));
                    let total_size: u64 = recordings.iter().map(|r| r.size).sum();
                    dirs.push(DirectoryInfo {
                        name: name.to_string(),
                        recording_count: recordings.len(),
                        total_size,
                    });
                }
            }
        }

        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        dirs
    }

    pub fn get_recording_info(
        &self,
        path: &Path,
        subdirectory: Option<&str>,
    ) -> Option<RecordingInfo> {
        let filename = path.file_name()?.to_str()?.to_string();

        let metadata = fs::metadata(path).ok()?;
        let size = metadata.len();
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);

        let header = MrrHeader::read(&mut reader).ok()?;

        use std::io::{Seek, SeekFrom};
        reader.seek(SeekFrom::End(-(FOOTER_SIZE as i64))).ok()?;
        let footer = MrrFooter::read(&mut reader).ok()?;

        Some(RecordingInfo {
            filename,
            path: path.to_path_buf(),
            size,
            duration_ms: footer.duration_ms,
            frame_count: footer.frame_count,
            start_time_ms: header.start_time_ms,
            modified_ms,
            radar_brand: header.radar_brand,
            spokes_per_rev: header.spokes_per_rev,
            max_spoke_len: header.max_spoke_len,
            subdirectory: subdirectory.map(String::from),
        })
    }

    pub fn get_recording(
        &self,
        filename: &str,
        subdirectory: Option<&str>,
    ) -> Option<RecordingInfo> {
        let path = self.get_recording_path(filename, subdirectory);

        if !self.is_safe_path(&path) {
            return None;
        }

        if path.exists() {
            self.get_recording_info(&path, subdirectory)
        } else {
            None
        }
    }

    pub fn get_recording_path(&self, filename: &str, subdirectory: Option<&str>) -> PathBuf {
        match subdirectory {
            Some(sub) => self.base_dir.join(sub).join(filename),
            None => self.base_dir.join(filename),
        }
    }

    pub fn delete_recording(
        &self,
        filename: &str,
        subdirectory: Option<&str>,
    ) -> Result<(), String> {
        let path = self.get_recording_path(filename, subdirectory);

        if !self.is_safe_path(&path) {
            return Err("Invalid path".to_string());
        }

        if !path.exists() {
            return Err(format!("Recording not found: {}", filename));
        }

        fs::remove_file(&path).map_err(|e| format!("Failed to delete: {}", e))?;
        info!("Deleted recording: {}", path.display());
        Ok(())
    }

    pub fn rename_recording(
        &self,
        filename: &str,
        new_filename: &str,
        subdirectory: Option<&str>,
    ) -> Result<(), String> {
        let new_filename = if new_filename.ends_with(".mrr") {
            new_filename.to_string()
        } else {
            format!("{}.mrr", new_filename)
        };

        let old_path = self.get_recording_path(filename, subdirectory);
        let new_path = self.get_recording_path(&new_filename, subdirectory);

        if !self.is_safe_path(&old_path) || !self.is_safe_path(&new_path) {
            return Err("Invalid path".to_string());
        }

        if !old_path.exists() {
            return Err(format!("Recording not found: {}", filename));
        }

        if new_path.exists() {
            return Err(format!("File already exists: {}", new_filename));
        }

        fs::rename(&old_path, &new_path).map_err(|e| format!("Failed to rename: {}", e))?;
        info!(
            "Renamed recording: {} -> {}",
            old_path.display(),
            new_path.display()
        );
        Ok(())
    }

    /// Remove any `<name>.upload` temp files left in the base directory.
    ///
    /// Uploads stream into `<filename>.upload` and atomically rename to the
    /// final name on success. A crash or dropped connection leaves the temp
    /// file behind, and `create_new(true)` then rejects future uploads of the
    /// same name with 409 Conflict. At startup no upload can be in progress,
    /// so anything matching `*.upload` is orphaned and safe to delete.
    pub fn cleanup_orphaned_uploads(&self) {
        let entries = match fs::read_dir(&self.base_dir) {
            Ok(e) => e,
            Err(e) => {
                error!(
                    "Failed to scan recordings directory for orphaned uploads: {}",
                    e
                );
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("upload") {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => info!("Removed orphaned upload: {}", path.display()),
                Err(e) => warn!("Failed to remove orphaned upload {}: {}", path.display(), e),
            }
        }
    }

    pub fn create_directory(&self, name: &str) -> Result<(), String> {
        if !is_valid_name(name) {
            return Err("Invalid directory name".to_string());
        }

        let path = self.base_dir.join(name);

        if path.exists() {
            return Err(format!("Directory already exists: {}", name));
        }

        fs::create_dir_all(&path).map_err(|e| format!("Failed to create directory: {}", e))?;
        info!("Created directory: {}", path.display());
        Ok(())
    }

    pub fn delete_directory(&self, name: &str) -> Result<(), String> {
        if !is_valid_name(name) {
            return Err("Invalid directory name".to_string());
        }

        let path = self.base_dir.join(name);

        if !path.exists() {
            return Err(format!("Directory not found: {}", name));
        }

        if !path.is_dir() {
            return Err(format!("Not a directory: {}", name));
        }

        let entries: Vec<_> = fs::read_dir(&path)
            .map_err(|e| format!("Failed to read directory: {}", e))?
            .flatten()
            .collect();

        if !entries.is_empty() {
            return Err(format!(
                "Directory not empty: {} ({} items)",
                name,
                entries.len()
            ));
        }

        fs::remove_dir(&path).map_err(|e| format!("Failed to delete directory: {}", e))?;
        info!("Deleted directory: {}", path.display());
        Ok(())
    }

    pub fn generate_filename(&self, prefix: Option<&str>, subdirectory: Option<&str>) -> String {
        let now = chrono::Utc::now();
        let prefix = prefix.filter(|p| is_valid_name(p)).unwrap_or("recording");
        let base_name = format!("{}_{}", prefix, now.format("%Y%m%d_%H%M%S"));

        let dir = match subdirectory {
            Some(sub) => self.base_dir.join(sub),
            None => self.base_dir.clone(),
        };

        let mut name = format!("{}.mrr", base_name);
        let mut counter = 1;
        while dir.join(&name).exists() {
            name = format!("{}_{}.mrr", base_name, counter);
            counter += 1;
        }

        name
    }

    fn is_safe_path(&self, path: &Path) -> bool {
        let canonical_base = match self.base_dir.canonicalize() {
            Ok(p) => p,
            Err(_) => return false,
        };

        match path.canonicalize() {
            Ok(canonical) => canonical.starts_with(&canonical_base),
            Err(_) => {
                if let Some(parent) = path.parent() {
                    match parent.canonicalize() {
                        Ok(canonical_parent) => canonical_parent.starts_with(&canonical_base),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
        }
    }
}

impl Default for RecordingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manager() -> (RecordingManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let manager = RecordingManager::with_base_dir(temp_dir.path().to_path_buf());
        (manager, temp_dir)
    }

    #[test]
    fn test_create_directory() {
        let (manager, _temp) = create_test_manager();

        assert!(manager.create_directory("test_dir").is_ok());
        assert!(manager.base_dir.join("test_dir").exists());

        // Creating again should fail
        assert!(manager.create_directory("test_dir").is_err());
    }

    #[test]
    fn test_delete_empty_directory() {
        let (manager, _temp) = create_test_manager();

        manager.create_directory("test_dir").unwrap();
        assert!(manager.delete_directory("test_dir").is_ok());
        assert!(!manager.base_dir.join("test_dir").exists());
    }

    #[test]
    fn test_generate_filename() {
        let (manager, _temp) = create_test_manager();

        let name1 = manager.generate_filename(Some("radar1"), None);
        assert!(name1.starts_with("radar1_"));
        assert!(name1.ends_with(".mrr"));

        fs::write(manager.base_dir.join(&name1), b"test").unwrap();

        let name2 = manager.generate_filename(Some("radar1"), None);
        assert_ne!(name1, name2);
    }

    #[test]
    fn test_invalid_directory_names() {
        let (manager, _temp) = create_test_manager();

        assert!(manager.create_directory("../escape").is_err());
        assert!(manager.create_directory("foo/bar").is_err());
        assert!(manager.create_directory("foo\\bar").is_err());
    }

    #[test]
    fn test_list_empty_directory() {
        let (manager, _temp) = create_test_manager();

        let recordings = manager.list_recordings(None);
        assert!(recordings.is_empty());

        let dirs = manager.list_directories();
        assert!(dirs.is_empty());
    }

    #[test]
    fn test_path_traversal_rejected() {
        let (manager, _temp) = create_test_manager();

        // Subdirectory traversal
        assert!(manager.list_recordings(Some("../etc")).is_empty());

        // Filename traversal
        assert!(manager.get_recording("../../../etc/passwd", None).is_none());

        // Delete traversal
        assert!(
            manager
                .delete_recording("../../../etc/passwd", None)
                .is_err()
        );

        // Delete directory traversal
        assert!(manager.delete_directory("../escape").is_err());
    }

    #[test]
    fn test_cleanup_orphaned_uploads() {
        let (manager, _temp) = create_test_manager();

        let orphan_a = manager.base_dir.join("a.mrr.upload");
        let orphan_b = manager.base_dir.join("b.mrr.gz.upload");
        let recording = manager.base_dir.join("kept.mrr");
        fs::write(&orphan_a, b"partial").unwrap();
        fs::write(&orphan_b, b"partial").unwrap();
        fs::write(&recording, b"data").unwrap();

        // Subdirectories must not be touched even if a stray .upload appears
        // there; the upload handler only writes to base_dir, but cleanup
        // shouldn't recurse and accidentally pick up unrelated content.
        manager.create_directory("sub").unwrap();
        let nested = manager.base_dir.join("sub").join("c.mrr.upload");
        fs::write(&nested, b"partial").unwrap();

        manager.cleanup_orphaned_uploads();

        assert!(!orphan_a.exists());
        assert!(!orphan_b.exists());
        assert!(recording.exists());
        assert!(nested.exists());
    }

    #[test]
    fn test_prefix_sanitization() {
        let (manager, _temp) = create_test_manager();

        // Malicious prefix falls back to "recording"
        let name = manager.generate_filename(Some("../escape"), None);
        assert!(name.starts_with("recording_"));
    }
}
