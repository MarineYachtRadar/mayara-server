//! Garmin radar capability bitmap.
//!
//! xHD and newer Garmin radars broadcast a 320-bit capability vector once per
//! session in message `0x09B1` (48 bytes total: 8-byte GMN header + 5 × u64
//! capability words). The MFD uses this bitmap to decide which controls to
//! show — only Fantom radars get the MotionScope/Doppler menu, only xHD2+
//! supports dual range, etc.
//!
//! Legacy HD radars do not broadcast capabilities. The MFD hardcodes a fixed
//! bitmap for them; we mirror that with [`GarminCapabilities::for_legacy_hd`].
//!
//! Bit numbering: `(bit / 64)` selects the u64 word, `(bit % 64)` selects
//! the bit within it. The bit→feature mapping is documented in
//! `research/garmin/feature-detection.md`.

#![allow(dead_code)]

/// Number of u64 capability words on the wire.
const CAP_WORDS: usize = 5;

/// Offset (within the `0x09B1` payload) where the first capability word
/// starts. The 8-byte GMN header is *already* stripped before the receiver
/// hands the payload to us, so this offset is into the *body* of the message.
const CAP_BODY_FIRST_WORD_OFFSET: usize = 8;

/// Total length (in bytes) of a `0x09B1` message body, including the 8-byte
/// header offset prefix.
const CAP_BODY_TOTAL_LEN: usize = CAP_BODY_FIRST_WORD_OFFSET + CAP_WORDS * 8;

/// Capability bit identifiers. Numeric values match the per-bit indices
/// used by the Garmin MFD; the multi-byte u64 layout is hidden inside
/// [`GarminCapabilities::has`].
#[allow(non_camel_case_types)]
pub(crate) mod cap {
    // ---- Operational ----
    pub(crate) const RANGE_MODE: u32 = 0x02;
    pub(crate) const RANGE_MODE_TOGGLE: u32 = 0x09;
    pub(crate) const RPM_MODE: u32 = 0x0c;
    pub(crate) const TRANSMIT_MODE: u32 = 0x13;
    pub(crate) const FRONT_OF_BOAT: u32 = 0x1c;
    pub(crate) const PARK_POSITION: u32 = 0x1d;
    pub(crate) const RESTORE_DEFAULTS: u32 = 0x20;
    pub(crate) const NO_TX_ZONE_1_MODE: u32 = 0x21;
    pub(crate) const NO_TX_ZONE_1_START: u32 = 0x22;
    pub(crate) const NO_TX_ZONE_1_STOP: u32 = 0x23;
    pub(crate) const SENTRY_MODE: u32 = 0x24;
    pub(crate) const SENTRY_TRANSMIT_TIME: u32 = 0x28;
    pub(crate) const SENTRY_STANDBY_TIME: u32 = 0x29;
    pub(crate) const DITHER_MODE: u32 = 0x37;
    pub(crate) const NOISE_BLANKER_MODE: u32 = 0x38;

    // ---- Range A ----
    pub(crate) const RANGE_A: u32 = 0x4a;
    pub(crate) const RANGE_A_GAIN_MODE: u32 = 0x4b;
    pub(crate) const RANGE_A_GAIN: u32 = 0x4c;
    pub(crate) const RANGE_A_RAIN_CONTROL: u32 = 0x4e;
    pub(crate) const RANGE_A_RAIN_MODE: u32 = 0x4f;
    pub(crate) const RANGE_A_RAIN_GAIN: u32 = 0x50;
    pub(crate) const RANGE_A_SEA_MODE: u32 = 0x51;
    pub(crate) const RANGE_A_SEA_STATE: u32 = 0x52;
    pub(crate) const RANGE_A_SEA_GAIN: u32 = 0x53;

    // ---- Range B (dual range) ----
    pub(crate) const RANGE_B: u32 = 0x56;
    pub(crate) const RANGE_B_GAIN_MODE: u32 = 0x57;
    pub(crate) const RANGE_B_GAIN: u32 = 0x58;
    pub(crate) const RANGE_B_RAIN_CONTROL: u32 = 0x5a;
    pub(crate) const RANGE_B_RAIN_MODE: u32 = 0x5b;
    pub(crate) const RANGE_B_RAIN_GAIN: u32 = 0x5c;
    pub(crate) const RANGE_B_SEA_MODE: u32 = 0x5d;
    pub(crate) const RANGE_B_SEA_STATE: u32 = 0x5e;
    pub(crate) const RANGE_B_SEA_GAIN: u32 = 0x5f;

