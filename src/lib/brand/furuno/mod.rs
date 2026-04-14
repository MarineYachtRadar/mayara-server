use bincode::deserialize;
use log::log_enabled;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{PrintableSlice, c_string};
use crate::{Brand, Cli};

use super::{LocatorId, RadarLocator};

mod command;
mod protocol;
mod report;
mod settings;

const REPLAY_FIRMWARE_VERSION: &str = "00.00";

use protocol::{
    ANNOUNCE_MAYARA_PACKET, BASE_PORT, BEACON_ADDRESS, BEACON_REPORT_HEADER,
    BEACON_REPORT_LENGTH_MIN, DATA_PORT, FurunoRadarModelReport, FurunoRadarReport,
    LOGIN_EXPECTED_HEADER, LOGIN_MESSAGE, LOGIN_TIMEOUT, MODEL_REPORT_LENGTH, PIXEL_VALUES,
    RadarModel, REQUEST_BEACON_PACKET, REQUEST_MODEL_PACKET, SPOKE_DATA_MULTICAST_ADDRESS, SPOKES,
    SPOKE_LEN,
};

fn login_to_radar(radar_addr: SocketAddrV4) -> Result<u16, io::Error> {
    let mut stream =
        std::net::TcpStream::connect_timeout(&std::net::SocketAddr::V4(radar_addr), LOGIN_TIMEOUT)?;

    stream.set_write_timeout(Some(LOGIN_TIMEOUT))?;
    stream.set_read_timeout(Some(LOGIN_TIMEOUT))?;

    stream.write_all(&LOGIN_MESSAGE)?;

    let mut buf: [u8; 8] = [0; 8];
    stream.read_exact(&mut buf)?;

    if buf != LOGIN_EXPECTED_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Unexpected reply {:?}", buf),
        ));
    }
    stream.read_exact(&mut buf[0..4])?;

    let port = BASE_PORT + ((buf[0] as u16) << 8) + buf[1] as u16;
    log::debug!(
        "Furuno radar logged in; using port {} for report/command data",
        port
    );
    Ok(port)
}

#[derive(Clone)]
struct FurunoLocator {
    args: Cli,
    half_found: HashMap<SocketAddrV4, RadarInfo>, // When the first of the two reports is found
}

impl RadarLocator for FurunoLocator {
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
        Box::new(Clone::clone(self))
    }
}

impl FurunoLocator {
    fn new(args: Cli) -> Self {
        FurunoLocator {
            args,
            half_found: HashMap::new(),
        }
    }

    fn found(
        &self,
        info: RadarInfo,
        info_b: Option<RadarInfo>,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
        beacon_model: &str,
    ) {
        if let Some(mut info) = radars.add(info) {
            // It's new, start the RadarProcessor thread

            let port: u16 = if !self.args.is_replay() {
                match login_to_radar(info.addr) {
                    Err(e) => {
                        log::error!("{}: Unable to connect for login: {}", info.key(), e);
                        radars.remove(&info.key());
                        return;
                    }
                    Ok(p) => p,
                }
            } else {
                DATA_PORT
            };
            if port != info.send_command_addr.port() {
                // Furuno radars use a single TCP/IP connection to send commands and
                // receive status reports, so report_addr and send_command_addr are identical.
                // Only one of these would be enough for Furuno.
                info.send_command_addr.set_port(port);
                info.report_addr.set_port(port);
            }

            let report_name = info.key();

            info.start_forwarding_radar_messages_to_stdout(&subsys);

            // In replay mode, detect model from the original beacon string
            // (not from controls which persistence may have overwritten).
            let replay_model = if self.args.is_replay() {
                let model = RadarModel::from_model_name(beacon_model);
                log::info!(
                    "{}: Radar model {} detected for replay mode (from beacon {:?})",
                    info.key(),
                    model,
                    beacon_model,
                );
                if model != RadarModel::Unknown {
                    settings::update_when_model_known(&mut info, model, REPLAY_FIRMWARE_VERSION);
                    radars.update(&mut info);
                }
                Some(model)
            } else {
                None
            };

            // Register and configure Range B if this is a dual-range model
            let mut info_b = info_b.and_then(|ib| radars.add(ib));
            if let Some(ref mut ib) = info_b {
                ib.send_command_addr.set_port(port);
                ib.report_addr.set_port(port);
                ib.start_forwarding_radar_messages_to_stdout(&subsys);
                if let Some(model) = replay_model.filter(|m| *m != RadarModel::Unknown) {
                    settings::update_when_model_known(ib, model, REPLAY_FIRMWARE_VERSION);
                    radars.update(ib);
                }
            }

            let mut report_receiver =
                report::FurunoReportReceiver::new(&self.args, radars.clone(), info);
            if let Some(ib) = info_b {
                report_receiver.set_range_b(&self.args, radars, ib);
            }
            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
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

        if report.len() >= BEACON_REPORT_LENGTH_MIN
            && report[16] == b'R'
            && report[0..11] == BEACON_REPORT_HEADER
        {
            self.process_beacon_report(report, from, via)
        } else if report.len() == MODEL_REPORT_LENGTH {
            self.process_beacon_model_report(report, from, via, radars, subsys)
        } else {
            Ok(())
        }
    }

    fn process_beacon_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
    ) -> Result<(), io::Error> {
        match deserialize::<FurunoRadarReport>(report) {
            Ok(data) => {
                if data.length as usize + 8 != report.len() {
                    log::error!(
                        "{}: Furuno report length mismatch: {} != {}",
                        from,
                        data.length,
                        report.len() - 8
                    );
                    return Ok(());
                }
                if self.half_found.contains_key(from) {
                    log::trace!("{}: Found radar address already", from);
                    return Ok(());
                }
                if let Some(name) = c_string(&data.name) {
                    let radar_addr: SocketAddrV4 = from.clone();

                    log::debug!(
                        "Furuno radar '{name}' seen at '{radar_addr} but looking for other report"
                    );
                }
            }
            Err(e) => {
                log::error!(
                    "{} via {}: Failed to decode Furuno radar report: {}",
                    from,
                    nic_addr,
                    e
                );
            }
        }

        Ok(())
    }

