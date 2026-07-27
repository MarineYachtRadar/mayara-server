//! Furuno → Garmin xHD spoke conversion.
//!
//! All logic here is a direct port of the tested Python implementation in
//! `garmin-radar-bridge/bridge.py`. See that file and the project README for
//! the derivation of every constant.

/// xHD range table (meters). The plotter only accepts values from this list.
pub(super) const XHD_RANGES_M: &[u32] = &[
    232, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448, 66672,
    88896,
];

/// Number of output samples per spoke at all ranges.
pub(super) const SAMPLES_PER_SPOKE: usize = 695;

/// xHD angle units per full revolution (1440 spokes × 8 units/spoke).
const XHD_ANGLE_MAX: u64 = 11520;

/// scan_length field in every spoke header (constant in all real xHD captures).
const SCAN_LENGTH: u16 = 0x02d3;

/// Furuno internal range ≈ this factor × the xHD range set via mayara.
/// Derived empirically: set=5556 → spoke=9902; set=7408 → spoke=13202.
pub(super) const FURUNO_RANGE_RATIO: f64 = 1.7822;

/// Return the xHD range-table entry closest to `meters`.
pub(super) fn nearest_xhd_range(meters: u32) -> u32 {
    XHD_RANGES_M
        .iter()
        .copied()
        .min_by_key(|&r| r.abs_diff(meters))
        .unwrap_or(XHD_RANGES_M[0])
}

