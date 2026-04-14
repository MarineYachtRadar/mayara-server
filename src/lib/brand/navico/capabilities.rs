//! Navico HALO capability parsing from 0xC409 StateDataBlock.
//!
//! HALO radars broadcast a TLV-encoded capability advertisement that
//! tells us exactly what the hardware supports: available modes, level
//! counts for each control, instrumented range limits, and antenna type.
//!
//! Only HALO models send 0xC409 — BR24, 3G, and 4G do not.
//! Verified across all available recordings:
//! - BR24: no 0xC409 in any capture (br24-filtered, br24-full, br24_davy)
//! - 4G: no 0xC409 in any capture (4g-boot, 4g-heading, 4g-ranges-km)
//! - HALO20+, HALO24, HALO3006: 0xC409 present in all captures
//!   that include state reports

use super::protocol::tlv;

/// Capabilities parsed from the 0xC409 TLV data block.
#[derive(Debug, Clone)]
pub struct NavicoCapabilities {
    /// Bitmask of supported operating modes (from TLV type 2).
    pub supported_modes_mask: u32,
    /// Bitmask of supported interference rejection levels (TLV type 3).
    pub interference_reject_mask: u8,
    /// Bitmask of supported noise rejection levels (TLV type 4).
    pub noise_reject_mask: u8,
    /// Bitmask of supported target boost (expansion) levels (TLV type 5).
    pub target_boost_mask: u8,
    /// Bitmask of supported beam sharpening (target separation) levels (TLV type 7).
    pub beam_sharpening_mask: u8,
    /// Bitmask of supported fast scan (scan speed) levels (TLV type 8).
    pub scan_speed_mask: u8,
    /// Sidelobe gain minimum (TLV type 9).
    pub sidelobe_gain_min: u8,
    /// Sidelobe gain maximum (TLV type 9).
    pub sidelobe_gain_max: u8,
    /// Minimum instrumented range in decimeters (TLV type 11).
    pub instrumented_range_min_dm: u32,
    /// Maximum instrumented range in decimeters (TLV type 11).
    pub instrumented_range_max_dm: u32,
    /// Bitmask of supported local interference rejection levels (TLV type 12).
    pub local_interference_mask: u8,
    /// True if the radar is a dome (vs open array) (TLV type 10).
    pub is_dome: bool,
}

impl NavicoCapabilities {
    /// Parse a 0xC409 StateDataBlock payload (after the 2-byte opcode header).
    pub fn parse(data: &[u8]) -> Self {
        let mut caps = NavicoCapabilities {
            supported_modes_mask: 0,
            interference_reject_mask: 0,
            noise_reject_mask: 0,
            target_boost_mask: 0,
            beam_sharpening_mask: 0,
            scan_speed_mask: 0,
            sidelobe_gain_min: 0,
            sidelobe_gain_max: 255,
            instrumented_range_min_dm: 0,
            instrumented_range_max_dm: 0,
            local_interference_mask: 0,
            is_dome: true,
        };

        let mut offset = 0;
        while offset + 3 <= data.len() {
            let tlv_type = data[offset];
            // data[offset + 1] is reserved (always 0)
            let tlv_len = data[offset + 2] as usize;
            offset += 3;

            if offset + tlv_len > data.len() {
                log::warn!("TLV type {} truncated at offset {}", tlv_type, offset);
                break;
            }

            let payload = &data[offset..offset + tlv_len];
            offset += tlv_len;

            match tlv_type {
                tlv::SUPPORTED_USE_MODES if payload.len() >= 4 => {
                    caps.supported_modes_mask =
                        u32::from_le_bytes(payload[..4].try_into().unwrap());
                }
                tlv::INTERFERENCE_REJECT if !payload.is_empty() => {
                    caps.interference_reject_mask = payload[0];
                }
                tlv::NOISE_REJECT if !payload.is_empty() => {
                    caps.noise_reject_mask = payload[0];
                }
                tlv::TARGET_BOOST if !payload.is_empty() => {
                    caps.target_boost_mask = payload[0];
                }
                tlv::BEAM_SHARPENING if !payload.is_empty() => {
                    caps.beam_sharpening_mask = payload[0];
                }
                tlv::FAST_SCAN if !payload.is_empty() => {
                    caps.scan_speed_mask = payload[0];
                }
                tlv::SIDELOBE_GAIN_RANGE if payload.len() >= 4 => {
                    caps.sidelobe_gain_min = payload[0];
                    caps.sidelobe_gain_max = payload[2];
                }
                tlv::SUPPORTED_ANTENNAS => {
                    // Dome sends a single 0x00 byte; open array sends count + sizes
                    caps.is_dome = payload.len() == 1 && payload[0] == 0x00;
                }
                tlv::INSTRUMENTED_RANGE if payload.len() >= 8 => {
                    caps.instrumented_range_min_dm =
                        u32::from_le_bytes(payload[..4].try_into().unwrap());
                    caps.instrumented_range_max_dm =
                        u32::from_le_bytes(payload[4..8].try_into().unwrap());
                }
                tlv::LOCAL_INTERFERENCE_REJECT if !payload.is_empty() => {
                    caps.local_interference_mask = payload[0];
                }
                tlv::STC_CURVE => {
                    // Parsed but not currently used for control gating
                }
                _ => {
                    log::trace!(
                        "TLV type {} len {} skipped: {:02X?}",
                        tlv_type,
                        tlv_len,
                        payload
                    );
                }
            }
        }

        let mut modes = Vec::new();
        if caps.supported_modes_mask & tlv::MODE_CUSTOM != 0 {
            modes.push("Custom");
        }
        if caps.supported_modes_mask & tlv::MODE_HARBOR != 0 {
            modes.push("Harbor");
        }
        if caps.supported_modes_mask & tlv::MODE_OFFSHORE != 0 {
            modes.push("Offshore");
        }
        if caps.supported_modes_mask & tlv::MODE_WEATHER != 0 {
            modes.push("Weather");
        }
        if caps.supported_modes_mask & tlv::MODE_BIRD != 0 {
            modes.push("Bird");
        }
        if caps.supported_modes_mask & tlv::MODE_DOPPLER != 0 {
            modes.push("Doppler");
        }
        if caps.supported_modes_mask & tlv::MODE_BUOY != 0 {
            modes.push("Buoy");
        }
        log::debug!(
            "Capabilities: modes=[{}] interference_reject=0x{:02x} noise_reject=0x{:02x} \
             target_boost=0x{:02x} beam_sharpening=0x{:02x} scan_speed=0x{:02x} range={}-{}m \
             antenna={} doppler={}",
            modes.join(", "),
            caps.interference_reject_mask,
            caps.noise_reject_mask,
            caps.target_boost_mask,
            caps.beam_sharpening_mask,
            caps.scan_speed_mask,
            caps.range_min_m(),
            caps.range_max_m(),
            if caps.is_dome { "dome" } else { "open array" },
            caps.has_doppler(),
        );

        caps
    }

