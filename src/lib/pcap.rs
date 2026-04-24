//! Pcap file parser for replay testing.
//!
//! Parses standard pcap files (`.pcap` and `.pcap.gz`, not `.pcapng`)
//! and extracts UDP packets with their source/destination addresses
//! and payloads.

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

/// Parse a pcap or NND file (optionally gzipped) and return all UDP packets.
///
/// Auto-detects the file format: NND files start with `Time:`, pcap files
/// start with a 4-byte magic number. Both `.gz` and uncompressed files are
/// supported.
pub fn parse_file(path: &Path) -> io::Result<Vec<PcapPacket>> {
    let data = if path.extension().map_or(false, |e| e == "gz") {
        let file = fs::File::open(path)?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        buf
    } else {
        fs::read(path)?
    };

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
            format!("unsupported pcap link type {} (only Ethernet/1 is supported)", link_type),
        ));
    }

    let mut packets = Vec::new();
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

        // Parse Ethernet + IP + UDP (timestamp filled in below)
        if let Some(mut pkt) = parse_udp_packet(pkt_data, Duration::ZERO) {
            // Anchor timing to the first UDP packet, not the first pcap record
            let first = first_ts.get_or_insert((ts_sec, ts_frac));
            let divisor: u64 = if nanoseconds { 1_000_000_000 } else { 1_000_000 };
            let abs_first = first.0 as u64 * divisor + first.1 as u64;
            let abs_now = ts_sec as u64 * divisor + ts_frac as u64;
            pkt.timestamp =
                Duration::from_nanos(abs_now.saturating_sub(abs_first) * (1_000_000_000 / divisor));
            packets.push(pkt);
        }
    }

    Ok(packets)
}

fn parse_udp_packet(data: &[u8], timestamp: Duration) -> Option<PcapPacket> {
    if data.len() < ETH_HEADER_LEN + IP_HEADER_MIN_LEN + UDP_HEADER_LEN {
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
    // Skip fragmented packets — non-initial fragments lack a UDP header
    let frag = u16::from_be_bytes(ip[6..8].try_into().ok()?);
    if frag & 0x3FFF != 0 {
        return None;
    }
    let src_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let dst_ip = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);

    // UDP header
    let udp = &ip[ihl..];
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
            "payload too large for UDP/IPv4: {} bytes", pkt.payload.len()
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

    if path.extension().map_or(false, |e| e == "gz") {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let file = fs::File::create(path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&data)?;
        encoder.finish()?;
        Ok(())
    } else {
        fs::write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
