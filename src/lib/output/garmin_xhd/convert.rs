//! Conversion of mayara spokes into Garmin xHD `0x0998` sweep packets.
//!
//! Three things differ between any given source radar and an xHD, and all
//! three are handled here:
//!
//! * **Angular resolution.** An xHD turns 1440 spokes per revolution. Source
//!   radars deliver anything from 250 (Raymarine Quantum) to 8192 (Furuno), so
//!   spokes are merged or repeated to land on exactly 1440.
//! * **Sample count.** An xHD spoke carries a fixed number of samples whatever
//!   the range; source spokes do not.
//! * **Sample values.** mayara spokes carry legend indices, whose meaning
//!   depends on the radar's legend, while an xHD carries 8-bit echo strength.

use std::collections::VecDeque;

use crate::brand::garmin::protocol::{
    ANGLE_UNITS_PER_SPOKE, MSG_SPOKE, SPOKE_HEADER_SIZE, SPOKES_PER_REVOLUTION,
};
use crate::radar::Legend;

/// The ranges an xHD offers, in meters: 1/8 NM to 48 NM. The plotter picks
/// what it shows from this list, so the emulated radar has to speak it too.
pub(super) const XHD_RANGES_M: &[u32] = &[
    232, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632, 44448, 66672,
    88896,
];

/// Samples per emitted spoke, at every range. Real xHD captures use this
/// count; the field is variable on the wire but there is no reason to differ.
const SAMPLES_PER_SPOKE: usize = 695;

/// `scan_length` header field, constant in every real xHD capture.
const SCAN_LENGTH: u16 = 0x02d3;

/// `fill_1` header field. Documented as padding, but a plotter fed zeroes here
/// locks up, so send what a real radar sends.
const FILL_1: u16 = 1;

/// `fills_4` header field, same story as [`FILL_1`].
const FILLS_4: u16 = 0x0108;

/// Upper bound on how many xHD spokes one source spoke may be repeated into.
/// A radar with few spokes per revolution needs the repeat to paint a solid
/// picture, but a gap wider than this means spokes went missing rather than
/// that the source is coarse, and smearing one spoke across the hole would
/// invent echoes that were never received.
const MAX_REPEAT: u16 = 16;

/// Return the xHD range-table entry closest to `meters`.
pub(super) fn nearest_xhd_range(meters: u32) -> u32 {
    XHD_RANGES_M
        .iter()
        .copied()
        .min_by_key(|&r| r.abs_diff(meters))
        .expect("xHD range table is not empty")
}

/// Maps mayara legend indices to xHD echo strength.
///
/// Legend indices below `pixel_colors` are echo strengths on a scale the radar
/// chose, and scale linearly onto the xHD's 0..=255. Above that sit the
/// Doppler classes and the rendering-only entries (target history, static
/// background) that a spoke straight off a radar never contains.
fn intensity_table(legend: &Legend) -> [u8; 256] {
    let mut table = [0u8; 256];

    let top = legend.pixel_colors.saturating_sub(1);
    if top > 0 {
        for (index, entry) in table
            .iter_mut()
            .enumerate()
            .take(legend.pixel_colors as usize)
        {
            *entry = (index * u8::MAX as usize / top as usize) as u8;
        }
    }

    // An xHD has no Doppler, so a moving target has to be shown as an echo.
    // Approaching and receding targets are solid returns; the rain class that
    // Furuno's NXT reports is a soft one, and painting it at full strength
    // would turn a rain shower into a coastline.
    let bands = [
        (legend.doppler_approaching, u8::MAX),
        (legend.doppler_receding, u8::MAX),
        (
            legend.doppler_rain,
            table[legend.medium_return.min(top) as usize],
        ),
    ];
    for (band, intensity) in bands {
        if let Some((first, count)) = band {
            for index in first..first.saturating_add(count) {
                table[index as usize] = intensity;
            }
        }
    }

    table
}

/// Resample `samples` to [`SAMPLES_PER_SPOKE`], mapping legend indices to
/// echo strength on the way.
///
/// Downsampling takes the strongest sample of each group rather than one
/// arbitrary member of it: a buoy is one or two samples wide, and picking a
/// neighbour instead drops it from the picture.
fn resample(samples: &[u8], intensity: &[u8; 256], out: &mut [u8; SAMPLES_PER_SPOKE]) {
    if samples.is_empty() {
        out.fill(0);
        return;
    }

    for (i, slot) in out.iter_mut().enumerate() {
        let start = i * samples.len() / SAMPLES_PER_SPOKE;
        let end = (((i + 1) * samples.len() / SAMPLES_PER_SPOKE).max(start + 1)).min(samples.len());
        *slot = samples[start..end]
            .iter()
            .map(|&v| intensity[v as usize])
            .max()
            .unwrap_or(0);
    }
}

