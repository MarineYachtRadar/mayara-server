//! Pcap file parser for replay testing.
//!
//! Parses standard pcap files (`.pcap` and `.pcap.gz`, not `.pcapng`)
//! and extracts UDP packets with their source/destination addresses
//! and payloads.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

/// A single UDP packet extracted from a pcap file.
#[derive(Debug, Clone)]
pub struct PcapPacket {
    /// Time offset from the first packet in the capture.
    pub timestamp: Duration,
    /// Source IP and port.
    pub src_addr: SocketAddrV4,
    /// Destination IP and port.
    pub dst_addr: SocketAddrV4,
    /// UDP payload (after Ethernet + IP + UDP headers are stripped).
    pub payload: Vec<u8>,
}

/// Pcap global header magic numbers.
const PCAP_MAGIC_LE: u32 = 0xa1b2c3d4;
const PCAP_MAGIC_BE: u32 = 0xd4c3b2a1;
const PCAP_MAGIC_NS_LE: u32 = 0xa1b23c4d; // nanosecond resolution
const PCAP_MAGIC_NS_BE: u32 = 0x4d3cb2a1;

/// Pcap link type for Ethernet.
const LINKTYPE_ETHERNET: u32 = 1;
/// Ethernet header length (no VLAN tags).
const ETH_HEADER_LEN: usize = 14;
/// Minimum IPv4 header length (no options).
const IP_HEADER_MIN_LEN: usize = 20;
/// UDP header length.
const UDP_HEADER_LEN: usize = 8;
/// IPv4 EtherType.
const ETHERTYPE_IPV4: u16 = 0x0800;
/// UDP IP protocol number.
const IP_PROTO_UDP: u8 = 17;

/// Read a file, decompressing it when it is gzipped.
fn read_maybe_gzip(path: &Path) -> io::Result<Vec<u8>> {
    if path.extension().is_some_and(|e| e == "gz") {
        let file = fs::File::open(path)?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        fs::read(path)
    }
}

/// Parse a pcap or NND file (optionally gzipped) and return all UDP packets.
///
/// Auto-detects the file format: NND files start with `Time:`, pcap files
/// start with a 4-byte magic number. Both `.gz` and uncompressed files are
/// supported.
pub fn parse_file(path: &Path) -> io::Result<Vec<PcapPacket>> {
    let data = read_maybe_gzip(path)?;

    if crate::nnd::is_nnd(&data) {
        return crate::nnd::parse_bytes(&data, path);
    }

    parse_bytes(&data)
}

/// Parse pcap data from a byte slice.
pub(crate) fn parse_bytes(data: &[u8]) -> io::Result<Vec<PcapPacket>> {
    if data.len() < 24 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pcap too short"));
    }

    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let (swap, nanoseconds) = match magic {
        PCAP_MAGIC_LE => (false, false),
        PCAP_MAGIC_BE => (true, false),
        PCAP_MAGIC_NS_LE => (false, true),
        PCAP_MAGIC_NS_BE => (true, true),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad pcap magic: 0x{:08x}", magic),
            ));
        }
    };

    let read_u32 = |off: usize| -> u32 {
        let v = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        if swap { v.swap_bytes() } else { v }
    };

    // Global header: magic(4) + version(4) + thiszone(4) + sigfigs(4) + snaplen(4) + linktype(4)
    let link_type = read_u32(20);
    if link_type != LINKTYPE_ETHERNET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported pcap link type {} (only Ethernet/1 is supported)",
                link_type
            ),
        ));
    }

    let mut packets = Vec::new();
    let mut fragments: HashMap<FragmentKey, FragmentSet> = HashMap::new();
    let mut offset = 24;
    let mut first_ts: Option<(u32, u32)> = None;

    while offset + 16 <= data.len() {
        let ts_sec = read_u32(offset);
        let ts_frac = read_u32(offset + 4);
        let incl_len = read_u32(offset + 8) as usize;
        let _orig_len = read_u32(offset + 12);
        offset += 16;

        if offset + incl_len > data.len() {
            break; // truncated
        }

        let pkt_data = &data[offset..offset + incl_len];
        offset += incl_len;

        let Some(part) = parse_ip_part(pkt_data) else {
            continue;
        };

        // A datagram that arrived whole is the common case. Fragments are held
        // until the last one lands: Navico spoke datagrams are ~17 kB and so
        // always fragmented, and dropping them makes the entire spoke stream
        // invisible to replay.
        let reassembled;
        let datagram: &[u8] = if part.offset == 0 && !part.more {
            part.payload
        } else {
            let key = (part.src, part.dst, part.id);
            let set = fragments.entry(key).or_default();
            set.add(part.offset, part.payload, !part.more);
            match set.take_complete() {
                Some(datagram) => {
                    fragments.remove(&key);
                    reassembled = datagram;
                    &reassembled
                }
                // Still missing pieces; it is emitted when the gap is filled.
                None => continue,
            }
        };

        // Parse UDP (timestamp filled in below)
        if let Some(mut pkt) = parse_udp_datagram(part.src, part.dst, datagram, Duration::ZERO) {
            // Anchor timing to the first datagram, not the first pcap record
            let first = first_ts.get_or_insert((ts_sec, ts_frac));
            let divisor: u64 = if nanoseconds {
                1_000_000_000
            } else {
                1_000_000
            };
            let abs_first = first.0 as u64 * divisor + first.1 as u64;
            let abs_now = ts_sec as u64 * divisor + ts_frac as u64;
            pkt.timestamp =
                Duration::from_nanos(abs_now.saturating_sub(abs_first) * (1_000_000_000 / divisor));
            packets.push(pkt);
        }
    }

    Ok(packets)
}

