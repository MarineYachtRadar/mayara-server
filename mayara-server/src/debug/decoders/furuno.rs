//! Furuno protocol decoder.
//!
//! Decodes Furuno ASCII-based TCP protocol messages.

use super::ProtocolDecoder;
use crate::debug::{DecodedMessage, IoDirection};

// =============================================================================
// FurunoDecoder
// =============================================================================

/// Decoder for Furuno radar protocol.
///
/// Furuno uses ASCII text commands over TCP with format:
/// - `$Sxx,...` - Set commands (sent to radar)
/// - `$Rxx,...` - Request commands (sent to radar)
/// - `$Nxx,...` - Response/notification (from radar)
///
/// Some firmware versions wrap commands in an 8-byte binary header.
pub struct FurunoDecoder;

/// Login message header (first 12 bytes of 56-byte login message).
const LOGIN_MESSAGE_HEADER: [u8; 4] = [0x08, 0x01, 0x00, 0x38];

/// Login response header (8 bytes).
const LOGIN_RESPONSE_HEADER: [u8; 8] = [0x09, 0x01, 0x00, 0x0c, 0x01, 0x00, 0x00, 0x00];

/// Try to decode a binary login message or response.
///
/// Returns Some(DecodedMessage) if the data matches login protocol,
/// None if it should be parsed as ASCII command.
fn try_decode_login(data: &[u8]) -> Option<DecodedMessage> {
    // Check for login response (12 bytes: 8-byte header + 2-byte port offset + 2 unknown)
    if data.len() >= 12 && data[0..8] == LOGIN_RESPONSE_HEADER {
        let port_offset = ((data[8] as u16) << 8) | (data[9] as u16);
        let command_port = 10000 + port_offset;
        return Some(DecodedMessage::Furuno {
            message_type: "login_response".to_string(),
            command_id: None,
            fields: serde_json::json!({
                "portOffset": port_offset,
                "commandPort": command_port,
                "rawBytes": format!("{:02x?}", &data[..data.len().min(12)])
            }),
            description: Some(format!("Login response: command port {}", command_port)),
        });
    }

    // Check for login message (56 bytes starting with 08 01 00 38)
    if data.len() >= 12 && data[0..4] == LOGIN_MESSAGE_HEADER {
        return Some(DecodedMessage::Furuno {
            message_type: "login_request".to_string(),
            command_id: None,
            fields: serde_json::json!({
                "length": data.len(),
                "hasCopyright": data.len() >= 56
            }),
            description: Some("Login request with copyright string".to_string()),
        });
    }

    None
}

/// Strip binary header if present.
///
/// Some Furuno models/firmware wrap ASCII commands in an 8-byte binary header:
/// - Bytes 0-3: Unknown (often `00 00 00 08`)
/// - Bytes 4-7: Unknown (often `00 00 00 00`)
/// - Bytes 8+: ASCII command starting with '$'
///
/// This function detects and strips the header by looking for '$' (0x24).
fn strip_binary_header(data: &[u8]) -> &[u8] {
    // If data starts with '$', no header present
    if data.first() == Some(&b'$') {
        return data;
    }

    // Look for '$' within first 16 bytes (header should be 8 bytes)
    if let Some(pos) = data.iter().take(16).position(|&b| b == b'$') {
        return &data[pos..];
    }

    // No '$' found, return original data
    data
}

