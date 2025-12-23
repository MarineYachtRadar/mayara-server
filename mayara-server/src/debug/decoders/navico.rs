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
///
/// Navico protocol uses the second byte to indicate message class:
/// - `0xC4` = Report (radar → host)
/// - `0xC2` = Request for reports (host → radar)
/// - `0xC1` = Control command (host → radar)
/// - `0xC6` = Response/acknowledgment
fn identify_navico_message(data: &[u8], direction: IoDirection) -> String {
    if data.len() < 2 {
        return "unknown".to_string();
    }

    let first_byte = data[0];
    let second_byte = data[1];

    // Spoke data typically starts with specific patterns and is large
    if data.len() > 100 {
        return "spoke".to_string();
    }

    // Check message class based on second byte
    match second_byte {
        0xC4 => {
            // Report from radar (XX C4)
            match first_byte {
                0x01 => "status".to_string(),
                0x02 => "settings".to_string(),
                0x03 => "firmware".to_string(),
                0x04 => "installation".to_string(),
                0x06 => "blanking".to_string(),
                0x07 => "statistics".to_string(),
                0x08 => "advanced".to_string(),
                0x09 => "tuning".to_string(),
                _ => format!("report-{:02x}", first_byte),
            }
        }
        0xC2 => {
            // Report request (XX C2)
            match first_byte {
                0x01 => "request-reports".to_string(),
                0x02 => "request-install".to_string(),
                0x03 => "request-settings".to_string(),
                0x04 => "request-model".to_string(),
                0x05 => "request-all".to_string(),
                0x0A => "request-install2".to_string(),
                _ => format!("request-{:02x}", first_byte),
            }
        }
        0xC1 => {
            // Control command (XX C1)
            match first_byte {
                0x00 => "cmd-prepare".to_string(),
                0x01 => "cmd-power".to_string(),
                0x03 => "cmd-range".to_string(),
                0x05 => "cmd-bearing".to_string(),
                0x06 => "cmd-gain-sea-rain".to_string(),
                0x08 => "cmd-ir".to_string(),
                0x09 => "cmd-target-exp".to_string(),
                0x0A => "cmd-target-boost".to_string(),
                0x0B => "cmd-sea-state".to_string(),
                0x0D => "cmd-notx-enable".to_string(),
                0x0E => "cmd-local-ir".to_string(),
                0x0F => "cmd-scan-speed".to_string(),
                0x10 => "cmd-mode".to_string(),
                0x11 => "cmd-sea-halo".to_string(),
                0x12 => "cmd-target-exp-halo".to_string(),
                0x21 => "cmd-noise".to_string(),
                0x22 => "cmd-target-sep".to_string(),
                0x23 => "cmd-doppler-mode".to_string(),
                0x24 => "cmd-doppler-speed".to_string(),
                0x30 => "cmd-antenna-height".to_string(),
                0x31 => "cmd-accent-light".to_string(),
                0xA0 => "cmd-stay-on".to_string(),
                0xC0 => "cmd-notx-angles".to_string(),
                _ => format!("cmd-{:02x}", first_byte),
            }
        }
        0xC6 => {
            // Response/acknowledgment
            format!("ack-{:02x}", first_byte)
        }
        _ => {
            // Unknown second byte - might be older format or different protocol
            if direction == IoDirection::Send && data.len() < 20 {
                "command".to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
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
            // Status report (0x01 0xC4) - contains power state
            let power_state = data.get(status_report::STATUS_OFFSET).copied().unwrap_or(0);
            let power_str = match power_state {
                0 => "off",
                1 => "standby",
                2 => "transmit",
                5 => "warming",
                _ => "unknown",
            };

            (
                Some(format!("Status: {}", power_str)),
                serde_json::json!({
                    "type": "status",
                    "power": power_state,
                    "powerStr": power_str,
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            )
        }
        "settings" => {
            // Settings report (0x02 0xC4) - contains gain, sea, rain
            let gain_auto = data
                .get(settings_report::GAIN_AUTO_OFFSET)
                .copied()
                .unwrap_or(0);
            let gain = data.get(settings_report::GAIN_OFFSET).copied().unwrap_or(0);
            let sea_auto = data
                .get(settings_report::SEA_AUTO_OFFSET)
                .copied()
                .unwrap_or(0);
            let sea = data.get(settings_report::SEA_OFFSET).copied().unwrap_or(0);
            let rain = data.get(settings_report::RAIN_OFFSET).copied().unwrap_or(0);
            let interference = data
                .get(settings_report::INTERFERENCE_OFFSET)
                .copied()
                .unwrap_or(0);

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
                    "type": "settings",
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
        "advanced" => {
            // Advanced settings report (0x08 0xC4)
            // Contains scan speed, doppler mode, etc.
            (
                Some("Advanced settings".to_string()),
                serde_json::json!({
                    "type": "advanced",
                    "length": data.len(),
                    "firstBytes": format!("{:02x?}", &data[..data.len().min(32)])
                }),
            )
        }
        // Request commands (XX C2)
        "request-reports" => (
            Some("Request reports (01 C2)".to_string()),
            serde_json::json!({
                "type": "request",
                "command": "request-reports",
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "request-model" => (
            Some("Request model info (04 C2)".to_string()),
            serde_json::json!({
                "type": "request",
                "command": "request-model",
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "request-settings" => (
            Some("Request settings (03 C2)".to_string()),
            serde_json::json!({
                "type": "request",
                "command": "request-settings",
                "bytes": format!("{:02x?}", data)
            }),
        ),
        // Control commands (XX C1)
        "cmd-prepare" => (
            Some("Prepare for power change (00 C1)".to_string()),
            serde_json::json!({
                "type": "command",
                "command": "prepare",
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "cmd-power" => {
            let state = data.get(2).copied().unwrap_or(0);
            let state_str = if state == 1 { "transmit" } else { "standby" };
            (
                Some(format!("Set power: {} (01 C1 {:02x})", state_str, state)),
                serde_json::json!({
                    "type": "command",
                    "command": "power",
                    "state": state,
                    "stateStr": state_str,
                    "bytes": format!("{:02x?}", data)
                }),
            )
        }
        "cmd-range" => {
            let range = if data.len() >= 6 {
                i32::from_le_bytes([
                    data.get(2).copied().unwrap_or(0),
                    data.get(3).copied().unwrap_or(0),
                    data.get(4).copied().unwrap_or(0),
                    data.get(5).copied().unwrap_or(0),
                ])
            } else {
                0
            };
            (
                Some(format!("Set range: {} dm ({} m)", range, range / 10)),
                serde_json::json!({
                    "type": "command",
                    "command": "range",
                    "rangeDm": range,
                    "rangeM": range / 10,
                    "bytes": format!("{:02x?}", data)
                }),
            )
        }
        "cmd-gain-sea-rain" => {
            // 06 C1 XX ... - subtype in byte 2
            let subtype = data.get(2).copied().unwrap_or(0);
            let desc = match subtype {
                0x00 => "Set gain",
                0x02 => "Set sea clutter",
                0x04 => "Set rain clutter",
                0x05 => "Set sidelobe suppression",
                _ => "Set control (unknown)",
            };
            (
                Some(format!("{} (06 C1 {:02x})", desc, subtype)),
                serde_json::json!({
                    "type": "command",
                    "command": "gain-sea-rain",
                    "subtype": subtype,
                    "bytes": format!("{:02x?}", data)
                }),
            )
        }
        "cmd-stay-on" => (
            Some("Stay-on keepalive (A0 C1)".to_string()),
            serde_json::json!({
                "type": "command",
                "command": "stay-on",
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "cmd-ir" => (
            Some("Set interference rejection".to_string()),
            serde_json::json!({
                "type": "command",
                "command": "interference-rejection",
                "value": data.get(2).copied().unwrap_or(0),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "cmd-scan-speed" => (
            Some("Set scan speed".to_string()),
            serde_json::json!({
                "type": "command",
                "command": "scan-speed",
                "value": data.get(2).copied().unwrap_or(0),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "cmd-doppler-mode" => (
            Some("Set doppler mode".to_string()),
            serde_json::json!({
                "type": "command",
                "command": "doppler-mode",
                "value": data.get(2).copied().unwrap_or(0),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        // Fallback for other commands
        _ if message_type.starts_with("cmd-") => (
            Some(format!("Command: {}", message_type)),
            serde_json::json!({
                "type": "command",
                "command": message_type,
                "length": data.len(),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        _ if message_type.starts_with("request-") => (
            Some(format!("Request: {}", message_type)),
            serde_json::json!({
                "type": "request",
                "command": message_type,
                "bytes": format!("{:02x?}", data)
            }),
        ),
        "command" => (
            Some("Control command".to_string()),
            serde_json::json!({
                "type": "command",
                "length": data.len(),
                "bytes": format!("{:02x?}", data)
            }),
        ),
        _ => (
            None,
            serde_json::json!({
                "type": message_type,
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
        // Status report: 0x01 0xC4, status at offset 2 = 2 (transmit)
        let mut data = vec![0x01, 0xC4, 0x02]; // 0x01 C4 = status report, offset 2 = transmit
        data.extend(vec![0x00; 15]); // Pad to 18 bytes (Report 01 size)

        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Navico {
                message_type,
                fields,
                ..
            } => {
                assert_eq!(message_type, "status");
                assert_eq!(fields.get("power").and_then(|v| v.as_u64()), Some(2));
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
        // Settings report: 0x02 0xC4
        // Build a packet with known values at the right offsets
        let mut data = vec![0x00; 99]; // Report 02 is 99 bytes
        data[0] = 0x02; // Report type
        data[1] = 0xC4; // Report class
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
                assert_eq!(
                    fields.get("gainAuto").and_then(|v| v.as_bool()),
                    Some(false)
                );
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
        let mut data = vec![0x00; 99];
        data[0] = 0x02; // Report type
        data[1] = 0xC4; // Report class
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
    fn test_decode_request_reports() {
        let decoder = NavicoDecoder;
        // Request reports command: 0x01 0xC2
        let data = vec![0x01, 0xC2];

        let msg = decoder.decode(&data, IoDirection::Send);

        match msg {
            DecodedMessage::Navico {
                message_type,
                description,
                ..
            } => {
                assert_eq!(message_type, "request-reports");
                assert!(description.unwrap().contains("Request reports"));
            }
            _ => panic!("Expected Navico message"),
        }
    }

    #[test]
    fn test_decode_power_command() {
        let decoder = NavicoDecoder;
        // Power command: 0x01 0xC1 0x01 (transmit)
        let data = vec![0x01, 0xC1, 0x01];

        let msg = decoder.decode(&data, IoDirection::Send);

        match msg {
            DecodedMessage::Navico {
                message_type,
                fields,
                description,
                ..
            } => {
                assert_eq!(message_type, "cmd-power");
                assert_eq!(fields.get("state").and_then(|v| v.as_u64()), Some(1));
                assert_eq!(
                    fields.get("stateStr").and_then(|v| v.as_str()),
                    Some("transmit")
                );
                assert!(description.unwrap().contains("transmit"));
            }
            _ => panic!("Expected Navico message"),
        }
    }

    #[test]
    fn test_decode_stay_on_command() {
        let decoder = NavicoDecoder;
        // Stay-on command: 0xA0 0xC1
        let data = vec![0xA0, 0xC1];

        let msg = decoder.decode(&data, IoDirection::Send);

        match msg {
            DecodedMessage::Navico {
                message_type,
                description,
                ..
            } => {
                assert_eq!(message_type, "cmd-stay-on");
                assert!(description.unwrap().contains("Stay-on"));
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
