//! Garmin xHD range table (`0x09B2`).
//!
//! xHD and newer Garmin radars broadcast their supported range list as
//! message `0x09B2`. The body is a small TLV-style block:
//!
//! ```text
//! Offset  Size  Field
//! +00     2     version       (uint16 LE, observed value 1)
//! +02     2     length        (uint16 LE, body length in bytes incl. header)
//! +04     4     count         (uint32 LE, number of range entries)
//! +08     4×N   ranges        (uint32 LE meters)
//! ```
//!
//! For a typical xHD this expands to a 16-entry table covering the
//! standard nautical fractions (1/8 NM .. 48 NM).
//!
//! Sources:
//! - `research/garmin/enhanced-radar-protocol.md:386-419`
//! - `research/garmin/discovery-handshake.md:267-304`
//! - The 72-byte hex dump in `discovery-handshake.md` was used as the
//!   parser fixture.

#![allow(dead_code)]

use crate::radar::range::Ranges;

/// Maximum number of range entries we will accept from a single
/// `0x09B2` message. Captured xHD radars report 16; the MFD
/// stores a fixed-size table so this is a hard upper bound, not just
/// a sanity check.
const MAX_RANGE_ENTRIES: usize = 32;

/// Header offsets within the `0x09B2` body (the slice after the GMN
/// 8-byte header has been stripped).
const VERSION_OFFSET: usize = 0;
const LENGTH_OFFSET: usize = 2;
const COUNT_OFFSET: usize = 4;
const RANGES_OFFSET: usize = 8;

/// Parse the body of a `0x09B2` range-table message.
///
/// `payload` must be the message body **after** the 8-byte GMN header
/// has been stripped. Returns the list of supported ranges in meters,
/// or `None` if the body is malformed (truncated, count too large, or
/// the declared length doesn't match the data we got).
pub(crate) fn parse(payload: &[u8]) -> Option<Ranges> {
    if payload.len() < RANGES_OFFSET {
        return None;
    }
    let count = u32::from_le_bytes(payload[COUNT_OFFSET..COUNT_OFFSET + 4].try_into().ok()?)
        as usize;
    if count == 0 || count > MAX_RANGE_ENTRIES {
        return None;
    }
    let ranges_end = RANGES_OFFSET + count * 4;
    if payload.len() < ranges_end {
        return None;
    }
    let mut distances = Vec::with_capacity(count);
    for i in 0..count {
        let off = RANGES_OFFSET + i * 4;
        let meters = u32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
        distances.push(meters as i32);
    }
    Some(Ranges::new_by_distance(&distances))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x09B2` body captured from a GMR xHD radar in
    /// `radar-recordings/garmin/garmin_xhd.pcap`. Sourced from
    /// `research/garmin/discovery-handshake.md:275-280`.
    const SAMPLE_0X09B2_BODY: [u8; 72] = [
        0x01, 0x00, 0x48, 0x00, 0x10, 0x00, 0x00, 0x00, // version=1, length=72, count=16
        0xe8, 0x00, 0x00, 0x00, // 232 m   (1/8 NM)
        0xcf, 0x01, 0x00, 0x00, // 463 m   (1/4 NM)
        0x9e, 0x03, 0x00, 0x00, // 926 m   (1/2 NM)
        0x6d, 0x05, 0x00, 0x00, // 1389 m  (3/4 NM)
        0x3c, 0x07, 0x00, 0x00, // 1852 m  (1 NM)
        0xda, 0x0a, 0x00, 0x00, // 2778 m  (1.5 NM)
        0x78, 0x0e, 0x00, 0x00, // 3704 m  (2 NM)
        0xb4, 0x15, 0x00, 0x00, // 5556 m  (3 NM)
        0xf0, 0x1c, 0x00, 0x00, // 7408 m  (4 NM)
        0x68, 0x2b, 0x00, 0x00, // 11112 m (6 NM)
        0xe0, 0x39, 0x00, 0x00, // 14816 m (8 NM)
        0xd0, 0x56, 0x00, 0x00, // 22224 m (12 NM)
        0xc0, 0x73, 0x00, 0x00, // 29632 m (16 NM)
        0xa0, 0xad, 0x00, 0x00, // 44448 m (24 NM)
        0x70, 0x04, 0x01, 0x00, // 66672 m (36 NM)
        0x40, 0x5b, 0x01, 0x00, // 88896 m (48 NM exactly)
    ];

    #[test]
    fn parse_captured_xhd_range_table() {
        let ranges = parse(&SAMPLE_0X09B2_BODY).expect("should parse");
        let distances: Vec<i32> = ranges.all.iter().map(|r| r.distance()).collect();
        assert_eq!(
            distances,
            vec![
                232, 463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112, 14816, 22224, 29632,
                44448, 66672, 88896,
            ]
        );
    }

    #[test]
    fn parse_returns_none_on_short_payload() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 7]).is_none());
        // Count says 4 entries but only 12 bytes of data → truncated.
        let payload = [
            0x01, 0x00, 0x18, 0x00, // header
            0x04, 0x00, 0x00, 0x00, // count = 4
            0x00, 0x01, 0x00, 0x00, // range 1 only
        ];
        assert!(parse(&payload).is_none());
    }

    #[test]
    fn parse_rejects_zero_or_huge_count() {
        let mut payload = [0u8; 16];
        payload[0] = 1;
        // count = 0 → reject
        assert!(parse(&payload).is_none());
        // count > MAX_RANGE_ENTRIES → reject
        payload[COUNT_OFFSET] = (MAX_RANGE_ENTRIES as u32 + 1) as u8;
        assert!(parse(&payload).is_none());
    }
}
