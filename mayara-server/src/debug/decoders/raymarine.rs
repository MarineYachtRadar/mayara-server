//! Raymarine protocol decoder.
//!
//! Decodes Raymarine binary UDP protocol messages (Quantum, RD series).
//!
//! Raymarine has two main protocol variants:
//! - **Quantum** (solid-state): 2-byte opcode format with 0x28 marker
//!   - Commands: [opcode_lo, opcode_hi, 0x28, value, ...]
//!   - Status: 260+ byte packets with controls at fixed offsets
//! - **RD** (magnetron): Legacy format with lead bytes
//!   - Commands: [0x00, 0xC1, lead, value, 0x00, ...]
//!   - Status: 250+ byte packets with controls at fixed offsets

use super::ProtocolDecoder;
use crate::debug::{DecodedMessage, IoDirection};

// =============================================================================
// Report offsets for status extraction
// =============================================================================

/// RD series status packet offsets (250+ byte packets)
mod rd_status {
    pub const STATUS_OFFSET: usize = 180; // 0=standby, 1=transmit
    pub const GAIN_OFFSET: usize = 200;
    pub const SEA_OFFSET: usize = 208;
    pub const RAIN_OFFSET: usize = 213;
}

/// Quantum status packet offsets (260 byte packets)
mod quantum_status {
    pub const STATUS_OFFSET: usize = 4; // Power state
    pub const GAIN_AUTO_OFFSET: usize = 10; // Auto gain flag
    pub const GAIN_OFFSET: usize = 11; // Gain value
    pub const SEA_AUTO_OFFSET: usize = 15; // Auto sea flag
    pub const SEA_OFFSET: usize = 16; // Sea value
    pub const RAIN_OFFSET: usize = 20; // Rain value
}

// =============================================================================
// RaymarineDecoder
// =============================================================================

/// Decoder for Raymarine radar protocol.
///
/// Raymarine has two main protocol variants:
/// - Quantum (solid-state): 2-byte opcode format
/// - RD (magnetron): Different format with lead bytes
pub struct RaymarineDecoder;

impl ProtocolDecoder for RaymarineDecoder {
    fn decode(&self, data: &[u8], direction: IoDirection) -> DecodedMessage {
        if data.is_empty() {
            return DecodedMessage::Unknown {
                reason: "Empty data".to_string(),
                partial: None,
            };
        }

        let (variant, message_type, description, fields) = decode_raymarine(data, direction);

        DecodedMessage::Raymarine {
            message_type,
            variant,
            fields,
            description,
        }
    }

    fn brand(&self) -> &'static str {
        "raymarine"
    }
}

