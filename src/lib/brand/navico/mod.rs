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

mod command;
mod info;
mod protocol;
mod report;
mod settings;

pub(super) use protocol::{
    HALO_HEADING_INFO_ADDRESS, HALO_SPEED_ADDRESS_A, HALO_SPEED_ADDRESS_B, SPOKES_PER_FRAME,
    SPOKES_PER_REVOLUTION, SPOKES_RAW, SPOKE_DATA_LENGTH, SPOKE_PIXEL_LEN,
};
use protocol::{
    BR24_DISCOVERY_ADDRESS, COMMAND_SUBTYPE, DISCOVERY_QUERY_PACKET, GEN3PLUS_DISCOVERY_ADDRESS,
    RADAR_SERVICE_TYPE, REPORT_SUBTYPE, SPOKE_DATA_SUBTYPE,
};

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Model {
    Unknown,
    BR24,
    Gen3,
    Gen4,
    HALO,
    HaloOrG4,
}

const BR24_MODEL_NAME: &str = "BR24";

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Unknown => "",
            Model::BR24 => BR24_MODEL_NAME,
            Model::Gen3 => "3G",
            Model::Gen4 => "4G",
            Model::HALO => "HALO",
            Model::HaloOrG4 => "HALO or 4G",
        };
        write!(f, "{}", s)
    }
}

impl Model {
    pub fn new(s: &str) -> Self {
        match s {
            BR24_MODEL_NAME => Model::BR24,
            "3G" => Model::Gen3,
            "4G" => Model::Gen4,
            "HALO" => Model::HALO,
            _ => Model::Unknown,
        }
    }

