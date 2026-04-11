use anyhow::{Error, bail};
use bincode::deserialize;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::brand::RadarLocator;
use crate::locator::LocatorAddress;
use crate::network::LittleEndianSocketAddrV4;
use crate::radar::settings::ControlId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{PrintableSlice, c_string};
use crate::{Brand, Cli};

use super::LocatorId;

mod command;
mod navdata;
mod protocol;
mod report;
mod settings;

const RD_SPOKES_PER_REVOLUTION: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
const RD_SPOKE_LEN: usize = 1024;

const QUANTUM_SPOKES_PER_REVOLUTION: usize = 250;
const QUANTUM_SPOKE_LEN: usize = 252;

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
            // Probably supports higher resulution though...
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

/*
Let's take a look at what Raymarine radars send in their beacons.
First of all, it looks as if all ethernet devices send a beacon of length 56 bytes,
and that they also send a beacon of length 36 bytes.

The observation so far is that the 56 byte beacon contains a 4 byte "link_id" field,
which the next 36 byte beacon also contains.

We put them in a map for now, but probably we only need to store the last one.
 */

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
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
#[repr(packed)]
struct RaymarineBeacon56 {
    beacon_type: [u8; 4], // 0: always 1
    subtype: [u8; 4],     // 4
    link_id: [u8; 4],     // 8
    _field4: [u8; 4],     // 12
    _field5: [u8; 4],     // 16
    model_name: [u8; 32], // 20: String like "QuantumRadar" when subtype = 0x66
    _field7: [u8; 4],     // 52
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum BaseModel {
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

#[derive(Clone)]
struct RaymarineLocator {
    args: Cli,
    ids: HashMap<LinkId, RadarState>,
}

impl RaymarineLocator {
    fn new(args: Cli) -> Self {
        RaymarineLocator {
            args,
            ids: HashMap::new(),
        }
    }

    fn process_beacon_36_report(
        &mut self,
        report: &[u8],
        from: &Ipv4Addr,
        radars: &SharedRadars,
    ) -> Result<Option<RadarInfo>, Error> {
        match deserialize::<RaymarineBeacon36>(report) {
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
                            if subtype != 0x28 {
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
                                0x01 => {} // Continue
                                8 | 21 | 26 | 27 | 30 | 35 => {
                                    // Known unknowns
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
                        BaseModel::Quantum => (QUANTUM_SPOKES_PER_REVOLUTION, QUANTUM_SPOKE_LEN),
                        BaseModel::RD => (RD_SPOKES_PER_REVOLUTION, RD_SPOKE_LEN),
                    };

                    let radar_addr: SocketAddrV4 = data.report.into();
                    let radar_send: SocketAddrV4 = data.command.into();

                    let location_info: RadarInfo = RadarInfo::new(
                        radars,
                        &self.args,
                        Brand::Raymarine,
                        None,
                        None,
                        0,
                        spokes_per_revolution,
                        max_spoke_len,
                        radar_addr.into(),
                        from.clone(),
                        radar_addr.into(),
                        radar_addr.into(),
                        radar_send.into(),
                        |id, tx| settings::new(id, tx, &self.args, info.model),
                        doppler,
                        false,
                    );

                    return Ok(Some(location_info));
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
        match deserialize::<RaymarineBeacon56>(report) {
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
                    log::warn!(
                        "{}: Raymarine 56 report: unexpected beacon type {}",
                        from,
                        beacon_type
                    );
                    return Ok(());
                }

                let link_id = u32::from_le_bytes(data.link_id);
                let subtype = u32::from_le_bytes(data.subtype);

                match subtype {
                    0x66 => {
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
                    0x01 => {
                        let model = BaseModel::RD;
                        let model_name = Some(model.to_string());

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
                    0x4d => {
                        // W3 wireless bridge beacon — the radar also sends
                        // a direct Quantum beacon (subtype 0x66) with the
                        // correct data addresses. Ignore the W3 identity
                        // and let the direct path handle discovery.
                        log::trace!("{}: W3 bridge beacon (ignored, using direct Quantum)", from,);
                    }
                    0x11 => {
                        // Request from an MFD, ignore it
                    }
                    _ => {
                        // log::w   arn!("{}: Raymarine 56 report: unknown subtype {}", from, subtype);
                    }
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string(&ControlId::UserName, info.key())
            .unwrap();

        if let Some(mut info) = radars.add(info) {
            // It's new, start the RadarProcessor thread
            info.start_forwarding_radar_messages_to_stdout(&subsys);

            let report_name = info.key();
            radars.update(&mut info);

            // Start the NavData sender to feed position/heading to the
            // radar every 100ms. Required for Doppler and MARPA.
            let send_addr = info.send_command_addr;
            let navdata_name = format!("{}-navdata", report_name);
            subsys.start(SubsystemBuilder::new(navdata_name, move |s| async move {
                match crate::network::create_multicast_send(&send_addr, &Ipv4Addr::UNSPECIFIED) {
                    Ok(sock) => navdata::run(s, sock).await.map_err(|e| e.into()),
                    Err(e) => {
                        log::warn!("Failed to create NavData socket: {}", e);
                        Ok::<(), anyhow::Error>(())
                    }
                }
            }));

            let report_receiver =
                report::RaymarineReportReceiver::new(&self.args, info, radars.clone());

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
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

        match report.len() {
            36 => {
                // Common Raymarine message

                match Self::process_beacon_36_report(self, report, nic_addr, radars) {
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

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(Clone::clone(self))
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

const BEACONS: [&'static [u8]; 3] = [
    &RAYMARINE_MFD_BEACON,
    &RAYMARINE_WAKE_RADAR,
    &RAYMARINE_WOL_RADAR,
];

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
        let beacon_address = if args.allow_wifi {
            &RAYMARINE_QUANTUM_WIFI_ADDRESS
        } else {
            &RAYMARINE_BEACON_ADDRESS
        };

        addresses.push(LocatorAddress::new(
            LocatorId::Raymarine,
            beacon_address,
            Brand::Raymarine,
            BEACONS.to_vec(),
            Box::new(RaymarineLocator::new(args.clone())),
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use clap::Parser;

    use crate::{Cli, brand::raymarine::RaymarineLocator, radar::SharedRadars};

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

        let mut state = RaymarineLocator::new(args.clone());
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
        let packets = parse_fixture(PELAGIA_FIXTURE);

        // Find the W3 56-byte beacon (subtype 0x4d in bytes 4..8)
        for (src, _, port, data) in &packets {
            if *port == 5800 && data.len() == 56 {
                let subtype = u32::from_le_bytes(data[4..8].try_into().unwrap());
                if subtype == 0x4d {
                    locator.process_beacon_56_report(data, src).unwrap();
                    assert!(
                        locator.ids.is_empty(),
                        "W3 beacon should not register a link_id"
                    );
                    return;
                }
            }
        }
        // No W3 beacon in fixture — skip
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
                56 => {
                    let _ = locator.process_beacon_56_report(data, src);
                }
                36 => {
                    if let Ok(Some(info)) = locator.process_beacon_36_report(data, src, radars) {
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