/// Decode a Raymarine packet.
fn decode_raymarine(
    data: &[u8],
    _direction: IoDirection,
) -> (Option<String>, String, Option<String>, serde_json::Value) {
    // Try to identify variant and message type
    if data.len() >= 56 {
        // Could be a beacon packet (56 bytes)
        if is_beacon(data) {
            return (
                None,
                "beacon".to_string(),
                Some("Discovery beacon".to_string()),
                serde_json::json!({
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            );
        }
    }

    // Check for Quantum status packet (260+ bytes)
    if data.len() >= 260 {
        return decode_quantum_status(data);
    }

    // Check for RD status packet (250+ bytes but < 260)
    if data.len() >= 250 && data.len() < 260 {
        return decode_rd_status(data);
    }

    // Quantum format: [opcode_lo, opcode_hi, 0x28, value, ...]
    if data.len() >= 4 && data.get(2) == Some(&0x28) {
        let opcode = u16::from_le_bytes([data[0], data[1]]);
        let value = data.get(3).copied().unwrap_or(0);
        let (desc, fields) = decode_quantum_command(opcode, value, &data[4..]);

        return (
            Some("quantum".to_string()),
            "command".to_string(),
            desc,
            fields,
        );
    }

    // RD format: [0x00, 0xc1, lead, value, 0x00, ...]
    if data.len() >= 5 && data.starts_with(&[0x00, 0xc1]) {
        let lead = data[2];
        let value = data[3];
        let (desc, fields) = decode_rd_command(lead, value);

        return (Some("rd".to_string()), "command".to_string(), desc, fields);
    }

    // Spoke data (typically 100-200 bytes, but not status-sized)
    if data.len() > 100 && data.len() < 250 {
        return (
            None,
            "spoke".to_string(),
            Some("Spoke data".to_string()),
            serde_json::json!({
                "length": data.len()
            }),
        );
    }

    // Unknown
    (
        None,
        "unknown".to_string(),
        None,
        serde_json::json!({
            "length": data.len(),
            "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
        }),
    )
}

/// Decode Quantum status packet (260+ bytes)
fn decode_quantum_status(
    data: &[u8],
) -> (Option<String>, String, Option<String>, serde_json::Value) {
    let power_state = data
        .get(quantum_status::STATUS_OFFSET)
        .copied()
        .unwrap_or(0);
    let gain_auto = data
        .get(quantum_status::GAIN_AUTO_OFFSET)
        .copied()
        .unwrap_or(0);
    let gain = data.get(quantum_status::GAIN_OFFSET).copied().unwrap_or(0);
    let sea_auto = data
        .get(quantum_status::SEA_AUTO_OFFSET)
        .copied()
        .unwrap_or(0);
    let sea = data.get(quantum_status::SEA_OFFSET).copied().unwrap_or(0);
    let rain = data.get(quantum_status::RAIN_OFFSET).copied().unwrap_or(0);

    let power_str = match power_state {
        0 => "standby",
        1 => "transmit",
        _ => "unknown",
    };

    let desc = format!(
        "Quantum Status: {} | Gain: {} ({}) | Sea: {} ({}) | Rain: {}",
        power_str,
        gain,
        if gain_auto == 1 { "Auto" } else { "Manual" },
        sea,
        if sea_auto == 1 { "Auto" } else { "Manual" },
        rain
    );

    (
        Some("quantum".to_string()),
        "status".to_string(),
        Some(desc),
        serde_json::json!({
            "power": power_state,
            "powerStr": power_str,
            "gain": gain,
            "gainAuto": gain_auto == 1,
            "sea": sea,
            "seaAuto": sea_auto == 1,
            "rain": rain,
            "length": data.len()
        }),
    )
}

/// Decode RD status packet (250-259 bytes)
fn decode_rd_status(data: &[u8]) -> (Option<String>, String, Option<String>, serde_json::Value) {
    let power_state = data.get(rd_status::STATUS_OFFSET).copied().unwrap_or(0);
    let gain = data.get(rd_status::GAIN_OFFSET).copied().unwrap_or(0);
    let sea = data.get(rd_status::SEA_OFFSET).copied().unwrap_or(0);
    let rain = data.get(rd_status::RAIN_OFFSET).copied().unwrap_or(0);

    let power_str = match power_state {
        0 => "standby",
        1 => "transmit",
        _ => "unknown",
    };

    let desc = format!(
        "RD Status: {} | Gain: {} | Sea: {} | Rain: {}",
        power_str, gain, sea, rain
    );

    (
        Some("rd".to_string()),
        "status".to_string(),
        Some(desc),
        serde_json::json!({
            "power": power_state,
            "powerStr": power_str,
            "gain": gain,
            "sea": sea,
            "rain": rain,
            "length": data.len()
        }),
    )
}

/// Check if data looks like a beacon packet.
fn is_beacon(data: &[u8]) -> bool {
    // Beacons are typically 56 bytes with specific patterns
    data.len() == 56 || data.len() == 36
}

/// Decode a Quantum format command.
fn decode_quantum_command(
    opcode: u16,
    value: u8,
    _rest: &[u8],
) -> (Option<String>, serde_json::Value) {
    let desc = match opcode {
        0xc401 => Some(format!("Gain: {}", value)),
        0xc402 => Some(format!("Sea: {}", value)),
        0xc403 => Some(format!("Rain: {}", value)),
        0xc404 => Some(format!("Range index: {}", value)),
        0xc405 => Some(format!("Power: {}", if value == 1 { "ON" } else { "OFF" })),
        _ => None,
    };

    (
        desc,
        serde_json::json!({
            "opcode": format!("0x{:04x}", opcode),
            "value": value
        }),
    )
}

/// Decode an RD format command.
fn decode_rd_command(lead: u8, value: u8) -> (Option<String>, serde_json::Value) {
    let desc = match lead {
        0x01 => Some(format!("Gain: {}", value)),
        0x02 => Some(format!("Sea: {}", value)),
        0x03 => Some(format!("Rain: {}", value)),
        _ => None,
    };

    (
        desc,
        serde_json::json!({
            "lead": format!("0x{:02x}", lead),
            "value": value
        }),
    )
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_quantum_command() {
        let decoder = RaymarineDecoder;
        let msg = decoder.decode(&[0x01, 0xc4, 0x28, 0x32], IoDirection::Send);

        match msg {
            DecodedMessage::Raymarine {
                variant,
                message_type,
                ..
            } => {
                assert_eq!(variant, Some("quantum".to_string()));
                assert_eq!(message_type, "command");
            }
            _ => panic!("Expected Raymarine message"),
        }
    }

    #[test]
    fn test_decode_rd_command() {
        let decoder = RaymarineDecoder;
        let msg = decoder.decode(&[0x00, 0xc1, 0x01, 0x32, 0x00], IoDirection::Send);

        match msg {
            DecodedMessage::Raymarine {
                variant,
                message_type,
                ..
            } => {
                assert_eq!(variant, Some("rd".to_string()));
                assert_eq!(message_type, "command");
            }
            _ => panic!("Expected Raymarine message"),
        }
    }

    #[test]
    fn test_decode_quantum_status() {
        let decoder = RaymarineDecoder;
        // Create a 260 byte packet with known values
        let mut data = vec![0x00; 260];
        data[quantum_status::STATUS_OFFSET] = 1; // transmit
        data[quantum_status::GAIN_AUTO_OFFSET] = 0; // manual
        data[quantum_status::GAIN_OFFSET] = 75;
        data[quantum_status::SEA_AUTO_OFFSET] = 1; // auto
        data[quantum_status::SEA_OFFSET] = 50;
        data[quantum_status::RAIN_OFFSET] = 25;

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Raymarine {
                variant,
                message_type,
                fields,
                ..
            } => {
                assert_eq!(variant, Some("quantum".to_string()));
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("power").and_then(|v| v.as_u64()), Some(1));
                assert_eq!(
                    fields.get("powerStr").and_then(|v| v.as_str()),
                    Some("transmit")
                );
                assert_eq!(fields.get("gain").and_then(|v| v.as_u64()), Some(75));
                assert_eq!(
                    fields.get("gainAuto").and_then(|v| v.as_bool()),
                    Some(false)
                );
                assert_eq!(fields.get("sea").and_then(|v| v.as_u64()), Some(50));
                assert_eq!(fields.get("seaAuto").and_then(|v| v.as_bool()), Some(true));
                assert_eq!(fields.get("rain").and_then(|v| v.as_u64()), Some(25));
            }
            _ => panic!("Expected Raymarine message"),
        }
    }

    #[test]
    fn test_decode_rd_status() {
        let decoder = RaymarineDecoder;
        // Create a 255 byte packet (RD status size)
        let mut data = vec![0x00; 255];
        data[rd_status::STATUS_OFFSET] = 0; // standby
        data[rd_status::GAIN_OFFSET] = 60;
        data[rd_status::SEA_OFFSET] = 40;
        data[rd_status::RAIN_OFFSET] = 30;

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Raymarine {
                variant,
                message_type,
                fields,
                ..
            } => {
                assert_eq!(variant, Some("rd".to_string()));
                assert_eq!(message_type, "status");
                assert_eq!(
                    fields.get("powerStr").and_then(|v| v.as_str()),
                    Some("standby")
                );
                assert_eq!(fields.get("gain").and_then(|v| v.as_u64()), Some(60));
                assert_eq!(fields.get("sea").and_then(|v| v.as_u64()), Some(40));
                assert_eq!(fields.get("rain").and_then(|v| v.as_u64()), Some(30));
            }
            _ => panic!("Expected Raymarine message"),
        }
    }

    #[test]
    fn test_decode_empty() {
        let decoder = RaymarineDecoder;
        let msg = decoder.decode(&[], IoDirection::Recv);

        assert!(matches!(msg, DecodedMessage::Unknown { .. }));
    }
}
