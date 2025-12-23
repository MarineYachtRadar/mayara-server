use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::{Brand, Session};

mod report;
mod settings;

// Use constants from core (single source of truth)
use mayara_core::protocol::raymarine::{
    QUANTUM_SPOKES_PER_REVOLUTION as QUANTUM_SPOKES_U16,
    QUANTUM_SPOKE_LEN as QUANTUM_SPOKE_LEN_U16, RD_SPOKES_PER_REVOLUTION as RD_SPOKES_U16,
    RD_SPOKE_LEN as RD_SPOKE_LEN_U16,
};

const RD_SPOKES_PER_REVOLUTION: usize = RD_SPOKES_U16 as usize;
const RD_SPOKE_LEN: usize = RD_SPOKE_LEN_U16 as usize;
const QUANTUM_SPOKES_PER_REVOLUTION: usize = QUANTUM_SPOKES_U16 as usize;
const QUANTUM_SPOKE_LEN: usize = QUANTUM_SPOKE_LEN_U16 as usize;

const NON_HD_PIXEL_VALUES: u8 = 16; // Old radars have one nibble
const HD_PIXEL_VALUES: u8 = 128; // New radars have one byte pixels, but we drop the last bit for other data

// deprecated_marked_for_delete: Only used by legacy locator
// const RAYMARINE_BEACON_ADDRESS: SocketAddr =
//     SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 5800);
// const RAYMARINE_QUANTUM_WIFI_ADDRESS: SocketAddr =
//     SocketAddr::new(IpAddr::V4(Ipv4Addr::new(232, 1, 1, 1)), 5800);

#[derive(Clone, Debug)]
struct RaymarineModel {
    model: BaseModel,
    hd: bool,             // true if HD = 256 bits per pixel
    max_spoke_len: usize, // 1024 for analog, 256 for Quantum?
    doppler: bool,        // true if Doppler is supported
    name: &'static str,
}

impl RaymarineModel {
    fn new_eseries() -> Self {
        RaymarineModel {
            model: BaseModel::RD,
            hd: false,
            max_spoke_len: 512,
            doppler: false,
            name: "E series Classic",
        }
    }

    fn try_into(model: &str) -> Option<Self> {
        let (model, hd, max_spoke_len, doppler, name) = match model {
            // All "E" strings derived from the raymarine.app.box.com EU declaration of conformity documents
            // Quantum models, believed working
            "E70210" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKE_LEN,
                false,
                "Quantum Q24",
            ),
            "E70344" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKE_LEN,
                false,
                "Quantum Q24C",
            ),
            "E70498" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKE_LEN,
                true,
                "Quantum Q24D",
            ),
            // Cyclone and Cyclone Pro models, untested, assume works as Quantum
            "E70620" => (BaseModel::Quantum, true, QUANTUM_SPOKE_LEN, true, "Cyclone"),
            "E70621" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKE_LEN,
                true,
                "Cyclone Pro",
            ),
            // Magnum, untested, assume works as RD
            "E70484" => (BaseModel::RD, true, RD_SPOKE_LEN, false, "Magnum 4kW"),
            "E70487" => (BaseModel::RD, true, RD_SPOKE_LEN, false, "Magnum 12kW"),
            // Open Array HD and SHD, introduced circa 2007
            "E52069" => (
                BaseModel::RD,
                true,
                RD_SPOKE_LEN,
                false,
                "Open Array HD 4kW",
            ),
            "E92160" => (
                BaseModel::RD,
                true,
                RD_SPOKE_LEN,
                false,
                "Open Array HD 12kW",
            ),
            "E52081" => (
                BaseModel::RD,
                true,
                RD_SPOKE_LEN,
                false,
                "Open Array SHD 4kW",
            ),
            "E52082" => (
                BaseModel::RD,
                true,
                RD_SPOKE_LEN,
                false,
                "Open Array SHD 12kW",
            ),
            // And the actual RD models, introduced circa 2004
            "E92142" => (BaseModel::RD, true, RD_SPOKE_LEN, false, "RD418HD"),
            "E92143" => (BaseModel::RD, true, RD_SPOKE_LEN, false, "RD424HD"),
            "E92130" => (BaseModel::RD, true, 512, false, "RD418D"),
            "E92132" => (BaseModel::RD, true, 512, false, "RD424D"),
            _ => return None,
        };
        Some(RaymarineModel {
            model,
            hd,
            max_spoke_len,
            doppler,
            name,
        })
    }
}