    pub fn from(model: u8) -> Self {
        match model {
            0x0e => Model::BR24, // Davy's NorthStar BR24 from 2009
            0x0f => Model::BR24,
            0x08 => Model::Gen3,
            0x01 => Model::HaloOrG4, // New Firmware in 2025
            0x00 => Model::HALO,
            _ => Model::Unknown,
        }
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
                scanner_pairs.push((subcomponent, RadarScanner { data: d, send: s, report: r }));
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

        log::debug!(
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

            let location_info = RadarInfo::new(
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
                dual_range,
                false,
            );
            self.found(location_info, radars, subsys);
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string(&ControlId::UserName, info.key())
            .unwrap();

        if let Some(mut info) = radars.add(info) {
            // It's new, start the RadarProcessor thread

            // Load the model name afresh, it may have been modified from persisted data
            let model = match info.controls.model_name() {
                Some(s) => Model::new(&s),
                None => Model::Unknown,
            };
            if model != Model::Unknown {
                let info2 = info.clone();
                settings::update_when_model_known(&mut info.controls, model, &info2);
                info.set_doppler(model == Model::HALO);
            }
            // In replay mode, use default Navico ranges if none are set
            if info.ranges.is_empty() && self.args.is_replay() {
                info.set_ranges(default_navico_ranges());
            }
            radars.update(&mut info);

            let report_name = info.key() + " reports";

            info.start_forwarding_radar_messages_to_stdout(&subsys);

            let report_receiver =
                report::NavicoReportReceiver::new(&self.args, info, radars.clone(), model);

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

/// Default Navico ranges for replay mode (up to 24 NM / 24 km)
/// Works for BR24, 3G, 4G, and Halo radars
fn default_navico_ranges() -> Ranges {
    // Combined metric and nautical distances in meters
    let distances = vec![
        // Metric ranges
        50,    // 50 m
        75,    // 75 m
        100,   // 100 m
        250,   // 250 m
        500,   // 500 m
        750,   // 750 m
        1000,  // 1 km
        1500,  // 1.5 km
        2000,  // 2 km
        3000,  // 3 km
        4000,  // 4 km
        6000,  // 6 km
        8000,  // 8 km
        12000, // 12 km
        16000, // 16 km
        24000, // 24 km
        // Nautical ranges
        115,   // 1/16 NM
        231,   // 1/8 NM
        463,   // 1/4 NM
        926,   // 1/2 NM
        1389,  // 3/4 NM
        1852,  // 1 NM
        2778,  // 1.5 NM
        3704,  // 2 NM
        5556,  // 3 NM
        7408,  // 4 NM
        11112, // 6 NM
        14816, // 8 NM
        22224, // 12 NM
    ];

    Ranges::new_by_distance(&distances)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::deserialize;
    use crate::network::NetworkSocketAddrV4;
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
        0x01, 0xB2, 0x31, 0x34, 0x30, 0x33, 0x33, 0x30, 0x32, 0x34, 0x35, 0x32, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xA9, 0xFE, 0x18, 0xC7, 0x01, 0x01, 0x06, 0x00, 0xFD, 0xFF,
        0x20, 0x01, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xA9, 0xFE, 0x18, 0xC7, 0x17, 0x60,
        0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x17, 0x1A, 0x1C, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x18, 0x1A, 0x1D, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16, 0x11, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x09,
        0x1A, 0x17, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x0D, 0x1A, 0x01, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0E, 0x1A, 0x02,
        0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0F, 0x1A, 0x03, 0x12, 0x00, 0x20, 0x01,
        0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12, 0x1A, 0x20, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x13, 0x1A, 0x21, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x0C, 0x1A, 0x04, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D,
        0x1A, 0x05, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0E, 0x1A, 0x06,
    ];

    // Real HALO 20+ dual-range beacon (222 bytes) — serial 129848770, IP 192.168.1.10
    const BEACON_HALO20P: [u8; 222] = [
        0x01, 0xB2, 0x31, 0x32, 0x39, 0x38, 0x34, 0x38, 0x37, 0x37, 0x30, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xC0, 0xA8, 0x01, 0x0A, 0x01, 0x33, 0x06, 0x00, 0xFD, 0xFF,
        0x20, 0x01, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0C, 0x17, 0x70,
        0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x17, 0x1A, 0x1C, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x18, 0x1A, 0x1D, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16, 0x11, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x0A, 0x1A, 0x18, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x09,
        0x1A, 0x17, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x0D, 0x17, 0x71, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0E, 0x17, 0x72,
        0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x73, 0x12, 0x00, 0x20, 0x01,
        0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x12, 0x1A, 0x20, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x13, 0x1A, 0x21, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x0D, 0x17, 0x74, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0F,
        0x17, 0x75, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x0D, 0x17, 0x76,
    ];

    // Real HALO 24 dual-range beacon (222 bytes) — serial 1902501034, IP 10.56.0.24
    const BEACON_HALO24: [u8; 222] = [
        0x01, 0xB2, 0x31, 0x39, 0x30, 0x32, 0x35, 0x30, 0x31, 0x30, 0x33, 0x34, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x0A, 0x38, 0x00, 0x18, 0x31, 0x31, 0x06, 0x00, 0xFD, 0xFF,
        0x20, 0x01, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x30, 0x1B, 0x90,
        0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x16, 0x1A, 0x26, 0x1F, 0x00, 0x20, 0x01,
        0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x31, 0x1B, 0x91, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x09, 0x32, 0x1B, 0x92, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x33, 0x1B, 0x93, 0x11, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x09, 0x34, 0x1B, 0x94, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x33,
        0x1B, 0x95, 0x10, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x09, 0x35, 0x1B, 0x96, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x36, 0x1B, 0x97,
        0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x98, 0x12, 0x00, 0x20, 0x01,
        0x03, 0x00, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x33, 0x1B, 0x99, 0x11, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x09, 0x37, 0x1B, 0x9A, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x09, 0x33, 0x1B, 0x9B, 0x12, 0x00, 0x20, 0x02, 0x03, 0x00, 0x10, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x09, 0x35, 0x1B, 0x9C, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x38,
        0x1B, 0x9D, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x09, 0x35, 0x1B, 0x9E,
    ];

    #[test]
    fn parse_4g_beacon() {
        let beacon = parse_gen3plus_beacon(&BEACON_4G).unwrap();

        assert_eq!(beacon.serial_no, "1403302452");
        assert_eq!(beacon.radar_addr, SocketAddrV4::new(Ipv4Addr::new(169, 254, 24, 199), 257));
        assert_eq!(beacon.num_devices, 6);
        assert_eq!(beacon.scanners.len(), 2);

        // Radar A
        assert_eq!(beacon.scanners[0].data, SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678));
        assert_eq!(beacon.scanners[0].send, SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680));
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
        assert_eq!(beacon.radar_addr, SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 307));
        assert_eq!(beacon.num_devices, 6);
        assert_eq!(beacon.scanners.len(), 2);

        // Radar A — same multicast group as 4G for the A scanner
        assert_eq!(beacon.scanners[0].data, SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 8), 6678));
        assert_eq!(beacon.scanners[0].send, SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 10), 6680));
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
        assert_eq!(beacon.radar_addr, SocketAddrV4::new(Ipv4Addr::new(10, 56, 0, 24), 12593));
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
            let old: NavicoBeaconDual = deserialize(packet).expect(&format!("{}: bincode failed", name));
            let new = parse_gen3plus_beacon(packet).expect(&format!("{}: dynamic parse failed", name));

            let old_a_data: SocketAddrV4 = old.a.data.into();
            let old_a_send: SocketAddrV4 = old.a.send.into();
            let old_a_report: SocketAddrV4 = old.a.report.into();
            let old_b_data: SocketAddrV4 = old.b.data.into();
            let old_b_send: SocketAddrV4 = old.b.send.into();
            let old_b_report: SocketAddrV4 = old.b.report.into();

            assert_eq!(new.scanners[0].data, old_a_data, "{}: A data mismatch", name);
            assert_eq!(new.scanners[0].send, old_a_send, "{}: A send mismatch", name);
            assert_eq!(new.scanners[0].report, old_a_report, "{}: A report mismatch", name);
            assert_eq!(new.scanners[1].data, old_b_data, "{}: B data mismatch", name);
            assert_eq!(new.scanners[1].send, old_b_send, "{}: B send mismatch", name);
            assert_eq!(new.scanners[1].report, old_b_report, "{}: B report mismatch", name);
        }
    }

    // Real BR24 beacon (98 bytes) — serial 1047300043, IP 169.254.210.23
    // Extracted from radar-recordings/navico/br24/104730043/br24-full.pcap
    const BEACON_BR24: [u8; 98] = [
        0x01, 0xB2, 0x31, 0x30, 0x34, 0x37, 0x33, 0x30, 0x30, 0x30, 0x34, 0x33, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xA9, 0xFE, 0xD2, 0x17, 0x01, 0x01, 0x02, 0x00, 0x12, 0x00,
        0x20, 0x01, 0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21,
        0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x10, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x12, 0x1A, 0x20, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x12, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x09, 0x1A, 0x17, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x0A, 0x1A, 0x18, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16,
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
        0x01, 0xB2, 0x30, 0x39, 0x32, 0x34, 0x41, 0x31, 0x30, 0x37, 0x34, 0x35, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xA9, 0xFE, 0xAE, 0x7F, 0x01, 0x01, 0x02, 0x00, 0x12, 0x00,
        0x20, 0x01, 0x03, 0x00, 0x12, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x13, 0x1A, 0x21,
        0x11, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x14, 0x1A, 0x22, 0x10, 0x00, 0x00, 0x00,
        0xEC, 0x06, 0x07, 0x12, 0x1A, 0x20, 0x10, 0x00, 0x20, 0x01, 0x03, 0x00, 0x12, 0x00,
        0x00, 0x00, 0xEC, 0x06, 0x07, 0x09, 0x1A, 0x17, 0x11, 0x00, 0x00, 0x00, 0xEC, 0x06,
        0x07, 0x0A, 0x1A, 0x18, 0x10, 0x00, 0x00, 0x00, 0xEC, 0x06, 0x07, 0x08, 0x1A, 0x16,
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
