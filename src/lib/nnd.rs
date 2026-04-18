//! NND file parser for Furuno TZtouch demo recordings.
//!
//! Parses `.nnd` and `.nnd.gz` files (Furuno NavNet Demo format) into
//! the same `PcapPacket` representation used by pcap replay. Each NND
//! record is a timestamped binary payload tagged with a LAN port number.
//!
//! Packets are routed by content inspection:
//! - IMO echo frames (byte\[0\]=0x02) → multicast echo address
//! - Beacon reports (header match) → beacon address
//! - Tile echo frames (byte\[0\]=0x01) → multicast echo address
//! - NMEA sentences → synthetic NMEA replay address

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

use crate::pcap::PcapPacket;

/// Multicast address for Furuno spoke echo data (239.255.0.2:10024).
const SPOKE_DATA_MULTICAST_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), 10024);

/// Expected header bytes in a Furuno beacon report (bytes 0–10).
const BEACON_REPORT_HEADER: [u8; 11] =
    [0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0];

/// Minimum beacon report size (56 bytes = `size_of::<FurunoRadarReport>()`).
const BEACON_REPORT_LENGTH_MIN: usize = 56;

/// Beacon address as `SocketAddrV4` (the protocol constant is `SocketAddr`).
const BEACON_ADDR: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(172, 31, 255, 255), 10010);

/// Synthetic source address for NND replay packets.
const NND_SRC_ADDR: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(172, 31, 0, 1), 10010);

/// Synthetic destination address for NMEA replay packets.
pub(crate) const NMEA_REPLAY_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 1), 1);

/// IMO echo frame magic byte.
const IMO_MAGIC: u8 = 0x02;

/// Tile echo / report frame magic byte.
const REPORT_MAGIC: u8 = 0x01;

/// Check whether `data` looks like an NND file (starts with `Time:`).
pub(crate) fn is_nnd(data: &[u8]) -> bool {
    data.starts_with(b"Time:")
}

/// Parse NND data from a byte slice into replay packets.
///
/// NND demo recordings don't contain beacon/discovery packets (the radar
/// was already discovered before the recording started). We synthesize
/// minimal beacon and model report packets at timestamp 0 so the Furuno
/// locator can detect the radar. The model string is extracted from the
/// filename (e.g., "DRS25A-NXT" from "Seattle_TZT3_DRS25A-NXT_...nnd.gz").
pub(crate) fn parse_bytes(data: &[u8], path: &Path) -> io::Result<Vec<PcapPacket>> {
    let model = model_from_filename(path);
    let mut packets = synthesize_beacon_packets(&model);
    let mut pos = 0;
    let mut current_ts = Duration::ZERO;

    while pos < data.len() {
        // Skip whitespace / newlines between records
        if data[pos] == b'\n' || data[pos] == b'\r' {
            pos += 1;
            continue;
        }

        // FSD separator — skip the line and the <FSDNN3FILE> line after it
        if data[pos..].starts_with(b"FSD") {
            pos = skip_line(data, pos);
            // Skip the <FSDNN3FILE> line if present
            if pos < data.len() && data[pos] == b'<' {
                pos = skip_line(data, pos);
            }
            continue;
        }

        // Partial FSDNN3FILE line (after split payload boundary).
        // The payload includes `FSD\n<FSDN` and the remaining
        // `N3FILE>\n` or `3FILE>\n` sits between records.
        if matches!(data[pos], b'N' | b'3' | b'<') {
            pos = skip_line(data, pos);
            continue;
        }

        // Time: header — extract millisecond offset
        if data[pos..].starts_with(b"Time:") {
            current_ts = parse_time_line(data, pos);
            pos = skip_line(data, pos);
            continue;
        }

        // Packet record: <length><whitespace>LAN<port>:<payload>
        match parse_record(data, pos) {
            Some((next_pos, payload, _lan_port)) => {
                if let Some(pkt) = classify_payload(payload, current_ts) {
                    packets.push(pkt);
                }
                pos = next_pos;
            }
            None => {
                // Unrecognized line — skip it
                pos = skip_line(data, pos);
            }
        }
    }

    Ok(packets)
}

