use num_derive::{FromPrimitive, ToPrimitive};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::{fmt, io};
use strum::VariantNames;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::radar::range::Ranges;
use crate::radar::settings::ControlId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::PrintableSlice;
use crate::util::c_string;
use crate::{Brand, Cli};

use super::{LocatorId, RadarLocator};

pub(crate) mod capabilities;
mod command;
mod info;
mod protocol;
mod report;
mod settings;

use protocol::{
    BR24_DISCOVERY_ADDRESS, COMMAND_SUBTYPE, DISCOVERY_QUERY_PACKET, GEN3PLUS_DISCOVERY_ADDRESS,
    RADAR_SERVICE_TYPE, REPORT_SUBTYPE, SPOKE_DATA_SUBTYPE,
};
pub(super) use protocol::{
    HALO_HEADING_INFO_ADDRESS, HALO_SPEED_ADDRESS_A, HALO_SPEED_ADDRESS_B, SPOKE_DATA_LENGTH,
    SPOKE_PIXEL_LEN, SPOKES_PER_FRAME, SPOKES_PER_REVOLUTION, SPOKES_RAW,
};

#[derive(Copy, Clone, PartialEq, Debug)]
#[repr(u32)]
pub enum Model {
    Unknown = 9,
    BR24 = 10,       // Broadband Radar 24 (FMCW dome)
    Gen3 = 12,       // Broadband 3G (FMCW dome)
    Gen4 = 13,       // Broadband 4G (FMCW dome, multi-range)
    Halo = 14,       // HALO generic (PComp, multi-range)
    Halo24 = 16,     // HALO 24" dome (PComp, multi-range)
    Halo20 = 17,     // HALO 20" dome (PComp, NO multi-range)
    Halo20Plus = 18, // HALO 20+ dome (PComp, multi-range)
    Halo2000 = 19,   // HALO 2000 open array
    Halo3000 = 20,   // HALO 3000 open array
    Halo5000 = 21,   // HALO 5000 open array
    Halo4000 = 22,   // HALO 4000 open array
    Halo6000 = 23,   // HALO 6000 open array
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Unknown => "Unknown",
            Model::BR24 => "BR24",
            Model::Gen3 => "3G",
            Model::Gen4 => "4G",
            Model::Halo => "HALO",
            Model::Halo24 => "HALO24",
            Model::Halo20 => "HALO20",
            Model::Halo20Plus => "HALO20+",
            Model::Halo2000 => "HALO2000",
            Model::Halo3000 => "HALO3000",
            Model::Halo5000 => "HALO5000",
            Model::Halo4000 => "HALO4000",
            Model::Halo6000 => "HALO6000",
        };
        write!(f, "{}", s)
    }
}

impl Model {
    /// Parse from a persisted model name string.
    #[allow(dead_code)]
    pub fn new(s: &str) -> Self {
        match s {
            "BR24" => Model::BR24,
            "3G" => Model::Gen3,
            "4G" => Model::Gen4,
            "HALO24" => Model::Halo24,
            "HALO20+" => Model::Halo20Plus,
            "HALO20" => Model::Halo20,
            "HALO2000" => Model::Halo2000,
            "HALO3000" => Model::Halo3000,
            "HALO4000" => Model::Halo4000,
            "HALO5000" => Model::Halo5000,
            "HALO6000" => Model::Halo6000,
            _ if s.starts_with("HALO") || s.starts_with("Halo") => Model::Halo,
            _ => Model::Unknown,
        }
    }

    /// Map from the eScannerType u32 in the 0xC403 StateProperties packet.
    pub fn from_scanner_type(scanner_type: u32) -> Self {
        match scanner_type {
            10 => Model::BR24,
            12 => Model::Gen3,
            13 => Model::Gen4,
            14 => Model::Halo,
            16 => Model::Halo24,
            17 => Model::Halo20,
            18 => Model::Halo20Plus,
            19 => Model::Halo2000,
            20 => Model::Halo3000,
            21 => Model::Halo5000,
            22 => Model::Halo4000,
            23 => Model::Halo6000,
            _ => Model::Unknown,
        }
    }

    /// Returns true for all HALO variants (dome and open array).
    pub fn is_halo(&self) -> bool {
        let v = *self as u32;
        v >= 14 && v <= 23
    }
}

