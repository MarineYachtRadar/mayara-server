//! Protocol decoders for debug events.
//!
//! Each brand has its own decoder that attempts to parse raw bytes
//! into structured protocol messages.

mod furuno;
mod garmin;
mod navico;
mod raymarine;

pub use furuno::FurunoDecoder;
pub use garmin::GarminDecoder;
pub use navico::NavicoDecoder;
pub use raymarine::RaymarineDecoder;

use super::{DecodedMessage, IoDirection};

// =============================================================================
// ProtocolDecoder Trait
// =============================================================================

/// Trait for protocol decoders.
///
/// Decoders attempt to parse raw bytes into structured messages.
/// If parsing fails, they should return `DecodedMessage::Unknown` with
/// as much partial information as possible.
pub trait ProtocolDecoder: Send + Sync {
    /// Decode raw bytes into a protocol message.
    ///
    /// # Arguments
    ///
    /// * `data` - The raw bytes to decode.
    /// * `direction` - Whether this is send or receive data.
    ///
    /// # Returns
    ///
    /// A decoded message, or `DecodedMessage::Unknown` if parsing fails.
    fn decode(&self, data: &[u8], direction: IoDirection) -> DecodedMessage;

    /// Get the brand name for this decoder.
    fn brand(&self) -> &'static str;
}

// =============================================================================
// Decoder Factory
// =============================================================================

/// Create a decoder for the given brand.
pub fn create_decoder(brand: &str) -> Box<dyn ProtocolDecoder + Send + Sync> {
    match brand.to_lowercase().as_str() {
        "furuno" => Box::new(FurunoDecoder),
        "navico" => Box::new(NavicoDecoder),
        "raymarine" => Box::new(RaymarineDecoder),
        "garmin" => Box::new(GarminDecoder),
        _ => Box::new(UnknownDecoder),
    }
}

// =============================================================================
// Unknown Decoder (fallback)
// =============================================================================

/// Fallback decoder for unknown brands.
struct UnknownDecoder;

impl ProtocolDecoder for UnknownDecoder {
    fn decode(&self, _data: &[u8], _direction: IoDirection) -> DecodedMessage {
        DecodedMessage::Unknown {
            reason: "Unknown brand".to_string(),
            partial: None,
        }
    }

    fn brand(&self) -> &'static str {
        "unknown"
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_decoder_furuno() {
        let decoder = create_decoder("furuno");
        assert_eq!(decoder.brand(), "furuno");
    }

    #[test]
    fn test_create_decoder_navico() {
        let decoder = create_decoder("navico");
        assert_eq!(decoder.brand(), "navico");
    }

    #[test]
    fn test_create_decoder_unknown() {
        let decoder = create_decoder("nonexistent");
        assert_eq!(decoder.brand(), "unknown");
    }

    #[test]
    fn test_create_decoder_case_insensitive() {
        let decoder = create_decoder("FURUNO");
        assert_eq!(decoder.brand(), "furuno");
    }
}