/// Parse a `Time: <ms> <date> <time>` line and return the duration.
fn parse_time_line(data: &[u8], pos: usize) -> Duration {
    // Format: "Time: <ms_offset> <date> <time>"
    let line_end = find_newline(data, pos);
    let line = &data[pos..line_end];

    // Extract the millisecond offset after "Time: "
    if line.len() > 6 {
        let after_prefix = &line[6..]; // skip "Time: "
        if let Some(space) = after_prefix.iter().position(|&b| b == b' ') {
            if let Ok(ms) = std::str::from_utf8(&after_prefix[..space])
                .unwrap_or("")
                .parse::<u64>()
            {
                return Duration::from_millis(ms);
            }
        }
    }
    Duration::ZERO
}

/// Parse a packet record: `<length><whitespace>LAN<port>:<payload>`.
/// Returns `(next_pos, payload_slice, lan_port)` or `None`.
///
/// The stated length is a record length that includes the header
/// (`<length><ws>LAN<port>:`) itself, plus 2. The actual payload
/// starts after the colon and has `stated_length - header_len + 2`
/// bytes. After the payload, `FSD\n<FSDNN3FILE>\n` follows as a
/// record separator.
fn parse_record(data: &[u8], pos: usize) -> Option<(usize, &[u8], u8)> {
    // Read the ASCII decimal length
    let mut i = pos;
    while i < data.len() && data[i].is_ascii_digit() {
        i += 1;
    }
    if i == pos {
        return None; // no digits
    }
    let stated_length: usize = std::str::from_utf8(&data[pos..i]).ok()?.parse().ok()?;

    // Skip whitespace to "LAN"
    while i < data.len() && (data[i] == b' ' || data[i] == b'\t') {
        i += 1;
    }

    // Expect "LAN<digit>:" or "LAN<digit><digit>:"
    if !data[i..].starts_with(b"LAN") {
        return None;
    }
    i += 3; // skip "LAN"

    let port_start = i;
    while i < data.len() && data[i].is_ascii_digit() {
        i += 1;
    }
    if i == port_start || i >= data.len() || data[i] != b':' {
        return None;
    }
    let lan_port: u8 = std::str::from_utf8(&data[port_start..i]).ok()?.parse().ok()?;
    i += 1; // skip ':'

    // The stated length includes the header plus 2 bytes of framing.
    let header_len = i - pos;
    let payload_len = (stated_length + 2).saturating_sub(header_len);
    let payload_end = i + payload_len;
    if payload_end > data.len() {
        return None; // truncated
    }
    let payload = &data[i..payload_end];

    // Skip the trailing FSD\n<FSDNN3FILE>\n separator if present.
    let mut next_pos = payload_end;
    if next_pos + 4 <= data.len() && &data[next_pos..next_pos + 4] == b"FSD\n" {
        next_pos += 4;
        if next_pos + 13 <= data.len() && &data[next_pos..next_pos + 13] == b"<FSDNN3FILE>\n" {
            next_pos += 13;
        }
    }

    Some((next_pos, payload, lan_port))
}

