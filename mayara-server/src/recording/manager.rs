//! Recording file manager.
//!
//! Handles listing, metadata extraction, upload, download, and deletion of recordings.

use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::get_project_dirs;

use super::file_format::{MrrFooter, MrrHeader, FOOTER_SIZE};

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
    /// Filename (without path)
    pub filename: String,
    /// Full path to the file
    #[serde(skip_serializing)]
    pub path: PathBuf,
    /// File size in bytes
    pub size: u64,
    /// Recording duration in milliseconds
    pub duration_ms: u64,
    /// Number of frames
    pub frame_count: u32,
    /// Recording start time (Unix timestamp ms)
    pub start_time_ms: u64,
    /// File modification time (Unix timestamp ms)
    pub modified_ms: u64,
    /// Radar brand (numeric ID)
    pub radar_brand: u32,
    /// Spokes per revolution
    pub spokes_per_rev: u32,
    /// Maximum spoke length
    pub max_spoke_len: u32,
    /// Subdirectory (relative to recordings root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdirectory: Option<String>,
}

/// Directory information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryInfo {
    /// Directory name
    pub name: String,
    /// Number of recordings in this directory
    pub recording_count: usize,
    /// Total size of all recordings in bytes
    pub total_size: u64,
}

/// Manager for recording files
pub struct RecordingManager {
    base_dir: PathBuf,
}

impl RecordingManager {
    /// Create a new RecordingManager
    pub fn new() -> Self {
        let base_dir = recordings_dir();

        // Ensure base directory exists
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create recordings directory: {}", e);
        } else {
            debug!("Recordings directory: {}", base_dir.display());
        }

