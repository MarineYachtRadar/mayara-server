//! Protocol Debugger - Real-time protocol analysis for reverse engineering.
//!
//! This module provides infrastructure for capturing, decoding, and analyzing
//! radar protocol traffic. It's only available when built with `--features dev`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        DebugHub                                  │
//! │  - Aggregates events from all DebugIoProviders                  │
//! │  - Ring buffer (10K events) for history                          │
//! │  - WebSocket broadcast to debug panel                            │
//! └────────────────────────────┬────────────────────────────────────┘
//!                              │
//!        ┌─────────────────────┼─────────────────────────┐
//!        ▼                     ▼                         ▼
//! ┌──────────────┐  ┌──────────────────┐  ┌──────────────────┐
//! │DebugIoProvider│  │DebugIoProvider   │  │ PassiveListener  │
//! │(Furuno)       │  │(Navico)          │  │(multicast)       │
//! └──────────────┘  └──────────────────┘  └──────────────────┘
//! ```

pub mod change_detection;
pub mod decoders;
pub mod hub;
pub mod io_wrapper;
pub mod passive_listener;
pub mod recording;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-exports
pub use decoders::ProtocolDecoder;
pub use hub::{DebugHub, DebugHubConfig};
pub use io_wrapper::DebugIoProvider;

// =============================================================================
// Core Types
// =============================================================================

/// Direction of network traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IoDirection {
    Send,
    Recv,
}

/// Protocol type being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolType {
    Udp,
    Tcp,
}

/// Source of the debug event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSource {
    /// Event captured through DebugIoProvider wrapper.
    IoProvider,
    /// Event captured through passive multicast listener.
    Passive,
}

// =============================================================================
// Socket Operations
// =============================================================================

/// Socket operation type for non-data events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum SocketOperation {
    Create { socket_type: ProtocolType },
    Bind { port: u16 },
    Connect { addr: String, port: u16 },
    JoinMulticast { group: String, interface: String },
    SetBroadcast { enabled: bool },
    Close,
}

// =============================================================================
// Decoded Messages
// =============================================================================

/// Decoded protocol message (brand-specific).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "brand", rename_all = "camelCase")]
pub enum DecodedMessage {
    /// Furuno protocol (ASCII-based TCP).
    #[serde(rename_all = "camelCase")]
    Furuno {
        /// Message category: "set", "request", "response", "keepalive", etc.
        message_type: String,
        /// Command identifier (e.g., "S69", "N69", "R69").
        command_id: Option<String>,
        /// Parsed fields as JSON.
        fields: serde_json::Value,
        /// Human-readable description.
        description: Option<String>,
    },

    /// Navico protocol (binary UDP).
    #[serde(rename_all = "camelCase")]
    Navico {
        /// Message type: "report", "spoke", "status", etc.
        message_type: String,
        /// Report ID for report messages.
        report_id: Option<u8>,
        /// Parsed fields as JSON.
        fields: serde_json::Value,
        /// Human-readable description.
        description: Option<String>,
    },

    /// Raymarine protocol (binary UDP).
    #[serde(rename_all = "camelCase")]
    Raymarine {
        /// Message type: "beacon", "command", "status", etc.
        message_type: String,
        /// Variant: "quantum" or "rd".
        variant: Option<String>,
        /// Parsed fields as JSON.
        fields: serde_json::Value,
        /// Human-readable description.
        description: Option<String>,
    },

    /// Garmin protocol (binary UDP).
    #[serde(rename_all = "camelCase")]
    Garmin {
        /// Message type: "status", "spoke", "command", etc.
        message_type: String,
        /// Parsed fields as JSON.
        fields: serde_json::Value,
        /// Human-readable description.
        description: Option<String>,
    },

    /// Unknown or unparseable message.
    #[serde(rename_all = "camelCase")]
    Unknown {
        /// Why parsing failed.
        reason: String,
        /// Partial decoding if structure is partially recognized.
        partial: Option<serde_json::Value>,
    },
}

impl DecodedMessage {
    /// Create an unknown message with a reason.
    pub fn unknown(reason: impl Into<String>) -> Self {
        DecodedMessage::Unknown {
            reason: reason.into(),
            partial: None,
        }
    }

