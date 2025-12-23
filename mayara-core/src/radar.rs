//! Radar data structures
//!
//! These structures represent radar metadata and configuration,
//! independent of any I/O or networking code.

use crate::Brand;
use serde::{Deserialize, Serialize};

/// Basic radar information discovered from beacon response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarDiscovery {
    /// Radar brand
    pub brand: Brand,
    /// Radar model (if known)
    pub model: Option<String>,
    /// Radar name/serial from beacon
    pub name: String,
    /// IP address as string (radar's DHCP address)
    pub address: String,
    /// Port for data streaming (legacy - use data_address if available)
    pub data_port: u16,
    /// Port for commands/reports (legacy - use report_address/send_address if available)
    pub command_port: u16,
    /// Number of spokes per revolution
    pub spokes_per_revolution: u16,
    /// Maximum spoke length in pixels
    pub max_spoke_len: u16,
    /// Pixel depth (e.g., 16, 64, 128)
    pub pixel_values: u8,
    /// Serial number (from model report)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// NIC address that received this beacon (for multi-interface systems)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nic_address: Option<String>,
    /// Suffix for dual-range radars ("A" or "B"), None for single-range
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Full data address including IP (for brands like Navico that use separate multicast addresses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_address: Option<String>,
    /// Full report address including IP (for brands like Navico that use separate multicast addresses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_address: Option<String>,
    /// Full send/command address including IP
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_address: Option<String>,
}

/// Legend entry for mapping pixel values to colors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegendEntry {
    /// Pixel type (Normal, TargetBorder, DopplerApproaching, etc.)
    #[serde(rename = "type")]
    pub pixel_type: String,
    /// RGBA color as hex string (e.g., "#00FF00FF")
    pub color: String,
}

/// Radar control value
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    /// Current value
    pub value: serde_json::Value,
    /// Whether this control is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Whether this control is in auto mode
    #[serde(default)]
    pub auto: bool,
}

/// Radar control definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    /// Control name
    pub name: String,
    /// Control description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Minimum value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Maximum value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Step value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Unit (e.g., "meters", "degrees")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Whether this control supports auto mode
    #[serde(default)]
    pub has_auto: bool,
}

/// Full radar state including controls and legend
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarState {
    /// Radar ID
    pub id: String,
    /// Radar name
    pub name: String,
    /// Brand
    pub brand: Brand,
    /// Model (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Current status
    pub status: RadarStatus,
    /// Number of spokes per revolution
    pub spokes_per_revolution: u16,
    /// Maximum spoke length
    pub max_spoke_len: u16,
    /// Legend for pixel color mapping
    pub legend: Vec<LegendEntry>,
    /// Available controls with current values
    pub controls: std::collections::HashMap<String, ControlValue>,
    /// Optional external stream URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_url: Option<String>,
}

/// Radar operational status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RadarStatus {
    /// Radar is off
    Off,
    /// Radar is warming up
    Warming,
    /// Radar is in standby mode
    Standby,
    /// Radar is transmitting
    Transmit,
    /// Radar status unknown
    Unknown,
}

impl Default for RadarStatus {
    fn default() -> Self {
        RadarStatus::Unknown
    }
}

impl std::fmt::Display for RadarStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RadarStatus::Off => write!(f, "off"),
            RadarStatus::Warming => write!(f, "warming"),
            RadarStatus::Standby => write!(f, "standby"),
            RadarStatus::Transmit => write!(f, "transmit"),
            RadarStatus::Unknown => write!(f, "unknown"),
        }
    }
}

/// Parsed IPv4 address with port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedAddress {
    pub ip: [u8; 4],
    pub port: u16,
}

impl ParsedAddress {
    /// Parse address string "ip:port" or just "ip" (port defaults to 0)
    pub fn parse(addr: &str) -> Result<Self, &'static str> {
        if let Some(colon_pos) = addr.rfind(':') {
            let ip_str = &addr[..colon_pos];
            let port_str = &addr[colon_pos + 1..];
            let ip = Self::parse_ipv4(ip_str)?;
            let port: u16 = port_str.parse().map_err(|_| "Invalid port")?;
            Ok(ParsedAddress { ip, port })
        } else {
            let ip = Self::parse_ipv4(addr)?;
            Ok(ParsedAddress { ip, port: 0 })
        }
    }

    /// Parse IPv4 address string into bytes
    fn parse_ipv4(s: &str) -> Result<[u8; 4], &'static str> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 4 {
            return Err("Invalid IPv4 format");
        }
        let mut ip = [0u8; 4];
        for (i, part) in parts.iter().enumerate() {
            ip[i] = part.parse().map_err(|_| "Invalid IPv4 octet")?;
        }
        Ok(ip)
    }
}
