use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::{Brand, Session};

mod data;
mod info;
mod report;
mod settings;

// Re-export core's Model type for compatibility
pub use mayara_core::protocol::navico::Model;

// Use constants from core (single source of truth)
use mayara_core::protocol::navico::{
    MAX_SPOKE_LEN as NAVICO_SPOKE_LEN_U16, SPOKES_PER_FRAME,
    SPOKES_PER_REVOLUTION as NAVICO_SPOKES_U16,
};

const NAVICO_SPOKES: usize = NAVICO_SPOKES_U16 as usize;
const NAVICO_SPOKE_LEN: usize = NAVICO_SPOKE_LEN_U16 as usize;

/// Parse a socket address string "ip:port" into SocketAddrV4
fn parse_socket_addr(s: &str) -> Option<SocketAddrV4> {
    let colon_pos = s.rfind(':')?;
    let ip_str = &s[..colon_pos];
    let port_str = &s[colon_pos + 1..];
    let ip: Ipv4Addr = ip_str.parse().ok()?;
    let port: u16 = port_str.parse().ok()?;
    Some(SocketAddrV4::new(ip, port))
}

// Spoke numbers go from [0..4096>, but only half of them are used.
// The actual image is 2048 x 1024 x 4 bits
const NAVICO_BITS_PER_PIXEL: usize = BITS_PER_NIBBLE;
const BITS_PER_BYTE: usize = 8;
const BITS_PER_NIBBLE: usize = 4;
const NAVICO_PIXELS_PER_BYTE: usize = BITS_PER_BYTE / NAVICO_BITS_PER_PIXEL;
const RADAR_LINE_DATA_LENGTH: usize = NAVICO_SPOKE_LEN / NAVICO_PIXELS_PER_BYTE;

// deprecated_marked_for_delete: Only used by legacy locator
// const NAVICO_BEACON_ADDRESS: SocketAddr =
//     SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 5)), 6878);

/* NAVICO API SPOKES */
/*
 * Data coming from radar is always 4 bits, packed two per byte.
 * The values 14 and 15 may be special depending on DopplerMode (only on HALO).
 *
 * To support targets, target trails and doppler we map those values 0..15 to
 * a
 */

/*
RADAR REPORTS

The radars send various reports. The first 2 bytes indicate what the report type is.
The types seen on a BR24 are:

2nd byte C4:   01 02 03 04 05 07 08
2nd byte F5:   08 0C 0D 0F 10 11 12 13 14

Not definitive list for
4G radars only send the C4 data.
*/

// deprecated_marked_for_delete: Only used by legacy locator
// const NAVICO_ADDRESS_REQUEST_PACKET: [u8; 2] = [0x01, 0xB1];

// deprecated_marked_for_delete: BR24 beacon comes from a different multicast address
// const NAVICO_BR24_BEACON_ADDRESS: SocketAddr =
//     SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 4)), 6768);

// =============================================================================
// DEPRECATED LEGACY CODE - COMMENTED OUT FOR BUILD VERIFICATION
// =============================================================================
// The following code has been replaced by CoreLocatorAdapter + process_discovery()
// Keeping as comments to verify nothing references it. Delete after verification.
// =============================================================================

