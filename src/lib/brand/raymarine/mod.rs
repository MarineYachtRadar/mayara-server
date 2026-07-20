use anyhow::{Error, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::brand::RadarLocator;
use crate::locator::LocatorAddress;
use crate::network::LittleEndianSocketAddrV4;
use crate::radar::settings::ControlId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{PrintableSlice, c_string, decode_bin};
use crate::{Brand, Cli};

use super::LocatorId;

mod command;
mod navdata;
mod protocol;
mod report;
mod settings;

const NON_HD_PIXEL_VALUES: u8 = 16; // Old radars have one nibble
const HD_PIXEL_VALUES_RAW: u16 = 256; // New radars have one byte pixels
const HD_PIXEL_VALUES: u8 = (HD_PIXEL_VALUES_RAW / 2) as u8; // ... but we drop the last bit so we have space for other data

const RAYMARINE_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 5800);
const RAYMARINE_QUANTUM_WIFI_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(232, 1, 1, 1)), 5800);

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
                protocol::QUANTUM_MAX_SPOKE_LEN,
                false,
                "Quantum Q24C",
            ),
            "E70344" => (
                BaseModel::Quantum,
                true,
                protocol::QUANTUM_MAX_SPOKE_LEN,
                false,
                "Quantum Q24W",
            ),
            "E70498" => (
                BaseModel::Quantum,
                true,
                protocol::QUANTUM_MAX_SPOKE_LEN,
                true,
                "Quantum Q24D",
            ),
            // Cyclone and Cyclone Pro models, untested, assume works as Quantum
            // Probably supports higher resulution though...
            "E70620" => (
                BaseModel::Quantum,
                true,
                protocol::QUANTUM_MAX_SPOKE_LEN,
                true,
                "Cyclone",
            ),
            "E70621" => (
                BaseModel::Quantum,
                true,
                protocol::QUANTUM_MAX_SPOKE_LEN,
                true,
                "Cyclone Pro",
            ),
            // Magnum, untested, assume works as RD
            "E70484" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Magnum 4kW",
            ),
            "E70487" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Magnum 12kW",
            ),
            // Open Array HD and SHD, introduced circa 2007
            "E52069" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Open Array HD 4kW",
            ),
            "E92160" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Open Array HD 12kW",
            ),
            "E52081" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Open Array SHD 4kW",
            ),
            "E52082" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "Open Array SHD 12kW",
            ),
            // And the actual RD models, introduced circa 2004
            "E92142" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "RD418HD",
            ),
            "E92143" => (
                BaseModel::RD,
                true,
                protocol::RD_HD_MAX_SPOKE_LEN,
                false,
                "RD424HD",
            ),
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

/*
Let's take a look at what Raymarine radars send in their beacons.
First of all, it looks as if all ethernet devices send a beacon of length 56 bytes,
and that they also send a beacon of length 36 bytes.

The observation so far is that the 56 byte beacon contains a 4 byte "link_id" field,
which the next 36 byte beacon also contains.

We put them in a map for now, but probably we only need to store the last one.
 */

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
struct RaymarineBeacon36 {
    beacon_type: [u8; 4],              // 0: always 0
    link_id: [u8; 4],                  // 4
    subtype: [u8; 4],                  // 8
    _field5: [u8; 4],                  // 12
    _field6: [u8; 4],                  // 16
    report: LittleEndianSocketAddrV4,  // 20
    _align1: [u8; 2],                  // 26
    command: LittleEndianSocketAddrV4, // 28
    _align2: [u8; 2],                  // 34
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
struct RaymarineBeacon56 {
    beacon_type: [u8; 4], // 0: always 1
    subtype: [u8; 4],     // 4
    link_id: [u8; 4],     // 8
    _field4: [u8; 4],     // 12
    _field5: [u8; 4],     // 16
    model_name: [u8; 32], // 20: String like "QuantumRadar" (subtype 0x66) or "Ethernet Dome" (subtype 0x0b)
    _field7: [u8; 4],     // 52
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum BaseModel {
    RD,
    Quantum,
}

impl fmt::Display for BaseModel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            BaseModel::RD => "RD",
            BaseModel::Quantum => "Quantum",
        };
        write!(f, "{}", s)
    }
}

type LinkId = u32;

#[derive(Clone)]
struct RadarState {
    model_name: Option<String>,
    model: BaseModel,
}

/// Witness for "another controller is on the beacon group right now".
/// Cloned (Arc-shared) into the locator and every Quantum report receiver
/// so the WiFi wake nudge can stay silent when an MFD (or another mayara)
/// is already managing the radar. See radar-wakeup-analysis.md path A.
#[derive(Debug, Default)]
pub(crate) struct ExternalControllerWitness {
    last_seen: Mutex<Option<Instant>>,
}

impl ExternalControllerWitness {
    pub(crate) fn mark(&self) {
        *self.last_seen.lock().unwrap() = Some(Instant::now());
    }

    pub(crate) fn quiet_for(&self, window: Duration) -> bool {
        match *self.last_seen.lock().unwrap() {
            None => true,
            Some(t) => t.elapsed() >= window,
        }
    }
}

