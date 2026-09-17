use log::log_enabled;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::radar::{RadarInfo, SharedRadars, identity_discriminator, mac_identity};
use crate::util::{PrintableSlice, c_string, decode_exact, decode_head};
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
    REQUEST_BEACON_PACKET, REQUEST_MODEL_PACKET, RadarModel, SPOKE_DATA_MULTICAST_ADDRESS,
    SPOKE_LEN, SPOKES,
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
        return Err(io::Error::other(format!("Unexpected reply {:?}", buf)));
    }
    stream.read_exact(&mut buf[0..4])?;

    let port = login_reply_port(buf[0], buf[1])?;
    log::debug!(
        "Furuno radar logged in; using port {} for report/command data",
        port
    );
    Ok(port)
}

/// The port the radar tells us to talk to, as an offset from [`BASE_PORT`]
/// in the first two bytes of the login reply, most significant byte first.
///
/// The offset comes off the socket, so it can say anything: an offset that
/// would carry the port past the end of the port range is a reply we cannot
/// act on, not a port to wrap around to.
fn login_reply_port(high: u8, low: u8) -> Result<u16, io::Error> {
    let offset = u16::from_be_bytes([high, low]);

    BASE_PORT.checked_add(offset).ok_or_else(|| {
        io::Error::other(format!(
            "login reply asks for port {} + {}, which is not a port",
            BASE_PORT, offset
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::login_reply_port;
    use crate::brand::furuno::protocol::BASE_PORT;

    /// The radar names its report port as an offset from the base port.
    #[test]
    fn a_login_reply_names_a_port() {
        assert_eq!(login_reply_port(0x00, 0x00).unwrap(), BASE_PORT);
        assert_eq!(login_reply_port(0x00, 0x18).unwrap(), BASE_PORT + 0x18);
        assert_eq!(login_reply_port(0x01, 0x00).unwrap(), BASE_PORT + 256);
    }

    /// The offset arrives off the socket, so it can name a port that does not
    /// exist. That used to carry the sum past the end of a u16 and panic.
    #[test]
    fn a_login_reply_past_the_last_port_is_refused() {
        assert!(login_reply_port(0xff, 0xff).is_err());
        assert!(login_reply_port(0xd9, 0x00).is_err());

        // The last offset that still lands inside the port range.
        let last = u16::MAX - BASE_PORT;
        assert_eq!(
            login_reply_port((last >> 8) as u8, last as u8).unwrap(),
            u16::MAX
        );
        assert!(login_reply_port(((last + 1) >> 8) as u8, (last + 1) as u8).is_err());
    }
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

            info.start_forwarding_radar_messages_to_stdout(subsys);

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
                ib.start_forwarding_radar_messages_to_stdout(subsys);
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
            subsys.start(SubsystemBuilder::new(
                report_name,
                async move |s: &mut SubsystemHandle| report_receiver.run(s).await,
            ));
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
        match decode_head::<FurunoRadarReport>(report) {
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
                    let radar_addr: SocketAddrV4 = *from;

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
        match decode_exact::<FurunoRadarModelReport>(report) {
            Ok(data) => {
                let model = c_string(&data.model);
                let serial_no = c_string(&data.serial_no);
                // NavNet 3D era units report an all-zero serial; their MAC is
                // the only thing telling two of them apart.
                let mac_id = mac_identity(&data.mac);
                let discriminator = identity_discriminator(serial_no, mac_id.as_deref());
                log::trace!(
                    "{}: Furuno model report: {}",
                    from,
                    PrintableSlice::new(report)
                );
                log::debug!("{}: model: {:?}", from, model);
                log::debug!("{}: serial_no: {:?}", from, serial_no);
                log::debug!("{}: mac: {:?}", from, mac_id);

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
                let send_command_addr: SocketAddrV4 = report_addr;

                // NXT models support dual range
                let is_dual_range = model.contains("NXT");

                // Range A (or only range for non-dual models)
                let dual_suffix = if is_dual_range { Some("A") } else { None };
                let radar_info = RadarInfo::new(
                    radars,
                    &self.args,
                    Brand::Furuno,
                    serial_no,
                    mac_id.as_deref(),
                    dual_suffix,
                    PIXEL_VALUES,
                    SPOKES,
                    SPOKE_LEN,
                    *from,
                    *nic_addr,
                    spoke_data_addr,
                    report_addr,
                    send_command_addr,
                    |id, tx| settings::new(id, tx, &self.args),
                    true,
                    true,
                );

                radar_info.controls.set_model_name(model.to_string());
                radar_info.controls.set_user_name(
                    format!("{model} {}", discriminator.unwrap_or(""))
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
                        mac_id.as_deref(),
                        Some("B"),
                        PIXEL_VALUES,
                        SPOKES,
                        SPOKE_LEN,
                        *from,
                        *nic_addr,
                        spoke_data_addr,
                        report_addr,
                        send_command_addr,
                        |id, tx| settings::new(id, tx, &self.args),
                        true,
                        true,
                    );
                    info_b.controls.set_model_name(model.to_string());
                    info_b.controls.set_user_name(
                        format!("{model} {} B", discriminator.unwrap_or(""))
                            .trim()
                            .to_string(),
                    );
                    info_b.controls.set_spoke_processing(2);
                    Some(info_b)
                } else {
                    None
                };

                self.found(radar_info, info_b, radars, subsys, model);
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

#[cfg(test)]
mod tests {
    use super::{
        BEACON_REPORT_HEADER, BEACON_REPORT_LENGTH_MIN, FurunoRadarModelReport, FurunoRadarReport,
        MODEL_REPORT_LENGTH,
    };
    use crate::util::{decode_exact, decode_head};

    /// Both reports are read at a length stated as a constant, and the
    /// dispatcher matches on those same constants: a beacon report has to be
    /// at least this long, a model report exactly that long. `decode_exact`
    /// fails both when a struct wants more bytes than the constant and when it
    /// leaves some unread, which is what holds the declaration and the
    /// constant together now that `size_of` no longer can.
    #[test]
    fn the_reports_are_as_long_as_the_dispatcher_expects() {
        assert!(decode_exact::<FurunoRadarReport>(&[0u8; BEACON_REPORT_LENGTH_MIN]).is_ok());
        assert!(decode_head::<FurunoRadarReport>(&[0u8; BEACON_REPORT_LENGTH_MIN - 1]).is_err());

        assert!(decode_exact::<FurunoRadarModelReport>(&[0u8; MODEL_REPORT_LENGTH]).is_ok());
        assert!(decode_head::<FurunoRadarModelReport>(&[0u8; MODEL_REPORT_LENGTH - 1]).is_err());
    }

    /// A real beacon report carries more than the part we declare -- 32 bytes
    /// from a DRS-4D NXT, and what follows differs by model -- so it is read
    /// as a head rather than exactly.
    #[test]
    fn a_beacon_report_reads_past_its_declaration() {
        // The DRS-4D NXT capture documented in protocol.rs: 32 bytes, name
        // "RD003212", byte 11 = 0x18 = 24 = the length after the outer header.
        let mut packet = [0u8; 32];
        packet[0..11].copy_from_slice(&BEACON_REPORT_HEADER);
        packet[11] = 0x18;
        packet[16..24].copy_from_slice(b"RD003212");

        let report: FurunoRadarReport = decode_head(&packet).expect("a beacon report");

        assert_eq!(report.length as usize + 8, packet.len());
        assert_eq!(&report.name, b"RD003212");
        // Byte 16 is what the dispatcher tests for 'R'.
        assert_eq!(report.name[0], b'R');
    }
}