    // ---- AFC ----
    pub(crate) const AFC_MODE: u32 = 0x61;
    pub(crate) const AFC_SETTING: u32 = 0x62;
    pub(crate) const AFC_COARSE: u32 = 0x65;

    // ---- Hardware ----
    pub(crate) const ANTENNA_SIZE: u32 = 0x9c;

    // ---- Doppler / MotionScope (Fantom) ----
    pub(crate) const DOPPLER_RANGE_A: u32 = 0xa3;
    pub(crate) const DOPPLER_RANGE_B: u32 = 0xa4;
    pub(crate) const DOPPLER_SENSITIVITY_A: u32 = 0xc3;
    pub(crate) const DOPPLER_SENSITIVITY_B: u32 = 0xc4;

    // ---- Echo trails (xHD2+ / Fantom) ----
    pub(crate) const ECHO_TRAIL_MODE_A: u32 = 0xa6;
    pub(crate) const ECHO_TRAIL_TIME_A: u32 = 0xa7;
    pub(crate) const ECHO_TRAIL_MODE_B: u32 = 0xa8;
    pub(crate) const ECHO_TRAIL_TIME_B: u32 = 0xa9;

    // ---- Pulse expansion (xHD2+) ----
    pub(crate) const PULSE_EXPANSION_A: u32 = 0xab;
    pub(crate) const PULSE_EXPANSION_B: u32 = 0xac;

    // ---- Transmit channel (Fantom Pro / Solid State) ----
    pub(crate) const TRANSMIT_CHANNEL_MODE: u32 = 0xb2;
    pub(crate) const TRANSMIT_CHANNEL_SELECT: u32 = 0xb5;

    // ---- Target size (xHD2 / Fantom) ----
    pub(crate) const TARGET_SIZE_MODE_A: u32 = 0xbd;
    pub(crate) const TARGET_SIZE_MODE_B: u32 = 0xbe;

    // ---- Second no-transmit zone (Fantom Pro) ----
    pub(crate) const NO_TX_ZONE_2_MODE: u32 = 0xbf;
    pub(crate) const NO_TX_ZONE_2_START: u32 = 0xc0;
    pub(crate) const NO_TX_ZONE_2_STOP: u32 = 0xc1;

    // ---- Misc ----
    pub(crate) const CROSSTALK_LEVEL: u32 = 0xc5;
    pub(crate) const SCAN_AVERAGE_MODE_A: u32 = 0xca;
    pub(crate) const SCAN_AVERAGE_MODE_B: u32 = 0xcb;
    pub(crate) const SCAN_AVERAGE_SENSITIVITY_A: u32 = 0xcc;
    pub(crate) const SCAN_AVERAGE_SENSITIVITY_B: u32 = 0xcd;
    pub(crate) const POWER_SAVE_MODE: u32 = 0xd2;
}

/// 320-bit capability vector reported by Garmin xHD and newer radars.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GarminCapabilities {
    bits: [u64; CAP_WORDS],
}

impl GarminCapabilities {
    /// Construct an empty (no-features) capability vector.
    pub(crate) const fn empty() -> Self {
        Self {
            bits: [0; CAP_WORDS],
        }
    }

    /// Synthesize the fixed legacy-HD capability bitmap.
    ///
    /// Synthesize the fixed legacy-HD capability bitmap. HD radars do not
    /// broadcast `0x09B1`, so the MFD hardcodes a known-good set of bits
    /// matching the legacy feature footprint
    /// (single range, manual/auto gain, sea/rain clutter, no-TX zone 1,
    /// sentry mode, RPM mode toggle).
    pub(crate) fn for_legacy_hd() -> Self {
        let mut caps = Self::empty();
        for &bit in LEGACY_HD_BITS {
            caps.set(bit);
        }
        caps
    }