fn hd_to_pixel_values(hd: bool) -> u8 {
    if hd {
        HD_PIXEL_VALUES
    } else {
        NON_HD_PIXEL_VALUES
    }
}

// Re-export BaseModel from core for use by other modules in this brand
pub use mayara_core::protocol::raymarine::BaseModel;

// =============================================================================
// DEPRECATED LEGACY CODE - COMMENTED OUT FOR BUILD VERIFICATION
// =============================================================================
// The following code has been replaced by CoreLocatorAdapter + process_discovery()
// Keeping as comments to verify nothing references it. Delete after verification.
// =============================================================================

/*
type LinkId = u32;

// deprecated_marked_for_delete: Legacy radar state for two-step discovery
#[derive(Clone)]
struct RadarState {
    beacon: ParsedBeacon56,
}

// deprecated_marked_for_delete: Legacy locator state - use process_discovery() instead
#[derive(Clone)]
struct RaymarineLocatorState {
    session: Session,
    ids: HashMap<LinkId, RadarState>,
}

impl RaymarineLocatorState {
    fn new(session: Session) -> Self {
        RaymarineLocatorState {
            session,
            ids: HashMap::new(),
        }
    }

    fn process_beacon_36_report(
        &mut self,
        report: &[u8],
        from: &Ipv4Addr,
    ) -> Result<Option<RadarInfo>, Error> {
        // Use core parsing
        let beacon = match parse_beacon_36(report) {
            Ok(b) => b,
            Err(e) => {
                log::debug!("{}: Failed to parse Raymarine 36 beacon: {}", from, e);
                return Ok(None);
            }
        };

        if let Some(info) = self.ids.get(&beacon.link_id) {
            log::debug!(
                "{}: link {:08X} report: {:02X?} model {}",
                from,
                beacon.link_id,
                report,
                info.beacon.base_model
            );

            let model = info.beacon.base_model;

            // Validate subtype for the model
            match model {
                BaseModel::Quantum => {
                    if beacon.subtype != SUBTYPE_QUANTUM_36 {
                        log::warn!(
                            "{}: Raymarine 36 report: unexpected subtype {} for Quantum",
                            from,
                            beacon.subtype
                        );
                        return Ok(None);
                    }
                }
                BaseModel::RD => {
                    match beacon.subtype {
                        SUBTYPE_RD_36 => {} // Continue
                        8 | 21 | 26 | 27 | 30 | 35 => {
                            // Known unknowns
                            return Ok(None);
                        }
                        _ => {
                            log::warn!(
                                "{}: Raymarine 36 report: unexpected subtype {} for RD",
                                from,
                                beacon.subtype
                            );
                            return Ok(None);
                        }
                    }
                }
            }
            let doppler = false; // Improved later when model is known better

            let (spokes_per_revolution, max_spoke_len) = match model {
                BaseModel::Quantum => (QUANTUM_SPOKES_PER_REVOLUTION, QUANTUM_SPOKE_LEN),
                BaseModel::RD => (RD_SPOKES_PER_REVOLUTION, RD_SPOKE_LEN),
            };

            // Parse addresses from core-parsed strings
            let radar_addr: SocketAddrV4 = beacon.report_addr.parse().map_err(|e| {
                anyhow::anyhow!("Invalid report address {}: {}", beacon.report_addr, e)
            })?;
            let radar_send: SocketAddrV4 = beacon.command_addr.parse().map_err(|e| {
                anyhow::anyhow!("Invalid command address {}: {}", beacon.command_addr, e)
            })?;

            let location_info: RadarInfo = RadarInfo::new(
                self.session.clone(),
                LocatorId::Raymarine,
                Brand::Raymarine,
                None,
                None,
                16,
                spokes_per_revolution,
                max_spoke_len,
                radar_addr.into(),
                from.clone(),
                radar_addr.into(),
                radar_addr.into(),
                radar_send.into(),
                settings::new(self.session.clone(), model),
                doppler,
            );

            return Ok(Some(location_info));
        } else {
            log::trace!(
                "{}: Raymarine 36 report: link_id {:08X} not found in ids: {:02X?}",
                from,
                beacon.link_id,
                report
            );
        }
        Ok(None)
    }

    fn process_beacon_56_report(&mut self, report: &[u8], from: &Ipv4Addr) -> Result<(), Error> {
        // Use core parsing
        let beacon = match parse_beacon_56(report) {
            Ok(b) => b,
            Err(e) => {
                // Not a valid 56-byte beacon (could be MFD request or unknown subtype)
                log::trace!("{}: Skipping Raymarine 56 beacon: {}", from, e);
                return Ok(());
            }
        };

        // Check if we already have this radar registered
        if self
            .ids
            .insert(beacon.link_id, RadarState { beacon: beacon.clone() })
            .is_none()
        {
            // New radar found
            match beacon.base_model {
                BaseModel::Quantum => {
                    log::debug!(
                        "{}: Quantum located via report: {:02X?} len {}",
                        from,
                        report,
                        report.len()
                    );
                    log::debug!(
                        "{}: Quantum located via report: {} len {}",
                        from,
                        PrintableSlice::new(report),
                        report.len()
                    );
                    log::debug!(
                        "{}: link_id {:08X} model_name: {:?} model {}",
                        from,
                        beacon.link_id,
                        beacon.model_name,
                        beacon.base_model
                    );
                }
                BaseModel::RD => {
                    log::debug!(
                        "{}: RD located via report: {:02X?} len {}",
                        from,
                        report,
                        report.len()
                    );
                    log::debug!(
                        "{}: link_id: {:08X} model_name: {:?} model {}",
                        from,
                        beacon.link_id,
                        beacon.model_name,
                        beacon.base_model
                    );
                }
            }
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string("userName", info.key())
            .unwrap();

        if let Some(info) = radars.located(info) {
            // It's new, start the RadarProcessor thread

            if self.session.read().unwrap().args.output {
                let info_clone2 = info.clone();

                subsys.start(SubsystemBuilder::new("stdout", move |s| {
                    info_clone2.forward_output(s)
                }));
            }

            let report_name = info.key();
            let info_clone = info.clone();
            let report_receiver = report::RaymarineReportReceiver::new(
                self.session.clone(),
                info_clone,
                radars.clone(),
            );

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

// deprecated_marked_for_delete: Legacy RadarLocatorState implementation
impl RadarLocatorState for RaymarineLocatorState {
    fn process(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        if report.len() < 2 {
            return Ok(());
        }

        log::trace!(
            "{}: Raymarine report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        log::trace!("{}: printable:     {}", from, PrintableSlice::new(report));

        match report.len() {
            36 => {
                // Common Raymarine message

                match Self::process_beacon_36_report(self, report, nic_addr) {
                    Ok(Some(info)) => {
                        self.found(info, radars, subsys);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::error!("{}: Error processing beacon: {}", from, e);
                    }
                }
            }
            56 => match Self::process_beacon_56_report(self, report, nic_addr) {
                Ok(()) => {}

                Err(e) => {
                    log::error!("{}: Error processing beacon: {}", from, e);
                }
            },
            _ => {
                log::trace!(
                    "{}: Unknown Raymarine report length: {}",
                    from,
                    report.len()
                );
            }
        }

        Ok(())
    }

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(Clone::clone(self))
    }
}

// deprecated_marked_for_delete: Legacy RaymarineLocator - use CoreLocatorAdapter instead
#[derive(Clone)]
struct RaymarineLocator {
    session: Session,
}

// deprecated_marked_for_delete: Legacy RadarLocator implementation
impl RadarLocator for RaymarineLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
            let beacon_address = if self.session.args().allow_wifi {
                &RAYMARINE_QUANTUM_WIFI_ADDRESS
            } else {
                &RAYMARINE_BEACON_ADDRESS
            };

            addresses.push(LocatorAddress::new(
                LocatorId::Raymarine,
                beacon_address,
                Brand::Raymarine,
                vec![&MFD_BEACON],
                Box::new(RaymarineLocatorState::new(self.session.clone())),
            ));
        }
    }
}

/// deprecated_marked_for_delete: Use CoreLocatorAdapter with process_discovery() instead
pub fn create_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = RaymarineLocator { session };
    Box::new(locator)
}
*/
// =============================================================================
// END DEPRECATED LEGACY CODE
// =============================================================================

