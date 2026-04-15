use anyhow::{Error, bail};
use bincode::deserialize;
use num_traits::FromPrimitive;
use serde::Deserialize;
use std::cmp::min;
use std::io;
use std::mem::transmute;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::Model;
use super::capabilities::NavicoCapabilities;
use super::command::Command;
use super::protocol::{
    STATE_CONFIG, STATE_FEATURES, STATE_INSTALLATION, STATE_MODE, STATE_PROPERTIES, STATE_SETUP,
    STATE_SETUP_EXTENDED, VALID_SPOKE_STATUSES,
};
use super::{
    DYNAMIC_ALLOWED_CONTROLS, SPOKE_DATA_LENGTH, SPOKE_PIXEL_LEN, SPOKES_PER_FRAME, SPOKES_RAW,
};

use crate::Cli;
use crate::brand::navico::info::{HaloHeadingPacket, HaloNavigationPacket, Information};
use crate::brand::navico::{HALO_HEADING_INFO_ADDRESS, HaloMode};
use crate::network;
use crate::radar::settings::ControlId;
use crate::radar::spoke::GenericSpoke;
use crate::radar::target::MS_TO_KN;
use crate::radar::{
    BYTE_LOOKUP_LENGTH, CommonRadar, DopplerMode, Legend, Power, RadarError, RadarInfo,
    SharedRadars, SpokeBearing,
};
use crate::replay::RadarSocket;
use crate::util::PrintableSpoke;
use crate::util::{c_string, c_wide_string};