    /// Parse a `0x09B1` payload (the message body **as received from the
    /// network**, including the 8-byte capability-message header that
    /// precedes the five u64 words).
    pub(crate) fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() < CAP_BODY_TOTAL_LEN {
            return None;
        }
        let mut caps = Self::empty();
        for word in 0..CAP_WORDS {
            let start = CAP_BODY_FIRST_WORD_OFFSET + word * 8;
            caps.bits[word] = u64::from_le_bytes(payload[start..start + 8].try_into().ok()?);
        }
        Some(caps)
    }

    /// Look up a single capability bit. Returns `false` for unknown / out
    /// of range bits.
    pub(crate) fn has(&self, bit: u32) -> bool {
        let word = (bit / 64) as usize;
        let shift = bit % 64;
        if word >= CAP_WORDS {
            panic!(
                "Capability bit {} is out of range (max {})",
                bit,
                CAP_WORDS * 64 - 1
            );
        }
        (self.bits[word] >> shift) & 1 != 0
    }

    pub(super) fn set(&mut self, bit: u32) {
        let word = (bit / 64) as usize;
        let shift = bit % 64;
        if word < CAP_WORDS {
            self.bits[word] |= 1u64 << shift;
        }
    }

    // ---- Convenience accessors --------------------------------------------
    //
    // Each accessor returns true if the radar supports the given feature
    // group. These are the gates the settings module needs to decide which
    // controls to register.

    pub(crate) fn has_dual_range(&self) -> bool {
        self.has(cap::RANGE_B)
    }

    pub(crate) fn has_motionscope(&self) -> bool {
        self.has(cap::DOPPLER_RANGE_A) || self.has(cap::DOPPLER_RANGE_B)
    }

    pub(crate) fn has_doppler_sensitivity(&self) -> bool {
        self.has(cap::DOPPLER_SENSITIVITY_A) || self.has(cap::DOPPLER_SENSITIVITY_B)
    }

    pub(crate) fn has_echo_trails(&self) -> bool {
        self.has(cap::ECHO_TRAIL_MODE_A)
    }

    pub(crate) fn has_pulse_expansion(&self) -> bool {
        self.has(cap::PULSE_EXPANSION_A)
    }

    pub(crate) fn has_target_size_mode(&self) -> bool {
        self.has(cap::TARGET_SIZE_MODE_A)
    }

    pub(crate) fn has_no_tx_zone_1(&self) -> bool {
        self.has(cap::NO_TX_ZONE_1_MODE)
    }

    pub(crate) fn has_no_tx_zone_2(&self) -> bool {
        self.has(cap::NO_TX_ZONE_2_MODE)
    }

    pub(crate) fn has_sentry_mode(&self) -> bool {
        self.has(cap::SENTRY_MODE)
    }

    pub(crate) fn has_afc(&self) -> bool {
        self.has(cap::AFC_MODE)
    }

    pub(crate) fn has_open_array_antenna(&self) -> bool {
        self.has(cap::ANTENNA_SIZE)
    }

    pub(crate) fn has_scan_average(&self) -> bool {
        self.has(cap::SCAN_AVERAGE_MODE_A)
    }

    pub(crate) fn has_power_save(&self) -> bool {
        self.has(cap::POWER_SAVE_MODE)
    }

    pub(crate) fn has_transmit_channel_select(&self) -> bool {
        self.has(cap::TRANSMIT_CHANNEL_MODE)
    }

    /// True if the radar reports any Fantom-class feature.
    pub(crate) fn is_fantom(&self) -> bool {
        self.has_motionscope() || self.has_doppler_sensitivity()
    }
}