// =============================================================================
// New unified discovery processing (used by CoreLocatorAdapter)
// =============================================================================

use mayara_core::radar::{ParsedAddress, RadarDiscovery};

/// Process a radar discovery from the core locator.
///
/// Note: Raymarine radars use a two-step discovery process (56-byte beacon first,
/// then 36-byte beacon with addresses). The core RadarDiscovery provides simplified
/// info. For full functionality, the existing stateful RaymarineLocatorState should
/// be used until the core properly handles the two-step process.
pub fn process_discovery(
    session: Session,
    discovery: &RadarDiscovery,
    nic_addr: Ipv4Addr,
    radars: &SharedRadars,
    subsys: &SubsystemHandle,
) -> Result<(), io::Error> {
    // Parse address from discovery using core's parser
    let parsed = ParsedAddress::parse(&discovery.address)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let radar_ip = Ipv4Addr::from(parsed.ip);
    let radar_addr = SocketAddrV4::new(radar_ip, if parsed.port > 0 { parsed.port } else { 5800 });

    // Determine model from discovery
    let model = if let Some(ref model_name) = discovery.model {
        RaymarineModel::try_into(model_name).unwrap_or_else(|| RaymarineModel::new_eseries())
    } else {
        RaymarineModel::new_eseries()
    };

    let spokes_per_revolution = if model.model == BaseModel::Quantum {
        QUANTUM_SPOKES_PER_REVOLUTION
    } else {
        RD_SPOKES_PER_REVOLUTION
    };

    let max_spoke_len = model.max_spoke_len;
    let pixel_values = if model.hd {
        HD_PIXEL_VALUES
    } else {
        NON_HD_PIXEL_VALUES
    };

    // For simplified discovery, use basic addresses
    let report_addr: SocketAddrV4 = SocketAddrV4::new(radar_ip, discovery.command_port);
    let data_addr: SocketAddrV4 = SocketAddrV4::new(radar_ip, discovery.data_port);
    let send_addr: SocketAddrV4 = SocketAddrV4::new(radar_ip, discovery.command_port);

    let info: RadarInfo = RadarInfo::new(
        session.clone(),
        LocatorId::Raymarine,
        Brand::Raymarine,
        discovery.serial_number.as_deref(),
        None,
        pixel_values,
        spokes_per_revolution,
        max_spoke_len,
        radar_addr,
        nic_addr,
        data_addr,
        report_addr,
        send_addr,
        settings::new(session.clone(), model.model.clone()),
        model.doppler,
    );

    // Set userName control
    info.controls.set_string("userName", info.key()).ok();

    // Check if this is a new radar
    let Some(info) = radars.located(info) else {
        log::debug!("Raymarine radar {} already known", discovery.name);
        return Ok(());
    };

    // Spawn subsystems
    if session.read().unwrap().args.output {
        let info_clone = info.clone();
        subsys.start(SubsystemBuilder::new("stdout", move |s| {
            info_clone.forward_output(s)
        }));
    }

    let report_name = info.key();
    let report_receiver =
        report::RaymarineReportReceiver::new(session.clone(), info.clone(), radars.clone());

    subsys.start(SubsystemBuilder::new(report_name, |s| {
        report_receiver.run(s)
    }));

    log::info!(
        "{}: Raymarine radar activated via CoreLocatorAdapter",
        discovery.name
    );
    Ok(())
}