#[derive(Clone)]
struct RaymarineLocator {
    args: Cli,
    ids: HashMap<LinkId, RadarState>,
    external_seen: Arc<ExternalControllerWitness>,
}

impl RaymarineLocator {
    fn new(args: Cli) -> Self {
        RaymarineLocator {
            args,
            ids: HashMap::new(),
            external_seen: Arc::new(ExternalControllerWitness::default()),
        }
    }

    fn process_beacon_36_report(
        &mut self,
        report: &[u8],
        from: &Ipv4Addr,
        radars: &SharedRadars,
    ) -> Result<Option<(RadarInfo, BaseModel)>, Error> {
        match decode_bin::<RaymarineBeacon36>(report) {
            Ok(data) => {
                let beacon_type = u32::from_le_bytes(data.beacon_type);
                let link_id_preview = u32::from_le_bytes(data.link_id);
                let subtype_preview = u32::from_le_bytes(data.subtype);
                log::debug!(
                    "{}: Beacon 36: type=0x{:x} link_id=0x{:08x} subtype=0x{:x} report={} cmd={}",
                    from,
                    beacon_type,
                    link_id_preview,
                    subtype_preview,
                    Into::<SocketAddrV4>::into(data.report),
                    Into::<SocketAddrV4>::into(data.command),
                );
                if beacon_type != 0 {
                    log::warn!(
                        "{}: Raymarine 36 report: unexpected beacon type {}",
                        from,
                        beacon_type
                    );
                    return Ok(None);
                }

                let link_id = u32::from_le_bytes(data.link_id);

                if let Some(info) = self.ids.get(&link_id) {
                    log::debug!(
                        "{}: link {:08X} report: {:02X?} model {}",
                        from,
                        link_id,
                        report,
                        info.model_name.as_deref().unwrap_or("<unknown>")
                    );
                    log::trace!("{}: data {:?}", from, data);

                    let model = info.model;
                    let subtype = u32::from_le_bytes(data.subtype);

                    match model {
                        BaseModel::Quantum => {
                            if subtype != protocol::beacon36::QUANTUM {
                                log::trace!(
                                    "{}: Raymarine 36 report: ignoring subtype 0x{:02x} for Quantum (not 0x28)",
                                    from,
                                    subtype
                                );
                                return Ok(None);
                            }
                        }
                        BaseModel::RD => {
                            match subtype {
                                protocol::beacon36::RD => {} // Continue
                                s if protocol::beacon36::RD_IGNORED.contains(&s) => {
                                    return Ok(None);
                                }
                                _ => {
                                    log::warn!(
                                        "{}: Raymarine 36 report: unexpected subtype {} for RD",
                                        from,
                                        subtype
                                    );
                                    return Ok(None);
                                }
                            }
                        }
                    }
                    let doppler = false; // Improved later when model is known better

                    let (spokes_per_revolution, max_spoke_len) = match model {
                        BaseModel::Quantum => (
                            protocol::QUANTUM_SPOKES_PER_REVOLUTION as usize,
                            protocol::QUANTUM_MAX_SPOKE_LEN,
                        ),
                        BaseModel::RD => (
                            protocol::RD_SPOKES_PER_REVOLUTION as usize,
                            protocol::RD_HD_MAX_SPOKE_LEN,
                        ),
                    };

                    let radar_send: SocketAddrV4 = data.command.into();

                    // Quantum WiFi radars connect to a well-known SSID and password,
                    // and the Quantum advertises an
                    // unspecified report address (0.0.0.0:0) and instead streams
                    // reports and spokes unicast back to whoever sends it commands.
                    // The command socket sends from the NIC on the command port,
                    // so the radar replies to that port: listen unicast there.
                    //
                    // In that topology the radar streams reports and spokes back
                    // on the same connected socket we send commands on — there
                    // is no separate listen address or multicast group. All four
                    // addresses on RadarInfo collapse onto the radar's command
                    // address; the report.rs code only reads report_addr.port()
                    // (with nic_addr) to bind our local end of the connected
                    // socket.
                    let beacon_report: SocketAddrV4 = data.report.into();
                    let radar_addr: SocketAddrV4 = if beacon_report.ip().is_unspecified() {
                        radar_send
                    } else {
                        beacon_report
                    };

                    let location_info: RadarInfo = RadarInfo::new(
                        radars,
                        &self.args,
                        Brand::Raymarine,
                        None,
                        None,
                        0,
                        spokes_per_revolution,
                        max_spoke_len,
                        radar_addr,
                        *from,
                        radar_addr,
                        radar_addr,
                        radar_send,
                        |id, tx| settings::new(id, tx, &self.args, info.model),
                        doppler,
                        false,
                    );

                    return Ok(Some((location_info, model)));
                } else {
                    log::trace!(
                        "{}: Raymarine 36 report: link_id {:08X} not found in ids: {:02X?}",
                        from,
                        link_id,
                        report
                    );
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(None)
    }

    fn process_beacon_56_report(&mut self, report: &[u8], from: &Ipv4Addr) -> Result<(), Error> {
        match decode_bin::<RaymarineBeacon56>(report) {
            Ok(data) => {
                let beacon_type = u32::from_le_bytes(data.beacon_type);
                let subtype = u32::from_le_bytes(data.subtype);
                let link_id = u32::from_le_bytes(data.link_id);
                log::debug!(
                    "{}: Beacon 56: type=0x{:x} subtype=0x{:x} link_id=0x{:08x} model={:?}",
                    from,
                    beacon_type,
                    subtype,
                    link_id,
                    c_string(&data.model_name),
                );
                if beacon_type != 0x01 {
                    // MFDs emit a type-2 announcement alongside their type-1
                    // MFD beacon; it is not a radar identity, so don't warn
                    // about it on every beacon cycle.
                    if beacon_type == 0x02 {
                        log::debug!("{}: ignoring type-2 MFD announcement", from);
                    } else {
                        log::warn!(
                            "{}: Raymarine 56 report: unexpected beacon type {}",
                            from,
                            beacon_type
                        );
                    }
                    return Ok(());
                }

                let link_id = u32::from_le_bytes(data.link_id);
                let subtype = u32::from_le_bytes(data.subtype);

                match subtype {
                    protocol::beacon56::QUANTUM => {
                        let model = BaseModel::Quantum;
                        let model_name: Option<String> =
                            c_string(&data.model_name).map(String::from);

                        if self
                            .ids
                            .insert(
                                link_id,
                                RadarState {
                                    model_name: model_name.clone(),
                                    model,
                                },
                            )
                            .is_none()
                        {
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
                                link_id,
                                model_name,
                                model
                            );
                            log::debug!("{}: data {:?}", from, data);
                        }
                    }
                    protocol::beacon56::RD | protocol::beacon56::RD_DOME => {
                        let model = BaseModel::RD;
                        // Ethernet radomes carry a model string ("Ethernet
                        // Dome"); the analog RD beacon's model field is
                        // garbage, so fall back to the base model name.
                        let model_name = c_string(&data.model_name)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .or_else(|| Some(model.to_string()));

                        if self
                            .ids
                            .insert(
                                link_id,
                                RadarState {
                                    model_name: model_name.clone(),
                                    model,
                                },
                            )
                            .is_none()
                        {
                            log::debug!(
                                "{}: RD located via report: {:02X?} len {}",
                                from,
                                report,
                                report.len()
                            );
                            log::debug!(
                                "{}: link_id: {:08X} model_name: {:?} model {}",
                                from,
                                link_id,
                                model_name,
                                model
                            );
                        }
                    }
                    protocol::beacon56::W3 => {
                        // W3 wireless bridge — the radar also sends a direct
                        // Quantum beacon (0x66) with the correct addresses.
                        log::debug!("{}: W3 bridge beacon (ignored, using direct Quantum)", from);
                    }
                    protocol::beacon56::MFD => {
                        log::debug!("{}: MFD announcement (ignored)", from);
                    }
                    _ => {
                        log::debug!("{}: unknown 56-byte beacon subtype 0x{:02x}", from, subtype);
                    }
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(())
    }

    fn found(
        &self,
        info: RadarInfo,
        base_model: BaseModel,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        info.controls
            .set_string(&ControlId::UserName, info.key())
            .unwrap();

        if let Some(mut info) = radars.add(info) {
            // It's new, start the RadarProcessor thread
            info.start_forwarding_radar_messages_to_stdout(subsys);

            let report_name = info.key();
            radars.update(&mut info);

            // Start the NavData sender to feed position/heading to the
            // radar every 100ms. Required for Doppler and MARPA.
            let send_addr = info.send_command_addr;
            let nic_addr = info.nic_addr;
            let navdata_name = format!("{}-navdata", report_name);
            subsys.start(SubsystemBuilder::new(
                navdata_name,
                async move |s: &mut SubsystemHandle| match crate::network::create_multicast_send(
                    &send_addr, &nic_addr,
                ) {
                    Ok(sock) => navdata::run(s, sock).await.map_err(|e| e.into()),
                    Err(e) => {
                        log::warn!("Failed to create NavData socket: {}", e);
                        Ok::<(), anyhow::Error>(())
                    }
                },
            ));

            let report_receiver = report::RaymarineReportReceiver::new(
                &self.args,
                info,
                radars.clone(),
                base_model,
                self.external_seen.clone(),
            );

            subsys.start(SubsystemBuilder::new(
                report_name,
                async move |s: &mut SubsystemHandle| report_receiver.run(s).await,
            ));
        }
    }
}

impl RadarLocator for RaymarineLocator {
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

        if from.ip() != nic_addr && is_external_controller_signal(report) {
            log::debug!(
                "{}: external Raymarine controller observed (len {})",
                from,
                report.len()
            );
            self.external_seen.mark();
        }

        match report.len() {
            protocol::beacon36::LEN => {
                match Self::process_beacon_36_report(self, report, nic_addr, radars) {
                    Ok(Some((info, base_model))) => {
                        self.found(info, base_model, radars, subsys);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::error!("{}: Error processing beacon: {}", from, e);
                    }
                }
            }
            protocol::beacon56::LEN => match Self::process_beacon_56_report(self, report, nic_addr)
            {
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

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(Clone::clone(self))
    }
}

/// True if `report` looks like a packet another Raymarine controller (MFD or
/// another mayara) would emit on the beacon group: the 16-byte `ABCDEFGHIJKLMNOP`
/// wake literal, the 102-byte WOL signature, or a 56-byte beacon with the
/// MFD subtype. Caller is responsible for the "not us" filter on source IP.
fn is_external_controller_signal(report: &[u8]) -> bool {
    match report.len() {
        16 => report == RAYMARINE_WAKE_RADAR,
        102 => report == RAYMARINE_WOL_RADAR,
        protocol::beacon56::LEN => {
            // beacon_type at 0, subtype at 4 (both u32 LE)
            let beacon_type = u32::from_le_bytes(report[0..4].try_into().unwrap());
            let subtype = u32::from_le_bytes(report[4..8].try_into().unwrap());
            beacon_type == 1 && subtype == protocol::beacon56::MFD
        }
        _ => false,
    }
}

const RAYMARINE_MFD_BEACON: [u8; 56] = [
    0x01, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x38, 0x8c, 0x81, 0xd4, 0x6a, 0x01, 0x0e, 0x83,
    0x6c, 0x03, 0x12, 0xc6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00,
];
const RAYMARINE_WAKE_RADAR: [u8; 16] = [
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 0x50,
];
const RAYMARINE_WOL_RADAR: [u8; 102] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd,
    0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11,
    0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0,
    0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef,
    0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd,
    0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11, 0xc7, 0xd, 0xef, 0xa0, 0x0, 0x11,
    0xc7, 0xd, 0xef, 0xa0,
];

/// How many WOL magic packets to send per beacon cycle. An Axiom wakes a
/// radar with a burst of 7–10 WOLs, never a single one — a lone multicast
/// datagram to a dozing WiFi radar is routinely lost. Wire-confirmed in
/// MarineYachtRadar/mayara-server#160.
const WOL_WAKE_BURST: usize = 8;

/// Raymarine MFDs emit their radar traffic with IP TTL 10, and an Axiom acting
/// as a radar's WiFi access point only relays packets whose TTL is > 1 (router
/// semantics: TTL 1 means link-local only). Send our wake bursts *and* our
/// command traffic with the same TTL so they are eligible for that relay — on
/// the local link a larger TTL changes nothing. Wire-confirmed against a live
/// Axiom in MarineYachtRadar/mayara-server#160. Raymarine-specific: other
/// brands keep the OS default.
pub(super) const RAYMARINE_RELAY_TTL: u32 = 10;

/// Gap between the packets of an on-demand wake burst, matching the ~20 ms
/// spacing observed from an Axiom.
const WAKE_BURST_SPACING: Duration = Duration::from_millis(20);

/// Send the WOL wake burst the way an Axiom does when the user presses "On":
/// [`WOL_WAKE_BURST`] magic packets [`WAKE_BURST_SPACING`] apart to the wired
/// beacon group, with [`RAYMARINE_RELAY_TTL`] so an Axiom relaying to a WiFi
/// radar forwards them. Failures are logged, not returned — the caller's mode
/// command should still go out.
async fn send_wake_burst(nic_addr: &Ipv4Addr) {
    let SocketAddr::V4(addr) = RAYMARINE_BEACON_ADDRESS else {
        return;
    };
    match crate::network::create_multicast_send(&addr, nic_addr) {
        Ok(sock) => {
            if let Err(e) = sock.set_multicast_ttl_v4(RAYMARINE_RELAY_TTL) {
                log::warn!("via {}: wake burst TTL: {}", nic_addr, e);
            }
            for _ in 0..WOL_WAKE_BURST {
                if let Err(e) = sock.send(&RAYMARINE_WOL_RADAR).await {
                    log::warn!("via {}: wake burst send failed: {}", nic_addr, e);
                    return;
                }
                tokio::time::sleep(WAKE_BURST_SPACING).await;
            }
            log::info!("via {}: sent WOL wake burst", nic_addr);
        }
        Err(e) => log::warn!("via {}: wake burst socket: {}", nic_addr, e),
    }
}

/// The discovery/wake packets sent to a beacon group each locator cycle:
/// the MFD announce, the wake literal, then the WOL magic packet as a burst.
fn beacon_request_packets() -> Vec<&'static [u8]> {
    let mut packets: Vec<&'static [u8]> = vec![&RAYMARINE_MFD_BEACON, &RAYMARINE_WAKE_RADAR];
    for _ in 0..WOL_WAKE_BURST {
        packets.push(&RAYMARINE_WOL_RADAR);
    }
    packets
}

/// Register the Raymarine locator's beacon/wake multicast groups.
///
/// The wired/RayNet group (`224.0.0.1:5800`) is always registered; the WiFi
/// group (`232.1.1.1:5800`) is added on top when `--allow-wifi` is set. No-op
/// if a Raymarine locator is already registered.
pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
        // The wired/RayNet beacon group is always needed — radomes and MFDs on
        // RayNet announce and listen on 224.0.0.1:5800. `--allow-wifi`
        // additionally enables the WiFi discovery group (232.1.1.1:5800); it
        // must *add* that group, not replace the wired one, or enabling WiFi
        // support would silently stop wired/RayNet discovery and wake.
        let mut beacon_addresses = vec![&RAYMARINE_BEACON_ADDRESS];
        if args.allow_wifi {
            beacon_addresses.push(&RAYMARINE_QUANTUM_WIFI_ADDRESS);
        }

        for beacon_address in beacon_addresses {
            addresses.push(
                LocatorAddress::new(
                    LocatorId::Raymarine,
                    beacon_address,
                    Brand::Raymarine,
                    beacon_request_packets(),
                    Box::new(RaymarineLocator::new(args.clone())),
                )
                .with_beacon_multicast_ttl(RAYMARINE_RELAY_TTL),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use clap::Parser;

    use super::{BaseModel, RAYMARINE_BEACON_ADDRESS, RAYMARINE_QUANTUM_WIFI_ADDRESS, protocol};
    use crate::brand::LocatorId;
    use crate::locator::LocatorAddress;
    use crate::{Cli, brand::raymarine::RaymarineLocator, radar::SharedRadars};

    fn raymarine_beacon_groups(extra_args: &[&str]) -> Vec<std::net::SocketAddr> {
        let mut argv = vec!["mayara-server"];
        argv.extend_from_slice(extra_args);
        let args = Cli::parse_from(argv);
        let mut addresses: Vec<LocatorAddress> = Vec::new();
        super::new(&args, &mut addresses);
        addresses
            .iter()
            .filter(|a| a.id == LocatorId::Raymarine)
            .map(|a| a.address)
            .collect()
    }

    #[test]
    fn default_beacons_to_wired_group_only() {
        let groups = raymarine_beacon_groups(&[]);
        assert_eq!(groups, vec![RAYMARINE_BEACON_ADDRESS]);
    }

    #[test]
    fn allow_wifi_adds_wifi_group_without_dropping_wired() {
        // --allow-wifi must *add* the WiFi discovery group, not replace the
        // wired/RayNet one — otherwise enabling WiFi support silently stops
        // wired discovery and wake (the radar/MFD listen on 224.0.0.1:5800).
        let groups = raymarine_beacon_groups(&["--allow-wifi"]);
        assert_eq!(
            groups,
            vec![RAYMARINE_BEACON_ADDRESS, RAYMARINE_QUANTUM_WIFI_ADDRESS],
            "--allow-wifi must register exactly the wired group plus the WiFi group, with no duplicates or extra groups"
        );
    }

    #[test]
    fn wol_wake_is_sent_as_burst() {
        // An Axiom wakes a radar with a burst of WOLs, never a single one —
        // a lone multicast datagram to a dozing WiFi radar is routinely lost.
        // Every Raymarine beacon group (wired, and the WiFi group added by
        // --allow-wifi) must get the MFD announce and wake literal once each,
        // the WOL magic packet repeated as a burst, and the relay TTL.
        for extra_args in [&[][..], &["--allow-wifi"][..]] {
            let args = Cli::parse_from(std::iter::once("mayara-server").chain(extra_args.to_vec()));
            let mut addresses: Vec<LocatorAddress> = Vec::new();
            super::new(&args, &mut addresses);
            let raymarine: Vec<_> = addresses
                .iter()
                .filter(|a| a.id == LocatorId::Raymarine)
                .collect();
            assert!(!raymarine.is_empty());
            for a in raymarine {
                let wols = a
                    .beacon_request_packets
                    .iter()
                    .filter(|p| **p == super::RAYMARINE_WOL_RADAR)
                    .count();
                assert_eq!(wols, super::WOL_WAKE_BURST, "group {}", a.address);
                assert_eq!(
                    a.beacon_request_packets.len(),
                    super::WOL_WAKE_BURST + 2,
                    "group {}: expected MFD announce + wake literal + WOL burst",
                    a.address
                );
                assert_eq!(
                    a.beacon_multicast_ttl,
                    Some(super::RAYMARINE_RELAY_TTL),
                    "group {}: Raymarine beacons must carry the relay-eligible TTL",
                    a.address
                );
            }
        }
    }

    #[test]
    fn decode_raymarine_locator_beacon() {
        let args = Cli::parse_from(["my_program"]);

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

        let radars = &SharedRadars::new();
        let mut state = RaymarineLocator::new(args.clone());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA1_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let (r, model) = r.unwrap();
        log::debug!("Radar: {:?} model: {:?}", r, model);
        assert_eq!(model, BaseModel::Quantum);
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

        let mut state = RaymarineLocator::new(args.clone());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA2_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let (r, model) = r.unwrap();
        log::debug!("Radar: {:?} model: {:?}", r, model);
        assert_eq!(model, BaseModel::Quantum);
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

        let mut state = RaymarineLocator::new(args);
        let r = state.process_beacon_36_report(&DATA3_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA3_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA3_36, &VIA, radars);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let (r, model) = r.unwrap();
        log::debug!("Radar: {:?} model: {:?}", r, model);
        assert_eq!(model, BaseModel::RD);
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

    #[test]
    fn rd418d_dome_discovery() {
        // Real beacons from an RD418D digital radome (issue #419): radar at
        // 10.18.106.155 announcing to 224.0.0.1:5800 on RayNet.
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);
        let radars = &SharedRadars::new();
        const SRC: Ipv4Addr = Ipv4Addr::new(10, 18, 106, 155);

        // 56-byte identity beacon: subtype 0x0b, model "Ethernet Dome".
        const BEACON_56: [u8; 56] = [
            0x01, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x53, 0xc0, 0xc0, 0xb8, 0x68, 0x00,
            0x00, 0x00, 0x9b, 0x6a, 0x12, 0x0a, 0x45, 0x74, 0x68, 0x65, 0x72, 0x6e, 0x65, 0x74,
            0x20, 0x44, 0x6f, 0x6d, 0x65, 0x00, 0x06, 0x00, 0xa0, 0x02, 0x05, 0x00, 0xfc, 0xc0,
            0x06, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xfd, 0xff, 0xff, 0xff, 0x00, 0x00, 0x05, 0x00,
        ];
        // 36-byte address beacon subtype 0x01 (RD): report=226.77.83.98:2572,
        // command=10.18.106.155:2573.
        const BEACON_36_RD: [u8; 36] = [
            0x00, 0x00, 0x00, 0x00, 0x53, 0xc0, 0xc0, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x1e, 0x00, 0x06, 0x08, 0x10, 0x00, 0x62, 0x53, 0x4d, 0xe2, 0x0c, 0x0a, 0x00, 0x00,
            0x9b, 0x6a, 0x12, 0x0a, 0x0d, 0x0a, 0x00, 0x00,
        ];
        // Service beacons the dome also emits each cycle (subtypes 0x24 and
        // 0x1b); both must be ignored without creating a radar.
        const BEACON_36_X24: [u8; 36] = [
            0x00, 0x00, 0x00, 0x00, 0x53, 0xc0, 0xc0, 0xb8, 0x24, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x1e, 0x00, 0x06, 0x08, 0x10, 0x00, 0x00, 0x01, 0x00, 0x00, 0x04, 0x08, 0x00, 0x00,
            0x9b, 0x6a, 0x12, 0x0a, 0x04, 0x08, 0x00, 0x00,
        ];
        const BEACON_36_X1B: [u8; 36] = [
            0x00, 0x00, 0x00, 0x00, 0x53, 0xc0, 0xc0, 0xb8, 0x1b, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x1e, 0x00, 0x06, 0x08, 0x10, 0x00, 0x02, 0x00, 0x00, 0xe0, 0xa9, 0x16, 0x00, 0x00,
            0x9b, 0x6a, 0x12, 0x0a, 0xaa, 0x16, 0x00, 0x00,
        ];

        // Address beacon before the identity beacon: link unknown, ignored.
        assert!(
            locator
                .process_beacon_36_report(&BEACON_36_RD, &SRC, radars)
                .unwrap()
                .is_none()
        );

        locator.process_beacon_56_report(&BEACON_56, &SRC).unwrap();
        let state = locator
            .ids
            .get(&0xb8c0c053)
            .expect("dome identity beacon must register its link_id");
        assert_eq!(state.model, BaseModel::RD);
        assert_eq!(state.model_name.as_deref(), Some("Ethernet Dome"));

        for service in [&BEACON_36_X24, &BEACON_36_X1B] {
            assert!(
                locator
                    .process_beacon_36_report(service, &SRC, radars)
                    .unwrap()
                    .is_none(),
                "service subtypes must not create a radar"
            );
        }

        let (info, model) = locator
            .process_beacon_36_report(&BEACON_36_RD, &SRC, radars)
            .unwrap()
            .expect("radar should be created");
        assert_eq!(model, BaseModel::RD);
        assert_eq!(info.controls.model_name(), Some("RD".to_string()));
        assert_eq!(
            info.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 18, 106, 155), 2573)
        );
        assert_eq!(
            info.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(226, 77, 83, 98), 2572)
        );
        assert_eq!(info.spoke_data_addr, info.report_addr);
    }

    #[test]
    fn mfd_type2_beacon_is_ignored() {
        // MFDs (e.g. an e7D) emit a 56-byte type-2 beacon (subtype 0x1e,
        // empty model) alongside their type-1 MFD announcement. It is not a
        // radar identity and must not register a link_id.
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);

        let mut beacon = [0u8; protocol::beacon56::LEN];
        beacon[0..4].copy_from_slice(&2u32.to_le_bytes());
        beacon[4..8].copy_from_slice(&0x1eu32.to_le_bytes());
        beacon[8..12].copy_from_slice(&0xbcc07c73u32.to_le_bytes());

        let src = Ipv4Addr::new(10, 26, 8, 43);
        locator.process_beacon_56_report(&beacon, &src).unwrap();
        assert!(
            locator.ids.is_empty(),
            "type-2 MFD beacon should not register a link_id"
        );
    }

    /// Parse fixture lines: `timestamp src_ip dst_ip:port payload_hex`
    fn parse_fixture(text: &str) -> Vec<(Ipv4Addr, Ipv4Addr, u16, Vec<u8>)> {
        let mut packets = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.splitn(4, ' ').collect();
            if parts.len() < 4 {
                continue;
            }
            let src_ip: Ipv4Addr = parts[1].parse().expect("bad src ip");
            let dst_parts: Vec<&str> = parts[2].splitn(2, ':').collect();
            let dst_ip: Ipv4Addr = dst_parts[0].parse().expect("bad dst ip");
            let dst_port: u16 = dst_parts[1].parse().expect("bad port");
            // Decode hex without external crate
            let hex = parts[3];
            let payload: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("bad hex"))
                .collect();
            packets.push((src_ip, dst_ip, dst_port, payload));
        }
        packets
    }

    const PELAGIA_FIXTURE: &str = include_str!("testdata/quantum-boot.txt");

    #[test]
    fn w3_beacon_is_ignored() {
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);

        // Synthetic W3 56-byte beacon: beacon_type=1, subtype=0x4d, link_id=0xAABBCCDD
        let mut w3_beacon = [0u8; protocol::beacon56::LEN];
        w3_beacon[0..4].copy_from_slice(&1u32.to_le_bytes()); // beacon_type
        w3_beacon[4..8].copy_from_slice(&(protocol::beacon56::W3).to_le_bytes());
        w3_beacon[8..12].copy_from_slice(&0xAABBCCDDu32.to_le_bytes());
        w3_beacon[20..31].copy_from_slice(b"Quantum_W3\0");

        let src = Ipv4Addr::new(198, 18, 0, 1);
        locator.process_beacon_56_report(&w3_beacon, &src).unwrap();
        assert!(
            locator.ids.is_empty(),
            "W3 beacon should not register a link_id"
        );
    }

    #[test]
    fn pelagia_full_discovery() {
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);
        let radars = &SharedRadars::new();
        let packets = parse_fixture(PELAGIA_FIXTURE);

        for (src, _, port, data) in &packets {
            if *port != 5800 {
                continue;
            }
            match data.len() {
                protocol::beacon56::LEN => {
                    let _ = locator.process_beacon_56_report(data, src);
                }
                protocol::beacon36::LEN => {
                    if let Ok(Some((info, model))) =
                        locator.process_beacon_36_report(data, src, radars)
                    {
                        assert_eq!(model, BaseModel::Quantum);
                        assert_eq!(
                            info.report_addr,
                            SocketAddrV4::new(Ipv4Addr::new(232, 1, 160, 1), 2574),
                            "report address should be the direct Quantum multicast"
                        );
                        assert_eq!(
                            info.send_command_addr,
                            SocketAddrV4::new(Ipv4Addr::new(198, 18, 0, 158), 2575),
                        );
                        return;
                    }
                }
                _ => {}
            }
        }
        panic!("No radar was created from the pelagia fixture beacons");
    }

    #[test]
    fn wake_signals_recognized() {
        use super::{
            RAYMARINE_MFD_BEACON, RAYMARINE_WAKE_RADAR, RAYMARINE_WOL_RADAR,
            is_external_controller_signal,
        };

        assert!(
            is_external_controller_signal(&RAYMARINE_WAKE_RADAR),
            "ABCDEFGHIJKLMNOP should be flagged as an external-controller signal"
        );
        assert!(
            is_external_controller_signal(&RAYMARINE_WOL_RADAR),
            "WOL signature should be flagged as an external-controller signal"
        );
        assert!(
            is_external_controller_signal(&RAYMARINE_MFD_BEACON),
            "MFD announcement beacon should be flagged"
        );
    }

    #[test]
    fn radar_beacon_is_not_a_controller_signal() {
        use super::is_external_controller_signal;

        // A Quantum radar identity beacon (subtype 0x66) must not be confused
        // with an MFD announcement — only the MFD subtype should mark.
        let mut radar_beacon = [0u8; protocol::beacon56::LEN];
        radar_beacon[0..4].copy_from_slice(&1u32.to_le_bytes());
        radar_beacon[4..8].copy_from_slice(&(protocol::beacon56::QUANTUM).to_le_bytes());
        assert!(!is_external_controller_signal(&radar_beacon));
    }

    #[test]
    fn witness_quiet_until_marked() {
        use super::ExternalControllerWitness;
        use std::time::Duration;

        let witness = ExternalControllerWitness::default();
        assert!(witness.quiet_for(Duration::from_secs(60)));
        witness.mark();
        // Just after marking it should not be quiet for any non-zero window.
        assert!(!witness.quiet_for(Duration::from_secs(60)));
        // A zero-window query treats "just marked" as already-elapsed and quiet.
        assert!(witness.quiet_for(Duration::from_secs(0)));
    }

    #[test]
    fn quantum_behind_mfd_ap_uses_unicast_report_stream() {
        // A Quantum behind an Axiom MFD acting as WiFi AP advertises an
        // unspecified report address (0.0.0.0:0) in its 36-byte beacon and
        // streams reports/spokes unicast back to the controller instead.
        // Real beacons from research capture Q2_with_Axiom_as_wifi_AP
        // (link_id 0xD681C8C3, "QuantumRadar", radar at 192.168.143.84).
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);
        let radars = &SharedRadars::new();
        const SRC: Ipv4Addr = Ipv4Addr::new(192, 168, 143, 84);

        // 56-byte identity beacon: subtype 0x66, model "QuantumRadar".
        const BEACON_56: [u8; 56] = [
            0x01, 0x00, 0x00, 0x00, 0x66, 0x00, 0x00, 0x00, 0xC3, 0xC8, 0x81, 0xD6, 0x03, 0x01,
            0x00, 0x00, 0x54, 0x8F, 0xA8, 0xC0, 0x51, 0x75, 0x61, 0x6E, 0x74, 0x75, 0x6D, 0x52,
            0x61, 0x64, 0x61, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        ];
        // 36-byte address beacon: report=0.0.0.0:0, command=192.168.143.84:2575.
        const BEACON_36: [u8; 36] = [
            0x00, 0x00, 0x00, 0x00, 0xC3, 0xC8, 0x81, 0xD6, 0x28, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x64, 0x00, 0x06, 0x08, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00,
            0x54, 0x8F, 0xA8, 0xC0, 0x0F, 0x0A, 0x37, 0x00,
        ];

        locator.process_beacon_56_report(&BEACON_56, &SRC).unwrap();
        let (info, model) = locator
            .process_beacon_36_report(&BEACON_36, &SRC, radars)
            .unwrap()
            .expect("radar should be created");

        // In the unicast topology all four addresses on RadarInfo collapse
        // onto the radar's command address — there is no separate listen
        // address or multicast group.
        let radar = SocketAddrV4::new(Ipv4Addr::new(192, 168, 143, 84), 2575);
        assert_eq!(model, BaseModel::Quantum);
        assert_eq!(info.addr, radar);
        assert_eq!(info.send_command_addr, radar);
        assert_eq!(
            info.report_addr, radar,
            "unicast topology: report_addr collapses onto the radar's command address"
        );
        assert_eq!(info.spoke_data_addr, radar);
    }

    #[test]
    fn discovered_quantum_without_ranges_is_visible_but_not_active() {
        // A Quantum that has been located via beacon but is still asleep has
        // no ranges yet. It must still surface from get_discovered()/
        // have_discovered() (so the /radars listing shows it and the locator
        // stops hunting), while get_active()/have_active() stay empty until a
        // status report fills the ranges. The locator relying on
        // have_discovered() is what lets the external-controller witness fall
        // quiet so the WiFi wake nudge can eventually fire — see report.rs.
        let args = Cli::parse_from(["mayara-server"]);
        let mut locator = RaymarineLocator::new(args);
        let radars = &SharedRadars::new();
        const SRC: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 31);

        // 56-byte identity beacon: subtype 0x66, model "QuantumRadar".
        const BEACON_56: [u8; 56] = [
            0x01, 0x00, 0x00, 0x00, 0x66, 0x00, 0x00, 0x00, 0x48, 0x81, 0x81, 0xD6, 0x03, 0x01,
            0x00, 0x00, 0x13, 0x2B, 0xA8, 0xC0, 0x51, 0x75, 0x61, 0x6E, 0x74, 0x75, 0x6D, 0x52,
            0x61, 0x64, 0x61, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        ];
        // 36-byte address beacon with a real MULTICAST report group
        // (report=232.1.19.1:2574, command=198.18.2.59:2575) — the topology
        // from the field capture, distinct from the MFD-as-AP unicast case.
        const BEACON_36: [u8; 36] = [
            0x00, 0x00, 0x00, 0x00, 0x48, 0x81, 0x81, 0xD6, 0x28, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x64, 0x00, 0x06, 0x08, 0x10, 0x00, 0x01, 0x13, 0x01, 0xE8, 0x0E, 0x0A, 0x00, 0x20,
            0x3B, 0x02, 0x12, 0xC6, 0x0F, 0x0A, 0x00, 0x00,
        ];

        locator.process_beacon_56_report(&BEACON_56, &SRC).unwrap();
        let (info, model) = locator
            .process_beacon_36_report(&BEACON_36, &SRC, radars)
            .unwrap()
            .expect("radar should be created");
        assert_eq!(model, BaseModel::Quantum);
        assert!(
            info.ranges.is_empty(),
            "a freshly located radar has no ranges yet"
        );

        // Register it the way the locator's found() does.
        radars.add(info);

        assert!(
            radars.have_discovered(),
            "a located radar must count as discovered even without ranges"
        );
        assert_eq!(
            radars.get_discovered().len(),
            1,
            "the rangeless radar must surface from get_discovered()"
        );
        assert!(!radars.have_active(), "a rangeless radar is not yet active");
        assert!(
            radars.get_active().is_empty(),
            "get_active() stays empty until ranges arrive"
        );
    }

    #[test]
    fn features_flags_parsed() {
        let packets = parse_fixture(PELAGIA_FIXTURE);
        let features_pkt = packets
            .iter()
            .find(|(_, _, port, data)| {
                *port == 2574
                    && data.len() >= 8
                    && u32::from_le_bytes(data[0..4].try_into().unwrap()) == 0x280007
            })
            .expect("no features message in fixture");

        let flags = u32::from_le_bytes(features_pkt.3[4..8].try_into().unwrap());
        let features = super::report::FeatureFlags { raw: flags };
        assert!(!features.is_cyclone(), "Q24D is not a Cyclone");
        assert!(features.has_doppler(), "Q24D has Doppler");
        assert!(features.has_marpa(), "Q24D has MARPA");
    }
}
