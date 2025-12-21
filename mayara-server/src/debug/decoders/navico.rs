//! Navico protocol decoder.
//!
//! Decodes Navico binary UDP protocol messages (Simrad, B&G, Lowrance).
//!
//! Navico uses several report types:
//! - 0x01: Status report (power state)
//! - 0x02: Settings report (gain, sea, rain, interference rejection)
//! - 0x03: Firmware info
//! - 0x04: Diagnostic/bearing alignment
//! - 0x08: Range report
//!
//! Report packets have a header structure:
//! - Byte 0: Report type
//! - Byte 1-3: Length/flags
//! - Byte 4+: Report-specific data

use super::ProtocolDecoder;
use crate::debug::{DecodedMessage, IoDirection};

// =============================================================================
// Report offsets (from mayara-core/src/protocol/navico)
// =============================================================================

/// Status report (0x01) - power state
mod status_report {
    pub const STATUS_OFFSET: usize = 2; // 0=off, 1=standby, 2=warmup, 3=transmit
}

/// Settings report (0x02) - gain, sea, rain, interference rejection
mod settings_report {
    pub const GAIN_OFFSET: usize = 12; // Gain value (0-255)
    pub const GAIN_AUTO_OFFSET: usize = 11; // 0=manual, 1=auto
    pub const SEA_OFFSET: usize = 17; // Sea clutter value (0-255)
    pub const SEA_AUTO_OFFSET: usize = 21; // 0=manual, 1=auto, 2=calm, 3=moderate, 4=rough
    pub const RAIN_OFFSET: usize = 22; // Rain clutter value (0-255)
    pub const INTERFERENCE_OFFSET: usize = 5; // Interference rejection (0-3)
}

// =============================================================================
// NavicoDecoder
// =============================================================================

/// Decoder for Navico radar protocol.
///
/// Navico uses binary UDP packets with various report types.
pub struct NavicoDecoder;

impl ProtocolDecoder for NavicoDecoder {
    fn decode(&self, data: &[u8], direction: IoDirection) -> DecodedMessage {
        if data.is_empty() {
            return DecodedMessage::Unknown {
                reason: "Empty data".to_string(),
                partial: None,
            };
        }

        // Navico packets typically have a report type in the first few bytes
        // The exact format varies by message type

        let message_type = identify_navico_message(data, direction);
        let (description, fields) = decode_navico_fields(data, &message_type);

        DecodedMessage::Navico {
            message_type,
            report_id: data.first().copied(),
            fields,
            description,
        }
    }

    fn brand(&self) -> &'static str {
        "navico"
    }
}

/// Identify the type of Navico message.
fn identify_navico_message(data: &[u8], direction: IoDirection) -> String {
    // Basic identification based on packet structure
    if data.len() >= 2 {
        let first_byte = data[0];

        // Spoke data typically starts with specific patterns and is large
        if data.len() > 100 {
            return "spoke".to_string();
        }

        // Status/control reports based on first byte (report type)
        match first_byte {
            0x01 => return "status".to_string(),
            0x02 => return "settings".to_string(),
            0x03 => return "firmware".to_string(),
            0x04 => return "diagnostic".to_string(),
            0x08 => return "range".to_string(),
            _ => {}
        }

        // Check for common patterns
        if direction == IoDirection::Send && data.len() < 20 {
            return "command".to_string();
        }
    }

    "unknown".to_string()
}