impl ProtocolDecoder for FurunoDecoder {
    fn decode(&self, data: &[u8], direction: IoDirection) -> DecodedMessage {
        // First, check for binary login protocol messages
        if let Some(login_msg) = try_decode_login(data) {
            return login_msg;
        }

        // Some Furuno firmware versions wrap ASCII commands in an 8-byte binary header.
        // Header format: [4 bytes unknown] [4 bytes unknown] followed by "$..." ASCII.
        // We detect this by looking for '$' (0x24) after the header.
        let data = strip_binary_header(data);

        // Convert to string
        let text = match std::str::from_utf8(data) {
            Ok(s) => s.trim(),
            Err(_) => {
                return DecodedMessage::Unknown {
                    reason: "Invalid UTF-8".to_string(),
                    partial: Some(serde_json::json!({
                        "length": data.len(),
                        "first_bytes": format!("{:02x?}", &data[..data.len().min(8)])
                    })),
                };
            }
        };

        // Empty or too short
        if text.len() < 3 {
            return DecodedMessage::Unknown {
                reason: "Too short".to_string(),
                partial: Some(serde_json::json!({"text": text})),
            };
        }

        // Check for valid Furuno command prefix
        if !text.starts_with('$') {
            return DecodedMessage::Unknown {
                reason: "Missing $ prefix".to_string(),
                partial: Some(serde_json::json!({"text": text})),
            };
        }

        // Determine message type from second character
        let message_type = match text.chars().nth(1) {
            Some('S') => "set",
            Some('R') => "request",
            Some('N') => "response",
            Some('C') => "command",
            Some(c) => {
                return DecodedMessage::Unknown {
                    reason: format!("Unknown command type: {}", c),
                    partial: Some(serde_json::json!({"text": text})),
                };
            }
            None => {
                return DecodedMessage::Unknown {
                    reason: "Missing command type".to_string(),
                    partial: Some(serde_json::json!({"text": text})),
                };
            }
        };

        // Extract command ID (e.g., "S63" from "$S63,...")
        let command_id = text
            .get(1..)
            .and_then(|s| s.split(',').next())
            .map(|s| s.to_string());

        // Parse the parameters
        let parts: Vec<&str> = text.split(',').collect();
        let params = if parts.len() > 1 {
            parts[1..].iter().map(|s| *s).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Try to decode known commands
        let (description, fields) = decode_furuno_command(&command_id, &params, direction);

        DecodedMessage::Furuno {
            message_type: message_type.to_string(),
            command_id,
            fields,
            description,
        }
    }

    fn brand(&self) -> &'static str {
        "furuno"
    }
}

// =============================================================================
// Command Decoding
// =============================================================================

