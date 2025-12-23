//! Garmin protocol decoder.
//!
//! Decodes Garmin binary UDP protocol messages (xHD series).
//!
//! Garmin uses a binary UDP protocol with:
//! - 12-byte command packets (sent)
//! - Status reports with packet type at offset 0-3
//! - 1440 spokes per revolution
//!
//! Status packet types (first 4 bytes LE):
//! - 0x0919: Transmit state
//! - 0x0924: Gain mode, 0x0925: Gain value
//! - 0x0939: Sea mode, 0x093a: Sea value
//! - 0x0933: Rain mode, 0x0934: Rain value
//! - 0x091e: Range

use super::ProtocolDecoder;
use crate::debug::{DecodedMessage, IoDirection};

// =============================================================================
// Garmin packet type codes
// =============================================================================

/// Garmin status packet type codes (in first 4 bytes as u32 LE)
mod packet_types {
    pub const TRANSMIT: u32 = 0x0919;
    pub const GAIN_MODE: u32 = 0x0924;
    pub const GAIN_VALUE: u32 = 0x0925;
    pub const SEA_MODE: u32 = 0x0939;
    pub const SEA_VALUE: u32 = 0x093a;
    pub const RAIN_MODE: u32 = 0x0933;
    pub const RAIN_VALUE: u32 = 0x0934;
    pub const RANGE: u32 = 0x091e;
}

// =============================================================================
// GarminDecoder
// =============================================================================

/// Decoder for Garmin radar protocol.
///
/// Garmin uses a relatively simple binary UDP protocol:
/// - 12-byte command packets
/// - Multicast status broadcasts
/// - 1440 spokes per revolution
pub struct GarminDecoder;

impl ProtocolDecoder for GarminDecoder {
    fn decode(&self, data: &[u8], direction: IoDirection) -> DecodedMessage {
        if data.is_empty() {
            return DecodedMessage::Unknown {
                reason: "Empty data".to_string(),
                partial: None,
            };
        }

        let (message_type, description, fields) = decode_garmin(data, direction);

        DecodedMessage::Garmin {
            message_type,
            fields,
            description,
        }
    }

    fn brand(&self) -> &'static str {
        "garmin"
    }
}

/// Decode a Garmin packet.
fn decode_garmin(
    data: &[u8],
    direction: IoDirection,
) -> (String, Option<String>, serde_json::Value) {
    // Garmin commands are typically 12 bytes
    if direction == IoDirection::Send && data.len() == 12 {
        return decode_garmin_command(data);
    }

    // Status packets (received on multicast) - check packet type
    if data.len() >= 8 && data.len() < 100 {
        // Extract packet type from first 4 bytes (LE)
        let packet_type = if data.len() >= 4 {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
        } else {
            0
        };

        // Value is typically at offset 4-7 as u32 LE
        let value = if data.len() >= 8 {
            u32::from_le_bytes([data[4], data[5], data[6], data[7]])
        } else {
            0
        };

        return decode_garmin_status(packet_type, value, data);
    }

    // Spoke data (longer packets)
    if data.len() > 100 {
        // Try to extract angle from spoke
        let angle = if data.len() >= 4 {
            u16::from_le_bytes([data[0], data[1]]) as i32
        } else {
            0
        };

        return (
            "spoke".to_string(),
            Some(format!("Spoke data (angle: {})", angle)),
            serde_json::json!({
                "angle": angle,
                "length": data.len()
            }),
        );
    }

    // Unknown
    (
        "unknown".to_string(),
        None,
        serde_json::json!({
            "length": data.len(),
            "bytes": format!("{:02x?}", &data[..data.len().min(32)])
        }),
    )
}