/// Decode Navico message fields.
fn decode_navico_fields(data: &[u8], message_type: &str) -> (Option<String>, serde_json::Value) {
    match message_type {
        "spoke" => {
            // Extract basic spoke info
            let angle = if data.len() >= 4 {
                u16::from_le_bytes([data[0], data[1]]) as i32
            } else {
                0
            };
            (
                Some(format!("Spoke data (angle: {})", angle)),
                serde_json::json!({
                    "angle": angle,
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(16)])
                }),
            )
        }
        "status" => {
            // Status report (0x01) - contains power state
            let power_state = data.get(status_report::STATUS_OFFSET).copied().unwrap_or(0);
            let power_str = match power_state {
                0 => "off",
                1 => "standby",
                2 => "warmup",
                3 => "transmit",
                _ => "unknown",
            };

            (
                Some(format!("Status: {}", power_str)),
                serde_json::json!({
                    "power": power_state,
                    "powerStr": power_str,
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            )
        }
        "settings" => {
            // Settings report (0x02) - contains gain, sea, rain
            let gain_auto = data.get(settings_report::GAIN_AUTO_OFFSET).copied().unwrap_or(0);
            let gain = data.get(settings_report::GAIN_OFFSET).copied().unwrap_or(0);
            let sea_auto = data.get(settings_report::SEA_AUTO_OFFSET).copied().unwrap_or(0);
            let sea = data.get(settings_report::SEA_OFFSET).copied().unwrap_or(0);
            let rain = data.get(settings_report::RAIN_OFFSET).copied().unwrap_or(0);
            let interference = data.get(settings_report::INTERFERENCE_OFFSET).copied().unwrap_or(0);

            let desc = format!(
                "Gain: {} ({}), Sea: {} ({}), Rain: {}",
                gain,
                if gain_auto == 1 { "Auto" } else { "Manual" },
                sea,
                match sea_auto {
                    0 => "Manual",
                    1 => "Auto",
                    2 => "Calm",
                    3 => "Moderate",
                    4 => "Rough",
                    _ => "Unknown",
                },
                rain
            );

            (
                Some(desc),
                serde_json::json!({
                    "gain": gain,
                    "gainAuto": gain_auto == 1,
                    "sea": sea,
                    "seaAuto": sea_auto,
                    "rain": rain,
                    "interference": interference,
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            )
        }
        "range" => {
            // Range report (0x08)
            let range_raw = if data.len() >= 8 {
                u32::from_le_bytes([
                    data.get(4).copied().unwrap_or(0),
                    data.get(5).copied().unwrap_or(0),
                    data.get(6).copied().unwrap_or(0),
                    data.get(7).copied().unwrap_or(0),
                ])
            } else {
                0
            };

            (
                Some(format!("Range: {} dm", range_raw)),
                serde_json::json!({
                    "rangeRaw": range_raw,
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            )
        }
        "command" => (
            Some("Control command".to_string()),
            serde_json::json!({
                "length": data.len(),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        _ => (
            None,
            serde_json::json!({
                "length": data.len(),
                "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
            }),
        ),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_status_report() {
        let decoder = NavicoDecoder;
        // Status report: type=0x01, status at offset 2 = 3 (transmit)
        let mut data = vec![0x01, 0x00, 0x03];
        data.extend(vec![0x00; 10]); // Pad to reasonable size

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Navico {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("power").and_then(|v| v.as_u64()), Some(3));
                assert_eq!(
                    fields.get("powerStr").and_then(|v| v.as_str()),
                    Some("transmit")
                );
            }
            _ => panic!("Expected Navico message"),
        }
    }

    #[test]
    fn test_decode_settings_report() {
        let decoder = NavicoDecoder;
        // Settings report: type=0x02
        // Build a packet with known values at the right offsets
        let mut data = vec![0x00; 32];
        data[0] = 0x02; // Report type
        data[settings_report::GAIN_AUTO_OFFSET] = 0; // Manual
        data[settings_report::GAIN_OFFSET] = 75; // Gain value
        data[settings_report::SEA_AUTO_OFFSET] = 2; // Calm
        data[settings_report::SEA_OFFSET] = 50; // Sea value
        data[settings_report::RAIN_OFFSET] = 25; // Rain value
        data[settings_report::INTERFERENCE_OFFSET] = 1; // Interference

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Navico {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "settings");
                assert_eq!(fields.get("gain").and_then(|v| v.as_u64()), Some(75));
                assert_eq!(fields.get("gainAuto").and_then(|v| v.as_bool()), Some(false));
                assert_eq!(fields.get("sea").and_then(|v| v.as_u64()), Some(50));
                assert_eq!(fields.get("seaAuto").and_then(|v| v.as_u64()), Some(2)); // Calm
                assert_eq!(fields.get("rain").and_then(|v| v.as_u64()), Some(25));
                assert_eq!(fields.get("interference").and_then(|v| v.as_u64()), Some(1));
            }
            _ => panic!("Expected Navico message"),
        }
    }

    #[test]
    fn test_decode_settings_auto_gain() {
        let decoder = NavicoDecoder;
        let mut data = vec![0x00; 32];
        data[0] = 0x02; // Report type
        data[settings_report::GAIN_AUTO_OFFSET] = 1; // Auto
        data[settings_report::GAIN_OFFSET] = 128; // Gain value

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Navico { fields, .. } => {
                assert_eq!(fields.get("gainAuto").and_then(|v| v.as_bool()), Some(true));
            }
            _ => panic!("Expected Navico message"),
        }
    }

    #[test]
    fn test_decode_empty() {
        let decoder = NavicoDecoder;
        let msg = decoder.decode(&[], IoDirection::Recv);

        assert!(matches!(msg, DecodedMessage::Unknown { .. }));
    }
}
