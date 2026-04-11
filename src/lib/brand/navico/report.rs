use anyhow::{Error, bail};
use bincode::deserialize;
use num_traits::FromPrimitive;
use serde::Deserialize;
use serde_json::{Number, Value};
use std::cmp::min;
use std::io;
use std::mem::transmute;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::Model;
use super::command::Command;
use super::protocol::{
    STATE_CONFIG, STATE_FEATURES, STATE_INSTALLATION, STATE_MODE, STATE_PROPERTIES, STATE_SETUP,
    VALID_SPOKE_STATUSES,
};
use super::{
    DYNAMIC_ALLOWED_CONTROLS, SPOKES_PER_FRAME, SPOKES_RAW, SPOKE_DATA_LENGTH, SPOKE_PIXEL_LEN,
};

use crate::Cli;
use crate::brand::CommandSender;
use crate::brand::navico::info::{HaloHeadingPacket, HaloNavigationPacket, Information};
use crate::brand::navico::{HALO_HEADING_INFO_ADDRESS, HaloMode};
use crate::network;
use crate::replay::RadarSocket;
use crate::radar::range::{RangeDetection, RangeDetectionResult};
use crate::radar::settings::{ControlId, ControlValue};
use crate::radar::spoke::GenericSpoke;
use crate::radar::target::MS_TO_KN;
use crate::radar::{
    BYTE_LOOKUP_LENGTH, CommonRadar, DopplerMode, Legend, Power, RadarError, RadarInfo,
    SharedRadars, SpokeBearing,
};
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

type PixelToBlobType = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

fn pixel_to_blob(legend: &Legend) -> [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] {
    let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] =
        [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
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

pub struct NavicoReportReceiver {
    common: CommonRadar,
    transmit_after_range_detection: bool,
    report_buf: Vec<u8>,
    report_socket: Option<RadarSocket>,
    info_buf: Vec<u8>,
    info_socket: Option<RadarSocket>,
    model: Model,
    command_sender: Option<Command>,
    info_sender: Option<Information>,
    range_timeout: Instant,
    info_send_timeout: Instant,
    report_request_timeout: Instant,
    reported_unknown: [bool; 256],

    // For data (spokes)
    data_buf: Vec<u8>,
    data_socket: Option<RadarSocket>,
    doppler: DopplerMode,
    pixel_to_blob: PixelToBlobType,
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

// When we are detecting ranges, we wait for 2 seconds before we send the next range
const RANGE_DETECTION_INTERVAL: Duration = Duration::from_secs(2);

// Used when we don't want to wait for something, we use now plus this
const FAR_FUTURE: Duration = Duration::from_secs(86400 * 365 * 30);

#[derive(Debug)]
#[repr(packed)]
struct StateMode {
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
struct StateConfig {
    _sub_opcode: u8,
    _category: u8,
    model: u8,      // So far: 01 = 4G and new 3G, 08 = 3G, 0E and 0F = BR24, 00 = HALO
    _u00: [u8; 31], // Lots of unknown
    hours: [u8; 4], // Hours of operation
    _u01: [u8; 20], // Lots of unknown
    firmware_date: [u8; 32], // Wide chars, assumed UTF16
    firmware_time: [u8; 32], // Wide chars, assumed UTF16
    _u02: [u8; 7],
}

impl StateConfig {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 129] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

// StateFeatures uses fixed offsets for BR24/4G/HALO radars observed in captures.
// Note: SRX firmware (2023+) uses tPackedDataParser TLV encoding for this opcode,
// with feature IDs: 2=UseModes, 3=InterferenceReject, 4=NoiseReject, 5=TargetBoost,
// 6=STCCurve, 7=BeamSharpening, 8=FastScan, 9=SidelobeGainRange,
// 10=SupportedAntennas, 11=InstrumentedRange, 12=LocalInterferenceReject.
// The TLV format uses 3-byte headers (type, unknown, length) per block.
// All captures to date use the fixed 66-byte format below.
#[derive(Debug)]
#[repr(packed)]
struct StateFeatures {
    _sub_opcode: u8,
    _category: u8,
    _u00: [u8; 4],              // 2..6
    bearing_alignment: [u8; 2], // 6..8
    _u01: [u8; 2],              // 8..10
    antenna_height: [u8; 4],    // 10..14 = Antenna height in mm (i32 LE)
    _u02: [u8; 5],              // 14..19
    accent_light: u8,           // 19 = Accent light
    _u03: [u8; 46],             // 20..66
}

impl StateFeatures {
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

#[derive(Debug)]
#[repr(packed)]
struct StateProperties68 {
    _sub_opcode: u8,
    _category: u8,
    _u00: [u8; 4],                       // 2..6
    name: [u8; 6],                       // 6..12
    _u01: [u8; 10],                      // 12..22
    antenna_forward: [u8; 4],            // 22..26 ahead offset in mm (i32 LE)
    antenna_starboard: [u8; 4],          // 26..30 starboard offset in mm (i32 LE)
    _u02: [u8; 6],                       // 30..36
    blanking: [SectorBlankingReport; 4], // 36..56
    _u03: [u8; 12],                      // 56..68
}

impl StateProperties68 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 68] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}
#[derive(Debug)]
#[repr(packed)]
struct StateProperties74 {
    _sub_opcode: u8,
    _category: u8,
    _u00: [u8; 4],                       // 2..6
    name: [u8; 6],                       // 6..12
    _u01: [u8; 10],                      // 12..22
    antenna_forward: [u8; 4],            // 22..26 ahead offset in mm (i32 LE)
    antenna_starboard: [u8; 4],          // 26..30 starboard offset in mm (i32 LE)
    _u02: [u8; 12],                      // 30..42
    blanking: [SectorBlankingReport; 4], // 42..62
    _u03: [u8; 12],                      // 62..74
}

impl StateProperties74 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 74] = bytes.try_into()?;
            transmute(report)
        })
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct StateInstallation {
    _sub_opcode: u8,            // 0  0x08
    _category: u8,              // 1  0xC4
    sea_state: u8,              // 2
    interference_rejection: u8, // 3
    scan_speed: u8,             // 4
    sls_auto: u8,               // 5 installation: sidelobe suppression auto
    _field6: u8,                // 6
    _field7: u8,                // 7
    _field8: u8,                // 8
    side_lobe_suppression: u8,  // 9 installation: sidelobe suppression
    _field10: u16,              // 10-11
    noise_rejection: u8,        // 12    noise rejection
    target_sep: u8,             // 13
    sea_clutter: u8,            // 14 sea clutter on Halo
    auto_sea_clutter: i8,       // 15 auto sea clutter on Halo
    _field13: u8,               // 16
    _field14: u8,               // 17
}

