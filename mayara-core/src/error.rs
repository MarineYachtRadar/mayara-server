//! Error types for protocol parsing

use thiserror::Error;

/// Errors that can occur when parsing radar packets
#[derive(Error, Debug, Clone, PartialEq)]
pub enum ParseError {
    /// Packet is too short to contain required data
    #[error("Packet too short: expected at least {expected} bytes, got {actual}")]
    TooShort { expected: usize, actual: usize },

    /// Packet header doesn't match expected format
    #[error("Invalid header: expected {expected:02X?}, got {actual:02X?}")]
    InvalidHeader { expected: Vec<u8>, actual: Vec<u8> },

    /// Length field doesn't match actual packet length
    #[error("Length mismatch: header says {header_len} bytes, packet has {actual_len}")]
    LengthMismatch {
        header_len: usize,
        actual_len: usize,
    },

    /// Failed to deserialize packet structure
    #[error("Deserialization failed: {0}")]
    DeserializationFailed(String),

    /// Unknown or unsupported radar model
    #[error("Unknown radar model: {0}")]
    UnknownModel(String),

    /// Invalid UTF-8 in string field
    #[error("Invalid string encoding")]
    InvalidString,

    /// Packet type not recognized
    #[error("Unknown packet type: {0:#04X}")]
    UnknownPacketType(u8),

    /// Invalid packet data
    #[error("Invalid packet: {0}")]
    InvalidPacket(String),
}

impl From<bincode::Error> for ParseError {
    fn from(e: bincode::Error) -> Self {
        ParseError::DeserializationFailed(e.to_string())
    }
}
