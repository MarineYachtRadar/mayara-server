use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::storage::load_installation_settings;
use crate::{Brand, Session};

// Modules - command.rs removed, now using unified controller from mayara-core
mod data;
mod report;
pub(crate) mod settings;

// Re-export core's Model type as RadarModel for compatibility
pub(crate) use mayara_core::protocol::furuno::Model as RadarModel;

// Use constants from core (single source of truth)
use mayara_core::protocol::furuno::{
    BEACON_PORT as FURUNO_BEACON_PORT, BROADCAST_ADDR as FURUNO_BROADCAST_ADDR,
    DATA_MULTICAST_ADDR as FURUNO_DATA_MULTICAST_ADDR, DATA_PORT as FURUNO_DATA_PORT,
    MAX_SPOKE_LEN as FURUNO_SPOKE_LEN_U16, SPOKES_PER_REVOLUTION as FURUNO_SPOKES_U16,
};

const FURUNO_SPOKES: usize = FURUNO_SPOKES_U16 as usize;
const FURUNO_SPOKE_LEN: usize = FURUNO_SPOKE_LEN_U16 as usize;

// Construct broadcast address from core's constants
// Note: Furuno uses broadcast on 172.31.255.255 for data fallback
fn furuno_broadcast_addr() -> SocketAddrV4 {
    let ip: Ipv4Addr = FURUNO_BROADCAST_ADDR
        .parse()
        .expect("Invalid FURUNO_BROADCAST_ADDR");
    SocketAddrV4::new(ip, FURUNO_DATA_PORT)
}

// Construct multicast data address from core's constants
fn furuno_data_multicast_addr() -> SocketAddrV4 {
    let ip: Ipv4Addr = FURUNO_DATA_MULTICAST_ADDR
        .parse()
        .expect("Invalid FURUNO_DATA_MULTICAST_ADDR");
    SocketAddrV4::new(ip, FURUNO_DATA_PORT)
}

// Beacon packet structures are now in mayara-core
// TCP login is handled by FurunoController in mayara-core

/// Restore persisted installation settings from Application Data API.
/// These are write-only controls that cannot be read from the radar hardware.
/// Called during initial discovery when model is known from persistence.
fn restore_installation_settings(radar_key: &str, info: &mut RadarInfo, _radars: &SharedRadars) {
    if let Some(settings) = load_installation_settings(radar_key) {
        log::info!(
            "{}: Restoring installation settings: {:?}",
            radar_key,
            settings
        );

        let mut restored_any = false;

        // Restore bearing alignment
        if let Some(degrees) = settings.bearing_alignment {
            info.controls
                .set("bearingAlignment", degrees as f32, None)
                .ok();
            log::info!("{}: Restored bearingAlignment = {}°", radar_key, degrees);
            restored_any = true;
        }

        // Restore antenna height
        if let Some(meters) = settings.antenna_height {
            info.controls.set("antennaHeight", meters as f32, None).ok();
            log::info!("{}: Restored antennaHeight = {}m", radar_key, meters);
            restored_any = true;
        }

        // Restore auto acquire (ARPA)
        if let Some(enabled) = settings.auto_acquire {
            info.controls
                .set("autoAcquire", if enabled { 1.0 } else { 0.0 }, None)
                .ok();
            log::info!("{}: Restored autoAcquire = {}", radar_key, enabled);
            restored_any = true;
        }

        if restored_any {
            // Note: radars.update() is called by the caller after this function
            log::info!("{}: Installation settings restored from storage", radar_key);
        }
    }
}

// =============================================================================
// DEPRECATED LEGACY CODE - COMMENTED OUT FOR BUILD VERIFICATION
// =============================================================================
// The following code has been replaced by CoreLocatorAdapter + process_discovery()
// Keeping as comments to verify nothing references it. Delete after verification.
// =============================================================================