/// One xHD spoke under construction.
struct Pending {
    /// Spoke index in `0..SPOKES_PER_REVOLUTION`.
    index: u16,
    samples: [u8; SAMPLES_PER_SPOKE],
    /// Range shown by the plotter, an entry of [`XHD_RANGES_M`].
    range_m: u32,
    /// Distance covered by the samples, which is what the plotter scales the
    /// image by. Not the same as `range_m` on every radar: a Furuno keeps
    /// sweeping past the selected range and delivers the overshoot.
    display_m: u32,
}

/// Turns a stream of mayara spokes into a stream of xHD spoke packets.
pub(super) struct SpokeStream {
    spokes_per_revolution: u32,
    intensity: [u8; 256],
    pending: Option<Pending>,
}

impl SpokeStream {
    pub(super) fn new(spokes_per_revolution: u16, legend: &Legend) -> Self {
        Self {
            spokes_per_revolution: spokes_per_revolution.max(1) as u32,
            intensity: intensity_table(legend),
            pending: None,
        }
    }

    /// Adopt a radar's spoke geometry and legend, which both settle only once
    /// the radar has reported its model.
    pub(super) fn reconfigure(&mut self, spokes_per_revolution: u16, legend: &Legend) {
        let spokes_per_revolution = spokes_per_revolution.max(1) as u32;
        if self.spokes_per_revolution != spokes_per_revolution {
            self.spokes_per_revolution = spokes_per_revolution;
            self.pending = None;
        }
        self.intensity = intensity_table(legend);
    }

    /// Feed one source spoke, appending finished xHD packets to `out`.
    ///
    /// A spoke is only complete once the following one shows where its sector
    /// ends, so this emits the *previous* spoke.
    pub(super) fn push(
        &mut self,
        angle: u32,
        samples: &[u8],
        range_m: u32,
        display_m: u32,
        out: &mut VecDeque<Vec<u8>>,
    ) {
        // Wrap before narrowing: an angle beyond a revolution — a radar that
        // reports more than it claims, or one whose spoke count changed under
        // us — would otherwise be truncated into an unrelated index.
        let index = ((angle as u64 * SPOKES_PER_REVOLUTION as u64)
            / self.spokes_per_revolution as u64
            % SPOKES_PER_REVOLUTION as u64) as u16;

        match &mut self.pending {
            // Several source spokes fall in the same xHD spoke: keep the
            // strongest echo of each sample rather than the last one.
            Some(pending) if pending.index == index => {
                let mut merged = [0u8; SAMPLES_PER_SPOKE];
                resample(samples, &self.intensity, &mut merged);
                for (slot, value) in pending.samples.iter_mut().zip(merged) {
                    *slot = (*slot).max(value);
                }
                pending.range_m = range_m;
                pending.display_m = display_m;
            }
            _ => {
                let mut new = Pending {
                    index,
                    samples: [0u8; SAMPLES_PER_SPOKE],
                    range_m,
                    display_m,
                };
                resample(samples, &self.intensity, &mut new.samples);
                if let Some(previous) = self.pending.replace(new) {
                    emit(&previous, index, out);
                }
            }
        }
    }
}

/// Write the finished spoke to `out`, repeated across the sector it covers —
/// every xHD spoke from its own index up to (but not including) `next_index`.
fn emit(pending: &Pending, next_index: u16, out: &mut VecDeque<Vec<u8>>) {
    let spokes = SPOKES_PER_REVOLUTION as u16;
    let span = (next_index + spokes - pending.index) % spokes;
    let repeat = span.clamp(1, MAX_REPEAT);

    for offset in 0..repeat {
        out.push_back(spoke_packet(pending, (pending.index + offset) % spokes));
    }
}