#[derive(Debug)]
#[repr(packed)]
struct StateInstallation21 {
    _base: StateInstallation,
    doppler_state: u8,
    doppler_speed: [u8; 2], // doppler speed threshold in values 0..1594 (in cm/s).
}

#[derive(Debug)]
#[repr(packed)]
struct StateInstallation32 {
    _base: StateInstallation,
    doppler_state: u8,
    doppler_speed: [u8; 2], // doppler speed threshold in values 0..1594 (in cm/s).
    w: u8,
    x: [u8; 10],
}

impl StateInstallation {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 18] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

impl StateInstallation21 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 21] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

impl StateInstallation32 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 32] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

impl NavicoReportReceiver {
    pub fn new(
        args: &Cli,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        model: Model,
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
            Some(Command::new(args.fake_errors, info.clone(), model.clone()))
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

        let pixel_to_blob = pixel_to_blob(&info.get_legend());

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
            transmit_after_range_detection: false,
            report_buf: Vec::with_capacity(1000),
            report_socket: None,
            info_buf: Vec::with_capacity(::core::mem::size_of::<HaloHeadingPacket>()),
            info_socket: None,
            model,
            command_sender,
            info_sender,
            range_timeout: now + FAR_FUTURE,
            info_send_timeout: now,
            report_request_timeout: now,
            reported_unknown: [false; 256],
            data_buf: Vec::with_capacity(size_of::<RadarFramePkt>()),
            data_socket: None,
            doppler: DopplerMode::None,
            pixel_to_blob,
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
        match network::create_udp_listen(&self.common.info.report_addr, &self.common.info.nic_addr, network::SocketType::Multicast)
        {
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
        match network::create_udp_listen(&HALO_HEADING_INFO_ADDRESS, &self.common.info.nic_addr, network::SocketType::Multicast) {
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

            let timeout = min(
                min(self.report_request_timeout, self.range_timeout),
                self.info_send_timeout,
            );

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Ok(());
                },

                _ = sleep_until(timeout) => {
                    let now = Instant::now();
                    if self.range_timeout <= now {
                        self.process_range(0).await?;
                    }
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
                    "Received {:04} spoke {}",
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
        let pixel_to_blob = &self.pixel_to_blob;

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
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        generic_spoke
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

    // If range detection is in progress, go to the next range
    async fn process_range(&mut self, range: i32) -> Result<(), RadarError> {
        let range = range / 10;

        if self.common.info.ranges.is_empty()
            && self.common.info.range_detection.is_none()
            && !self.common.replay
        {
            if let Some(status) = self.common.info.controls.get_status() {
                if status == Power::Transmit {
                    log::warn!(
                        "{}: No ranges available, but radar is transmitting, standby during range detection",
                        self.common.key
                    );
                    self.send_status(Power::Standby).await?;
                    self.transmit_after_range_detection = true;
                }
            } else {
                log::warn!(
                    "{}: No ranges available and no radar status found, cannot start range detection",
                    self.common.key
                );
                return Ok(());
            }
            if let Some(control) = self.common.info.controls.get(&ControlId::Range) {
                self.common.info.range_detection = Some(RangeDetection::new(
                    self.common.key.clone(),
                    50,
                    control.item().max_value.unwrap() as i32,
                    true,
                    true,
                ));
            }
        }

        if let Some(range_detection) = &mut self.common.info.range_detection {
            match range_detection.found_range(range) {
                RangeDetectionResult::NoRange => {
                    return Ok(());
                }
                RangeDetectionResult::Complete(ranges, saved_range) => {
                    self.common.set_ranges(ranges);
                    self.range_timeout = Instant::now() + FAR_FUTURE;

                    self.send_range(saved_range).await?;
                    if self.transmit_after_range_detection {
                        self.transmit_after_range_detection = false;
                        self.send_status(Power::Transmit).await?;
                    }
                }
                RangeDetectionResult::NextRange(r) => {
                    self.range_timeout = Instant::now() + RANGE_DETECTION_INTERVAL;

                    self.send_range(r).await?;
                }
            }
        }

        Ok(())
    }

    async fn send_status(&mut self, status: Power) -> Result<(), RadarError> {
        let status = status as i128;
        let status = Number::from_i128(status).unwrap();
        let cv = ControlValue::new(ControlId::Power, Value::Number(status));
        self.command_sender
            .as_mut()
            .unwrap() // Safe, as we only create a range detection when replay is false
            .set_control(&cv, &self.common.info.controls)
            .await?;
        Ok(())
    }

    async fn send_range(&mut self, range: i32) -> Result<(), RadarError> {
        let value = Number::from_i128(range as i128).unwrap();
        let cv: ControlValue = ControlValue::new(ControlId::Range, Value::Number(value));
        self.command_sender
            .as_mut()
            .unwrap() // Safe, as we only create a range detection when replay is false
            .set_control(&cv, &self.common.info.controls)
            .await?;
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
        match sub_opcode {
            STATE_MODE => {
                return self.process_state_mode().await;
            }
            STATE_SETUP => {
                if self.model != Model::Unknown {
                    return self.process_state_setup().await;
                }
            }
            STATE_CONFIG => {
                return self.process_state_config().await;
            }
            STATE_FEATURES => {
                return self.process_state_features().await;
            }
            STATE_PROPERTIES => {
                if self.model != Model::Unknown {
                    if data.len() == 68 {
                        return self.process_state_properties_68().await;
                    }
                    return self.process_state_properties_74().await;
                }
            }
            STATE_INSTALLATION => {
                if self.model != Model::Unknown {
                    return self.process_state_installation().await;
                }
            }
            _ => {
                if !self.reported_unknown[sub_opcode as usize] {
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
        if self.model == Model::HALO {
            if let Some(halo_mode) = HaloMode::from_i32(mode) {
                self.common.set_value(&ControlId::Mode, mode as f64);
                let allowed = halo_mode == HaloMode::Custom;
                let controls = &self.common.info.controls;
                for ct in &DYNAMIC_ALLOWED_CONTROLS {
                    controls.set_allowed(ct, allowed);
                }
            } else {
                log::error!("{}: Unsupported HALO mode {}", self.common.key, mode);
            }

            // todo!() if mode !=
        }
        self.common
            .set_value_auto(&ControlId::Gain, gain as f64, gain_auto);
        if self.model != Model::HALO {
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

        self.process_range(range).await?;

        Ok(())
    }

    async fn process_state_config(&mut self) -> Result<(), Error> {
        let report = StateConfig::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let model_raw = report.model;
        let hours = i32::from_le_bytes(report.hours);
        let firmware_date = c_wide_string(&report.firmware_date);
        let firmware_time = c_wide_string(&report.firmware_time);
        let model = Model::from(model_raw);
        match model {
            Model::Unknown => {
                if !self.reported_unknown[model_raw as usize] {
                    self.reported_unknown[model_raw as usize] = true;
                    log::error!(
                        "{}: Unknown radar model 0x{:02x}",
                        self.common.key,
                        model_raw
                    );
                }
            }
            Model::HaloOrG4 => {
                // We need to look at the length of report 8, and its content
                // to distinguish HALO from 4G.
                if self.model == Model::Unknown {
                    self.model = Model::HaloOrG4;
                }
            }
            _ => {
                if self.model != model {
                    log::debug!("{}: Radar is model {}", self.common.key, model);
                    let info2 = self.common.info.clone();
                    self.model = model;
                    super::settings::update_when_model_known(
                        &mut self.common.info.controls,
                        model,
                        &info2,
                    );
                    self.common.info.set_doppler(model == Model::HALO);

                    self.common.update();
                }
            }
        }

        let firmware = format!("{} {}", firmware_date, firmware_time);
        self.common
            .set_value(&ControlId::TransmitTime, hours as f64);
        self.common
            .set_string(&ControlId::FirmwareVersion, firmware);

        Ok(())
    }

    async fn process_state_features(&mut self) -> Result<(), Error> {
        let report = StateFeatures::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        self.common.set_value(
            &ControlId::BearingAlignment,
            i16::from_le_bytes(report.bearing_alignment) as f64,
        );
        self.common.set_value(
            &ControlId::AntennaHeight,
            i32::from_le_bytes(report.antenna_height) as f64,
        );
        if self.model == Model::HALO {
            self.common
                .set_value(&ControlId::AccentLight, report.accent_light as f64);
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 2006
    ///
    async fn process_state_properties_68(&mut self) -> Result<(), Error> {
        let report = StateProperties68::transmute(&self.report_buf)?;

        log::debug!("{}: report {:?}", self.common.key, report);

        let name = c_string(&report.name);
        self.common
            .set_string(&ControlId::ModelName, name.unwrap_or("").to_string());

        self.common.set_value(
            &ControlId::AntennaForward,
            i32::from_le_bytes(report.antenna_forward) as f64,
        );
        self.common.set_value(
            &ControlId::AntennaStarboard,
            i32::from_le_bytes(report.antenna_starboard) as f64,
        );

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

    ///
    /// Blanking (No Transmit) report as seen on HALO 24 (Firmware 2023)
    ///
    async fn process_state_properties_74(&mut self) -> Result<(), Error> {
        let report = StateProperties74::transmute(&self.report_buf)?;

        log::debug!("{}: report {:?}", self.common.key, report);

        let name = c_string(&report.name);
        // self.common.set_string(&ControlId::ModelName, name.unwrap_or("").to_string());
        log::debug!(
            "Radar name '{}' model '{}'",
            name.unwrap_or("null"),
            self.model
        );

        self.common.set_value(
            &ControlId::AntennaForward,
            i32::from_le_bytes(report.antenna_forward) as f64,
        );
        self.common.set_value(
            &ControlId::AntennaStarboard,
            i32::from_le_bytes(report.antenna_starboard) as f64,
        );

        for (i, sector) in super::BLANKING_SECTORS {
            let blanking = &report.blanking[i];
            let start_angle = i16::from_le_bytes(blanking.start_angle);
            let end_angle = i16::from_le_bytes(blanking.end_angle);
            let enabled = Some(blanking.enabled > 0);
            log::debug!(
                "no_transmit_sector_{}: {}-{} en={:?}",
                i,
                start_angle,
                end_angle,
                enabled
            );
            self.common.info.controls.set_sector(
                &sector,
                start_angle as f64,
                end_angle as f64,
                enabled,
            )?;
        }

        Ok(())
    }

    async fn process_state_installation(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

        if data.len() != size_of::<StateInstallation>()
            && data.len() != size_of::<StateInstallation21>()
            && data.len() != size_of::<StateInstallation21>() + 1
            && data.len() != size_of::<StateInstallation32>()
        {
            bail!(
                "{}: StateInstallation (0xC408) invalid length {}",
                self.common.key,
                data.len()
            );
        }

        let model = if data.len() >= size_of::<StateInstallation21>() {
            Model::HALO
        } else if self.model == Model::HaloOrG4 {
            Model::Gen4
        } else {
            self.model
        };

        if self.model != model {
            log::debug!("{}: Radar is model {}", self.common.key, model);
            let info2 = self.common.info.clone();
            self.model = model;
            super::settings::update_when_model_known(&mut self.common.info.controls, model, &info2);
            self.common.info.set_doppler(model == Model::HALO);

            self.common.update();
        }

        let report = StateInstallation::transmute(&data[0..size_of::<StateInstallation>()])?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let sea_state = report.sea_state as i32;
        let local_interference_rejection = report.interference_rejection as i32;
        let scan_speed = report.scan_speed as i32;
        let sidelobe_suppression_auto = report.sls_auto;
        let sidelobe_suppression = report.side_lobe_suppression as i32;
        let noise_reduction = report.noise_rejection as i32;
        let target_sep = report.target_sep as i32;
        let sea_clutter = report.sea_clutter as i32;
        let auto_sea_clutter = report.auto_sea_clutter;

        // There are reports of size 21, but also 22. HALO new firmware sends 22. The last byte content is unknown.
        if data.len() >= size_of::<StateInstallation32>() {
            let report = StateInstallation32::transmute(&data[0..size_of::<StateInstallation32>()])?;

            log::debug!("{}: report {:?}", self.common.key, report);

            let doppler_speed = u16::from_le_bytes(report.doppler_speed);
            let doppler_state = report.doppler_state;

            let doppler_mode: Result<DopplerMode, _> = doppler_state.try_into();
            match doppler_mode {
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.common.key,
                        report.doppler_state
                    );
                }
                Ok(doppler_mode) => {
                    log::debug!(
                        "{}: doppler mode={} speed={}",
                        self.common.key,
                        doppler_mode,
                        doppler_speed
                    );
                    self.doppler = doppler_mode;
                }
            }
            self.common
                .set_value(&ControlId::Doppler, doppler_state as f64);
            self.common
                .set_value(&ControlId::DopplerSpeedThreshold, doppler_speed as f64);

            log::debug!(
                "{}: report w={} x={:?}",
                self.common.key,
                report.w,
                report.x,
            );
        } else if data.len() >= size_of::<StateInstallation21>() {
            let report = StateInstallation21::transmute(&data[0..size_of::<StateInstallation21>()])?;

            log::trace!("{}: report {:?}", self.common.key, report);

            let doppler_speed = u16::from_le_bytes(report.doppler_speed);
            let doppler_state = report.doppler_state;

            let doppler_mode: Result<DopplerMode, _> = doppler_state.try_into();
            match doppler_mode {
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.common.key,
                        report.doppler_state
                    );
                }
                Ok(doppler_mode) => {
                    log::debug!(
                        "{}: doppler mode={} speed={}",
                        self.common.key,
                        doppler_mode,
                        doppler_speed
                    );
                    self.doppler = doppler_mode;
                }
            }
            self.common
                .set_value(&ControlId::Doppler, doppler_state as f64);
            self.common
                .set_value(&ControlId::DopplerSpeedThreshold, doppler_speed as f64);
        }

        if self.model == Model::HALO {
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
            sidelobe_suppression_auto,
        );
        self.common
            .set_value(&ControlId::NoiseRejection, noise_reduction as f64);
        if self.model == Model::HALO || self.model == Model::Gen4 {
            self.common
                .set_value(&ControlId::TargetSeparation, target_sep as f64);
        } else if target_sep > 0 {
            log::trace!(
                "{}: Target separation value {} not supported on model {}",
                self.common.key,
                target_sep,
                self.model
            );
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
            log::warn!("Spoke with unknown or invalid status (0x{:x}) ignored", header.status);
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
            log::warn!("Spoke with unknown or invalid status (0x{:x}) ignored", header.status);
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