/*
// deprecated_marked_for_delete: Legacy locator state - use process_discovery() instead
#[derive(Clone)]
struct NavicoLocatorState {
    session: Session,
}

impl NavicoLocatorState {
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

        if report == NAVICO_ADDRESS_REQUEST_PACKET {
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
        // Use core parsing for beacon
        use mayara_core::protocol::navico::parse_beacon_endpoints;

        let beacon = match parse_beacon_endpoints(report) {
            Ok(b) => b,
            Err(e) => {
                log::debug!(
                    "{} via {}: Failed to parse beacon: {}",
                    from,
                    via,
                    e
                );
                return Ok(());
            }
        };

        log::debug!("{} via {}: Beacon parsed: {:?}", from, via, beacon);

        let locator_id = if beacon.is_br24 {
            LocatorId::GenBR24
        } else {
            LocatorId::Gen3Plus
        };

        let model_name = if beacon.is_br24 {
            Some(BR24_MODEL_NAME)
        } else {
            None
        };

        // Parse radar address (must be IPv4)
        let radar_addr: SocketAddrV4 = beacon.radar_addr.parse().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Invalid radar addr: {}", e))
        })?;

        for radar_endpoint in beacon.radars {
            let data_addr: SocketAddrV4 = radar_endpoint.data_addr.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("Invalid data addr: {}", e))
            })?;
            let report_addr: SocketAddrV4 = radar_endpoint.report_addr.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("Invalid report addr: {}", e))
            })?;
            let send_addr: SocketAddrV4 = radar_endpoint.send_addr.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("Invalid send addr: {}", e))
            })?;

            let location_info: RadarInfo = RadarInfo::new(
                self.session.clone(),
                locator_id,
                Brand::Navico,
                Some(&beacon.serial_no),
                radar_endpoint.suffix.as_deref(),
                16,
                NAVICO_SPOKES,
                NAVICO_SPOKE_LEN,
                radar_addr,
                via.clone(),
                data_addr,
                report_addr,
                send_addr,
                settings::new(self.session.clone(), model_name),
                beacon.is_dual_range,
            );
            self.found(location_info, radars, subsys);
        }

        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string("userName", info.key())
            .unwrap();

        if let Some(mut info) = radars.located(info) {
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
                radars.update(&info);
            }

            let data_name = info.key() + " data";
            let report_name = info.key() + " reports";
            let info_clone = info.clone();

            if self.session.read().unwrap().args.output {
                let info_clone2 = info.clone();

                subsys.start(SubsystemBuilder::new("stdout", move |s| {
                    info_clone2.forward_output(s)
                }));
            }

            let data_receiver = data::NavicoDataReceiver::new(&self.session, info);
            let report_receiver = report::NavicoReportReceiver::new(
                self.session.clone(),
                info_clone,
                radars.clone(),
                model,
            );

            subsys.start(SubsystemBuilder::new(
                data_name,
                move |s: SubsystemHandle| data_receiver.run(s),
            ));
            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

// deprecated_marked_for_delete: Legacy RadarLocatorState implementation
impl RadarLocatorState for NavicoLocatorState {
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

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(NavicoLocatorState {
            session: self.session.clone(),
        }) // Navico is stateless
    }
}

// deprecated_marked_for_delete: Legacy NavicoLocator - use CoreLocatorAdapter instead
#[derive(Clone)]
struct NavicoLocator {
    session: Session,
}

// deprecated_marked_for_delete: Legacy RadarLocator implementation
impl RadarLocator for NavicoLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !self.session.read().unwrap().args.replay {
            beacon_request_packets.push(&NAVICO_ADDRESS_REQUEST_PACKET);
        };
        if !addresses.iter().any(|i| i.id == LocatorId::Gen3Plus) {
            addresses.push(LocatorAddress::new(
                LocatorId::Gen3Plus,
                &NAVICO_BEACON_ADDRESS,
                Brand::Navico,
                beacon_request_packets,
                Box::new(NavicoLocatorState {
                    session: self.session.clone(),
                }),
            ));
        }
    }
}

/// deprecated_marked_for_delete: Use CoreLocatorAdapter with process_discovery() instead
pub fn create_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = NavicoLocator { session };
    Box::new(locator)
}

// deprecated_marked_for_delete: Legacy NavicoBR24Locator - use CoreLocatorAdapter instead
#[derive(Clone)]
struct NavicoBR24Locator {
    session: Session,
}

// deprecated_marked_for_delete: Legacy RadarLocator implementation
impl RadarLocator for NavicoBR24Locator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::GenBR24) {
            addresses.push(LocatorAddress::new(
                LocatorId::GenBR24,
                &NAVICO_BR24_BEACON_ADDRESS,
                Brand::Navico,
                vec![&NAVICO_ADDRESS_REQUEST_PACKET],
                Box::new(NavicoLocatorState {
                    session: self.session.clone(),
                }),
            ));
        }
    }
}

/// deprecated_marked_for_delete: Use CoreLocatorAdapter with process_discovery() instead
pub fn create_br24_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = NavicoBR24Locator { session };
    Box::new(locator)
}
*/
// =============================================================================
// END DEPRECATED LEGACY CODE
// =============================================================================

const BLANKING_SETS: [(usize, &str, &str); 4] = [
    (0, "noTransmitStart1", "noTransmitEnd1"),
    (1, "noTransmitStart2", "noTransmitEnd2"),
    (2, "noTransmitStart3", "noTransmitEnd3"),
    (3, "noTransmitStart4", "noTransmitEnd4"),
];

// =============================================================================
// New unified discovery processing (used by CoreLocatorAdapter)
// =============================================================================