/// Classify a payload by content and produce a `PcapPacket` with the
/// appropriate synthetic addresses, or `None` to skip.
fn classify_payload(payload: &[u8], timestamp: Duration) -> Option<PcapPacket> {
    if payload.is_empty() {
        return None;
    }

    match payload[0] {
        IMO_MAGIC => {
            // IMO echo spoke frame
            Some(PcapPacket {
                timestamp,
                src_addr: NND_SRC_ADDR,
                dst_addr: SPOKE_DATA_MULTICAST_ADDRESS,
                payload: payload.to_vec(),
            })
        }
        REPORT_MAGIC => {
            // Could be a beacon report or Tile echo frame
            if is_beacon_report(payload) {
                Some(PcapPacket {
                    timestamp,
                    src_addr: NND_SRC_ADDR,
                    dst_addr: BEACON_ADDR,
                    payload: payload.to_vec(),
                })
            } else {
                // Tile echo frame — route to same echo address
                Some(PcapPacket {
                    timestamp,
                    src_addr: NND_SRC_ADDR,
                    dst_addr: SPOKE_DATA_MULTICAST_ADDRESS,
                    payload: payload.to_vec(),
                })
            }
        }
        _ => {
            // Check for NMEA sentences (8-byte header + ASCII starting with $ or !)
            if payload.len() > 9 && (payload[8] == b'$' || payload[8] == b'!') {
                let nmea_data = &payload[8..];
                // Only accept if the content is printable ASCII (reject
                // binary $ARPA packets that start with '$' but contain
                // non-ASCII data).
                if !nmea_data.is_empty()
                    && nmea_data.iter().all(|&b| b.is_ascii_graphic() || b == b' ' || b == b'\r' || b == b'\n')
                {
                    return Some(PcapPacket {
                        timestamp,
                        src_addr: NND_SRC_ADDR,
                        dst_addr: NMEA_REPLAY_ADDRESS,
                        payload: nmea_data.to_vec(),
                    });
                }
            }
            None
        }
    }
}

/// Check whether a payload with byte\[0\]=0x01 is a Furuno beacon report.
fn is_beacon_report(payload: &[u8]) -> bool {
    payload.len() >= BEACON_REPORT_LENGTH_MIN
        && payload.len() >= BEACON_REPORT_HEADER.len()
        && payload[..BEACON_REPORT_HEADER.len()] == BEACON_REPORT_HEADER
        && payload.len() >= 17
        && payload[16] == b'R'
}

/// Extract a Furuno model string from the NND filename.
///
/// Looks for substrings starting with "DRS" or "FAR" (the prefixes the
/// Furuno locator requires). Hyphens are stripped since the locator
/// matches e.g. "DRS25ANXT" not "DRS25A-NXT". Falls back to "DRS4DNXT"
/// if no model is found.
fn model_from_filename(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        // Strip .nnd from .nnd.gz (file_stem gives "foo.nnd")
        .map(|s| s.strip_suffix(".nnd").unwrap_or(s))
        .unwrap_or("");

    for part in stem.split('_') {
        let upper = part.to_ascii_uppercase();
        if upper.starts_with("DRS") || upper.starts_with("FAR") {
            return upper.replace('-', "");
        }
    }
    "DRS4DNXT".to_string()
}

/// Synthesize beacon and model report packets for radar discovery.
///
/// NND demo files lack beacon packets. We craft a 32-byte beacon report
/// and a 170-byte model report so the Furuno locator recognizes the radar
/// and creates a RadarInfo.
fn synthesize_beacon_packets(model: &str) -> Vec<PcapPacket> {
    // 32-byte beacon report: header[0..11] + length[11] + pad[12..16] + name[16..24]
    let mut beacon = [0u8; 32];
    beacon[..BEACON_REPORT_HEADER.len()].copy_from_slice(&BEACON_REPORT_HEADER);
    beacon[11] = 24; // length = total - 8 header bytes
    beacon[16..24].copy_from_slice(b"RD003212"); // name (8 bytes, starts with 'R')

    // 170-byte model report: pad[0..24] + model[24..56] + firmware[56..88] +
    //                        firmware2[88..120] + serial[120..152] + pad[152..170]
    let mut model_report = [0u8; 170];
    model_report[..BEACON_REPORT_HEADER.len()].copy_from_slice(&BEACON_REPORT_HEADER);
    model_report[11] = 162; // length = 170 - 8
    let model_bytes = model.as_bytes();
    let copy_len = model_bytes.len().min(32); // model field is 32 bytes
    model_report[24..24 + copy_len].copy_from_slice(&model_bytes[..copy_len]);

    vec![
        PcapPacket {
            timestamp: Duration::ZERO,
            src_addr: NND_SRC_ADDR,
            dst_addr: BEACON_ADDR,
            payload: beacon.to_vec(),
        },
        PcapPacket {
            timestamp: Duration::ZERO,
            src_addr: NND_SRC_ADDR,
            dst_addr: BEACON_ADDR,
            payload: model_report.to_vec(),
        },
    ]
}

