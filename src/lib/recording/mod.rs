//! Radar recording and playback module.
//!
//! Provides functionality to:
//! - Record radar data to `.mrr` files (MaYaRa Radar Recording)
//! - Play back recordings as virtual radars
//! - Manage recording files (list, upload, download, delete)
//!
//! ## File Format
//!
//! The `.mrr` format is a binary format optimized for streaming:
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
mod identity;
pub mod manager;
pub mod player;
pub mod recorder;

pub use file_format::{MrrFooter, MrrHeader, MrrReader, MrrWriter};
pub use identity::RecordingIdentity;
pub use manager::{RecordingInfo, RecordingManager, recordings_dir};
pub use player::{ActivePlayback, PlaybackSettings, PlaybackState, PlaybackStatus};
pub use recorder::{ActiveRecording, RecordingState, RecordingStatus};