/*
// deprecated_marked_for_delete: Legacy locator state - use process_discovery() instead
#[derive(Clone)]
struct FurunoLocatorState {
    session: Session,
    radar_keys: HashMap<SocketAddrV4, String>,
    model_found: bool,
}

// deprecated_marked_for_delete: Legacy RadarLocatorState implementation
impl RadarLocatorState for FurunoLocatorState {
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
        Box::new(Clone::clone(self))
    }
}

impl FurunoLocatorState {
    fn new(session: Session, radar_keys: HashMap<SocketAddrV4, String>, model_found: bool) -> Self {
        FurunoLocatorState {
            session,
            radar_keys,
            model_found,
        }
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) -> bool {
        info.controls
            .set_string("userName", info.key())
            .unwrap();

        if let Some(mut info) = radars.located(info) {
            // It's new, start the RadarProcessor thread

            // Load the model name afresh, it may have been modified from persisted data
            // let model = match info.model_name() {
            //     Some(s) => Model::new(&s),
            //     None => Model::Unknown,
            // };
            // if model != Model::Unknown {
            //     let info2 = info.clone();
            //     info.controls.update_when_model_known(model, &info2);
            //     info.set_legend(model == Model::HALO);
            //     radars.update(&info);
            // }

            // Furuno radars use a single TCP/IP connection to send commands and
            // receive status reports, so report_addr and send_command_addr are identical.
            // Only one of these would be enough for Furuno.
            let port: u16 = match login_to_radar(self.session.clone(), info.addr) {
                Err(e) => {
                    log::error!("{}: Unable to connect for login: {}", info.key(), e);
                    radars.remove(&info.key());
                    return false;
                }
                Ok(p) => p,
            };
            if port != info.send_command_addr.port() {
                info.send_command_addr.set_port(port);
                info.report_addr.set_port(port);
                radars.update(&info);
            }

            // Clone everything moved into future twice or more
            let data_name = info.key() + " data";
            let report_name = info.key() + " reports";

            if self.session.read().unwrap().args.output {
                let info_clone2 = info.clone();

                subsys.start(SubsystemBuilder::new("stdout", move |s| {
                    info_clone2.forward_output(s)
                }));
            }

            let data_receiver = data::FurunoDataReceiver::new(self.session.clone(), info.clone());
            subsys.start(SubsystemBuilder::new(
                data_name,
                move |s: SubsystemHandle| data_receiver.run(s),
            ));

            if !self.session.read().unwrap().args.replay {
                let report_receiver = report::FurunoReportReceiver::new(self.session.clone(), info);
                subsys.start(SubsystemBuilder::new(report_name, |s| {
                    report_receiver.run(s)
                }));
            } else {
                let model = RadarModel::DRS4DNXT; // Default model for replay
                let version = "01.05";
                log::info!(
                    "{}: Radar model {} assumed for replay mode",
                    info.key(),
                    model.to_str(),
                );
                settings::update_when_model_known(&mut info, model, version);
            }

            return true;
        }
        return false;
    }

    fn process_locator_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < 2 {
            return Ok(());
        }

        if log_enabled!(log::Level::Debug) {
            log::debug!(
                "{}: Furuno report: {:02X?} len {}",
                from,
                report,
                report.len()
            );
            log::debug!("{}: printable:     {}", from, PrintableSlice::new(report));
        }

        // Use core functions to check packet type
        if is_beacon_response(report) {
            self.process_beacon_report(report, from, via, radars, subsys)
        } else if is_model_report(report) {
            self.process_beacon_model_report(report, from, via, radars)
        } else {
            Ok(())
        }
    }

    fn process_beacon_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        // Use core parsing
        let discovery = match parse_beacon_response(report, &from.to_string()) {
            Ok(d) => d,
            Err(e) => {
                log::error!(
                    "{} via {}: Failed to decode Furuno beacon: {}",
                    from,
                    nic_addr,
                    e
                );
                return Ok(());
            }
        };

        let radar_addr: SocketAddrV4 = from.clone();

        // DRS: spoke data all on a well-known multicast address from core
        let spoke_data_addr: SocketAddrV4 = furuno_data_multicast_addr();

        let report_addr: SocketAddrV4 = SocketAddrV4::new(*from.ip(), 0); // Port is set in login_to_radar
        let send_command_addr: SocketAddrV4 = report_addr.clone();
        let location_info: RadarInfo = RadarInfo::new(
            self.session.clone(),
            LocatorId::Furuno,
            Brand::Furuno,
            None,
            Some(&discovery.name),
            64,
            FURUNO_SPOKES,
            FURUNO_SPOKE_LEN,
            radar_addr,
            nic_addr.clone(),
            spoke_data_addr,
            report_addr,
            send_command_addr,
            settings::new(self.session.clone()),
            true,
        );
        let key = location_info.key();
        if self.found(location_info, radars, subsys) {
            self.radar_keys.insert(from.clone(), key);
        }

        Ok(())
    }

    fn process_beacon_model_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
    ) -> Result<(), io::Error> {
        if self.model_found {
            return Ok(());
        }
        let radar_addr: SocketAddrV4 = from.clone();
        // Is this known as a Furuno radar?
        if let Some(key) = self.radar_keys.get(&radar_addr) {
            // Use core parsing
            match parse_model_report(report) {
                Ok((model, serial_no)) => {
                    log::debug!(
                        "{}: Furuno model report: {}",
                        from,
                        PrintableSlice::new(report)
                    );
                    log::debug!("{}: model: {:?}", from, model);
                    log::debug!("{}: serial_no: {:?}", from, serial_no);

                    if let Some(serial_no) = serial_no {
                        radars.update_serial_no(key, serial_no);
                    }

                    if let Some(ref model_name) = model {
                        self.model_found = true;
                        radars.update_furuno_model(key, model_name);
                    }
                }
                Err(e) => {
                    log::error!(
                        "{} via {}: Failed to decode Furuno model report: {}",
                        from,
                        nic_addr,
                        e
                    );
                }
            }
        }

        Ok(())
    }
}

// deprecated_marked_for_delete: Legacy FurunoLocator - use CoreLocatorAdapter instead
#[derive(Clone)]
struct FurunoLocator {
    session: Session,
}

// deprecated_marked_for_delete: Legacy RadarLocator implementation
#[async_trait]
impl RadarLocator for FurunoLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Furuno) {
            addresses.push(LocatorAddress::new(
                LocatorId::Furuno,
                &FURUNO_BEACON_ADDRESS,
                Brand::Furuno,
                vec![
                    &REQUEST_BEACON_PACKET,
                    &REQUEST_MODEL_PACKET,
                    &ANNOUNCE_PACKET,
                ],
                Box::new(FurunoLocatorState::new(
                    self.session.clone(),
                    HashMap::new(),
                    false,
                )),
            ));
        }
    }
}

/// deprecated_marked_for_delete: Use CoreLocatorAdapter with process_discovery() instead
pub fn create_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = FurunoLocator { session };
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
/// This creates a RadarInfo and spawns the data/report receivers.
/// TCP login is handled by FurunoController when the report receiver starts.
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
    let radar_addr = SocketAddrV4::new(
        Ipv4Addr::from(parsed.ip),
        if parsed.port > 0 {
            parsed.port
        } else {
            FURUNO_BEACON_PORT
        },
    );

    // DRS: spoke data all on a well-known multicast address from core
    let spoke_data_addr: SocketAddrV4 = furuno_data_multicast_addr();

    let report_addr: SocketAddrV4 = SocketAddrV4::new(*radar_addr.ip(), 0); // Port is set in login_to_radar
    let send_command_addr: SocketAddrV4 = report_addr.clone();

    // Use name (e.g., "RD003212") as serial identifier for unique key generation
    // Use None for 'which' since Furuno doesn't have multi-unit setups like Navico A/B
    let info: RadarInfo = RadarInfo::new(
        session.clone(),
        LocatorId::Furuno,
        Brand::Furuno,
        Some(&discovery.name), // serial_no: radar identifier from beacon
        None,                  // which: not used for Furuno
        64,
        FURUNO_SPOKES,
        FURUNO_SPOKE_LEN,
        radar_addr,
        nic_addr,
        spoke_data_addr,
        report_addr,
        send_command_addr,
        settings::new(session.clone()),
        true,
    );

    // Set userName control
    info.controls.set_string("userName", info.key()).ok();

    // Check if this is a new radar
    let Some(mut info) = radars.located(info) else {
        log::debug!("Furuno radar {} already known", discovery.name);
        return Ok(());
    };

    // Apply model-specific settings if known from discovery OR from persistence
    // After located(), model_name may be set from persisted config
    let model_name = discovery
        .model
        .clone()
        .or_else(|| info.controls.model_name());
    if let Some(ref model_name) = model_name {
        let model = RadarModel::from_name(model_name);
        let version = "unknown"; // Version comes from $N96 via report receiver
        log::info!(
            "{}: Model known: {} ({:?}) [source: {}]",
            info.key(),
            model_name,
            model,
            if discovery.model.is_some() {
                "discovery"
            } else {
                "persistence"
            }
        );
        settings::update_when_model_known(&mut info, model, version);

        // Restore persisted installation settings (write-only controls like bearingAlignment)
        // These must be restored here since ModelDetected event won't fire if model is from persistence
        restore_installation_settings(&info.key(), &mut info, &radars);

        radars.update(&info);
    }

    // Note: TCP login to get the command/report port is handled by FurunoController
    // in mayara-core when the report receiver starts

    // Spawn subsystems
    let data_name = info.key() + " data";
    let report_name = info.key() + " reports";

    if session.read().unwrap().args.output {
        let info_clone = info.clone();
        subsys.start(SubsystemBuilder::new("stdout", move |s| {
            info_clone.forward_output(s)
        }));
    }

    log::debug!("{}: Creating data receiver", info.key());
    let data_receiver = data::FurunoDataReceiver::new(session.clone(), info.clone());
    log::debug!("{}: Starting data receiver subsystem", info.key());
    subsys.start(SubsystemBuilder::new(
        data_name,
        move |s: SubsystemHandle| data_receiver.run(s),
    ));

    log::debug!("{}: Checking replay mode for report receiver", info.key());
    if !session.read().unwrap().args.replay {
        log::debug!("{}: Creating report receiver", info.key());
        let info_key = info.key().clone();
        let report_receiver = report::FurunoReportReceiver::new(session.clone(), info);
        log::debug!("{}: Starting report receiver subsystem", info_key);
        subsys.start(SubsystemBuilder::new(report_name, |s| {
            report_receiver.run(s)
        }));
        log::debug!("{}: Report receiver subsystem started", info_key);
    } else if discovery.model.is_none() {
        // In replay mode without model info from discovery, warn the user
        // Model should come from beacon parsing - if missing, replay may not work correctly
        log::warn!(
            "{}: Replay mode without model info - radar controls may not work correctly. \
             Model should be detected from beacon data.",
            info.key(),
        );
    }

    log::info!(
        "{}: Furuno radar activated via CoreLocatorAdapter",
        discovery.name
    );
    Ok(())
}