/// Capability bits for legacy HD radars, matching what the MFD
/// hardcodes for them. See `research/garmin/feature-detection.md`.
const LEGACY_HD_BITS: &[u32] = &[
    cap::RANGE_MODE,
    cap::RANGE_MODE_TOGGLE,
    cap::RPM_MODE,
    cap::TRANSMIT_MODE,
    cap::FRONT_OF_BOAT,
    cap::PARK_POSITION,
    cap::NO_TX_ZONE_1_MODE,
    cap::NO_TX_ZONE_1_START,
    cap::NO_TX_ZONE_1_STOP,
    cap::SENTRY_MODE,
    cap::SENTRY_TRANSMIT_TIME,
    cap::SENTRY_STANDBY_TIME,
    cap::DITHER_MODE,
    cap::NOISE_BLANKER_MODE,
    cap::RANGE_A,
    cap::RANGE_A_GAIN_MODE,
    cap::RANGE_A_GAIN,
    cap::RANGE_A_RAIN_GAIN,
    cap::RANGE_A_SEA_MODE,
    cap::RANGE_A_SEA_STATE,
    cap::RANGE_A_SEA_GAIN,
    cap::AFC_MODE,
    cap::AFC_SETTING,
    cap::AFC_COARSE,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x09B1` payload from the GMR xHD captured in
    /// `radar-recordings/garmin/garmin_xhd.pcap`. The first 8 bytes are
    /// the message header, the remaining 40 bytes are the five u64
    /// capability words.
    ///
    /// Sourced from `research/garmin/discovery-handshake.md:271-274`.
    const SAMPLE_0X09B1_BODY: [u8; 48] = [
        // header
        0x01, 0x00, 0x30, 0x00, 0x9d, 0x00, 0x0a, 0x00, // word 0
        0xdf, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // word 1
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // word 2
        0xfd, 0xff, 0xff, 0x07, 0x00, 0x00, 0x00, 0x00, // word 3
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // word 4
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn parse_xhd_capability_message() {
        let caps = GarminCapabilities::parse(&SAMPLE_0X09B1_BODY).unwrap();

        // Word 0 = 0xfffffffffffffedf — almost everything in the basic
        // 0..63 range is supported.
        assert!(caps.has(cap::TRANSMIT_MODE));
        assert!(caps.has(cap::RPM_MODE));
        assert!(caps.has(cap::FRONT_OF_BOAT));
        assert!(caps.has(cap::SENTRY_MODE));

        // Word 1 = 0xffffffffffffffff — every bit in the 64..127 range
        // is set, which covers range A, range B, AFC, and the open
        // array antenna size bit. The captured GMR xHD reports dual
        // range capability even though that's usually attributed to
        // xHD2+ — the MFD appears to set the entire word
        // unconditionally.
        assert!(caps.has(cap::RANGE_A));
        assert!(caps.has(cap::RANGE_A_GAIN));
        assert!(caps.has(cap::RANGE_A_SEA_GAIN));
        assert!(caps.has_dual_range());
        assert!(caps.has_afc());

        // Word 2 = 0x0000_0007_ffff_fffd — bit 0 set, bits 2..26 set,
        // everything above unset. Bit 0 of word 2 is caps bit 128 (no
        // named feature in our table), bits 2..26 cover caps bits
        // 130..154 — none of which map to echo trails (0xa6=166),
        // pulse expansion (0xab=171), or any of the other Fantom
        // features. So the captured xHD has none of those.
        assert!(!caps.has_echo_trails());
        assert!(!caps.has_pulse_expansion());

        // Word 3+ are zero — no Fantom MotionScope, no second no-TX
        // zone, no Doppler sensitivity.
        assert!(!caps.has_motionscope());
        assert!(!caps.has_doppler_sensitivity());
        assert!(!caps.is_fantom());
        assert!(!caps.has_no_tx_zone_2());
    }

    #[test]
    fn parse_too_short_returns_none() {
        assert!(GarminCapabilities::parse(&[0u8; 47]).is_none());
    }

    #[test]
    fn legacy_hd_capabilities_match_expected() {
        let caps = GarminCapabilities::for_legacy_hd();
        // The legacy bitmap covers single range, gain, clutter, sentry,
        // dither, no-TX zone 1.
        assert!(caps.has(cap::TRANSMIT_MODE));
        assert!(caps.has(cap::RANGE_A));
        assert!(caps.has(cap::RANGE_A_GAIN));
        assert!(caps.has(cap::RANGE_A_SEA_GAIN));
        assert!(caps.has(cap::RANGE_A_RAIN_GAIN));
        assert!(caps.has(cap::SENTRY_MODE));
        assert!(caps.has(cap::DITHER_MODE));
        assert!(caps.has(cap::NO_TX_ZONE_1_MODE));
        // HD has no dual range, no Doppler, no second no-TX zone.
        assert!(!caps.has_dual_range());
        assert!(!caps.has_motionscope());
        assert!(!caps.has_no_tx_zone_2());
    }

    #[test]
    fn no_tx_zone_2_accessor_checks_bit_0xbf() {
        // Bit 0xbf (191) is in word 2 (191/64=2) at bit 63 (191%64=63),
        // i.e. the high bit of the third u64. Set just that bit and
        // verify the convenience accessor finds it without seeing any
        // other zone 2 bits.
        let mut caps = GarminCapabilities::empty();
        caps.set(cap::NO_TX_ZONE_2_MODE);
        assert!(caps.has_no_tx_zone_2());
        assert!(!caps.has_no_tx_zone_1());
        assert!(!caps.has_motionscope());
    }

    #[test]
    fn empty_caps_have_nothing() {
        let caps = GarminCapabilities::empty();
        assert!(!caps.has(cap::TRANSMIT_MODE));
        assert!(!caps.has(cap::RANGE_A));
        assert!(!caps.has_dual_range());
        assert!(!caps.is_fantom());
    }
}