/// One IPv4 datagram, or one fragment of one, as it came off the wire.
struct IpPart<'a> {
    src: Ipv4Addr,
    dst: Ipv4Addr,
    /// Identification field, which is what ties fragments of one datagram
    /// together.
    id: u16,
    /// Where this fragment's bytes sit in the datagram.
    offset: usize,
    /// The "more fragments" flag: false on the last fragment, and on a
    /// datagram that was never fragmented.
    more: bool,
    /// The IP payload — the UDP datagram, or a slice of it.
    payload: &'a [u8],
}

/// Fragments of one datagram, identified by source, destination and IP id.
type FragmentKey = (Ipv4Addr, Ipv4Addr, u16);

/// The pieces of one datagram, held until every byte of it has arrived.
#[derive(Default)]
struct FragmentSet {
    /// Fragments kept sorted by offset, so completeness is one walk.
    pieces: Vec<(usize, Vec<u8>)>,
    /// The datagram's length, known once the last fragment arrives.
    total: Option<usize>,
}

impl FragmentSet {
    fn add(&mut self, offset: usize, payload: &[u8], last: bool) {
        if last {
            self.total = Some(offset + payload.len());
        }
        let at = self.pieces.partition_point(|(o, _)| *o < offset);
        self.pieces.insert(at, (offset, payload.to_vec()));
    }

    /// The reassembled datagram, once the fragments cover it end to end.
    ///
    /// A capture can hold the same fragment twice, or fragments that overlap,
    /// so each one contributes only the part that extends what is already
    /// covered rather than being appended blindly.
    fn take_complete(&self) -> Option<Vec<u8>> {
        let total = self.total?;
        let mut datagram = Vec::with_capacity(total);
        for (offset, bytes) in &self.pieces {
            if *offset > datagram.len() {
                return None; // a hole: fragments are still missing
            }
            let already_have = datagram.len() - offset;
            if let Some(rest) = bytes.get(already_have..) {
                datagram.extend_from_slice(rest);
            }
        }
        (datagram.len() == total).then_some(datagram)
    }
}