/// Decode a Garmin status packet based on packet type.
fn decode_garmin_status(
    packet_type: u32,
    value: u32,
    data: &[u8],
) -> (String, Option<String>, serde_json::Value) {
    match packet_type {
        packet_types::TRANSMIT => {
            let power_str = if value == 1 { "transmit" } else { "standby" };
            (
                "status".to_string(),
                Some(format!("Power: {}", power_str)),
                serde_json::json!({
                    "packetType": format!("0x{:04x}", packet_type),
                    "power": value,
                    "powerStr": power_str,
                    "length": data.len()
                }),
            )
        }
        packet_types::GAIN_MODE => {
            let mode_str = if value == 1 { "auto" } else { "manual" };
            (
                "status".to_string(),
                Some(format!("Gain mode: {}", mode_str)),
                serde_json::json!({
                    "packetType": format!("0x{:04x}", packet_type),
                    "gainAuto": value == 1,
                    "length": data.len()
                }),
            )
        }
        packet_types::GAIN_VALUE => (
            "status".to_string(),
            Some(format!("Gain: {}", value)),
            serde_json::json!({
                "packetType": format!("0x{:04x}", packet_type),
                "gain": value,
                "length": data.len()
            }),
        ),
        packet_types::SEA_MODE => {
            let mode_str = if value == 1 { "auto" } else { "manual" };
            (
                "status".to_string(),
                Some(format!("Sea mode: {}", mode_str)),
                serde_json::json!({
                    "packetType": format!("0x{:04x}", packet_type),
                    "seaAuto": value == 1,
                    "length": data.len()
                }),
            )
        }
        packet_types::SEA_VALUE => (
            "status".to_string(),
            Some(format!("Sea: {}", value)),
            serde_json::json!({
                "packetType": format!("0x{:04x}", packet_type),
                "sea": value,
                "length": data.len()
            }),
        ),
        packet_types::RAIN_MODE => {
            let mode_str = if value == 1 { "auto" } else { "manual" };
            (
                "status".to_string(),
                Some(format!("Rain mode: {}", mode_str)),
                serde_json::json!({
                    "packetType": format!("0x{:04x}", packet_type),
                    "rainAuto": value == 1,
                    "length": data.len()
                }),
            )
        }
        packet_types::RAIN_VALUE => (
            "status".to_string(),
            Some(format!("Rain: {}", value)),
            serde_json::json!({
                "packetType": format!("0x{:04x}", packet_type),
                "rain": value,
                "length": data.len()
            }),
        ),
        packet_types::RANGE => (
            "status".to_string(),
            Some(format!("Range: {} m", value)),
            serde_json::json!({
                "packetType": format!("0x{:04x}", packet_type),
                "range": value,
                "length": data.len()
            }),
        ),
        _ => (
            "status".to_string(),
            None,
            serde_json::json!({
                "packetType": format!("0x{:04x}", packet_type),
                "value": value,
                "length": data.len(),
                "firstBytes": format!("{:02x?}", &data[..data.len().min(16)])
            }),
        ),
    }
}

/// Decode a 12-byte Garmin command.
fn decode_garmin_command(data: &[u8]) -> (String, Option<String>, serde_json::Value) {
    if data.len() != 12 {
        return (
            "command".to_string(),
            None,
            serde_json::json!({"length": data.len()}),
        );
    }

    // Command ID is typically in the first few bytes
    let cmd_type = data[0];
    let value = data.get(4).copied().unwrap_or(0);

    let desc = match cmd_type {
        0x01 => Some(format!("Power: {}", if value == 1 { "ON" } else { "OFF" })),
        0x02 => Some(format!("Range: {}", value)),
        0x03 => Some(format!("Gain: {}", value)),
        0x04 => Some(format!("Sea: {}", value)),
        0x05 => Some(format!("Rain: {}", value)),
        _ => None,
    };

    (
        "command".to_string(),
        desc,
        serde_json::json!({
            "commandType": format!("0x{:02x}", cmd_type),
            "value": value,
            "bytes": format!("{:02x?}", data)
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
    fn test_decode_command() {
        let decoder = GarminDecoder;
        let data = [
            0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let msg = decoder.decode(&data, IoDirection::Send);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                description,
                ..
            } => {
                assert_eq!(message_type, "command");
                assert!(description.unwrap().contains("Power"));
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_transmit_status() {
        let decoder = GarminDecoder;
        // Packet type 0x0919 (transmit), value 1 (transmit)
        let mut data = vec![0x19, 0x09, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        data.extend(vec![0x00; 8]); // Pad to 16 bytes

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("power").and_then(|v| v.as_u64()), Some(1));
                assert_eq!(
                    fields.get("powerStr").and_then(|v| v.as_str()),
                    Some("transmit")
                );
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_gain_value_status() {
        let decoder = GarminDecoder;
        // Packet type 0x0925 (gain value), value 75
        let mut data = vec![0x25, 0x09, 0x00, 0x00, 75, 0x00, 0x00, 0x00];
        data.extend(vec![0x00; 8]); // Pad to 16 bytes

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("gain").and_then(|v| v.as_u64()), Some(75));
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_gain_auto_status() {
        let decoder = GarminDecoder;
        // Packet type 0x0924 (gain mode), value 1 (auto)
        let mut data = vec![0x24, 0x09, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        data.extend(vec![0x00; 8]); // Pad to 16 bytes

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("gainAuto").and_then(|v| v.as_bool()), Some(true));
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_sea_value_status() {
        let decoder = GarminDecoder;
        // Packet type 0x093a (sea value), value 50
        let mut data = vec![0x3a, 0x09, 0x00, 0x00, 50, 0x00, 0x00, 0x00];
        data.extend(vec![0x00; 8]); // Pad to 16 bytes

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("sea").and_then(|v| v.as_u64()), Some(50));
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_rain_value_status() {
        let decoder = GarminDecoder;
        // Packet type 0x0934 (rain value), value 25
        let mut data = vec![0x34, 0x09, 0x00, 0x00, 25, 0x00, 0x00, 0x00];
        data.extend(vec![0x00; 8]); // Pad to 16 bytes

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("rain").and_then(|v| v.as_u64()), Some(25));
            }
            _ => panic!("Expected Garmin message"),
        }
    }

    #[test]
    fn test_decode_empty() {
        let decoder = GarminDecoder;
        let msg = decoder.decode(&[], IoDirection::Recv);

        assert!(matches!(msg, DecodedMessage::Unknown { .. }));
    }
}