/*
 Heading on radar. Observed in field:
 - Hakan: BR24, no RI: 0x9234 = negative, with recognisable 1234 in hex?
 - Marcus: 3G, RI, true heading: 0x45be
 - Kees: 4G, RI, mag heading: 0x07d6 = 2006 = 176,6 deg
 - Kees: 4G, RI, no heading: 0x8000 = -1 = negative
 - Kees: Halo, true heading: 0x4xxx => true
 Known values for heading value:
*/
const HEADING_TRUE_FLAG: u16 = 0x4000;
const HEADING_MASK: u16 = SPOKES_RAW - 1;
fn is_heading_true(x: u16) -> bool {
    (x & HEADING_TRUE_FLAG) != 0
}
fn is_valid_heading_value(x: u16) -> bool {
    (x & !(HEADING_TRUE_FLAG | HEADING_MASK)) == 0
}
fn extract_heading_value(x: u16) -> Option<u16> {
    match is_valid_heading_value(x) && is_heading_true(x) {
        true => Some(x & HEADING_MASK),
        false => None,
    }
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct GenBr24Header {
    header_len: u8,        // 1 bytes
    status: u8,            // 1 bytes
    _scan_number: [u8; 2], // 2 bytes
    _mark: [u8; 4],        // 4 bytes, on BR24 this is always 0x00, 0x44, 0x0d, 0x0e
    angle: [u8; 2],        // 2 bytes
    heading: [u8; 2],      // 2 bytes heading with RI-10/11. See bitmask explanation above.
    range: [u8; 4],        // 4 bytes
    _u01: [u8; 2],         // 2 bytes blank
    _u02: [u8; 2],         // 2 bytes
    _u03: [u8; 4],         // 4 bytes blank
} /* total size = 24 */

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct Gen3PlusHeader {
    header_len: u8,        // 1 bytes
    status: u8,            // 1 bytes
    _scan_number: [u8; 2], // 1 byte (HALO and newer), 2 bytes (4G and older)
    _mark: [u8; 2],        // 2 bytes
    large_range: [u8; 2],  // 2 bytes, on 4G and up
    angle: [u8; 2],        // 2 bytes
    heading: [u8; 2],      // 2 bytes heading with RI-10/11. See bitmask explanation above.
    small_range: [u8; 2],  // 2 bytes or -1
    _rotation: [u8; 2],    // 2 bytes or -1
    _u01: [u8; 4],         // 4 bytes signed integer, always -1
    _u02: [u8; 4], // 4 bytes signed integer, mostly -1 (0x80 in last byte) or 0xa0 in last byte
} /* total size = 24 */

#[derive(Debug, Clone, Copy)]
#[repr(packed)]
struct RadarLine {
    _header: Gen3PlusHeader, // or GenBr24Header
    _data: [u8; SPOKE_DATA_LENGTH],
}

#[repr(packed)]
struct FrameHeader {
    _frame_hdr: [u8; 8],
}

#[repr(packed)]
struct RadarFramePkt {
    _header: FrameHeader,
    _line: [RadarLine; SPOKES_PER_FRAME], //  scan lines, or spokes
}

const FRAME_HEADER_LENGTH: usize = size_of::<FrameHeader>();
const RADAR_LINE_HEADER_LENGTH: usize = size_of::<Gen3PlusHeader>();

const RADAR_LINE_LENGTH: usize = size_of::<RadarLine>();

// The LookupSpokeEnum is an index into an array, really
enum LookupDoppler {
    LowNormal = 0,
    LowBoth = 1,
    LowApproaching = 2,
    HighNormal = 3,
    HighBoth = 4,
    HighApproaching = 5,
}
const LOOKUP_DOPPLER_LENGTH: usize = (LookupDoppler::HighApproaching as usize) + 1;

type WireToLegendTable = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

fn wire_to_legend(legend: &Legend) -> WireToLegendTable {
    let mut lookup: WireToLegendTable = [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
    // Cannot use for() in const expr, so use while instead
    let mut j: usize = 0;
    while j < BYTE_LOOKUP_LENGTH {
        let low: u8 = (j as u8) & 0x0f;
        let high: u8 = ((j as u8) >> 4) & 0x0f;

        lookup[LookupDoppler::LowNormal as usize][j] = low;
        lookup[LookupDoppler::HighNormal as usize][j] = high;

        if let Some((approaching_idx, _)) = legend.doppler_approaching {
            if let Some((receding_idx, _)) = legend.doppler_receding {
                lookup[LookupDoppler::LowBoth as usize][j] = match low {
                    0x0f => approaching_idx,
                    0x0e => receding_idx,
                    0x08..=0x0d => low + 2,
                    _ => low + 1,
                };
                lookup[LookupDoppler::HighBoth as usize][j] = match high {
                    0x0f => approaching_idx,
                    0x0e => receding_idx,
                    0x08..=0x0d => high + 2,
                    _ => high + 1,
                };
            }
            lookup[LookupDoppler::LowApproaching as usize][j] = match low {
                0x0f => approaching_idx,
                _ => low + 1,
            };

            lookup[LookupDoppler::HighApproaching as usize][j] = match high {
                0x0f => approaching_idx,
                _ => high + 1,
            };
        }
        j += 1;
    }
    lookup
}

pub(crate) struct NavicoReportReceiver {
    common: CommonRadar,
    report_buf: Vec<u8>,
    report_socket: Option<RadarSocket>,
    info_buf: Vec<u8>,
    info_socket: Option<RadarSocket>,
    model: Model,
    capabilities: Option<NavicoCapabilities>,
    /// True when we've seen 0xC403 with a HALO model byte but haven't
    /// received 0xC409 yet. Controls/ranges are deferred until capabilities arrive.
    awaiting_capabilities: bool,
    command_sender: Option<Command>,
    info_sender: Option<Information>,
    info_send_timeout: Instant,
    report_request_timeout: Instant,
    reported_unknown: [bool; 256],
    reported_setup_ext_oversize: bool,
    has_use_mode_from_ext: bool,

    // For data (spokes)
    data_buf: Vec<u8>,
    data_socket: Option<RadarSocket>,
    doppler: DopplerMode,
    wire_to_legend: WireToLegendTable,
}

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

// When another MFD or sender is broadcasting INFO reports, we suppress our
// own transmission for this long after each received packet. radar_pi uses
// 10 seconds.
const INFO_BY_OTHERS_TIMEOUT: Duration = Duration::from_secs(10);

// How often the info send loop wakes up. The actual per-packet rates are
// rate-limited inside `Information::send_info_packets()`: heading at 100 ms,
// navigation and speed at 250 ms.
const INFO_BY_US_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
#[repr(packed)]
struct StateMode {
    // 0xC401
    _sub_opcode: u8,
    _category: u8,
    status: u8,
    _u00: [u8; 15],
}

impl StateMode {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 18] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

#[derive(Debug)]
#[repr(packed)]
struct StateSetup {
    // 0xC402
    _sub_opcode: u8,
    _category: u8,
    range: [u8; 4],             // 2..6 = range
    _u00: [u8; 1],              // 6
    mode: u8,                   // 7 = mode
    gain_auto: u8,              // 8
    _u01: [u8; 3],              // 9..12
    gain: u8,                   // 12
    sea_auto: u8,               // 13 = sea_auto, 0 = off, 1 = harbor, 2 = offshore
    _u02: [u8; 3],              // 14..17
    sea: [u8; 4],               // 17..21
    _u03: u8,                   // 21
    rain: u8,                   // 22
    _u04: [u8; 11],             // 23..34
    interference_rejection: u8, // 34
    _u05: [u8; 3],              // 35..38
    target_expansion: u8,       // 38
    _u06: [u8; 3],              // 39..42
    target_boost: u8,           // 42
    _u07: [u8; 56],             // 43..99
}

impl StateSetup {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 99] = bytes.try_into()?;
            transmute(report)
        })
    }
}

#[derive(Debug)]
#[repr(packed)]
struct StateProperties {
    // 0xC403 — fixed 129 bytes
    _sub_opcode: u8,                 //   0  0x03
    _category: u8,                   //   1  0xC4
    _u00: [u8; 12],                  //   2..14
    feature_flags: u8,               //  14  bit0=FeaturesReport, bit1=IsDownMast
    _u01: u8,                        //  15
    sw_build: [u8; 2],               //  16..18  u16 LE (SW version 3rd field)
    _u02: [u8; 12],                  //  18..30
    scanner_type: [u8; 4],           //  30..34  eScannerType (u32 LE, 0..23)
    transmit_time: [u8; 4],          //  34..38  u32 LE (operating hours)
    warmup_time: [u8; 4],            //  38..42  u32 LE
    max_range: [u8; 4],              //  42..46  u32 LE (max range in decimeters)
    _u03: [u8; 4],                   //  46..50
    sw_version_major: [u8; 4],       //  50..54  u32 LE
    sw_version_minor: [u8; 4],       //  54..58  u32 LE
    build_date: [u8; 32],            //  58..90  UTF-16LE, 16 chars
    build_time: [u8; 32],            //  90..122 UTF-16LE, 16 chars
    radar_protocol_version: [u8; 4], // 122..126 u32 LE
    scanner_detail_supported: u8,    // 126
    _u04: u8,                        // 127
    _flag: u8,                       // 128 (zeroed when scanner_type ≤ 9)
}