// =============================================================================
// DEPRECATED TESTS - COMMENTED OUT WITH LEGACY CODE
// =============================================================================
// These tests use RaymarineLocatorState which has been replaced by
// CoreLocatorAdapter + process_discovery(). Delete after verification.
// =============================================================================

/*
#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use crate::brand::raymarine::RaymarineLocatorState;

    #[test]
    fn decode_raymarine_locator_beacon() {
        let session = crate::Session::new_fake();

        const VIA: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

        // This is a real beacon message from a Raymarine Quantum radar (E704980880217-NewZealand)
        // File "radar transmitting with range changes.pcap.gz"
        // Radar sends from 198.18.6.214 to 224.0.0.1:5800
        // packets of length 36, 56 and 70.
        // Spoke data seems to be on 232.1.243.1:2574
        const DATA1_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0x28, 0x0, 0x0, 0x0, 0x3, 0x0, 0x64, 0x0,
            0x6, 0x8, 0x10, 0x0, 0x1, 0xf3, 0x1, 0xe8, 0xe, 0xa, 0x11, 0x0, 0xd6, 0x6, 0x12, 0xc6,
            0xf, 0xa, 0x36, 0x0,
        ];
        const DATA1_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, 0x66, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0xf3, 0x0, 0x0, 0x0,
            0xf3, 0x0, 0xa8, 0xc0, 0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x52, 0x61, 0x64,
            0x61, 0x72, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x2, 0x0, 0x0, 0x0,
        ];

        // The same radar transmitting via wired connection
        // Radar IP 10.30.200.221 sends to UDP 5800 a lot but only the messages
        // coming from port 5800 seem to be useful (sofar!)
        //
        const DATA2_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, // message_type
            0x58, 0x6b, 0x80, 0xd6, // link id
            0x28, 0x0, 0x0, 0x0, // submessage type
            0x3, 0x0, 0x64, 0x0, // ?
            0x6, 0x8, 0x10, 0x0, // ?
            0x1, 0xa7, 0x1, 0xe8, 0xe, 0xa, // 232.1.167.1:2574
            0x11, 0x0, // ?
            0xdd, 0xc8, 0x1e, 0xa, 0xf, 0xa, // 10.30.200.221:2575
            0x36, 0x0, // ?
        ];
        const DATA2_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, // message_type
            0x66, 0x0, 0x0, 0x0, // subtype?
            0x58, 0x6b, 0x80, 0xd6, // link id
            0xf3, 0x0, 0x0, 0x0, //
            0xa7, 0x27, 0xa8, 0xc0, //
            0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, // "Quantum"
            0x52, 0x61, 0x64, 0x61, 0x72, // "Radar"
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, // remaining blank bytes fills 32 bytes
            0x2, 0x0, 0x0, 0x0,
        ];

        // Analog radar connected to Eseries MFD
        // MFD IP addr 10.0.234.47
        const DATA3_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, // message_type
            0xb1, 0x69, 0xc2, 0xb2, // link_id
            0x1, 0x0, 0x0, 0x0, // sub_type 1
            0x1, 0x0, 0x1e, 0x0, //
            0xb, 0x8, 0x10, 0x0, //
            231, 69, 29, 224, 0x6, 0xa, 0x0, 0x0, // 224.29.69.231:2566 The radar sends to ...
            47, 234, 0, 10, 11, 8, 0, 0, // 10.0.234.47:2059 ... and receives on
        ];
        const DATA3_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, // message_type
            0x1, 0x0, 0x0, 0x0, // sub_type
            0xb1, 0x69, 0xc2, 0xb2, // link_id
            0xb, 0x2, 0x0, 0x0, //
            0x2f, 0xea, 0x0, 0xa, 0x0, //
            // From here on lots of ascii number (3 = 0x33) and 0xcc ...
            0x31, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33, 0x4e, 0x37, 0xcc, 0x27, 0xcc,
            0x33, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x30, 0xcc, 0x13, 0xc8, 0x33, 0xcc, 0x13, 0xcc,
            0x33, 0xc0, 0x13, 0x2, 0x0, 0x1, 0x0,
        ];

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA1_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("Quantum".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(198, 18, 6, 214), 2575)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 243, 1), 2574)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 243, 1), 2574)
        );

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA2_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("Quantum".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 30, 200, 221), 2575)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 167, 1), 2574)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 167, 1), 2574)
        );

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA3_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA3_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA3_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("RD".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 0, 234, 47), 2059)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 29, 69, 231), 2566)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 29, 69, 231), 2566)
        );
    }
}
*/
// =============================================================================
// END DEPRECATED TESTS
// =============================================================================