    fn process_beacon_model_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        match deserialize::<FurunoRadarModelReport>(report) {
            Ok(data) => {
                let model = c_string(&data.model);
                let serial_no = c_string(&data.serial_no);
                log::trace!(
                    "{}: Furuno model report: {}",
                    from,
                    PrintableSlice::new(report)
                );
                log::debug!("{}: model: {:?}", from, model);
                log::debug!("{}: serial_no: {:?}", from, serial_no);

                let model = match model {
                    Some(t) => t,
                    None => {
                        return Ok(());
                    }
                };
                if !(model.starts_with("DRS") || model.starts_with("FAR")) {
                    return Ok(());
                }

                let spoke_data_addr = SPOKE_DATA_MULTICAST_ADDRESS;

                let report_addr: SocketAddrV4 = SocketAddrV4::new(*from.ip(), 0); // Port is set in login_to_radar
                let send_command_addr: SocketAddrV4 = report_addr.clone();

                // NXT models support dual range
                let is_dual_range = model.contains("NXT");

                // Range A (or only range for non-dual models)
                let dual_suffix = if is_dual_range { Some("A") } else { None };
                let radar_info = RadarInfo::new(
                    radars,
                    &self.args,
                    Brand::Furuno,
                    serial_no,
                    dual_suffix,
                    PIXEL_VALUES,
                    SPOKES,
                    SPOKE_LEN,
                    *from,
                    nic_addr.clone(),
                    spoke_data_addr,
                    report_addr,
                    send_command_addr,
                    |id, tx| settings::new(id, tx, &self.args),
                    true,
                    true,
                );

                radar_info.controls.set_model_name(model.to_string());
                radar_info.controls.set_user_name(
                    format!("{model} {}", serial_no.unwrap_or(""))
                        .trim()
                        .to_string(),
                );
                // Furuno radars report more spokes than they send, default to "Reduce" mode (2)
                radar_info.controls.set_spoke_processing(2);

                // Range B for dual-range NXT models
                let info_b = if is_dual_range {
                    let info_b = RadarInfo::new(
                        radars,
                        &self.args,
                        Brand::Furuno,
                        serial_no,
                        Some("B"),
                        PIXEL_VALUES,
                        SPOKES,
                        SPOKE_LEN,
                        *from,
                        nic_addr.clone(),
                        spoke_data_addr,
                        report_addr,
                        send_command_addr,
                        |id, tx| settings::new(id, tx, &self.args),
                        true,
                        true,
                    );
                    info_b.controls.set_model_name(model.to_string());
                    info_b.controls.set_user_name(
                        format!("{model} {} B", serial_no.unwrap_or(""))
                            .trim()
                            .to_string(),
                    );
                    info_b.controls.set_spoke_processing(2);
                    Some(info_b)
                } else {
                    None
                };

                self.found(radar_info, info_b, radars, subsys, &model);
            }
            Err(e) => {
                log::error!(
                    "{} via {}: Failed to decode Furuno radar report: {}",
                    from,
                    nic_addr,
                    e
                );
            }
        }

        Ok(())
    }
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Furuno) {
        addresses.push(LocatorAddress::new(
            LocatorId::Furuno,
            &BEACON_ADDRESS,
            Brand::Furuno,
            vec![
                &REQUEST_BEACON_PACKET,
                &REQUEST_MODEL_PACKET,
                &ANNOUNCE_MAYARA_PACKET,
            ],
            Box::new(FurunoLocator::new(args.clone())),
        ));
    }
}