impl StateProperties {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 129] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

#[derive(Debug)]
#[repr(packed)]
struct StateConfig {
    // 0xC404
    _sub_opcode: u8,
    _category: u8,
    _u00: [u8; 4],                       // 2..6
    bearing_alignment: [u8; 2],          // 6..8
    _u01: [u8; 2],                       // 8..10
    antenna_height: [u8; 4],             // 10..14 = Antenna height in mm (i32 LE)
    _u02: [u8; 5],                       // 14..19
    accent_light: u8,                    // 19 = Accent light
    antenna_forward: [u8; 2],            // 20..22 = Antenna forward offset in mm (i16 LE)
    _u03a: u8,                           // 22
    antenna_starboard: [u8; 2],          // 23..25 = Antenna starboard offset in mm (i16 LE)
    _u03b: [u8; 9],                      // 25..34
    blanking: [SectorBlankingReport; 4], // 34..54
    _u04: [u8; 12],                      // 54..66
}

impl StateConfig {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 66] = bytes.try_into()?;
            transmute(report)
        })
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct SectorBlankingReport {
    enabled: u8,
    start_angle: [u8; 2],
    end_angle: [u8; 2],
}

// 0xC406 StateInstallation is TLV-encoded (not fixed-layout).
// After the 2-byte opcode prefix, each entry has a 4-byte header:
//   u16 tag (LE), u16 length (LE), then `length` bytes of data.
mod installation_tag {
    pub(crate) const NAME: u16 = 0x0000;
    pub(crate) const ANTENNA_GEOMETRY: u16 = 0x0001;
    pub(crate) const SECTOR_BLANKING: u16 = 0x0003;
    pub(crate) const CABLE_CALIBRATION: u16 = 0x0004;
}

// 0xC408 StateSetupExtended — variable length, parsed sequentially like the
// SRX firmware's tDataParser. Offsets after the 2-byte opcode header:
//
//   0..16  (16 bytes) control block:
//          [0] stc_curve  [1] local_interference  [2] scan_speed
//          [3] sls_auto   [4..6] unknown          [7] sidelobe_value
//          [8..9] unknown (u16)  [10] noise_reject [11] target_sep
//          [12] sea_clutter  [13] auto_sea_clutter  [14] anti_clutter_mode
//          [15] unknown
//   16..19 (3 bytes) doppler: state(u8) + speed(u16 LE)
//   19     (1 byte)  unknown
//   20..22 (2 bytes) tUseMode: mode(u8) + variant(u8)
const SETUP_EXT_HEADER: usize = 2;
const SETUP_EXT_CONTROLS: usize = 16;
const SETUP_EXT_DOPPLER: usize = 3;
const SETUP_EXT_UNKNOWN: usize = 1;
const SETUP_EXT_USE_MODE: usize = 2;
const SETUP_EXT_MIN_LEN: usize = SETUP_EXT_HEADER + SETUP_EXT_CONTROLS;
const SETUP_EXT_MAX_KNOWN: usize =
    SETUP_EXT_MIN_LEN + SETUP_EXT_DOPPLER + SETUP_EXT_UNKNOWN + SETUP_EXT_USE_MODE; // 24
const SETUP_EXT_MAX_SEEN: usize = 32; // Observed in the field, log a warning if we see more than this

