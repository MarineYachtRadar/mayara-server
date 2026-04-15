use std::f64::consts::TAU;
use std::mem::transmute;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;

use crate::brand::navico::{HALO_HEADING_INFO_ADDRESS, HALO_SPEED_ADDRESS_A, HALO_SPEED_ADDRESS_B};
use crate::navdata::{get_cog, get_heading_true, get_sog};
use crate::network::create_multicast_send;
use crate::radar::{RadarError, RadarInfo};

/// Heading is reported in the range [0..0xF800), with 0xF800 representing 360°.
const HEADING_SCALE: f64 = 0xF800 as f64;

/// Default `u01` payload — observed identical across BR24, 4G, HALO captures
/// and matches what radar_pi sends.
const U01_DEFAULT: [u8; 26] = [
    0, 0, 0x10, 0, 0, 0x14, 0, 0, 4, 0, 0, 0, 0, 0, 5, 0x3C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x20,
];

/// Trailing 5 bytes of the heading packet — also matches radar_pi defaults.
const HEADING_U07_DEFAULT: [u8; 5] = [0xff, 0x7f, 0x79, 0xf8, 0xfc];

#[derive(Debug)]
#[repr(packed)]
#[allow(dead_code)]
pub(crate) struct HaloHeadingPacket {
    marker: [u8; 4],   //  0..4   "NKOE"
    preamble: [u8; 4], //  4..8   00 01 90 02
    counter: [u8; 2],  //  8..10  big-endian counter
    u01: [u8; 26],     // 10..36  fixed
    u02: [u8; 4],      // 36..40  type discriminator: 12 f1 01 00
    epoch: [u8; 8],    // 40..48  millis since 1970
    u04: [u8; 8],      // 48..56  always 02 00 00 00 00 00 00 00
    u05a: [u8; 4],     // 56..60  unknown (possibly position) — radar_pi sends 0
    u05b: [u8; 4],     // 60..64  unknown (possibly position) — radar_pi sends 0
    u06: [u8; 1],      // 64..65  always 0xff
    pub heading: [u8; 2], // 65..67 heading: u16 LE, scale 0..0xF800 = 0..360°
    u07: [u8; 5],      // 67..72  HEADING_U07_DEFAULT
}

impl HaloHeadingPacket {
    pub(crate) fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        Ok(unsafe {
            let report: [u8; 72] = bytes.try_into()?;
            transmute(report)
        })
    }
}

#[derive(Debug)]
#[repr(packed)]
#[allow(dead_code)]
pub(crate) struct HaloNavigationPacket {
    marker: [u8; 4],   //  0..4   "NKOE"
    preamble: [u8; 4], //  4..8   00 01 90 02
    counter: [u8; 2],  //  8..10  big-endian counter
    u01: [u8; 26],     // 10..36  fixed
    u02: [u8; 4],      // 36..40  type discriminator: 02 f8 01 00
    epoch: [u8; 8],    // 40..48  millis since 1970
    u04: [u8; 8],      // 48..56  always 02 00 00 00 00 00 00 00
    u05a: [u8; 4],     // 56..60  unknown (possibly position) — radar_pi sends 0
    u05b: [u8; 4],     // 60..64  unknown (possibly position) — radar_pi sends 0
    u06: [u8; 1],      // 64..65  always 0xff
    u07: [u8; 1],      // 65..66  always 0xfc
    pub cog: [u8; 2],  // 66..68  COG: u16 LE, scale 0..0xF800 = 0..360°
    pub sog: [u8; 2],  // 68..70  SOG: u16 LE, in cm/s
    u08: [u8; 2],      // 70..72  always 0xff 0xff
}

impl HaloNavigationPacket {
    pub(crate) fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        Ok(unsafe {
            let report: [u8; 72] = bytes.try_into()?;
            transmute(report)
        })
    }
}

#[derive(Debug)]
#[repr(packed)]
#[allow(dead_code)]
pub(crate) struct HaloSpeedPacket {
    marker: [u8; 6], //  0..6   01 d3 01 00 00 00
    pub sog: [u8; 2], // 6..8   SOG: u16 LE, in dm/s (m/s × 10)
    u00: [u8; 6],    //  8..14  00 00 01 00 00 00
    pub cog: [u8; 2], // 14..16 COG: u16 LE, in tenths of degrees (0..3600)
    u02: [u8; 6],    // 16..22  00 00 01 33 00 00
    u03: u8,         // 22      00
}

/// Index into the `Information::sock` array.
enum SocketIndex {
    HeadingAndNavigation = 0,
    SpeedA = 1,
    SpeedB = 2,
}

const SOCKET_ADDRESS: [SocketAddrV4; 3] = [
    HALO_HEADING_INFO_ADDRESS,
    HALO_SPEED_ADDRESS_A,
    HALO_SPEED_ADDRESS_B,
];

/// radar_pi sends heading every 100 ms, navigation and speed every 250 ms.
const HEADING_INTERVAL: Duration = Duration::from_millis(100);
const NAVIGATION_INTERVAL: Duration = Duration::from_millis(250);
const SPEED_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct Information {
    key: String,
    nic_addr: Ipv4Addr,
    sock: [Option<UdpSocket>; 3],
    counter: u16,
    last_heading: Option<Instant>,
    last_navigation: Option<Instant>,
    last_speed: Option<Instant>,
}

fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    unsafe {
        ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
    }
}