/// Build one xHD `0x0998` spoke UDP payload.
///
/// * `src_angle`          — spoke angle in source units (0..src_spokes_per_rev)
/// * `src_spokes_per_rev` — full revolution in source spoke units
/// * `src_data`           — raw 8-bit intensity samples from the source radar
/// * `display_range`      — actual range in meters (used for chart scaling)
///
/// `range_meters` in the spoke header is snapped to the nearest xHD table
/// entry; `display_meters` carries `display_range` so the plotter scales the
/// image correctly when the source radar's range doesn't align exactly.
pub(super) fn to_xhd_spoke(
    src_angle: u32,
    src_spokes_per_rev: u32,
    src_data: &[u8],
    display_range: u32,
) -> Vec<u8> {
    // Angle: map source units → xHD 1/8° units, then quantize to step=8.
    // Values above 11512 crash the plotter — the % keeps us in [0,11519],
    // and the /8*8 keeps us in [0,11512] (since 11512 is already a multiple).
    let raw_angle =
        ((src_angle as u64 * XHD_ANGLE_MAX) / src_spokes_per_rev as u64) % XHD_ANGLE_MAX;
    let xhd_angle = ((raw_angle / 8) * 8) as u16;
    debug_assert!(
        xhd_angle <= 11512,
        "angle {xhd_angle} > 11512 would crash plotter"
    );

    // Sample resampling: nearest-neighbour from src_data → SAMPLES_PER_SPOKE.
    let n_src = src_data.len();
    let mut samples = vec![0u8; SAMPLES_PER_SPOKE];
    if n_src > 0 {
        for (i, slot) in samples.iter_mut().enumerate() {
            let j = (i * n_src / SAMPLES_PER_SPOKE).min(n_src - 1);
            *slot = src_data[j];
        }
    }

    let range_meters = nearest_xhd_range(display_range);
    let pay_len = (28 + SAMPLES_PER_SPOKE) as u32; // bytes after the GMN 8-byte header

    // struct radar_line (from GarminxHDReceive.cpp, pragma pack 1):
    //   [0]  u32 packet_type       = 0x0998
    //   [4]  u32 len1              = pay_len
    //   [8]  u16 fill_1            = 1  (must not be 0 — crashes plotter)
    //   [10] u16 scan_length       = 0x02d3
    //   [12] u16 angle             = xhd_angle
    //   [14] u16 fill_2            = 0
    //   [16] u32 range_meters      = nearest xHD table value
    //   [20] u32 display_meters    = display_range (actual range for chart scale)
    //   [24] u8  a_b_range         = 0
    //   [25] u8  dual_range        = 0
    //   [26] u16 scan_length_bytes_s = SAMPLES_PER_SPOKE
    //   [28] u16 fills_4           = 0x0108  (must not be 0 — crashes plotter)
    //   [30] u32 scan_length_bytes_i = SAMPLES_PER_SPOKE
    //   [34] u16 fills_5           = 0
    //   [36+] line_data
    let mut pkt = Vec::with_capacity(36 + SAMPLES_PER_SPOKE);
    pkt.extend_from_slice(&0x0998_u32.to_le_bytes());
    pkt.extend_from_slice(&pay_len.to_le_bytes());
    pkt.extend_from_slice(&1_u16.to_le_bytes()); // fill_1 = 1
    pkt.extend_from_slice(&SCAN_LENGTH.to_le_bytes());
    pkt.extend_from_slice(&xhd_angle.to_le_bytes());
    pkt.extend_from_slice(&0_u16.to_le_bytes()); // fill_2
    pkt.extend_from_slice(&range_meters.to_le_bytes());
    pkt.extend_from_slice(&display_range.to_le_bytes());
    pkt.push(0); // a_b_range
    pkt.push(0); // dual_range
    pkt.extend_from_slice(&(SAMPLES_PER_SPOKE as u16).to_le_bytes());
    pkt.extend_from_slice(&0x0108_u16.to_le_bytes()); // fills_4: encode=0x08, spare=0x01 LE
    pkt.extend_from_slice(&(SAMPLES_PER_SPOKE as u32).to_le_bytes());
    pkt.extend_from_slice(&0_u16.to_le_bytes()); // fills_5
    pkt.extend_from_slice(&samples);
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_xhd_range_exact() {
        assert_eq!(nearest_xhd_range(3704), 3704);
        assert_eq!(nearest_xhd_range(7408), 7408);
    }

    #[test]
    fn nearest_xhd_range_rounds_to_closest() {
        // Midway between 5556 and 7408 → picks 5556 (closer by 1)
        // Actually 5556+926=6482 is halfway; let's use 6000 → closer to 5556
        assert_eq!(nearest_xhd_range(6000), 5556);
        assert_eq!(nearest_xhd_range(7000), 7408);
    }

    #[test]
    fn spoke_angle_maps_furuno_8192_spokes() {
        // Furuno: 8192 spokes/rev. Angle 0 → xhd 0, angle 1024 → xhd 1440 (= 11520/8).
        // For angle 1024: raw = 1024*11520/8192 = 1440; quantized = 1440.
        let pkt = to_xhd_spoke(1024, 8192, &[], 3704);
        let angle = u16::from_le_bytes([pkt[12], pkt[13]]);
        assert_eq!(angle, 1440);
    }

    #[test]
    fn spoke_angle_never_exceeds_11512() {
        // Angle just before wrap: 8191/8192 * 11520 = 11518.59 → quantize → 11512
        let pkt = to_xhd_spoke(8191, 8192, &[], 3704);
        let angle = u16::from_le_bytes([pkt[12], pkt[13]]);
        assert!(angle <= 11512, "angle {angle} exceeds max");
    }

    #[test]
    fn spoke_angle_wraps_at_max() {
        // Angle 8192 (= full revolution) → xhd 0 (wraps)
        let pkt = to_xhd_spoke(8192, 8192, &[], 3704);
        let angle = u16::from_le_bytes([pkt[12], pkt[13]]);
        assert_eq!(angle, 0);
    }

    #[test]
    fn spoke_range_fields() {
        let display = 9902_u32; // Furuno internal range for xhd=5556
        let pkt = to_xhd_spoke(0, 8192, &[], display);
        let range_m = u32::from_le_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]);
        let display_m = u32::from_le_bytes([pkt[20], pkt[21], pkt[22], pkt[23]]);
        assert_eq!(range_m, 11112); // nearest_xhd_range(9902)
        assert_eq!(display_m, 9902);
    }

    #[test]
    fn spoke_critical_constants() {
        let pkt = to_xhd_spoke(0, 8192, &[0u8; 1024], 3704);
        assert_eq!(pkt.len(), 36 + SAMPLES_PER_SPOKE);
        // fill_1 at byte 8 must be 1
        let fill_1 = u16::from_le_bytes([pkt[8], pkt[9]]);
        assert_eq!(fill_1, 1, "fill_1 must be 1");
        // fills_4 at byte 28 must be 0x0108
        let fills_4 = u16::from_le_bytes([pkt[28], pkt[29]]);
        assert_eq!(fills_4, 0x0108, "fills_4 must be 0x0108");
    }

    #[test]
    fn sample_resampling_1024_to_695() {
        let src: Vec<u8> = (0..1024_u16).map(|i| (i % 256) as u8).collect();
        let pkt = to_xhd_spoke(0, 8192, &src, 3704);
        let samples = &pkt[36..];
        assert_eq!(samples.len(), SAMPLES_PER_SPOKE);
        // First sample should be the first source byte
        assert_eq!(samples[0], src[0]);
        // Last sample should be a byte from near the end of src
        let j_last = ((SAMPLES_PER_SPOKE - 1) * 1024 / SAMPLES_PER_SPOKE).min(1023);
        assert_eq!(samples[SAMPLES_PER_SPOKE - 1], src[j_last]);
    }

    #[test]
    fn empty_src_data_gives_zero_samples() {
        let pkt = to_xhd_spoke(0, 8192, &[], 3704);
        assert!(pkt[36..].iter().all(|&b| b == 0));
    }
}