impl NavicoReportReceiver {
    pub(crate) fn new(
        args: &Cli,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
    ) -> NavicoReportReceiver {
        let key = info.key();

        let replay = args.is_replay();
        log::debug!(
            "{}: Creating NavicoReportReceiver with args {:?}",
            key,
            args
        );
        // If we are in replay mode, we don't need a command sender, as we will not send any commands
        let command_sender = if !replay {
            log::debug!("{}: Starting command sender", key);
            Some(Command::new(args.fake_errors, info.clone()))
        } else {
            log::debug!("{}: No command sender, replay mode", key);
            None
        };
        let info_sender = if !replay {
            log::debug!("{}: Starting info sender", key);
            Some(Information::new(key.clone(), &info))
        } else {
            log::debug!("{}: No info sender, replay mode", key);
            None
        };

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let wire_to_legend = wire_to_legend(&info.get_legend());

        let common = CommonRadar::new(
            &args,
            key,
            info,
            radars.clone(),
            control_update_rx,
            replay,
            blob_tx,
        );

        let now = Instant::now();
        NavicoReportReceiver {
            common,
            report_buf: Vec::with_capacity(1000),
            report_socket: None,
            info_buf: Vec::with_capacity(::core::mem::size_of::<HaloHeadingPacket>()),
            info_socket: None,
            model: Model::Unknown,
            capabilities: None,
            awaiting_capabilities: false,
            command_sender,
            info_sender,
            info_send_timeout: now,
            report_request_timeout: now,
            reported_unknown: [false; 256],
            reported_setup_ext_oversize: false,
            has_use_mode_from_ext: false,
            data_buf: Vec::with_capacity(size_of::<RadarFramePkt>()),
            data_socket: None,
            doppler: DopplerMode::None,
            wire_to_legend,
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_report_socket()?;
        loop {
            match self.socket_loop(&subsys).await {
                Ok(()) => {
                    break Ok(());
                }
                Err(e) => {
                    log::error!("{}: trying to recover from error {}", self.common.key, e);
                }
            }
            sleep(Duration::from_millis(2000)).await;
        }
    }

    fn start_report_socket(&mut self) -> io::Result<()> {
        match network::create_udp_listen(
            &self.common.info.report_addr,
            &self.common.info.nic_addr,
            network::SocketType::Multicast,
        ) {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    fn start_info_socket(&mut self) -> io::Result<()> {
        if self.info_socket.is_some() {
            return Ok(()); // Already started
        }
        match network::create_udp_listen(
            &HALO_HEADING_INFO_ADDRESS,
            &self.common.info.nic_addr,
            network::SocketType::Multicast,
        ) {
            Ok(socket) => {
                self.info_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for info reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    fn start_data_socket(&mut self) -> io::Result<()> {
        if self.data_socket.is_some() {
            return Ok(()); // Already started
        }
        match network::create_udp_listen(
            &self.common.info.spoke_data_addr,
            &self.common.info.nic_addr,
            network::SocketType::Multicast,
        ) {
            Ok(sock) => {
                self.data_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} via {}: create multicast failed: {}",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);

        loop {
            if !self.common.replay {
                self.start_info_socket()?;
            }
            self.start_data_socket()?;

            let timeout = min(self.report_request_timeout, self.info_send_timeout);

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Ok(());
                },

                _ = sleep_until(timeout) => {
                    let now = Instant::now();
                    if self.report_request_timeout <= now {
                        self.send_report_requests().await?;
                    }
                    if self.info_send_timeout <= now {
                        // If no other device sends these packets, send them ourselves.
                        // This enables Doppler returns from the HALO radars.
                        self.send_info_packets().await?;
                    }
                },

                r = self.report_socket.as_mut().unwrap().recv_buf_from(&mut self.report_buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_report().await {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            self.report_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                Some(r) = Self::conditional_receive(&mut self.info_socket, &mut self.info_buf) => {
                    match r {
                        Ok((_len, addr)) => {
                            self.process_info(&addr);
                            self.info_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive info error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.data_socket.as_mut().unwrap().recv_buf_from(&mut self.data_buf)  => {
                    match r {
                        Ok(_) => {
                            self.process_frame();
                            self.data_buf.clear();
                        },
                        Err(e) => {
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.common.control_update_rx.recv() => {
                    match r {
                        Ok(cu) => {let _ = self.common.process_control_update(cu, &mut self.command_sender).await;},
                        Err(_) => {},
                    }
                }


            }
        }
    }

    async fn conditional_receive(
        socket: &mut Option<RadarSocket>,
        buf: &mut Vec<u8>,
    ) -> Option<io::Result<(usize, SocketAddr)>> {
        match socket {
            Some(s) => Some(s.recv_buf_from(buf).await),
            None => None,
        }
    }

    fn process_frame(&mut self) {
        if self.data_buf.len() < FRAME_HEADER_LENGTH + RADAR_LINE_LENGTH {
            log::warn!(
                "UDP data frame with even less than one spoke, len {} dropped",
                self.data_buf.len()
            );
            return;
        }

        let spokes_in_frame = (self.data_buf.len() - FRAME_HEADER_LENGTH) / RADAR_LINE_LENGTH;

        log::trace!("Received UDP frame with {} spokes", &spokes_in_frame);

        self.common.new_spoke_message();

        let mut offset: usize = FRAME_HEADER_LENGTH;
        for scanline in 0..spokes_in_frame {
            let header_slice = &self.data_buf[offset..offset + RADAR_LINE_HEADER_LENGTH];
            let spoke_slice = &self.data_buf[offset + RADAR_LINE_HEADER_LENGTH
                ..offset + RADAR_LINE_HEADER_LENGTH + SPOKE_DATA_LENGTH];

            if let Some((range, angle, heading)) = self.validate_header(header_slice, scanline) {
                log::trace!("range {} angle {} heading {:?}", range, angle, heading);
                log::trace!(
                    "Received  {:04} spoke {}",
                    scanline,
                    PrintableSpoke::new(spoke_slice)
                );

                self.common
                    .add_spoke(range, angle, heading, self.process_spoke(spoke_slice));
            } else {
                log::warn!("Invalid spoke: header {:02X?}", &header_slice);
            }

            offset += RADAR_LINE_LENGTH;
        }
        self.common.send_spoke_message();
    }

    fn process_spoke(&self, spoke: &[u8]) -> GenericSpoke {
        let wire_to_legend = &self.wire_to_legend;

        // Convert the spoke data to bytes
        let mut generic_spoke: Vec<u8> = Vec::with_capacity(SPOKE_PIXEL_LEN);
        let low_nibble_index = (match self.doppler {
            DopplerMode::None => LookupDoppler::LowNormal,
            DopplerMode::Both => LookupDoppler::LowBoth,
            DopplerMode::Approaching => LookupDoppler::LowApproaching,
        }) as usize;
        let high_nibble_index = (match self.doppler {
            DopplerMode::None => LookupDoppler::HighNormal,
            DopplerMode::Both => LookupDoppler::HighBoth,
            DopplerMode::Approaching => LookupDoppler::HighApproaching,
        }) as usize;

        for pixel in spoke {
            let pixel = *pixel as usize;
            generic_spoke.push(wire_to_legend[low_nibble_index][pixel]);
            generic_spoke.push(wire_to_legend[high_nibble_index][pixel]);
        }

        generic_spoke
    }

    /// Set the HALO operating mode and update dynamic control permissions.
    /// Non-custom modes lock certain controls to read-only.
    fn set_halo_mode(&mut self, mode: i32) {
        if let Some(hm) = HaloMode::from_i32(mode) {
            self.common.set_value(&ControlId::Mode, mode as f64);
            let allowed = hm == HaloMode::Custom;
            for ct in &DYNAMIC_ALLOWED_CONTROLS {
                self.common.info.controls.set_allowed(ct, allowed);
            }
        } else {
            log::error!("{}: Unsupported HALO mode {}", self.common.key, mode);
        }
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        if let Some(command_sender) = &mut self.command_sender {
            command_sender.send_report_requests().await?;
        }
        self.report_request_timeout += REPORT_REQUEST_INTERVAL;
        Ok(())
    }

    async fn send_info_packets(&mut self) -> Result<(), RadarError> {
        if let Some(info_sender) = &mut self.info_sender {
            info_sender.send_info_packets().await?;
        }
        self.info_send_timeout += INFO_BY_US_INTERVAL;
        Ok(())
    }

    fn process_info(&mut self, addr: &SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            if addr.ip() == &self.common.info.nic_addr {
                log::trace!(
                    "{}: Ignoring info from ourselves ({})",
                    self.common.key,
                    addr
                );
            } else {
                log::trace!(
                    "{}: {} is sending information updates",
                    self.common.key,
                    addr
                );
                self.info_send_timeout = Instant::now() + INFO_BY_OTHERS_TIMEOUT;

                if self.info_buf.len() >= ::core::mem::size_of::<HaloNavigationPacket>() {
                    if self.info_buf[36] == 0x02 {
                        if let Ok(report) = HaloNavigationPacket::transmute(&self.info_buf) {
                            let sog = u16::from_le_bytes(report.sog) as f64 * 0.01 * MS_TO_KN;
                            let cog = u16::from_le_bytes(report.cog) as f64 * 360.0 / 63488.0;
                            log::trace!(
                                "{}: Halo sog={sog} cog={cog} from navigation report {:?}",
                                self.common.key,
                                report
                            );
                        }
                    } else {
                        if let Ok(report) = HaloHeadingPacket::transmute(&self.info_buf) {
                            log::trace!("{}: Halo heading report {:?}", self.common.key, report);
                        }
                    }
                }
            }
        }
    }

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

        if data.len() < 2 {
            bail!("UDP report len {} dropped", data.len());
        }

        if data[1] != 0xc4 {
            if data[1] == 0xc6 {
                match data[0] {
                    0x11 => {
                        if data.len() != 3 || data[2] != 0 {
                            bail!("Strange content of report 0x0a 0xc6: {:02X?}", data);
                        }
                        // this is just a response to the MFD sending 0x0a 0xc2,
                        // not sure what purpose it serves.
                    }
                    _ => {
                        log::trace!("Unknown report 0x{:02x} 0xc6: {:02X?}", data[0], data);
                    }
                }
            } else {
                log::trace!("Unknown report {:02X?} dropped", data)
            }
            return Ok(());
        }
        let sub_opcode = data[0];
        let ready = self.model != Model::Unknown && !self.awaiting_capabilities;

        match sub_opcode {
            // These two identify the model and set ranges — always process
            STATE_PROPERTIES => {
                return self.process_state_properties().await;
            }
            STATE_FEATURES => {
                return self.process_state_features().await;
            }
            // Everything else is gated on model and controls being fully set up
            STATE_MODE if ready => {
                return self.process_state_mode().await;
            }
            STATE_SETUP if ready => {
                return self.process_state_setup().await;
            }
            STATE_CONFIG if ready => {
                return self.process_state_config().await;
            }
            STATE_INSTALLATION if ready => {
                return self.process_state_installation().await;
            }
            STATE_SETUP_EXTENDED if ready => {
                return self.process_state_setup_extended().await;
            }
            _ => {
                if ready && !self.reported_unknown[sub_opcode as usize] {
                    self.reported_unknown[sub_opcode as usize] = true;
                    log::trace!(
                        "Unknown state sub_opcode 0x{:02X} len {} data {:02X?} dropped",
                        sub_opcode,
                        data.len(),
                        data
                    );
                }
            }
        }
        Ok(())
    }

    async fn process_state_mode(&mut self) -> Result<(), Error> {
        let report = StateMode::transmute(&self.report_buf)?;

        log::debug!("{}: report {:?}", self.common.key, report);

        self.set_status(report.status)
    }

    fn set_status(&mut self, status: u8) -> Result<(), Error> {
        let status = match status {
            0 => Power::Off,
            1 => Power::Standby,
            2 => Power::Transmit,
            5 => Power::Preparing,
            _ => {
                bail!("{}: Unknown radar status {}", self.common.key, status);
            }
        };
        self.common
            .set_value(&ControlId::Power, status as i32 as f64);
        Ok(())
    }

    async fn process_state_setup(&mut self) -> Result<(), Error> {
        let report = StateSetup::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let mode = report.mode as i32;
        let range = i32::from_le_bytes(report.range);
        let gain_auto: u8 = report.gain_auto;
        let gain = report.gain as i32;
        let sea_auto = report.sea_auto;
        let sea = i32::from_le_bytes(report.sea);
        let rain = report.rain as i32;
        let interference_rejection = report.interference_rejection as i32;
        let target_expansion = report.target_expansion as i32;
        let target_boost = report.target_boost as i32;

        self.common.set_value(&ControlId::Range, range as f64);
        if self.model.is_halo() && !self.has_use_mode_from_ext {
            // 0xC402 only carries the mode byte without variant, so it can't
            // distinguish Bird from Bird+. Once 0xC408 provides the full
            // tUseMode (with variant), we skip mode updates from 0xC402.
            self.set_halo_mode(mode);
        }
        self.common
            .set_value_auto(&ControlId::Gain, gain as f64, gain_auto);
        if !self.model.is_halo() {
            self.common
                .set_value_auto(&ControlId::Sea, sea as f64, sea_auto);
        } else {
            self.common
                .info
                .controls
                .set_auto_state(&ControlId::Sea, sea_auto > 0)
                .unwrap(); // Only crashes if control not supported which would be an internal bug
        }
        self.common.set_value(&ControlId::Rain, rain as f64);
        self.common.set_value(
            &ControlId::InterferenceRejection,
            interference_rejection as f64,
        );
        self.common
            .set_value(&ControlId::TargetExpansion, target_expansion as f64);
        self.common
            .set_value(&ControlId::TargetBoost, target_boost as f64);

        Ok(())
    }

    async fn process_state_properties(&mut self) -> Result<(), Error> {
        let report = StateProperties::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let scanner_type = u32::from_le_bytes(report.scanner_type);
        let transmit_time = u32::from_le_bytes(report.transmit_time);
        let warmup_time = u32::from_le_bytes(report.warmup_time);
        let max_range_dm = u32::from_le_bytes(report.max_range);
        let sw_major = u32::from_le_bytes(report.sw_version_major);
        let sw_minor = u32::from_le_bytes(report.sw_version_minor);
        let sw_build = u16::from_le_bytes(report.sw_build);
        let build_date = c_wide_string(&report.build_date);
        let build_time = c_wide_string(&report.build_time);
        let protocol_version = u32::from_le_bytes(report.radar_protocol_version);
        let scanner_detail_supported = report.scanner_detail_supported;
        let model = Model::from_scanner_type(scanner_type);

        if model == Model::Unknown {
            if !self.reported_unknown[scanner_type as usize & 0xFF] {
                self.reported_unknown[scanner_type as usize & 0xFF] = true;
                log::error!(
                    "{}: Unknown scanner type {} in 0xC403",
                    self.common.key,
                    scanner_type
                );
            }
        } else if self.model != model {
            let max_range_m = max_range_dm / 10;
            log::info!(
                "{}: Radar is model {} (scanner type {}, firmware {}.{}.{} {} {}, \
                 protocol v{}, max range {}m, transmit {}h, warmup {}s, features 0x{:02x}, details supported {})",
                self.common.key,
                model,
                scanner_type,
                sw_major,
                sw_minor,
                sw_build,
                build_date,
                build_time,
                protocol_version,
                max_range_m,
                transmit_time,
                warmup_time,
                report.feature_flags,
                scanner_detail_supported,
            );
            self.model = model;
            if let Some(cs) = &mut self.command_sender {
                cs.set_model(model);
            }

            if model.is_halo() {
                if let Some(caps) = &self.capabilities {
                    // 0xC409 arrived before 0xC403; consume cached capabilities now
                    let caps = caps.clone();
                    self.finalize_halo(&caps);
                } else {
                    // HALO: defer control and range setup until 0xC409 capabilities arrive
                    self.awaiting_capabilities = true;
                }
            } else {
                // Non-HALO (BR24/3G/4G): finalize controls and ranges immediately
                let info2 = self.common.info.clone();
                super::settings::update_when_model_known(
                    &mut self.common.info.controls,
                    model,
                    &info2,
                );
                let ranges = crate::radar::range::Ranges::from_range(50, max_range_m as i32);
                self.common.set_ranges(ranges);
                self.common.update();
            }
        }

        let firmware = format!(
            "{}.{}.{} {} {}",
            sw_major, sw_minor, sw_build, build_date, build_time
        );
        self.common
            .set_value(&ControlId::TransmitTime, transmit_time as f64);
        self.common
            .set_string(&ControlId::FirmwareVersion, firmware);
        log::trace!(
            "{}: warmup={}s protocol=v{}",
            self.common.key,
            warmup_time,
            protocol_version,
        );

        Ok(())
    }

    async fn process_state_features(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;
        if data.len() < 4 {
            return Ok(());
        }

        // Skip the 2-byte opcode header (0xC409)
        let caps = NavicoCapabilities::parse(&data[2..]);

        // If we were waiting for capabilities before finalizing HALO controls, do it now
        if self.awaiting_capabilities {
            self.finalize_halo(&caps);
        }

        self.capabilities = Some(caps);
        Ok(())
    }

    /// Finalize HALO controls and ranges from 0xC409 capabilities.
    /// Called either when 0xC409 arrives after 0xC403, or when 0xC403
    /// arrives and cached capabilities are already available.
    fn finalize_halo(&mut self, caps: &NavicoCapabilities) {
        self.awaiting_capabilities = false;

        let model = self.model;
        log::info!(
            "{}: Capabilities received for {} (doppler={})",
            self.common.key,
            model,
            caps.has_doppler(),
        );

        let info2 = self.common.info.clone();
        super::settings::update_when_model_known(&mut self.common.info.controls, model, &info2);
        self.common.info.set_doppler(caps.has_doppler());
        self.wire_to_legend = wire_to_legend(&self.common.info.get_legend());
        super::settings::update_from_capabilities(&mut self.common.info.controls, caps);

        if caps.instrumented_range_max_dm > 0 {
            let ranges =
                crate::radar::range::Ranges::from_range(caps.range_min_m(), caps.range_max_m());
            self.common.set_ranges(ranges);
        }
        self.common.update();
    }

    async fn process_state_config(&mut self) -> Result<(), Error> {
        let report = StateConfig::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        self.common.set_value(
            &ControlId::BearingAlignment,
            i16::from_le_bytes(report.bearing_alignment) as f64,
        );
        self.common.set_value(
            &ControlId::AntennaHeight,
            i32::from_le_bytes(report.antenna_height) as f64,
        );
        if self.model.is_halo() {
            self.common
                .set_value(&ControlId::AccentLight, report.accent_light as f64);
        }

        // Antenna offsets (i16 LE, mm)
        self.common.set_value(
            &ControlId::AntennaForward,
            i16::from_le_bytes(report.antenna_forward) as f64,
        );
        self.common.set_value(
            &ControlId::AntennaStarboard,
            i16::from_le_bytes(report.antenna_starboard) as f64,
        );

        // Sector blanking: 4× {u8 enabled, i16 start, i16 end} at offset 34
        for (i, sector) in super::BLANKING_SECTORS {
            let blanking = &report.blanking[i];
            let start_angle = i16::from_le_bytes(blanking.start_angle);
            let end_angle = i16::from_le_bytes(blanking.end_angle);
            let enabled = Some(blanking.enabled > 0);
            self.common.info.controls.set_sector(
                &sector,
                start_angle as f64,
                end_angle as f64,
                enabled,
            )?;
        }

        Ok(())
    }

    /// 0xC406 StateInstallation — TLV-encoded.
    /// Tags: 0=name, 1=antenna geometry, 3=sector blanking, 4=cable cal, 6=HS craft.
    /// Antenna offsets and sector blanking are also delivered via 0xC404 StateConfig,
    /// so we only parse the name and cable calibration here.
    async fn process_state_installation(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;
        if data.len() < 4 {
            return Ok(());
        }

        // Skip 2-byte opcode prefix, then walk TLV entries
        let mut offset = 2usize;
        while offset + 4 <= data.len() {
            let tag = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
            let len = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + len > data.len() {
                log::warn!(
                    "{}: 0xC406 TLV tag {} truncated at offset {}",
                    self.common.key,
                    tag,
                    offset
                );
                break;
            }

            let payload = &data[offset..offset + len];
            offset += len;

            match tag {
                installation_tag::NAME => {
                    if let Some(name) = c_string(payload) {
                        if !name.is_empty() {
                            let _ = self
                                .common
                                .info
                                .controls
                                .set_string(&ControlId::UserName, name.to_string());
                        }
                    }
                }
                installation_tag::ANTENNA_GEOMETRY if payload.len() >= 14 => {
                    // Offsets within tag data: 6-7 = forward, 10-11 = starboard (i16 LE, mm)
                    let forward = i16::from_le_bytes(payload[6..8].try_into().unwrap());
                    let starboard = i16::from_le_bytes(payload[10..12].try_into().unwrap());
                    self.common
                        .set_value(&ControlId::AntennaForward, forward as f64);
                    self.common
                        .set_value(&ControlId::AntennaStarboard, starboard as f64);
                }
                installation_tag::SECTOR_BLANKING if !payload.is_empty() => {
                    let count = payload[0] as usize;
                    let mut pos = 1;
                    for (i, sector) in super::BLANKING_SECTORS {
                        if i >= count || pos + 5 > payload.len() {
                            break;
                        }
                        let enabled = Some(payload[pos] > 0);
                        let start =
                            i16::from_le_bytes(payload[pos + 1..pos + 3].try_into().unwrap());
                        let end = i16::from_le_bytes(payload[pos + 3..pos + 5].try_into().unwrap());
                        let _ = self.common.info.controls.set_sector(
                            &sector,
                            start as f64,
                            end as f64,
                            enabled,
                        );
                        pos += 5;
                    }
                }
                installation_tag::CABLE_CALIBRATION if payload.len() >= 12 => {
                    let cable_length = i32::from_le_bytes(payload[4..8].try_into().unwrap());
                    log::trace!("{}: cable length {} mm", self.common.key, cable_length);
                }
                _ => {
                    log::trace!(
                        "{}: 0xC406 unknown tag {} len {}",
                        self.common.key,
                        tag,
                        len
                    );
                }
            }
        }

        Ok(())
    }

    async fn process_state_setup_extended(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;
        let len = data.len();

        if len < SETUP_EXT_MIN_LEN {
            bail!(
                "{}: StateSetupExtended (0xC408) too short: {} bytes",
                self.common.key,
                len
            );
        }
        if len > SETUP_EXT_MAX_SEEN && !self.reported_setup_ext_oversize {
            self.reported_setup_ext_oversize = true;
            log::warn!(
                "{}: StateSetupExtended (0xC408) {} bytes, expected at most {}",
                self.common.key,
                len,
                SETUP_EXT_MAX_SEEN
            );
        }

        // Parse the 16-byte control block at offset 2
        let ctl = &data[SETUP_EXT_HEADER..];
        let sea_state = ctl[0];
        let local_interference_rejection = ctl[1];
        let scan_speed = ctl[2];
        let sls_auto = ctl[3];
        let sidelobe_suppression = ctl[7];
        let noise_rejection = ctl[10];
        let target_sep = ctl[11];
        let sea_clutter = ctl[12];
        let auto_sea_clutter = ctl[13] as i8;

        // Doppler block (3 bytes) at offset 18
        if len >= SETUP_EXT_MIN_LEN + SETUP_EXT_DOPPLER {
            let dop = &data[SETUP_EXT_MIN_LEN..];
            let doppler_state = dop[0];
            let doppler_speed = u16::from_le_bytes([dop[1], dop[2]]);

            match doppler_state.try_into() {
                Ok(doppler_mode) => {
                    log::debug!(
                        "{}: doppler mode={} speed={}",
                        self.common.key,
                        doppler_mode,
                        doppler_speed
                    );
                    self.doppler = doppler_mode;
                }
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.common.key,
                        doppler_state
                    );
                }
            }
            self.common
                .set_value(&ControlId::Doppler, doppler_state as f64);
            self.common
                .set_value(&ControlId::DopplerSpeedThreshold, doppler_speed as f64);
        }

        // tUseMode (2 bytes) at offset 22, after 1 unknown byte.
        // Bird Plus = bird mode (5) with variant 1; mapped to HaloMode::BirdPlus (6).
        if len >= SETUP_EXT_MAX_KNOWN && self.model.is_halo() {
            let um = &data[SETUP_EXT_MAX_KNOWN - SETUP_EXT_USE_MODE..];
            let mode = um[0];
            let variant = um[1];

            self.has_use_mode_from_ext = true;
            // Bird Plus = bird mode (5) with variant 1
            let halo_mode = if mode == HaloMode::Bird as u8 && variant == 1 {
                HaloMode::BirdPlus as i32
            } else {
                mode as i32
            };
            self.set_halo_mode(halo_mode);
        }

        if self.model.is_halo() {
            self.common
                .set_value(&ControlId::SeaState, sea_state as f64);
            self.common.set_value_with_many_auto(
                &ControlId::Sea,
                sea_clutter as f64,
                auto_sea_clutter as f64,
            );
        }
        self.common.set_value(
            &ControlId::LocalInterferenceRejection,
            local_interference_rejection as f64,
        );
        self.common
            .set_value(&ControlId::ScanSpeed, scan_speed as f64);
        self.common.set_value_auto(
            &ControlId::SideLobeSuppression,
            sidelobe_suppression as f64,
            sls_auto,
        );
        self.common
            .set_value(&ControlId::NoiseRejection, noise_rejection as f64);
        if self.model.is_halo() || self.model == Model::Gen4 {
            self.common
                .set_value(&ControlId::TargetSeparation, target_sep as f64);
        }

        Ok(())
    }

    fn validate_header(
        &self,
        header_slice: &[u8],
        scanline: usize,
    ) -> Option<(u32, SpokeBearing, Option<u16>)> {
        match self.model {
            Model::BR24 | Model::Gen3 => match deserialize::<GenBr24Header>(&header_slice) {
                Ok(header) => {
                    log::trace!("Received {:04} header {:?}", scanline, header);

                    Self::validate_br24_header(&header)
                }
                Err(e) => {
                    log::warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
            _ => match deserialize::<Gen3PlusHeader>(&header_slice) {
                Ok(header) => {
                    log::trace!("Received {:04} header {:?}", scanline, header);

                    Self::validate_4g_header(&header)
                }
                Err(e) => {
                    log::warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
        }
    }

    fn validate_4g_header(header: &Gen3PlusHeader) -> Option<(u32, SpokeBearing, Option<u16>)> {
        if header.header_len != (RADAR_LINE_HEADER_LENGTH as u8) {
            log::warn!(
                "Spoke with illegal header length ({}) ignored",
                header.header_len
            );
            return None;
        }
        if !VALID_SPOKE_STATUSES.contains(&header.status) {
            log::warn!(
                "Spoke with unknown or invalid status (0x{:x}) ignored",
                header.status
            );
            return None;
        }

        let heading = u16::from_le_bytes(header.heading);
        let angle = u16::from_le_bytes(header.angle) / 2;
        let large_range = u16::from_le_bytes(header.large_range);
        let small_range = u16::from_le_bytes(header.small_range);

        let range = if large_range == 0x80 {
            if small_range == 0xffff {
                0
            } else {
                (small_range as u32) / 4
            }
        } else {
            ((large_range as u32) * (small_range as u32)) / 512
        };

        let heading = extract_heading_value(heading);
        Some((range, angle, heading))
    }

    fn validate_br24_header(header: &GenBr24Header) -> Option<(u32, SpokeBearing, Option<u16>)> {
        if header.header_len != (RADAR_LINE_HEADER_LENGTH as u8) {
            log::warn!(
                "Spoke with illegal header length ({}) ignored",
                header.header_len
            );
            return None;
        }
        if !VALID_SPOKE_STATUSES.contains(&header.status) {
            log::warn!(
                "Spoke with unknown or invalid status (0x{:x}) ignored",
                header.status
            );
            return None;
        }

        let heading = u16::from_le_bytes(header.heading);
        let angle = u16::from_le_bytes(header.angle) / 2;
        const BR24_RANGE_FACTOR: f64 = 10.0 / 1.414; // 10 m / sqrt(2)
        let range =
            ((u32::from_le_bytes(header.range) & 0xffffff) as f64 * BR24_RANGE_FACTOR) as u32;

        let heading = extract_heading_value(heading);

        Some((range, angle, heading))
    }
}