        Self { base_dir }
    }

    /// Create with a custom base directory (for testing)
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create recordings directory: {}", e);
        }
        Self { base_dir }
    }

    /// Get the base directory path
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// List all recordings in a directory
    pub fn list_recordings(&self, subdirectory: Option<&str>) -> Vec<RecordingInfo> {
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
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext == "mrr" {
                            if let Some(info) = self.get_recording_info(&path, subdirectory) {
                                recordings.push(info);
                            }
                        }
                    }
                }
            }
        }

        // Sort by modification time, newest first
        recordings.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));

        recordings
    }

    /// List all subdirectories
    pub fn list_directories(&self) -> Vec<DirectoryInfo> {
        let mut dirs = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
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
        }

        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        dirs
    }

    /// Get information about a specific recording
    pub fn get_recording_info(
        &self,
        path: &Path,
        subdirectory: Option<&str>,
    ) -> Option<RecordingInfo> {
        let filename = path.file_name()?.to_str()?.to_string();

        // Get file metadata
        let metadata = fs::metadata(path).ok()?;
        let size = metadata.len();
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Try to read MRR header and footer
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);

        let header = MrrHeader::read(&mut reader).ok()?;

        // Read footer from end of file
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

    /// Get recording by filename
    pub fn get_recording(
        &self,
        filename: &str,
        subdirectory: Option<&str>,
    ) -> Option<RecordingInfo> {
        let path = match subdirectory {
            Some(sub) => self.base_dir.join(sub).join(filename),
            None => self.base_dir.join(filename),
        };

        if path.exists() {
            self.get_recording_info(&path, subdirectory)
        } else {
            None
        }
    }

    /// Get full path for a recording
    pub fn get_recording_path(&self, filename: &str, subdirectory: Option<&str>) -> PathBuf {
        match subdirectory {
            Some(sub) => self.base_dir.join(sub).join(filename),
            None => self.base_dir.join(filename),
        }
    }

    /// Delete a recording
    pub fn delete_recording(
        &self,
        filename: &str,
        subdirectory: Option<&str>,
    ) -> Result<(), String> {
        let path = self.get_recording_path(filename, subdirectory);

        if !path.exists() {
            return Err(format!("Recording not found: {}", filename));
        }

        // Ensure the path is within our base directory (security check)
        if !self.is_safe_path(&path) {
            return Err("Invalid path".to_string());
        }

        fs::remove_file(&path).map_err(|e| format!("Failed to delete: {}", e))?;
        info!("Deleted recording: {}", path.display());
        Ok(())
    }

    /// Rename a recording
    pub fn rename_recording(
        &self,
        filename: &str,
        new_filename: &str,
        subdirectory: Option<&str>,
    ) -> Result<(), String> {
        // Ensure new filename ends with .mrr
        let new_filename = if new_filename.ends_with(".mrr") {
            new_filename.to_string()
        } else {
            format!("{}.mrr", new_filename)
        };

        let old_path = self.get_recording_path(filename, subdirectory);
        let new_path = self.get_recording_path(&new_filename, subdirectory);

        if !old_path.exists() {
            return Err(format!("Recording not found: {}", filename));
        }

        if new_path.exists() {
            return Err(format!("File already exists: {}", new_filename));
        }

        // Security check
        if !self.is_safe_path(&old_path) || !self.is_safe_path(&new_path) {
            return Err("Invalid path".to_string());
        }

        fs::rename(&old_path, &new_path).map_err(|e| format!("Failed to rename: {}", e))?;
        info!(
            "Renamed recording: {} -> {}",
            old_path.display(),
            new_path.display()
        );
        Ok(())
    }

    /// Move a recording to a different directory
    pub fn move_recording(
        &self,
        filename: &str,
        from_subdirectory: Option<&str>,
        to_subdirectory: Option<&str>,
    ) -> Result<(), String> {
        let old_path = self.get_recording_path(filename, from_subdirectory);
        let new_path = self.get_recording_path(filename, to_subdirectory);

        if !old_path.exists() {
            return Err(format!("Recording not found: {}", filename));
        }

        if new_path.exists() {
            return Err(format!(
                "File already exists in target directory: {}",
                filename
            ));
        }

        // Security check
        if !self.is_safe_path(&old_path) || !self.is_safe_path(&new_path) {
            return Err("Invalid path".to_string());
        }

        // Ensure target directory exists
        if let Some(parent) = new_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        fs::rename(&old_path, &new_path).map_err(|e| format!("Failed to move: {}", e))?;
        info!(
            "Moved recording: {} -> {}",
            old_path.display(),
            new_path.display()
        );
        Ok(())
    }

    /// Create a new subdirectory
    pub fn create_directory(&self, name: &str) -> Result<(), String> {
        // Validate directory name
        if name.contains('/') || name.contains('\\') || name.contains("..") {
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

    /// Delete an empty subdirectory
    pub fn delete_directory(&self, name: &str) -> Result<(), String> {
        let path = self.base_dir.join(name);

        if !path.exists() {
            return Err(format!("Directory not found: {}", name));
        }

        if !path.is_dir() {
            return Err(format!("Not a directory: {}", name));
        }

        // Check if directory is empty (or only contains .mrr files that we'll delete)
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

    /// Generate a unique filename for a new recording
    pub fn generate_filename(&self, prefix: Option<&str>, subdirectory: Option<&str>) -> String {
        let now = chrono::Utc::now();
        let prefix = prefix.unwrap_or("recording");
        let base_name = format!("{}_{}", prefix, now.format("%Y%m%d_%H%M%S"));

        let dir = match subdirectory {
            Some(sub) => self.base_dir.join(sub),
            None => self.base_dir.clone(),
        };

        // Find a unique name
        let mut name = format!("{}.mrr", base_name);
        let mut counter = 1;
        while dir.join(&name).exists() {
            name = format!("{}_{}.mrr", base_name, counter);
            counter += 1;
        }

        name
    }

    /// Check if a path is safely within our base directory
    fn is_safe_path(&self, path: &Path) -> bool {
        match path.canonicalize() {
            Ok(canonical) => canonical.starts_with(&self.base_dir),
            Err(_) => {
                // Path doesn't exist yet, check parent
                if let Some(parent) = path.parent() {
                    match parent.canonicalize() {
                        Ok(canonical_parent) => canonical_parent.starts_with(&self.base_dir),
                        Err(_) => false,
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Save uploaded recording data to a file
    /// Handles both .mrr and .mrr.gz (decompresses gzip)
    pub fn save_upload(
        &self,
        filename: &str,
        data: &[u8],
        subdirectory: Option<&str>,
    ) -> Result<RecordingInfo, String> {
        // Determine target filename and whether to decompress
        let (target_filename, needs_decompress) = if filename.ends_with(".mrr.gz") {
            // Strip .gz suffix for storage
            (filename.trim_end_matches(".gz").to_string(), true)
        } else if filename.ends_with(".mrr") {
            (filename.to_string(), false)
        } else {
            return Err("Invalid file extension. Must be .mrr or .mrr.gz".to_string());
        };

        let target_path = self.get_recording_path(&target_filename, subdirectory);

        // Check if file already exists
        if target_path.exists() {
            return Err(format!("File already exists: {}", target_filename));
        }

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        // Security check
        if !self.is_safe_path(&target_path) {
            return Err("Invalid path".to_string());
        }

        // Write the file (decompress if needed)
        let final_data = if needs_decompress {
            use std::io::Read;
            let mut decoder = flate2::read::GzDecoder::new(data);
            let mut decompressed = Vec::new();
            decoder
                .read_to_end(&mut decompressed)
                .map_err(|e| format!("Failed to decompress gzip data: {}", e))?;
            decompressed
        } else {
            data.to_vec()
        };

        // Validate it's a valid MRR file by checking magic
        if final_data.len() < 4 || &final_data[0..4] != b"MRR1" {
            return Err("Invalid MRR file format".to_string());
        }

        fs::write(&target_path, &final_data).map_err(|e| format!("Failed to write file: {}", e))?;

        info!(
            "Uploaded recording: {} ({} bytes)",
            target_path.display(),
            final_data.len()
        );

        // Return info about the saved file
        self.get_recording_info(&target_path, subdirectory)
            .ok_or_else(|| "Failed to read uploaded file info".to_string())
    }

    /// Get total storage used
    pub fn total_storage_used(&self) -> u64 {
        self.calculate_dir_size(&self.base_dir)
    }

    fn calculate_dir_size(&self, dir: &Path) -> u64 {
        let mut total = 0u64;

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(metadata) = fs::metadata(&path) {
                        total += metadata.len();
                    }
                } else if path.is_dir() {
                    total += self.calculate_dir_size(&path);
                }
            }
        }

        total
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

        // Create the file
        fs::write(manager.base_dir.join(&name1), b"test").unwrap();

        // Next name should be different (with counter)
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
}