/// Build one `0x0998` spoke packet.
///
/// ```text
///   [0]  u32 packet_type         0x0998
///   [4]  u32 payload_len
///   [8]  u16 fill_1              1
///   [10] u16 scan_length         0x02d3
///   [12] u16 angle               1/8 degree units, 0..11519
///   [14] u16 fill_2              0
///   [16] u32 range_meters        range the plotter displays
///   [20] u32 display_meters      distance the samples cover
///   [24] u8  range_indicator     0 = range A
///   [25] u8  dual_range          0
///   [26] u16 scan_length_bytes_s sample count
///   [28] u16 fills_4             0x0108
///   [30] u32 scan_length_bytes_i sample count again
///   [34] u16 fills_5             0
///   [36] u8  line_data[]
/// ```
fn spoke_packet(pending: &Pending, index: u16) -> Vec<u8> {
    let payload_len = (SPOKE_HEADER_SIZE - 8 + SAMPLES_PER_SPOKE) as u32;
    let mut packet = Vec::with_capacity(SPOKE_HEADER_SIZE + SAMPLES_PER_SPOKE);

    packet.extend_from_slice(&MSG_SPOKE.to_le_bytes());
    packet.extend_from_slice(&payload_len.to_le_bytes());
    packet.extend_from_slice(&FILL_1.to_le_bytes());
    packet.extend_from_slice(&SCAN_LENGTH.to_le_bytes());
    packet.extend_from_slice(&(index * ANGLE_UNITS_PER_SPOKE).to_le_bytes());
    packet.extend_from_slice(&0u16.to_le_bytes()); // fill_2
    packet.extend_from_slice(&pending.range_m.to_le_bytes());
    packet.extend_from_slice(&pending.display_m.to_le_bytes());
    packet.push(0); // range_indicator: range A
    packet.push(0); // dual_range
    packet.extend_from_slice(&(SAMPLES_PER_SPOKE as u16).to_le_bytes());
    packet.extend_from_slice(&FILLS_4.to_le_bytes());
    packet.extend_from_slice(&(SAMPLES_PER_SPOKE as u32).to_le_bytes());
    packet.extend_from_slice(&0u16.to_le_bytes()); // fills_5
    packet.extend_from_slice(&pending.samples);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TargetMode;
    use crate::radar::default_legend;

    fn legend(pixel_values: u8, doppler_levels: u8) -> Legend {
        default_legend(&TargetMode::None, doppler_levels, false, pixel_values)
    }

    /// Feed a whole revolution of source spokes and return the emitted packets.
    fn revolution(spokes_per_revolution: u16, legend: &Legend, samples: &[u8]) -> Vec<Vec<u8>> {
        let mut stream = SpokeStream::new(spokes_per_revolution, legend);
        let mut out = VecDeque::new();
        for angle in 0..spokes_per_revolution as u32 {
            stream.push(angle, samples, 3704, 3704, &mut out);
        }
        // The last spoke stays pending until the next revolution starts.
        stream.push(0, samples, 3704, 3704, &mut out);
        out.into()
    }

    fn angle_of(packet: &[u8]) -> u16 {
        u16::from_le_bytes([packet[12], packet[13]])
    }

    #[test]
    fn nearest_range_snaps_to_the_table() {
        assert_eq!(nearest_xhd_range(3704), 3704);
        assert_eq!(nearest_xhd_range(6000), 5556);
        assert_eq!(nearest_xhd_range(7000), 7408);
        assert_eq!(nearest_xhd_range(0), 232);
        assert_eq!(nearest_xhd_range(u32::MAX), 88896);
    }

    #[test]
    fn a_revolution_yields_one_full_xhd_revolution_whatever_the_source() {
        let legend = legend(16, 0);
        for spokes in [250u16, 720, 1440, 2048, 4096, 8192] {
            let packets = revolution(spokes, &legend, &[1u8; 512]);
            assert_eq!(
                packets.len(),
                SPOKES_PER_REVOLUTION,
                "{spokes} spokes/revolution"
            );
            let angles: Vec<u16> = packets.iter().map(|p| angle_of(p)).collect();
            let expected: Vec<u16> = (0..SPOKES_PER_REVOLUTION as u16)
                .map(|i| i * ANGLE_UNITS_PER_SPOKE)
                .collect();
            assert_eq!(angles, expected, "{spokes} spokes/revolution");
        }
    }

    #[test]
    fn an_angle_beyond_a_revolution_wraps_instead_of_truncating() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(2048, &legend);
        let mut out = VecDeque::new();
        // Far enough past a revolution that the quotient does not fit a u16:
        // 100000 × 1440 / 2048 = 70312, which wraps to spoke 1192. Narrowing
        // before the wrap would truncate to 4776 and land on spoke 456.
        stream.push(100_000, &[1u8; 512], 3704, 3704, &mut out);
        stream.push(100_001, &[1u8; 512], 3704, 3704, &mut out);

        assert_eq!(angle_of(&out[0]), 1192 * ANGLE_UNITS_PER_SPOKE);
    }

    #[test]
    fn angle_stays_within_the_wire_range() {
        let legend = legend(16, 0);
        for packet in revolution(8192, &legend, &[1u8; 512]) {
            assert!(angle_of(&packet) < 11520);
        }
    }

    #[test]
    fn packet_carries_the_constants_a_plotter_insists_on() {
        let legend = legend(16, 0);
        let packet = revolution(1440, &legend, &[0u8; 512]).remove(0);

        assert_eq!(packet.len(), SPOKE_HEADER_SIZE + SAMPLES_PER_SPOKE);
        assert_eq!(
            u32::from_le_bytes(packet[0..4].try_into().unwrap()),
            MSG_SPOKE
        );
        assert_eq!(
            u32::from_le_bytes(packet[4..8].try_into().unwrap()) as usize,
            SPOKE_HEADER_SIZE - 8 + SAMPLES_PER_SPOKE
        );
        assert_eq!(
            u16::from_le_bytes(packet[8..10].try_into().unwrap()),
            FILL_1
        );
        assert_eq!(
            u16::from_le_bytes(packet[28..30].try_into().unwrap()),
            FILLS_4
        );
        assert_eq!(
            u16::from_le_bytes(packet[26..28].try_into().unwrap()) as usize,
            SAMPLES_PER_SPOKE
        );
    }

    #[test]
    fn range_fields_keep_display_range_apart_from_selected_range() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        // A Furuno at 3 NM sweeps well past the range it displays.
        stream.push(0, &[1u8; 512], 5556, 9902, &mut out);
        stream.push(1, &[1u8; 512], 5556, 9902, &mut out);

        let packet = out.pop_front().unwrap();
        assert_eq!(u32::from_le_bytes(packet[16..20].try_into().unwrap()), 5556);
        assert_eq!(u32::from_le_bytes(packet[20..24].try_into().unwrap()), 9902);
    }

    #[test]
    fn legend_indices_scale_onto_the_full_intensity_range() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        let samples: Vec<u8> = (0..16).collect();
        stream.push(0, &samples, 3704, 3704, &mut out);
        stream.push(1, &samples, 3704, 3704, &mut out);

        let packet = out.pop_front().unwrap();
        let data = &packet[SPOKE_HEADER_SIZE..];
        assert_eq!(data[0], 0, "no return stays black");
        assert_eq!(
            data[SAMPLES_PER_SPOKE - 1],
            u8::MAX,
            "strongest legend index becomes the strongest echo"
        );
    }

    #[test]
    fn doppler_targets_become_strong_echoes() {
        let legend = legend(16, 4);
        let (first, count) = legend.doppler_approaching.expect("doppler legend");
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        let samples = vec![first + count - 1; 8];
        stream.push(0, &samples, 3704, 3704, &mut out);
        stream.push(1, &samples, 3704, 3704, &mut out);

        let packet = out.pop_front().unwrap();
        assert_eq!(packet[SPOKE_HEADER_SIZE], u8::MAX);
    }

    #[test]
    fn downsampling_keeps_the_strongest_sample() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        // One strong sample among 2048, far more than fit in a spoke: it has
        // to survive the resampling.
        let mut samples = vec![0u8; 2048];
        samples[1000] = 15;
        stream.push(0, &samples, 3704, 3704, &mut out);
        stream.push(1, &samples, 3704, 3704, &mut out);

        let packet = out.pop_front().unwrap();
        assert!(packet[SPOKE_HEADER_SIZE..].contains(&u8::MAX));
    }

    #[test]
    fn merged_spokes_keep_the_strongest_echo() {
        let legend = legend(16, 0);
        // 2880 source spokes: two land on every xHD spoke.
        let mut stream = SpokeStream::new(2880, &legend);
        let mut out = VecDeque::new();
        stream.push(0, &[15u8; 695], 3704, 3704, &mut out);
        stream.push(1, &[0u8; 695], 3704, 3704, &mut out);
        stream.push(2, &[0u8; 695], 3704, 3704, &mut out);

        let packet = out.pop_front().unwrap();
        assert_eq!(packet[SPOKE_HEADER_SIZE], u8::MAX);
    }

    #[test]
    fn a_gap_wider_than_the_repeat_limit_is_not_smeared() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        stream.push(0, &[1u8; 512], 3704, 3704, &mut out);
        stream.push(1000, &[1u8; 512], 3704, 3704, &mut out);

        assert_eq!(out.len(), MAX_REPEAT as usize);
    }

    #[test]
    fn empty_spoke_data_yields_an_empty_spoke() {
        let legend = legend(16, 0);
        let mut stream = SpokeStream::new(1440, &legend);
        let mut out = VecDeque::new();
        stream.push(0, &[], 3704, 3704, &mut out);
        stream.push(1, &[], 3704, 3704, &mut out);

        let packet = out.pop_front().unwrap();
        assert!(packet[SPOKE_HEADER_SIZE..].iter().all(|&v| v == 0));
    }
}