/// Decode a specific Furuno command.
fn decode_furuno_command(
    command_id: &Option<String>,
    params: &[&str],
    _direction: IoDirection,
) -> (Option<String>, serde_json::Value) {
    let id = match command_id {
        Some(id) => id.as_str(),
        None => return (None, serde_json::json!({"params": params})),
    };

    // Command number (without S/R/N prefix)
    let cmd_num = id.get(1..).unwrap_or("");

    match cmd_num {
        // Power/Status
        "01" => {
            let transmitting = params.first().map(|s| *s == "1").unwrap_or(false);
            (
                Some(format!(
                    "{}",
                    if transmitting {
                        "Transmit ON"
                    } else {
                        "Standby"
                    }
                )),
                serde_json::json!({"transmitting": transmitting}),
            )
        }

        // Range
        "02" | "36" => {
            let range_index = params.first().and_then(|s| s.parse::<i32>().ok());
            (
                Some(format!("Range index: {:?}", range_index)),
                serde_json::json!({"rangeIndex": range_index, "params": params}),
            )
        }

        // Gain
        "63" => decode_gain_sea_rain("Gain", params),

        // Sea clutter
        "64" => decode_gain_sea_rain("Sea", params),

        // Rain clutter
        "65" => decode_gain_sea_rain("Rain", params),

        // Noise reduction
        "66" => {
            let enabled = params.first().map(|s| *s == "1").unwrap_or(false);
            (
                Some(format!(
                    "Noise reduction: {}",
                    if enabled { "ON" } else { "OFF" }
                )),
                serde_json::json!({"enabled": enabled}),
            )
        }

        // Interference rejection
        "67" => {
            let enabled = params.first().map(|s| *s == "1").unwrap_or(false);
            (
                Some(format!(
                    "Interference rejection: {}",
                    if enabled { "ON" } else { "OFF" }
                )),
                serde_json::json!({"enabled": enabled}),
            )
        }

        // RezBoost / Beam Sharpening
        "68" => {
            let level = params
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let level_name = match level {
                0 => "OFF",
                1 => "Low",
                2 => "Medium",
                3 => "High",
                _ => "Unknown",
            };
            (
                Some(format!("Beam Sharpening: {}", level_name)),
                serde_json::json!({"level": level, "levelName": level_name}),
            )
        }

        // Bird Mode
        "69" => {
            let level = params
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let level_name = match level {
                0 => "OFF",
                1 => "Low",
                2 => "Medium",
                3 => "High",
                _ => "Unknown",
            };
            (
                Some(format!("Bird Mode: {}", level_name)),
                serde_json::json!({"level": level, "levelName": level_name}),
            )
        }

        // Target Analyzer / Doppler Mode
        "6A" => {
            let enabled = params.first().map(|s| *s == "1").unwrap_or(false);
            let mode = params
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let mode_name = match mode {
                0 => "Target",
                1 => "Rain",
                _ => "Unknown",
            };
            (
                Some(format!(
                    "Target Analyzer: {} ({})",
                    if enabled { "ON" } else { "OFF" },
                    mode_name
                )),
                serde_json::json!({"enabled": enabled, "mode": mode, "modeName": mode_name}),
            )
        }

        // Scan speed
        "6B" => {
            let speed = params
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let speed_name = match speed {
                0 => "24 RPM",
                2 => "Auto",
                _ => "Unknown",
            };
            (
                Some(format!("Scan Speed: {}", speed_name)),
                serde_json::json!({"speed": speed, "speedName": speed_name}),
            )
        }

        // Main bang suppression
        "6C" => {
            let value = params
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            (
                Some(format!("Main Bang Suppression: {}%", value)),
                serde_json::json!({"value": value}),
            )
        }

        // Login response
        "LOGIN" => (
            Some("Login message".to_string()),
            serde_json::json!({"params": params}),
        ),

        // Keep-alive
        "KA" | "KEEPALIVE" => (Some("Keep-alive".to_string()), serde_json::json!({})),

        // Operating time
        "5B" => {
            let seconds = params
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let hours = seconds / 3600;
            (
                Some(format!("Operating time: {} hours", hours)),
                serde_json::json!({"seconds": seconds, "hours": hours}),
            )
        }

        // Unknown command
        _ => (
            None,
            serde_json::json!({
                "commandId": cmd_num,
                "params": params,
                "note": "Unknown command ID"
            }),
        ),
    }
}

