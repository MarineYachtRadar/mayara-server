//! Session recording for debug events.
//!
//! Allows recording debug sessions to files that can be shared with
//! other developers for analysis.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::DebugEvent;

// =============================================================================
// Recording Metadata
// =============================================================================

/// Metadata for a debug recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingMetadata {
    /// Version of the recording format.
    pub format_version: u32,

    /// When recording started.
    pub start_time: DateTime<Utc>,

    /// When recording ended.
    pub end_time: Option<DateTime<Utc>>,

    /// mayara-server version.
    pub server_version: String,

    /// Radars that were connected during recording.
    pub radars: Vec<RecordedRadar>,

    /// Number of events in the recording.
    pub event_count: u64,

    /// User annotations.
    pub annotations: Vec<Annotation>,
}

/// Information about a recorded radar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedRadar {
    pub radar_id: String,
    pub brand: String,
    pub model: Option<String>,
    pub address: Option<String>,
}

/// User annotation on a recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    /// Timestamp in the recording.
    pub timestamp: u64,

    /// User's note.
    pub note: String,
}

// =============================================================================
// Recording File Format
// =============================================================================

/// A complete debug recording.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugRecording {
    pub metadata: RecordingMetadata,
    pub events: Vec<DebugEvent>,
}

// =============================================================================
// DebugRecorder
// =============================================================================

/// Records debug events to a file.
pub struct DebugRecorder {
    output_dir: PathBuf,
    recording: AtomicBool,
    current_file: RwLock<Option<PathBuf>>,
    events: RwLock<Vec<DebugEvent>>,
    metadata: RwLock<Option<RecordingMetadata>>,
}

impl DebugRecorder {
    /// Create a new recorder.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            recording: AtomicBool::new(false),
            current_file: RwLock::new(None),
            events: RwLock::new(Vec::new()),
            metadata: RwLock::new(None),
        }
    }

    /// Check if currently recording.
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// Start recording.
    ///
    /// Returns the filename that will be used.
    pub fn start(&self, radars: Vec<RecordedRadar>) -> Result<String, std::io::Error> {
        if self.is_recording() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "Already recording",
            ));
        }

        // Create output directory if needed
        fs::create_dir_all(&self.output_dir)?;

        // Generate filename
        let now = Utc::now();
        let filename = format!("debug-{}.mdbg", now.format("%Y%m%d-%H%M%S"));
        let path = self.output_dir.join(&filename);

        // Initialize metadata
        let metadata = RecordingMetadata {
            format_version: 1,
            start_time: now,
            end_time: None,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            radars,
            event_count: 0,
            annotations: Vec::new(),
        };

        *self.metadata.write().unwrap() = Some(metadata);
        *self.current_file.write().unwrap() = Some(path);
        self.events.write().unwrap().clear();
        self.recording.store(true, Ordering::SeqCst);

        Ok(filename)
    }

    /// Stop recording and save to file.
    ///
    /// Returns the path to the saved file.
    pub fn stop(&self) -> Result<PathBuf, std::io::Error> {
        if !self.is_recording() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Not recording",
            ));
        }

        self.recording.store(false, Ordering::SeqCst);

        // Finalize metadata
        let mut metadata = self.metadata.write().unwrap().take().unwrap();
        metadata.end_time = Some(Utc::now());
        metadata.event_count = self.events.read().unwrap().len() as u64;

        // Get path
        let path = self.current_file.write().unwrap().take().unwrap();

        // Create recording
        let recording = DebugRecording {
            metadata,
            events: std::mem::take(&mut *self.events.write().unwrap()),
        };

        // Write to file
        let file = File::create(&path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &recording)?;

        Ok(path)
    }

    /// Add an event to the current recording.
    pub fn add_event(&self, event: DebugEvent) {
        if self.is_recording() {
            self.events.write().unwrap().push(event);
        }
    }

    /// Add an annotation to the current recording.
    pub fn add_annotation(&self, timestamp: u64, note: String) {
        if let Some(ref mut metadata) = *self.metadata.write().unwrap() {
            metadata.annotations.push(Annotation { timestamp, note });
        }
    }

    /// Get current event count.
    pub fn event_count(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Get output directory.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
}

// =============================================================================
// DebugPlayer
// =============================================================================

/// Plays back a debug recording.
pub struct DebugPlayer {
    recording: DebugRecording,
    position: usize,
}