#[derive(PartialEq, FromPrimitive, ToPrimitive, VariantNames)]
enum HaloMode {
    Custom = 0,
    Harbor = 1,
    Offshore = 2,
    Buoy = 3,
    Weather = 4,
    Bird = 5,
    #[strum(serialize = "Bird+")]
    BirdPlus = 6,
}

/// There are some controls that turn read-only when the HaloMode is not Custom
const DYNAMIC_ALLOWED_CONTROLS: [ControlId; 5] = [
    ControlId::NoiseRejection,
    ControlId::TargetExpansion,
    ControlId::TargetSeparation,
    ControlId::LocalInterferenceRejection,
    ControlId::ScanSpeed,
];

#[derive(Debug, PartialEq)]
struct RadarScanner {
    data: SocketAddrV4,
    send: SocketAddrV4,
    report: SocketAddrV4,
}

#[derive(Debug)]
#[allow(dead_code)]
struct Gen3PlusBeacon<'a> {
    serial_no: &'a str,
    radar_addr: SocketAddrV4,
    num_devices: u16,
    scanners: Vec<RadarScanner>,
}

/// Parse a Gen3+ beacon packet (3G, 4G, HALO) using the dynamic device/service
/// format instead of fixed-size structs. The packet layout is:
///
///   [opcode: 2] [serial: 16] [radar_addr: 6] [num_devices: 2]
///   For each device group:
///     [service_type: 2] [reserved: 1] [subcomponent: 1] [num_services: 2]
///     For each service entry:
///       [subtype: 2] [unknown: 2] [ip: 4] [port: 2]
fn parse_gen3plus_beacon(data: &[u8]) -> Option<Gen3PlusBeacon<'_>> {
    if data.len() < 26 {
        return None;
    }

    let serial_no = c_string(&data[2..18])?;
    let radar_ip = Ipv4Addr::new(data[18], data[19], data[20], data[21]);
    let radar_port = u16::from_be_bytes([data[22], data[23]]);
    let radar_addr = SocketAddrV4::new(radar_ip, radar_port);
    let num_devices = u16::from_le_bytes([data[24], data[25]]);

    let mut offset = 26;
    let mut scanner_pairs: Vec<(u8, RadarScanner)> = Vec::new();

    for _ in 0..num_devices {
        if offset + 6 > data.len() {
            break;
        }
        let service_type = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let subcomponent = data[offset + 3];
        let num_services = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
        offset += 6;

        let services_len = num_services * 10;
        if offset + services_len > data.len() {
            break;
        }

        if service_type == RADAR_SERVICE_TYPE {
            let mut data_addr = None;
            let mut send_addr = None;
            let mut report_addr = None;

            for _ in 0..num_services {
                let subtype = u16::from_le_bytes([data[offset], data[offset + 1]]);
                let ip = Ipv4Addr::new(
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                );
                let port = u16::from_be_bytes([data[offset + 8], data[offset + 9]]);
                let addr = SocketAddrV4::new(ip, port);

                match subtype {
                    SPOKE_DATA_SUBTYPE => data_addr = Some(addr),
                    COMMAND_SUBTYPE => send_addr = Some(addr),
                    REPORT_SUBTYPE => report_addr = Some(addr),
                    _ => {}
                }
                offset += 10;
            }

            if let (Some(d), Some(s), Some(r)) = (data_addr, send_addr, report_addr) {
                scanner_pairs.push((
                    subcomponent,
                    RadarScanner {
                        data: d,
                        send: s,
                        report: r,
                    },
                ));
            }
        } else {
            offset += services_len;
        }
    }

    // Sort by subcomponent so A (0x01) comes before B (0x02)
    scanner_pairs.sort_by_key(|(sub, _)| *sub);
    let scanners = scanner_pairs.into_iter().map(|(_, s)| s).collect();

    Some(Gen3PlusBeacon {
        serial_no,
        radar_addr,
        num_devices,
        scanners,
    })
}

#[derive(Clone)]
struct NavicoLocator {
    args: Cli,
}