/// Decode gain/sea/rain commands which have similar structure.
fn decode_gain_sea_rain(name: &str, params: &[&str]) -> (Option<String>, serde_json::Value) {
    let auto = params.first().map(|s| *s == "1").unwrap_or(false);
    let value = params
        .get(1)
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);

    (
        Some(format!(
            "{}: {} ({})",
            name,
            value,
            if auto { "Auto" } else { "Manual" }
        )),
        serde_json::json!({
            "auto": auto,
            "value": value,
            "allParams": params
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
    fn test_decode_set_gain() {
        let decoder = FurunoDecoder;
        let msg = decoder.decode(b"$S63,0,50,0,80,0\r\n", IoDirection::Send);

        match msg {
            DecodedMessage::Furuno {
                message_type,
                command_id,
                description,
                ..
            } => {
                assert_eq!(message_type, "set");
                assert_eq!(command_id, Some("S63".to_string()));
                assert!(description.unwrap().contains("Gain"));
            }
            _ => panic!("Expected Furuno message"),
        }
    }

    #[test]
    fn test_decode_response_gain() {
        let decoder = FurunoDecoder;
        let msg = decoder.decode(b"$N63,1,75,0,80,0\r\n", IoDirection::Recv);

        match msg {
            DecodedMessage::Furuno {
                message_type,
                command_id,
                description,
                ..
            } => {
                assert_eq!(message_type, "response");
                assert_eq!(command_id, Some("N63".to_string()));
                assert!(description.unwrap().contains("Auto"));
            }
            _ => panic!("Expected Furuno message"),
        }
    }

    #[test]
    fn test_decode_power() {
        let decoder = FurunoDecoder;
        let msg = decoder.decode(b"$N01,1\r\n", IoDirection::Recv);

        match msg {
            DecodedMessage::Furuno {
                description,
                fields,
                ..
            } => {
                assert!(description.unwrap().contains("Transmit"));
                assert_eq!(fields["transmitting"], true);
            }
            _ => panic!("Expected Furuno message"),
        }
    }

    #[test]
    fn test_decode_invalid_utf8() {
        let decoder = FurunoDecoder;
        let msg = decoder.decode(&[0x80, 0x81, 0x82], IoDirection::Recv);

        match msg {
            DecodedMessage::Unknown { reason, .. } => {
                assert!(reason.contains("UTF-8"));
            }
            _ => panic!("Expected Unknown message"),
        }
    }

    #[test]
    fn test_decode_unknown_command() {
        let decoder = FurunoDecoder;
        let msg = decoder.decode(b"$SFF,1,2,3\r\n", IoDirection::Send);

        match msg {
            DecodedMessage::Furuno { command_id, .. } => {
                assert_eq!(command_id, Some("SFF".to_string()));
            }
            _ => panic!("Expected Furuno message"),
        }
    }

    #[test]
    fn test_decode_with_binary_header() {
        let decoder = FurunoDecoder;
        // Real packet from capture: 8-byte binary header + ASCII command
        let data = [
            0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, // 8-byte header
            b'$', b'N', b'6', b'9', b',', b'2', b',', b'0', b',', b'0', b',', b'6', b'0', b',',
            b'3', b'0', b'0', b',', b'0', // $N69,2,0,0,60,300,0
        ];
        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Furuno {
                message_type,
                command_id,
                ..
            } => {
                assert_eq!(message_type, "response");
                assert_eq!(command_id, Some("N69".to_string()));
            }
            _ => panic!("Expected Furuno message, got {:?}", msg),
        }
    }

    #[test]
    fn test_strip_binary_header() {
        // With header
        let with_header = [0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, b'$', b'N'];
        assert_eq!(strip_binary_header(&with_header), &[b'$', b'N']);

        // Without header (starts with $)
        let without_header = [b'$', b'N', b'6', b'9'];
        assert_eq!(strip_binary_header(&without_header), &without_header[..]);

        // No $ found
        let no_dollar = [0x00, 0x01, 0x02, 0x03];
        assert_eq!(strip_binary_header(&no_dollar), &no_dollar[..]);
    }

    #[test]
    fn test_decode_login_response() {
        let decoder = FurunoDecoder;
        // Real login response: command port 10100 (offset = 100 = 0x64)
        let data = [
            0x09, 0x01, 0x00, 0x0c, 0x01, 0x00, 0x00, 0x00, // header
            0x00, 0x64, // port offset = 100
            0x00, 0x00, // unknown
        ];
        let msg = decoder.decode(&data, IoDirection::Recv);

        match msg {
            DecodedMessage::Furuno {
                message_type,
                description,
                fields,
                ..
            } => {
                assert_eq!(message_type, "login_response");
                assert!(description.unwrap().contains("10100"));
                assert_eq!(fields["commandPort"], 10100);
            }
            _ => panic!("Expected Furuno login response, got {:?}", msg),
        }
    }

    #[test]
    fn test_decode_login_request() {
        let decoder = FurunoDecoder;
        // Login message header (first 12 bytes of 56-byte message)
        let data = [
            0x08, 0x01, 0x00, 0x38, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        ];
        let msg = decoder.decode(&data, IoDirection::Send);

        match msg {
            DecodedMessage::Furuno {
                message_type,
                description,
                ..
            } => {
                assert_eq!(message_type, "login_request");
                assert!(description.unwrap().contains("Login request"));
            }
            _ => panic!("Expected Furuno login request, got {:?}", msg),
        }
    }
}