impl DebugPlayer {
    /// Load a recording from file.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let recording: DebugRecording = serde_json::from_reader(reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(Self {
            recording,
            position: 0,
        })
    }

    /// Get the recording metadata.
    pub fn metadata(&self) -> &RecordingMetadata {
        &self.recording.metadata
    }

    /// Get all events.
    pub fn events(&self) -> &[DebugEvent] {
        &self.recording.events
    }

    /// Get events in a time range.
    pub fn events_in_range(&self, start: u64, end: u64) -> Vec<&DebugEvent> {
        self.recording
            .events
            .iter()
            .filter(|e| e.timestamp >= start && e.timestamp <= end)
            .collect()
    }

    /// Get total duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.recording
            .events
            .last()
            .map(|e| e.timestamp)
            .unwrap_or(0)
    }

    /// Seek to a position (event index).
    pub fn seek(&mut self, position: usize) {
        self.position = position.min(self.recording.events.len());
    }

    /// Get next event without advancing.
    pub fn peek(&self) -> Option<&DebugEvent> {
        self.recording.events.get(self.position)
    }

    /// Get next event and advance position.
    pub fn next(&mut self) -> Option<&DebugEvent> {
        let event = self.recording.events.get(self.position);
        if event.is_some() {
            self.position += 1;
        }
        event
    }

    /// Reset to beginning.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Check if at end.
    pub fn is_finished(&self) -> bool {
        self.position >= self.recording.events.len()
    }
}

// =============================================================================
// Recording Manager
// =============================================================================

/// Manages debug recordings.
pub struct RecordingManager {
    output_dir: PathBuf,
}

impl RecordingManager {
    /// Create a new recording manager.
    pub fn new(output_dir: PathBuf) -> Self {
        Self { output_dir }
    }

    /// List all recordings.
    pub fn list(&self) -> Result<Vec<RecordingInfo>, std::io::Error> {
        let mut recordings = Vec::new();

        if !self.output_dir.exists() {
            return Ok(recordings);
        }

        for entry in fs::read_dir(&self.output_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "mdbg").unwrap_or(false) {
                if let Ok(info) = self.get_recording_info(&path) {
                    recordings.push(info);
                }
            }
        }

        // Sort by date, newest first
        recordings.sort_by(|a, b| b.start_time.cmp(&a.start_time));

        Ok(recordings)
    }

    /// Get info about a recording without loading all events.
    fn get_recording_info(&self, path: &Path) -> Result<RecordingInfo, std::io::Error> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;

        // Read just the metadata (first part of JSON)
        // For simplicity, we load the whole file
        let reader = BufReader::new(file);
        let recording: DebugRecording = serde_json::from_reader(reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(RecordingInfo {
            filename: path.file_name().unwrap().to_string_lossy().to_string(),
            path: path.to_path_buf(),
            size_bytes: metadata.len(),
            start_time: recording.metadata.start_time,
            end_time: recording.metadata.end_time,
            event_count: recording.metadata.event_count,
            radars: recording.metadata.radars,
        })
    }

    /// Delete a recording.
    pub fn delete(&self, filename: &str) -> Result<(), std::io::Error> {
        let path = self.output_dir.join(filename);
        fs::remove_file(path)
    }
}

/// Summary info about a recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub filename: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub event_count: u64,
    pub radars: Vec<RecordedRadar>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_test_event(i: usize) -> DebugEvent {
        DebugEvent {
            id: i as u64,
            timestamp: i as u64 * 100,
            radar_id: "radar-1".to_string(),
            brand: "furuno".to_string(),
            source: super::super::EventSource::IoProvider,
            payload: super::super::DebugEventPayload::Data {
                direction: super::super::IoDirection::Send,
                protocol: super::super::ProtocolType::Tcp,
                local_addr: None,
                remote_addr: "172.31.1.4".to_string(),
                remote_port: 10050,
                raw_hex: format!("event{}", i),
                raw_ascii: format!("event{}", i),
                decoded: None,
                length: 6,
            },
        }
    }

    #[test]
    fn test_recorder_start_stop() {
        let dir = tempdir().unwrap();
        let recorder = DebugRecorder::new(dir.path().to_path_buf());

        assert!(!recorder.is_recording());

        let filename = recorder.start(Vec::new()).unwrap();
        assert!(recorder.is_recording());
        assert!(filename.ends_with(".mdbg"));

        let path = recorder.stop().unwrap();
        assert!(!recorder.is_recording());
        assert!(path.exists());
    }

    #[test]
    fn test_recording_events() {
        let dir = tempdir().unwrap();
        let recorder = DebugRecorder::new(dir.path().to_path_buf());

        recorder.start(Vec::new()).unwrap();

        // Add some events
        for i in 0..5 {
            recorder.add_event(make_test_event(i));
        }

        assert_eq!(recorder.event_count(), 5);

        let path = recorder.stop().unwrap();

        // Load and verify
        let player = DebugPlayer::load(&path).unwrap();
        assert_eq!(player.events().len(), 5);
    }
}