/// Pull the IPv4 payload out of an Ethernet frame, keeping the fragmentation
/// fields so the caller can reassemble what arrived in pieces.
fn parse_ip_part(data: &[u8]) -> Option<IpPart<'_>> {
    if data.len() < ETH_HEADER_LEN + IP_HEADER_MIN_LEN {
        return None;
    }

    // Ethernet header
    let ethertype = u16::from_be_bytes(data[12..14].try_into().ok()?);
    if ethertype != ETHERTYPE_IPV4 {
        return None; // not IPv4
    }

    let ip = &data[ETH_HEADER_LEN..];
    if ip.len() < IP_HEADER_MIN_LEN {
        return None;
    }

    // IPv4 header
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ihl < IP_HEADER_MIN_LEN || ip.len() < ihl {
        return None;
    }
    let protocol = ip[9];
    if protocol != IP_PROTO_UDP {
        return None;
    }
    // The frame may be padded to Ethernet's 60-byte minimum, so the datagram
    // ends where the IP header says it does, not where the frame does.
    let total_len = u16::from_be_bytes(ip[2..4].try_into().ok()?) as usize;
    if total_len < ihl || total_len > ip.len() {
        return None;
    }

    let id = u16::from_be_bytes(ip[4..6].try_into().ok()?);
    let frag = u16::from_be_bytes(ip[6..8].try_into().ok()?);

    Some(IpPart {
        src: Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]),
        dst: Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]),
        id,
        // The fragment offset counts 8-byte units.
        offset: (frag & 0x1FFF) as usize * 8,
        more: frag & 0x2000 != 0,
        payload: &ip[ihl..total_len],
    })
}

/// Parse a complete UDP datagram — header and payload — into a packet.
fn parse_udp_datagram(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    udp: &[u8],
    timestamp: Duration,
) -> Option<PcapPacket> {
    if udp.len() < UDP_HEADER_LEN {
        return None;
    }
    let src_port = u16::from_be_bytes(udp[0..2].try_into().ok()?);
    let dst_port = u16::from_be_bytes(udp[2..4].try_into().ok()?);
    let udp_len = u16::from_be_bytes(udp[4..6].try_into().ok()?) as usize;

    if udp_len < UDP_HEADER_LEN || udp_len > udp.len() {
        return None;
    }
    let payload = udp[UDP_HEADER_LEN..udp_len].to_vec();

    Some(PcapPacket {
        timestamp,
        src_addr: SocketAddrV4::new(src_ip, src_port),
        dst_addr: SocketAddrV4::new(dst_ip, dst_port),
        payload,
    })
}

/// Write packets back to a pcap file. Creates a valid pcap with
/// Ethernet + IPv4 + UDP headers wrapping each payload.
#[cfg(any(test, feature = "pcap-replay"))]
pub fn write_file(path: &Path, packets: &[PcapPacket]) -> io::Result<()> {
    write_bytes(path, &encode_packets(packets))
}

/// Write `packets` to `path` unless it already holds exactly these packets,
/// reporting whether it wrote.
///
/// The comparison is against what the file decompresses to, not against its
/// bytes. The same packets compress to different bytes under different gzip
/// implementations, and the fixtures in `testdata/` were written by several of
/// them over the years, so writing unconditionally rewrites files whose content
/// has not moved — leaving a diff of unreadable binary churn for a reviewer to
/// wade through.
#[cfg(any(test, feature = "pcap-replay"))]
pub fn write_file_if_changed(path: &Path, packets: &[PcapPacket]) -> io::Result<bool> {
    let data = encode_packets(packets);
    if read_maybe_gzip(path).is_ok_and(|existing| existing == data) {
        return Ok(false);
    }
    write_bytes(path, &data)?;
    Ok(true)
}