    /// Whether Doppler mode is supported.
    pub fn has_doppler(&self) -> bool {
        self.supported_modes_mask & tlv::MODE_DOPPLER != 0
    }

    /// Whether bird mode is supported.
    #[allow(dead_code)]
    pub fn has_bird_mode(&self) -> bool {
        self.supported_modes_mask & tlv::MODE_BIRD != 0
    }

    /// Instrumented range minimum in meters.
    pub fn range_min_m(&self) -> i32 {
        (self.instrumented_range_min_dm / 10) as i32
    }

    /// Instrumented range maximum in meters.
    pub fn range_max_m(&self) -> i32 {
        (self.instrumented_range_max_dm / 10) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty() {
        let caps = NavicoCapabilities::parse(&[]);
        assert_eq!(caps.supported_modes_mask, 0);
        assert_eq!(caps.instrumented_range_min_dm, 0);
        assert!(caps.is_dome);
    }

    #[test]
    fn parse_supported_modes() {
        // Type 2, reserved 0, length 4, payload 0x37 0x00 0x00 0x00
        let data = [0x02, 0x00, 0x04, 0x37, 0x00, 0x00, 0x00];
        let caps = NavicoCapabilities::parse(&data);
        assert_eq!(caps.supported_modes_mask, 0x37);
        assert!(caps.has_doppler()); // bit 5
        assert!(caps.has_bird_mode()); // bit 4
    }

    #[test]
    fn parse_instrumented_range() {
        // Type 11, reserved 0, length 8
        // min=500dm (50m), max=888960dm (88896m ≈ 48 NM)
        let mut data = vec![0x0B, 0x00, 0x08];
        data.extend_from_slice(&500u32.to_le_bytes());
        data.extend_from_slice(&888960u32.to_le_bytes());
        let caps = NavicoCapabilities::parse(&data);
        assert_eq!(caps.instrumented_range_min_dm, 500);
        assert_eq!(caps.instrumented_range_max_dm, 888960);
        assert_eq!(caps.range_min_m(), 50);
        assert_eq!(caps.range_max_m(), 88896);
    }

    #[test]
    fn parse_level_features() {
        // Byte0 is a bitmask of supported values.
        // IR: 0x0F=0b1111 → values 0,1,2,3
        // NR: 0x07=0b0111 → values 0,1,2
        // FS: 0x0F=0b1111 → values 0,1,2,3
        let data = [
            0x03, 0x00, 0x05, 0x0F, 0x00, 0x00, 0x83, 0x80, // IR: mask 0x0F
            0x04, 0x00, 0x05, 0x07, 0x00, 0x00, 0x03, 0x00, // NR: mask 0x07
            0x08, 0x00, 0x05, 0x0F, 0x00, 0x00, 0x82, 0x80, // FS: mask 0x0F
        ];
        let caps = NavicoCapabilities::parse(&data);
        assert_eq!(caps.interference_reject_mask, 0x0F);
        assert_eq!(caps.noise_reject_mask, 0x07);
        assert_eq!(caps.scan_speed_mask, 0x0F);
    }

    #[test]
    fn parse_dome_antenna() {
        let data = [0x0A, 0x00, 0x01, 0x00]; // Type 10, len 1, payload 0x00
        let caps = NavicoCapabilities::parse(&data);
        assert!(caps.is_dome);
    }

    #[test]
    fn parse_open_array_antenna() {
        // Type 10, len 5: count=2, sizes=[1800mm, 2400mm]
        let data = [0x0A, 0x00, 0x05, 0x02, 0x08, 0x07, 0x60, 0x09];
        let caps = NavicoCapabilities::parse(&data);
        assert!(!caps.is_dome);
    }

    #[test]
    fn unknown_types_skipped() {
        // Type 99 (unknown), len 3, some payload, followed by type 2
        let data = [
            0x63, 0x00, 0x03, 0xAA, 0xBB, 0xCC, // unknown type 99
            0x02, 0x00, 0x04, 0x37, 0x00, 0x00, 0x00, // modes
        ];
        let caps = NavicoCapabilities::parse(&data);
        assert_eq!(caps.supported_modes_mask, 0x37);
    }
}