impl NavicoLocator {
    fn process_locator_report(
        &self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < 2 {
            return Ok(());
        }

        log::trace!(
            "{}: Navico report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        log::trace!("{}: printable:     {}", from, PrintableSlice::new(report));

        if report == DISCOVERY_QUERY_PACKET {
            log::trace!("Radar address request packet from {}", from);
            return Ok(());
        }
        if report[0] == 0x1 && report[1] == 0xB2 {
            // Common Navico message

            return self.process_beacon_report(report, from, via, radars, subsys);
        }
        Ok(())
    }

    fn process_beacon_report(
        &self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        if radars.is_radar_active_by_addr(&Brand::Navico, from) {
            log::debug!("{}: already active Navico radar", from);
            return Ok(());
        }

        // All Navico beacons (BR24, 3G, 4G, HALO) use the same dynamic
        // device/service format — parse with the unified parser.
        let beacon = match parse_gen3plus_beacon(report) {
            Some(b) => b,
            None => {
                log::debug!(
                    "{} via {}: Incomplete Gen3+ beacon, length {}",
                    from,
                    via,
                    report.len()
                );
                return Ok(());
            }
        };

        if beacon.scanners.is_empty() {
            log::debug!(
                "{} via {}: Gen3+ beacon has no radar scanners (serial {})",
                from,
                via,
                beacon.serial_no
            );
            return Ok(());
        }

        let dual_range = beacon.scanners.len() > 1;
        let scanner_names: &[&str] = if dual_range {
            &["A", "B", "C", "D"]
        } else {
            &[""]
        };

        log::info!(
            "{} via {}: Gen3+ beacon serial {} with {} scanner(s)",
            from,
            via,
            beacon.serial_no,
            beacon.scanners.len()
        );

        for (i, scanner) in beacon.scanners.iter().enumerate() {
            let suffix = if dual_range {
                Some(scanner_names.get(i).copied().unwrap_or("?"))
            } else {
                None
            };

            let mut location_info = RadarInfo::new(
                radars,
                &self.args,
                Brand::Navico,
                Some(beacon.serial_no),
                suffix,
                16,
                SPOKES_PER_REVOLUTION,
                SPOKE_PIXEL_LEN,
                (*from).into(),
                via.clone(),
                scanner.data.into(),
                scanner.report.into(),
                scanner.send.into(),
                |id, tx| settings::new(id, tx, &self.args, None),
                false, // Doppler unknown at beacon time; set from capabilities later
                false,
            );
            location_info.dual_range = dual_range;
            self.found(location_info, radars, subsys);
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string(&ControlId::UserName, info.key())
            .unwrap();

        if let Some(mut info) = radars.add(info) {
            // It's new, start the RadarProcessor thread.
            // Model and ranges are determined from 0xC403/0xC409 reports;
            // persistence may have already restored them via radars.add().

            // If the persisted model name can't be parsed to a specific model
            // (e.g. stale "HALO" from before scanner_type detection), discard
            // ranges so they get re-determined from 0xC403/0xC409.
            if let Some(name) = info.controls.model_name() {
                let model = Model::new(&name);
                if model == Model::Unknown || model == Model::Halo {
                    info.set_ranges(Ranges::empty());
                    radars.update(&mut info);
                }
            }

            let report_name = info.key() + " reports";

            info.start_forwarding_radar_messages_to_stdout(&subsys);

            let report_receiver =
                report::NavicoReportReceiver::new(&self.args, info, radars.clone());

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

impl RadarLocator for NavicoLocator {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        self.process_locator_report(message, from, nic_addr, radars, subsys)
    }

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(NavicoLocator {
            args: self.args.clone(),
        }) // Navico is stateless
    }
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Gen3Plus) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !args.is_replay() {
            beacon_request_packets.push(&DISCOVERY_QUERY_PACKET);
        };
        addresses.push(LocatorAddress::new(
            LocatorId::Gen3Plus,
            &GEN3PLUS_DISCOVERY_ADDRESS,
            Brand::Navico,
            beacon_request_packets,
            Box::new(NavicoLocator { args: args.clone() }),
        ));
    }

    if !addresses.iter().any(|i| i.id == LocatorId::GenBR24) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !args.is_replay() {
            beacon_request_packets.push(&DISCOVERY_QUERY_PACKET);
        };
        addresses.push(LocatorAddress::new(
            LocatorId::GenBR24,
            &BR24_DISCOVERY_ADDRESS,
            Brand::Navico,
            beacon_request_packets,
            Box::new(NavicoLocator { args: args.clone() }),
        ));
    }
}