/// Write `data` to `path`, gzipping it when the path says so.
#[cfg(any(test, feature = "pcap-replay"))]
fn write_bytes(path: &Path, data: &[u8]) -> io::Result<()> {
    if path.extension().is_some_and(|e| e == "gz") {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let file = fs::File::create(path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(data)?;
        encoder.finish()?;
        Ok(())
    } else {
        fs::write(path, data)
    }
}

/// Encode packets as pcap bytes, before any compression.
#[cfg(any(test, feature = "pcap-replay"))]
fn encode_packets(packets: &[PcapPacket]) -> Vec<u8> {
    let mut data = Vec::new();

    // Global header (24 bytes): magic, version 2.4, timezone 0, sigfigs 0, snaplen 65535, linktype 1 (Ethernet)
    data.extend_from_slice(&PCAP_MAGIC_LE.to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes()); // version major
    data.extend_from_slice(&4u16.to_le_bytes()); // version minor
    data.extend_from_slice(&0i32.to_le_bytes()); // thiszone
    data.extend_from_slice(&0u32.to_le_bytes()); // sigfigs
    data.extend_from_slice(&65535u32.to_le_bytes()); // snaplen
    data.extend_from_slice(&1u32.to_le_bytes()); // linktype (Ethernet)

    for pkt in packets {
        debug_assert!(
            pkt.payload.len() <= u16::MAX as usize - IP_HEADER_MIN_LEN - UDP_HEADER_LEN,
            "payload too large for UDP/IPv4: {} bytes",
            pkt.payload.len()
        );
        let udp_len = (UDP_HEADER_LEN + pkt.payload.len()) as u16;
        let ip_total_len = (IP_HEADER_MIN_LEN + UDP_HEADER_LEN + pkt.payload.len()) as u16;
        let frame_len = ETH_HEADER_LEN + IP_HEADER_MIN_LEN + UDP_HEADER_LEN + pkt.payload.len();

        // Timestamp
        let ts_sec = pkt.timestamp.as_secs() as u32;
        let ts_usec = pkt.timestamp.subsec_micros();

        // Record header (16 bytes)
        data.extend_from_slice(&ts_sec.to_le_bytes());
        data.extend_from_slice(&ts_usec.to_le_bytes());
        data.extend_from_slice(&(frame_len as u32).to_le_bytes()); // incl_len
        data.extend_from_slice(&(frame_len as u32).to_le_bytes()); // orig_len

        // Ethernet header (14 bytes): dst MAC, src MAC, EtherType
        data.extend_from_slice(&[0x00; 6]); // dst MAC
        data.extend_from_slice(&[0x00; 6]); // src MAC
        data.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

        // IPv4 header (20 bytes)
        data.push(0x45); // version + IHL
        data.push(0x00); // DSCP/ECN
        data.extend_from_slice(&ip_total_len.to_be_bytes());
        data.extend_from_slice(&[0x00; 2]); // identification
        data.extend_from_slice(&[0x00; 2]); // flags + fragment offset
        data.push(64); // TTL
        data.push(IP_PROTO_UDP);
        data.extend_from_slice(&[0x00; 2]); // checksum (0 = skip)
        data.extend_from_slice(&pkt.src_addr.ip().octets());
        data.extend_from_slice(&pkt.dst_addr.ip().octets());

        // UDP header (8 bytes)
        data.extend_from_slice(&pkt.src_addr.port().to_be_bytes());
        data.extend_from_slice(&pkt.dst_addr.port().to_be_bytes());
        data.extend_from_slice(&udp_len.to_be_bytes());
        data.extend_from_slice(&[0x00; 2]); // checksum (0 = skip)

        // Payload
        data.extend_from_slice(&pkt.payload);
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. Cleared on entry, so a run that panics
    /// half way cannot leave a file behind that decides the next run's result,
    /// and two runs at once cannot collide.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mayara-pcap-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn sample_packets() -> Vec<PcapPacket> {
        vec![
            PcapPacket {
                timestamp: Duration::from_millis(0),
                src_addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 1234),
                dst_addr: SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 1), 5678),
                payload: vec![0x01, 0x02, 0x03],
            },
            PcapPacket {
                timestamp: Duration::from_millis(100),
                src_addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 4321),
                dst_addr: SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 2), 8765),
                payload: vec![0xAA, 0xBB],
            },
        ]
    }

    /// The bug this guards: a fixture holding exactly these packets, but
    /// compressed by a different gzip than the one we write with, must be left
    /// alone. Comparing file bytes would rewrite it and fill the diff with
    /// binary churn that says nothing.
    #[test]
    fn a_fixture_compressed_differently_is_left_alone() {
        let packets = sample_packets();
        let dir = scratch_dir("recompressed");
        let fixture = dir.join("fixture.pcap.gz");

        // The same content, written by a gzip that stamps its header
        // differently from ours — standing in for whichever gzip wrote the
        // committed fixtures.
        {
            use flate2::Compression;
            use flate2::GzBuilder;
            use std::io::Write;
            let file = fs::File::create(&fixture).expect("create");
            let mut encoder = GzBuilder::new()
                .mtime(0x5EA5_0000)
                .write(file, Compression::best());
            encoder.write_all(&encode_packets(&packets)).expect("write");
            encoder.finish().expect("finish");
        }
        let before = fs::read(&fixture).expect("read");

        // Both files are gzip, so this compares one gzip encoding against the
        // other. Without it the test would pass even if the two agreed byte for
        // byte, which is the one case in which it proves nothing.
        let ours = dir.join("ours.pcap.gz");
        write_file(&ours, &packets).expect("write");
        assert_ne!(
            before,
            fs::read(&ours).expect("read"),
            "the two gzips must differ, or this test proves nothing"
        );

        assert!(
            !write_file_if_changed(&fixture, &packets).expect("compare"),
            "identical packets must not be rewritten"
        );
        assert_eq!(
            fs::read(&fixture).expect("read"),
            before,
            "file was touched"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The other half: content that really did move is written.
    #[test]
    fn a_fixture_whose_packets_changed_is_rewritten() {
        let mut packets = sample_packets();
        let dir = scratch_dir("moved");
        let fixture = dir.join("fixture.pcap.gz");
        assert!(
            write_file_if_changed(&fixture, &packets).expect("write"),
            "a fixture that does not exist yet is written"
        );
        assert!(
            !write_file_if_changed(&fixture, &packets).expect("compare"),
            "writing it again changes nothing"
        );

        packets[1].payload = vec![0xAA, 0xBB, 0xCC];
        assert!(
            write_file_if_changed(&fixture, &packets).expect("write"),
            "a changed payload is written"
        );
        let parsed = parse_file(&fixture).expect("parse");
        assert_eq!(parsed[1].payload, vec![0xAA, 0xBB, 0xCC]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A UDP datagram: header plus payload, as it sits inside IP.
    fn udp_datagram(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&src_port.to_be_bytes());
        d.extend_from_slice(&dst_port.to_be_bytes());
        d.extend_from_slice(&((UDP_HEADER_LEN + payload.len()) as u16).to_be_bytes());
        d.extend_from_slice(&[0x00; 2]); // checksum
        d.extend_from_slice(payload);
        d
    }

    /// One Ethernet frame carrying an IPv4 fragment. `offset` is in bytes and
    /// `more` is the MF flag, so a whole datagram is `(0, false)`.
    fn frame(id: u16, offset: usize, more: bool, part: &[u8]) -> Vec<u8> {
        let mut f = vec![0x00; 12];
        f.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        f.push(0x45);
        f.push(0x00);
        f.extend_from_slice(&((IP_HEADER_MIN_LEN + part.len()) as u16).to_be_bytes());
        f.extend_from_slice(&id.to_be_bytes());
        let flags = (if more { 0x2000u16 } else { 0 }) | (offset / 8) as u16;
        f.extend_from_slice(&flags.to_be_bytes());
        f.push(64);
        f.push(IP_PROTO_UDP);
        f.extend_from_slice(&[0x00; 2]); // checksum
        f.extend_from_slice(&[10, 0, 0, 1]);
        f.extend_from_slice(&[239, 0, 0, 1]);
        f.extend_from_slice(part);
        f
    }

    /// A pcap file holding the given frames, one record each.
    fn pcap_of(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&PCAP_MAGIC_LE.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&0i32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&65535u32.to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        for (i, f) in frames.iter().enumerate() {
            d.extend_from_slice(&(i as u32).to_le_bytes()); // ts_sec
            d.extend_from_slice(&0u32.to_le_bytes()); // ts_usec
            d.extend_from_slice(&(f.len() as u32).to_le_bytes());
            d.extend_from_slice(&(f.len() as u32).to_le_bytes());
            d.extend_from_slice(f);
        }
        d
    }

    /// A Navico spoke datagram is ~17 kB and so always arrives in fragments.
    /// Dropping them leaves replay with beacons and reports only, which is why
    /// no test could see a decoded echo.
    #[test]
    fn a_fragmented_datagram_is_reassembled() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let datagram = udp_datagram(6680, 6678, &payload);
        let (first, rest) = datagram.split_at(1480);

        let packets = parse_bytes(&pcap_of(&[
            frame(0x1234, 0, true, first),
            frame(0x1234, 1480, false, rest),
        ]))
        .expect("parse");

        assert_eq!(packets.len(), 1, "the fragments are one datagram");
        assert_eq!(packets[0].dst_addr.port(), 6678);
        assert_eq!(packets[0].payload, payload);
    }

    /// Fragments are not guaranteed to be captured in order.
    #[test]
    fn fragments_arriving_out_of_order_are_reassembled() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let datagram = udp_datagram(6680, 6678, &payload);
        let (first, rest) = datagram.split_at(1480);

        let packets = parse_bytes(&pcap_of(&[
            frame(0x1234, 1480, false, rest),
            frame(0x1234, 0, true, first),
        ]))
        .expect("parse");

        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].payload, payload);
    }

    /// Two streams talking at once interleave their fragments; the IP id keeps
    /// them apart.
    #[test]
    fn interleaved_datagrams_are_kept_apart() {
        let a: Vec<u8> = vec![0xAA; 2000];
        let b: Vec<u8> = vec![0xBB; 2000];
        let da = udp_datagram(1, 1000, &a);
        let db = udp_datagram(2, 2000, &b);

        let packets = parse_bytes(&pcap_of(&[
            frame(0x0001, 0, true, &da[..1480]),
            frame(0x0002, 0, true, &db[..1480]),
            frame(0x0001, 1480, false, &da[1480..]),
            frame(0x0002, 1480, false, &db[1480..]),
        ]))
        .expect("parse");

        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].dst_addr.port(), 1000);
        assert_eq!(packets[0].payload, a);
        assert_eq!(packets[1].dst_addr.port(), 2000);
        assert_eq!(packets[1].payload, b);
    }

    /// A capture that starts mid-datagram, or drops a fragment, must not
    /// produce a truncated packet that looks like a real one.
    #[test]
    fn an_incomplete_datagram_is_dropped() {
        let payload: Vec<u8> = vec![0x5A; 3000];
        let datagram = udp_datagram(6680, 6678, &payload);

        let only_first =
            parse_bytes(&pcap_of(&[frame(0x1234, 0, true, &datagram[..1480])])).expect("parse");
        assert!(only_first.is_empty(), "no last fragment, so no datagram");

        // First and last present, middle missing: the end is known but there
        // is a hole before it.
        let with_hole = parse_bytes(&pcap_of(&[
            frame(0x1234, 0, true, &datagram[..1480]),
            frame(0x1234, 2960, false, &datagram[2960..]),
        ]))
        .expect("parse");
        assert!(with_hole.is_empty(), "a hole means no datagram");
    }

    /// A frame padded to Ethernet's 60-byte minimum must not have its padding
    /// read as payload.
    #[test]
    fn ethernet_padding_is_not_payload() {
        let datagram = udp_datagram(6680, 6678, &[0x01, 0x02, 0x03]);
        let mut padded = frame(0x1234, 0, false, &datagram);
        padded.resize(60, 0x00);

        let packets = parse_bytes(&pcap_of(&[padded])).expect("parse");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].payload, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn roundtrip_write_parse() {
        let packets = vec![
            PcapPacket {
                timestamp: Duration::from_millis(0),
                src_addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 1234),
                dst_addr: SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 1), 5678),
                payload: vec![0x01, 0x02, 0x03],
            },
            PcapPacket {
                timestamp: Duration::from_millis(100),
                src_addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 4321),
                dst_addr: SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 2), 8765),
                payload: vec![0xAA, 0xBB],
            },
        ];

        let tmp = std::env::temp_dir().join("test_roundtrip.pcap");
        write_file(&tmp, &packets).expect("write");
        let parsed = parse_file(&tmp).expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].payload, vec![0x01, 0x02, 0x03]);
        assert_eq!(parsed[1].payload, vec![0xAA, 0xBB]);
        assert_eq!(parsed[0].src_addr.port(), 1234);
        assert_eq!(parsed[1].dst_addr.port(), 8765);
        std::fs::remove_file(&tmp).ok();
    }
}