    /// Check if this is an unknown message.
    pub fn is_unknown(&self) -> bool {
        matches!(self, DecodedMessage::Unknown { .. })
    }
}

// =============================================================================
// Debug Events
// =============================================================================

/// A single debug event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEvent {
    /// Unique event ID (monotonically increasing within session).
    pub id: u64,

    /// Timestamp in milliseconds since session start.
    pub timestamp: u64,

    /// Radar identifier this event is associated with.
    pub radar_id: String,

    /// Brand of the radar (furuno, navico, raymarine, garmin).
    pub brand: String,

    /// Source of this event.
    pub source: EventSource,

    /// The event payload.
    #[serde(flatten)]
    pub payload: DebugEventPayload,
}

/// Event payload - network data, socket operation, or state change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "eventType", rename_all = "camelCase")]
pub enum DebugEventPayload {
    /// Network data was sent or received.
    #[serde(rename_all = "camelCase")]
    Data {
        /// Send or receive direction.
        direction: IoDirection,
        /// UDP or TCP.
        protocol: ProtocolType,
        /// Local address (if known).
        local_addr: Option<String>,
        /// Remote address.
        remote_addr: String,
        /// Remote port.
        remote_port: u16,
        /// Raw bytes as hex string.
        raw_hex: String,
        /// Raw bytes as printable ASCII (non-printable as dots).
        raw_ascii: String,
        /// Decoded protocol message (if parseable).
        decoded: Option<DecodedMessage>,
        /// Number of bytes.
        length: usize,
    },

    /// Socket operation (connect, bind, etc.).
    #[serde(rename_all = "camelCase")]
    SocketOp {
        /// The operation performed.
        operation: SocketOperation,
        /// Whether it succeeded.
        success: bool,
        /// Error message if failed.
        error: Option<String>,
    },

    /// Radar state change detected.
    #[serde(rename_all = "camelCase")]
    StateChange {
        /// Control that changed.
        control_id: String,
        /// Previous value.
        before: serde_json::Value,
        /// New value.
        after: serde_json::Value,
        /// Event ID that likely triggered this change.
        trigger_event_id: Option<u64>,
    },
}

// =============================================================================
// State Snapshots
// =============================================================================

/// Snapshot of radar state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarStateSnapshot {
    /// Radar identifier.
    pub radar_id: String,

    /// Brand name.
    pub brand: String,

    /// Timestamp when snapshot was taken.
    pub timestamp: u64,

    /// Control values as JSON.
    pub controls: HashMap<String, serde_json::Value>,

    /// Connection state.
    pub connection_state: ConnectionState,
}

/// Connection state for a radar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error { message: String },
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Encode bytes as hex string.
pub fn hex_encode(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encode bytes as printable ASCII (non-printable as dots).
pub fn ascii_encode(data: &[u8]) -> String {
    data.iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x42]), "00 ff 42");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn test_ascii_encode() {
        assert_eq!(ascii_encode(b"Hello\x00World"), "Hello.World");
        assert_eq!(ascii_encode(&[0x00, 0x01, 0x02]), "...");
    }

    #[test]
    fn test_decoded_message_unknown() {
        let msg = DecodedMessage::unknown("Too short");
        assert!(msg.is_unknown());
    }

    #[test]
    fn test_event_serialization() {
        let event = DebugEvent {
            id: 1,
            timestamp: 1000,
            radar_id: "radar-1".to_string(),
            brand: "furuno".to_string(),
            source: EventSource::IoProvider,
            payload: DebugEventPayload::Data {
                direction: IoDirection::Send,
                protocol: ProtocolType::Tcp,
                local_addr: None,
                remote_addr: "172.31.1.4".to_string(),
                remote_port: 10050,
                raw_hex: "24 53 36 39".to_string(),
                raw_ascii: "$S69".to_string(),
                decoded: Some(DecodedMessage::Furuno {
                    message_type: "set".to_string(),
                    command_id: Some("S69".to_string()),
                    fields: serde_json::json!({"value": 50}),
                    description: Some("Set gain to 50".to_string()),
                }),
                length: 4,
            },
        };

        let json = serde_json::to_string_pretty(&event).unwrap();
        assert!(json.contains("radar-1"));
        assert!(json.contains("furuno"));
    }
}