const BLANKING_SECTORS: [(usize, ControlId); 4] = [
    (0, ControlId::NoTransmitSector1),
    (1, ControlId::NoTransmitSector2),
    (2, ControlId::NoTransmitSector3),
    (3, ControlId::NoTransmitSector4),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkSocketAddrV4;
    use bincode::deserialize;
    use serde::Deserialize as TestDeserialize;
    use std::net::{Ipv4Addr, SocketAddrV4};

    // Old fixed-size beacon structs, retained here to verify the dynamic parser
    // produces identical results. These were the production structs before the
    // switch to dynamic parsing.

    #[derive(TestDeserialize, Debug, Copy, Clone)]
    #[repr(packed)]
    struct NavicoBeaconHeader {
        _id: u16,
        _serial_no: [u8; 16],
        _radar_addr: NetworkSocketAddrV4,
        _filler1: [u8; 12],
        _addr1: NetworkSocketAddrV4,
        _filler2: [u8; 4],
        _addr2: NetworkSocketAddrV4,
        _filler3: [u8; 10],
        _addr3: NetworkSocketAddrV4,
        _filler4: [u8; 4],
        _addr4: NetworkSocketAddrV4,
    }

    #[derive(TestDeserialize, Debug, Copy, Clone)]
    #[repr(packed)]
    struct NavicoBeaconRadar {
        _filler1: [u8; 10],
        data: NetworkSocketAddrV4,
        _filler2: [u8; 4],
        send: NetworkSocketAddrV4,
        _filler3: [u8; 4],
        report: NetworkSocketAddrV4,
    }

    #[derive(TestDeserialize, Debug, Copy, Clone)]
    #[repr(packed)]
    struct NavicoBeaconDual {
        _header: NavicoBeaconHeader,
        a: NavicoBeaconRadar,
        b: NavicoBeaconRadar,
    }

    // Real 4G dual-range beacon (222 bytes) — serial 1403302452, IP 169.254.24.199
    const BEACON_4G: [u8; 222] = [
        0x01, 0xB2, 0x31, 0x34, 0x30, 0x33, 0x33, 0x30, 0x32, 0x34, 0x35, 0x32, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xA9, 0xFE, 0x18, 0xC7, 0x01, 0x01, 0x06, 0x00, 0xFD, 0xFF, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xA9, 0xFE, 0x18, 0xC7, 0x17, 0x60, 0x11, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01, 0x02, 0x00, 0x10, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x17, 0x1A, 0x1C, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07,
        0x18, 0x1A, 0x1D, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x08, 0x1A, 0x16, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x12,
        0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x09, 0x1A, 0x17, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x1A, 0x01, 0x11, 0x00, 0x00, 0x00, 0xEC,
        0x06, 0x07, 0x0E, 0x1A, 0x02, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0F, 0x1A, 0x03,
        0x12, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12, 0x1A,
        0x20, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x12, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x0C, 0x1A, 0x04, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D,
        0x1A, 0x05, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0E, 0x1A, 0x06,
    ];

    // Real HALO 20+ dual-range beacon (222 bytes) — serial 129848770, IP 192.168.1.10
    const BEACON_HALO20P: [u8; 222] = [
        0x01, 0xB2, 0x31, 0x32, 0x39, 0x38, 0x34, 0x38, 0x37, 0x37, 0x30, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xC0, 0xA8, 0x01, 0x0A, 0x01, 0x33, 0x06, 0x00, 0xFD, 0xFF, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0C, 0x17, 0x70, 0x11, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01, 0x02, 0x00, 0x10, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x17, 0x1A, 0x1C, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07,
        0x18, 0x1A, 0x1D, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x08, 0x1A, 0x16, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x12,
        0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x09, 0x1A, 0x17, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x71, 0x11, 0x00, 0x00, 0x00, 0xEC,
        0x06, 0x07, 0x0E, 0x17, 0x72, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x73,
        0x12, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12, 0x1A,
        0x20, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x12, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x74, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0F,
        0x17, 0x75, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x76,
    ];

    // Real HALO 24 dual-range beacon (222 bytes) — serial 1902501034, IP 10.56.0.24
    const BEACON_HALO24: [u8; 222] = [
        0x01, 0xB2, 0x31, 0x39, 0x30, 0x32, 0x35, 0x30, 0x31, 0x30, 0x33, 0x34, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x0A, 0x38, 0x00, 0x18, 0x31, 0x31, 0x06, 0x00, 0xFD, 0xFF, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x30, 0x1B, 0x90, 0x11, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01, 0x02, 0x00, 0x10, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x09, 0x31, 0x1B, 0x91, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09,
        0x32, 0x1B, 0x92, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x09, 0x33, 0x1B, 0x93, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x34, 0x1B, 0x94, 0x12,
        0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x33, 0x1B, 0x95, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x96, 0x11, 0x00, 0x00, 0x00, 0xEC,
        0x06, 0x09, 0x36, 0x1B, 0x97, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x98,
        0x12, 0x00, 0x20, 0x01, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x33, 0x1B,
        0x99, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x37, 0x1B, 0x9A, 0x12, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x09, 0x33, 0x1B, 0x9B, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x9C, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x38,
        0x1B, 0x9D, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x9E,
    ];

    #[test]
    fn parse_4g_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_4G).unwrap();

        assert_eq!(beacon.serial_no, "1403302452");
        assert_eq!(
            beacon.radar_addr,
            SocketAddrV4::new(Ipv4Addr::new(169, 254, 24, 199), 257)
        );
        assert_eq!(beacon.num_devices, 6);
        assert_eq!(beacon.scanners.len(), 2);

        // Radar A
        assert_eq!(
            beacon.scanners[0].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678)
        );
        assert_eq!(
            beacon.scanners[0].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680)
        );
        assert_eq!(
            beacon.scanners[0].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 9), 6679)
        );

        // Radar B
        assert_eq!(
            beacon.scanners[1].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 13), 6657)
        );
        assert_eq!(
            beacon.scanners[1].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 14), 6658)
        );
        assert_eq!(
            beacon.scanners[1].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 15), 6659)
        );
    }

    #[test]
    fn parse_halo20p_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_HALO20P).unwrap();

        assert_eq!(beacon.serial_no, "129848770");
        assert_eq!(
            beacon.radar_addr,
            SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 307)
        );
        assert_eq!(beacon.num_devices, 6);
        assert_eq!(beacon.scanners.len(), 2);

        // Radar A — same multicast group as 4G for the A scanner
        assert_eq!(
            beacon.scanners[0].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678)
        );
        assert_eq!(
            beacon.scanners[0].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680)
        );
        assert_eq!(
            beacon.scanners[0].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 9), 6679)
        );

        // Radar B — HALO 20+ uses different ports for B than 4G
        assert_eq!(
            beacon.scanners[1].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 13), 6001)
        );
        assert_eq!(
            beacon.scanners[1].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 14), 6002)
        );
        assert_eq!(
            beacon.scanners[1].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 13), 6003)
        );
    }

    #[test]
    fn parse_halo24_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_HALO24).unwrap();

        assert_eq!(beacon.serial_no, "1902501034");
        assert_eq!(
            beacon.radar_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 56, 0, 24), 12593)
        );
        assert_eq!(beacon.num_devices, 6);
        assert_eq!(beacon.scanners.len(), 2);

        // Radar A — HALO 24 uses 236.6.9.x subnet and 7000+ ports
        assert_eq!(
            beacon.scanners[0].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 51), 7059)
        );
        assert_eq!(
            beacon.scanners[0].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 52), 7060)
        );
        assert_eq!(
            beacon.scanners[0].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 51), 7061)
        );

        // Radar B
        assert_eq!(
            beacon.scanners[1].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 53), 7062)
        );
        assert_eq!(
            beacon.scanners[1].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 54), 7063)
        );
        assert_eq!(
            beacon.scanners[1].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 9, 53), 7064)
        );
    }

    /// Verify the dynamic parser extracts the same radar addresses as the old
    /// fixed-struct bincode approach for all three captured beacons.
    #[test]
    fn dynamic_parser_matches_fixed_structs() {
        for (name, packet) in [
            ("4G", &BEACON_4G[..]),
            ("HALO 20+", &BEACON_HALO20P[..]),
            ("HALO 24", &BEACON_HALO24[..]),
        ] {
            let old: NavicoBeaconDual =
                deserialize(packet).expect(&format!("{}: bincode failed", name));
            let new =
                parse_gen3plus_beacon(packet).expect(&format!("{}: dynamic parse failed", name));

            let old_a_data: SocketAddrV4 = old.a.data.into();
            let old_a_send: SocketAddrV4 = old.a.send.into();
            let old_a_report: SocketAddrV4 = old.a.report.into();
            let old_b_data: SocketAddrV4 = old.b.data.into();
            let old_b_send: SocketAddrV4 = old.b.send.into();
            let old_b_report: SocketAddrV4 = old.b.report.into();

            assert_eq!(
                new.scanners[0].data, old_a_data,
                "{}: A data mismatch",
                name
            );
            assert_eq!(
                new.scanners[0].send, old_a_send,
                "{}: A send mismatch",
                name
            );
            assert_eq!(
                new.scanners[0].report, old_a_report,
                "{}: A report mismatch",
                name
            );
            assert_eq!(
                new.scanners[1].data, old_b_data,
                "{}: B data mismatch",
                name
            );
            assert_eq!(
                new.scanners[1].send, old_b_send,
                "{}: B send mismatch",
                name
            );
            assert_eq!(
                new.scanners[1].report, old_b_report,
                "{}: B report mismatch",
                name
            );
        }
    }

    // Real BR24 beacon (98 bytes) — serial 1047300043, IP 169.254.210.23
    // Extracted from radar-recordings/navico/br24/104730043/br24-full.pcap
    const BEACON_BR24: [u8; 98] = [
        0x01, 0xB2, 0x31, 0x30, 0x34, 0x37, 0x33, 0x30, 0x30, 0x30, 0x34, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xA9, 0xFE, 0xD2, 0x17, 0x01, 0x01, 0x02, 0x00, 0x12, 0x00, 0x20, 0x01,
        0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21, 0x11, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12,
        0x1A, 0x20, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07,
        0x09, 0x1A, 0x17, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x10, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16,
    ];

    #[test]
    fn parse_br24_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_BR24).unwrap();

        assert_eq!(beacon.serial_no, "1047300043");
        assert_eq!(
            beacon.radar_addr,
            SocketAddrV4::new(Ipv4Addr::new(169, 254, 210, 23), 257)
        );
        assert_eq!(beacon.num_devices, 2);
        assert_eq!(beacon.scanners.len(), 1);

        // Single scanner — service_type 0x0010 is the radar
        assert_eq!(
            beacon.scanners[0].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678)
        );
        assert_eq!(
            beacon.scanners[0].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680)
        );
        assert_eq!(
            beacon.scanners[0].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 9), 6679)
        );
    }

    // Real BR24 NorthStar beacon (98 bytes) — serial 0924A10745, IP 169.254.174.127
    // Extracted from radar-recordings/navico/br24/northstar/br24_davy.pcapng
    const BEACON_BR24_NORTHSTAR: [u8; 98] = [
        0x01, 0xB2, 0x30, 0x39, 0x32, 0x34, 0x41, 0x31, 0x30, 0x37, 0x34, 0x35, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xA9, 0xFE, 0xAE, 0x7F, 0x01, 0x01, 0x02, 0x00, 0x12, 0x00, 0x20, 0x01,
        0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21, 0x11, 0x00, 0x00,
        0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12,
        0x1A, 0x20, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07,
        0x09, 0x1A, 0x17, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x10, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16,
    ];

    #[test]
    fn parse_br24_northstar_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_BR24_NORTHSTAR).unwrap();

        assert_eq!(beacon.serial_no, "0924A10745");
        assert_eq!(
            beacon.radar_addr,
            SocketAddrV4::new(Ipv4Addr::new(169, 254, 174, 127), 257)
        );
        assert_eq!(beacon.num_devices, 2);
        assert_eq!(beacon.scanners.len(), 1);

        // Same service addresses as the other BR24 — all BR24s use the same multicast group
        assert_eq!(
            beacon.scanners[0].data,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678)
        );
        assert_eq!(
            beacon.scanners[0].send,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680)
        );
        assert_eq!(
            beacon.scanners[0].report,
            SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 9), 6679)
        );
    }

    #[test]
    fn parse_truncated_beacon_returns_none() {
        // Too short for header
        assert!(parse_gen3plus_beacon(&BEACON_4G[..25]).is_none());
        // Empty
        assert!(parse_gen3plus_beacon(&[]).is_none());
    }

    #[test]
    fn parse_truncated_services_returns_partial() {
        // Truncate mid-way through device 3 (radar A) — should still parse
        // the non-radar device groups but find no radar scanners
        let beacon = parse_gen3plus_beacon(&BEACON_4G[..80]);
        assert!(beacon.is_some());
        let beacon = beacon.unwrap();
        assert_eq!(beacon.serial_no, "1403302452");
        // May have 0 scanners since the radar device entry is incomplete
    }
}