impl Information {
    pub(crate) fn new(key: String, info: &RadarInfo) -> Self {
        Information {
            key,
            nic_addr: info.nic_addr.clone(),
            sock: [None, None, None],
            counter: 0,
            last_heading: None,
            last_navigation: None,
            last_speed: None,
        }
    }

    async fn start_socket(&mut self, index: usize) -> Result<(), RadarError> {
        if self.sock[index].is_some() {
            return Ok(());
        }
        match create_multicast_send(&SOCKET_ADDRESS[index], &self.nic_addr) {
            Ok(sock) => {
                log::debug!(
                    "{} {} via {}: sending info",
                    self.key,
                    &SOCKET_ADDRESS[index],
                    &self.nic_addr
                );
                self.sock[index] = Some(sock);
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key,
                    &SOCKET_ADDRESS[index],
                    &self.nic_addr,
                    e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    async fn send(&mut self, index: usize, message: &[u8]) -> Result<(), RadarError> {
        self.start_socket(index).await?;
        if let Some(sock) = &self.sock[index] {
            sock.send(message).await.map_err(RadarError::Io)?;
            log::trace!("{}: sent {:02X?}", self.key, message);
        }
        Ok(())
    }

    async fn send_heading_packet(&mut self) -> Result<(), RadarError> {
        if let Some(heading) = get_heading_true() {
            // heading is in radians [0..2π); convert to the radar's [0..0xF800) scale
            let heading = (heading * HEADING_SCALE / TAU) as u16;
            let epoch = chrono::Utc::now().timestamp_millis().to_le_bytes();
            let packet = HaloHeadingPacket {
                marker: [b'N', b'K', b'O', b'E'],
                preamble: [0, 1, 0x90, 0x02],
                counter: self.counter.to_be_bytes(),
                u01: U01_DEFAULT,
                u02: [0x12, 0xf1, 0x01, 0x00],
                epoch,
                u04: [0x02, 0, 0, 0, 0, 0, 0, 0],
                u05a: [0; 4],
                u05b: [0; 4],
                u06: [0xff],
                heading: heading.to_le_bytes(),
                u07: HEADING_U07_DEFAULT,
            };

            let bytes: &[u8] = any_as_u8_slice(&packet);
            self.counter = self.counter.wrapping_add(1);

            self.send(SocketIndex::HeadingAndNavigation as usize, bytes)
                .await?;
        }
        Ok(())
    }

    async fn send_navigation_packet(&mut self) -> Result<(), RadarError> {
        if let (Some(sog), Some(cog)) = (get_sog(), get_cog()) {
            // cog is radians [0..2π) → scale [0..0xF800)
            let cog = (cog * HEADING_SCALE / TAU) as u16;
            // sog is m/s → wire format is cm/s (× 100)
            let sog = (sog * 100.0) as u16;
            let epoch = chrono::Utc::now().timestamp_millis().to_le_bytes();
            let packet = HaloNavigationPacket {
                marker: [b'N', b'K', b'O', b'E'],
                preamble: [0, 1, 0x90, 0x02],
                counter: self.counter.to_be_bytes(),
                u01: U01_DEFAULT,
                u02: [0x02, 0xf8, 0x01, 0x00],
                epoch,
                u04: [0x02, 0, 0, 0, 0, 0, 0, 0],
                u05a: [0; 4],
                u05b: [0; 4],
                u06: [0xff],
                u07: [0xfc],
                cog: cog.to_le_bytes(),
                sog: sog.to_le_bytes(),
                u08: [0xff, 0xff],
            };

            let bytes: &[u8] = any_as_u8_slice(&packet);
            self.counter = self.counter.wrapping_add(1);

            self.send(SocketIndex::HeadingAndNavigation as usize, bytes)
                .await?;
        }
        Ok(())
    }

    async fn send_speed_packet(&mut self) -> Result<(), RadarError> {
        if let (Some(sog), Some(cog)) = (get_sog(), get_cog()) {
            // sog is m/s → wire format is dm/s (× 10)
            let sog = (sog * 10.0) as u16;
            // cog is radians → wire format is tenths of degrees (× 1800/π = × 10 in degrees)
            let cog = (cog.to_degrees() * 10.0) as u16;
            let packet = HaloSpeedPacket {
                marker: [0x01, 0xd3, 0x01, 0x00, 0x00, 0x00],
                sog: sog.to_le_bytes(),
                u00: [0x00, 0x00, 0x01, 0x00, 0x00, 0x00],
                cog: cog.to_le_bytes(),
                u02: [0x00, 0x00, 0x01, 0x33, 0x00, 0x00],
                u03: 0,
            };

            let bytes: &[u8] = any_as_u8_slice(&packet);

            self.send(SocketIndex::SpeedA as usize, bytes).await?;
            self.send(SocketIndex::SpeedB as usize, bytes).await?;
        }
        Ok(())
    }

    pub(super) async fn send_info_packets(&mut self) -> Result<(), RadarError> {
        let now = Instant::now();
        if self.last_heading.is_none_or(|t| now - t >= HEADING_INTERVAL) {
            self.send_heading_packet().await?;
            self.last_heading = Some(now);
        }
        if self.last_navigation.is_none_or(|t| now - t >= NAVIGATION_INTERVAL) {
            self.send_navigation_packet().await?;
            self.last_navigation = Some(now);
        }
        if self.last_speed.is_none_or(|t| now - t >= SPEED_INTERVAL) {
            self.send_speed_packet().await?;
            self.last_speed = Some(now);
        }
        Ok(())
    }
}