fn find_newline(data: &[u8], pos: usize) -> usize {
    data[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |i| pos + i)
}

fn skip_line(data: &[u8], pos: usize) -> usize {
    let nl = find_newline(data, pos);
    if nl < data.len() { nl + 1 } else { nl }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_nnd_detection() {
        assert!(is_nnd(b"Time: 0 2019/8/29 12:36:15\n"));
        assert!(!is_nnd(b"\xa1\xb2\xc3\xd4")); // pcap magic
        assert!(!is_nnd(b""));
    }

    #[test]
    fn parse_time_line_extracts_ms() {
        let line = b"Time: 42 2019/8/29 12:36:15\n";
        let ts = parse_time_line(line, 0);
        assert_eq!(ts, Duration::from_millis(42));
    }

    #[test]
    fn parse_record_extracts_payload() {
        // stated_length=16, header="16    LAN3:"=11 bytes
        // payload = 16 - 11 + 2 = 7 bytes
        let data = b"16    LAN3:\x01\x02\x03\x04\x05\x06\x07FSD\n<FSDNN3FILE>\nnext";
        let (next, payload, port) = parse_record(data, 0).unwrap();
        assert_eq!(port, 3);
        assert_eq!(payload, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        assert_eq!(&data[next..], b"next");
    }

    #[test]
    fn classify_imo_echo() {
        let payload = &[0x02, 0x00, 0x00, 0x00]; // IMO magic
        let pkt = classify_payload(payload, Duration::ZERO).unwrap();
        assert_eq!(pkt.dst_addr, SPOKE_DATA_MULTICAST_ADDRESS);
    }

    #[test]
    fn classify_beacon_report() {
        let mut payload = vec![0u8; 60];
        payload[..BEACON_REPORT_HEADER.len()].copy_from_slice(&BEACON_REPORT_HEADER);
        payload[16] = b'R';
        let pkt = classify_payload(&payload, Duration::ZERO).unwrap();
        assert_eq!(pkt.dst_addr, BEACON_ADDR);
    }

    #[test]
    fn classify_nmea() {
        let mut payload = vec![0u8; 8]; // 8-byte header
        payload.extend_from_slice(b"$IIHDT,335.2,T*25\r\n");
        let pkt = classify_payload(&payload, Duration::ZERO).unwrap();
        assert_eq!(pkt.dst_addr, NMEA_REPLAY_ADDRESS);
        assert_eq!(&pkt.payload, b"$IIHDT,335.2,T*25\r\n");
    }

    #[test]
    fn classify_rejects_binary_arpa() {
        let mut payload = vec![0u8; 8]; // 8-byte header
        payload.extend_from_slice(b"$ARPA,0,0,0032,0000,0032,\x00\x12\x04");
        assert!(classify_payload(&payload, Duration::ZERO).is_none());
    }

    #[test]
    fn model_from_nnd_filename() {
        assert_eq!(
            model_from_filename(Path::new("Seattle_TZT3_DRS25A-NXT_TargetAnalyzer_ON.nnd.gz")),
            "DRS25ANXT"
        );
        assert_eq!(
            model_from_filename(Path::new("capture_FAR-2127_test.nnd")),
            "FAR2127"
        );
        // Fallback when no model found
        assert_eq!(
            model_from_filename(Path::new("unknown_recording.nnd.gz")),
            "DRS4DNXT"
        );
    }
}
