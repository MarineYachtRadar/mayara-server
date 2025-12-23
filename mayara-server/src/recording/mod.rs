//! Radar recording and playback module.
//!
//! This module provides functionality to:
//! - Record radar data to `.mrr` files (MaYaRa Radar Recording)
//! - Play back recordings as virtual radars
//! - Manage recording files (list, upload, download, delete)
//!
//! ## File Format
//!
//! The `.mrr` format is a simple binary format optimized for streaming:
//!
//! ```text
//! ┌──────────────────────────┐
//! │ Header (256 bytes)       │  magic "MRR1", version, radar metadata
//! ├──────────────────────────┤
//! │ Capabilities (JSON)      │  length-prefixed JSON
//! ├──────────────────────────┤
//! │ Initial State (JSON)     │  length-prefixed JSON (controls state)
//! ├──────────────────────────┤
//! │ Frame 0                  │  timestamp + protobuf RadarMessage
//! │ Frame 1                  │
//! │ ...                      │
//! ├──────────────────────────┤
//! │ Index (for seeking)      │  array of (timestamp, file_offset)
//! ├──────────────────────────┤
//! │ Footer (32 bytes)        │  index offset, frame count, duration
//! └──────────────────────────┘
//! ```

pub mod file_format;
pub mod manager;
pub mod player;
pub mod recorder;

pub use file_format::{MrrFooter, MrrHeader, MrrIndexEntry, MrrReader, MrrWriter};
pub use manager::{recordings_dir, RecordingInfo, RecordingManager};
pub use player::{
    load_recording, unregister_playback_radar, ActivePlayback, PlaybackSettings, PlaybackState,
    PlaybackStatus,
};
pub use recorder::{
    build_initial_state, start_recording, ActiveRecording, RecordingState, RecordingStatus,
};