use mayara_core::radar::{ParsedAddress, RadarDiscovery};

/// Process a radar discovery from the core locator.
///
/// Navico radars provide separate multicast addresses for data/report/send in their
/// beacon packets. For dual-range radars (4G, HALO), this is called twice - once for
/// each range (A and B), each with its own complete set of addresses.
pub fn process_discovery(
    session: Session,
    discovery: &RadarDiscovery,
    nic_addr: Ipv4Addr,
    radars: &SharedRadars,
    subsys: &SubsystemHandle,
) -> Result<(), io::Error> {
    // Parse radar's main address
    let parsed = ParsedAddress::parse(&discovery.address)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let radar_ip = Ipv4Addr::from(parsed.ip);
    let radar_addr = SocketAddrV4::new(radar_ip, parsed.port);

    // Use the full addresses from beacon if available, otherwise fall back to port-based
    let data_addr: SocketAddrV4 = if let Some(addr) = &discovery.data_address {
        parse_socket_addr(addr).unwrap_or_else(|| SocketAddrV4::new(radar_ip, discovery.data_port))
    } else {
        SocketAddrV4::new(radar_ip, discovery.data_port)
    };

    let report_addr: SocketAddrV4 = if let Some(addr) = &discovery.report_address {
        parse_socket_addr(addr)
            .unwrap_or_else(|| SocketAddrV4::new(radar_ip, discovery.command_port))
    } else {
        SocketAddrV4::new(radar_ip, discovery.command_port)
    };

    let send_addr: SocketAddrV4 = if let Some(addr) = &discovery.send_address {
        parse_socket_addr(addr)
            .unwrap_or_else(|| SocketAddrV4::new(radar_ip, discovery.command_port))
    } else {
        SocketAddrV4::new(radar_ip, discovery.command_port)
    };

    // Determine locator ID and model
    let is_br24 = discovery.model.as_deref() == Some("BR24");
    let locator_id = if is_br24 {
        LocatorId::GenBR24
    } else {
        LocatorId::Gen3Plus
    };
    let model_name = discovery.model.as_deref();

    // Determine if this is a dual-range radar based on suffix
    let is_dual_range = discovery.suffix.is_some();

    let info: RadarInfo = RadarInfo::new(
        session.clone(),
        locator_id,
        Brand::Navico,
        discovery.serial_number.as_deref(),
        discovery.suffix.as_deref(),
        16,
        NAVICO_SPOKES,
        NAVICO_SPOKE_LEN,
        radar_addr,
        nic_addr,
        data_addr,
        report_addr,
        send_addr,
        settings::new(session.clone(), model_name),
        is_dual_range,
    );

    // Set userName control
    info.controls.set_string("userName", info.key()).ok();

    // Check if this is a new radar
    let Some(mut info) = radars.located(info) else {
        log::debug!("Navico radar {} already known", discovery.name);
        return Ok(());
    };

    // Apply model-specific settings if known
    let model = match model_name {
        Some(name) => Model::from_name(name),
        None => Model::Unknown,
    };

    if model != Model::Unknown {
        let info2 = info.clone();
        settings::update_when_model_known(&mut info.controls, model, &info2);
        info.set_doppler(model.has_doppler());
        radars.update(&info);
    }

    // Spawn subsystems
    let data_name = info.key() + " data";
    let report_name = info.key() + " reports";

    if session.read().unwrap().args.output {
        let info_clone = info.clone();
        subsys.start(SubsystemBuilder::new("stdout", move |s| {
            info_clone.forward_output(s)
        }));
    }

    let data_receiver = data::NavicoDataReceiver::new(&session, info.clone());
    let report_receiver =
        report::NavicoReportReceiver::new(session.clone(), info, radars.clone(), model);

    subsys.start(SubsystemBuilder::new(
        data_name,
        move |s: SubsystemHandle| data_receiver.run(s),
    ));
    subsys.start(SubsystemBuilder::new(report_name, |s| {
        report_receiver.run(s)
    }));

    log::info!(
        "{}: Navico radar activated via CoreLocatorAdapter",
        discovery.name
    );
    Ok(())
}

/// Update controls for a Navico radar when model is known.
///
/// Called from SharedRadars::update_navico_model when a model update arrives.
pub fn update_controls_for_model(info: &mut RadarInfo, model: Model) {
    let info2 = info.clone();
    settings::update_when_model_known(&mut info.controls, model, &info2);
    info.set_doppler(model.has_doppler());
}
