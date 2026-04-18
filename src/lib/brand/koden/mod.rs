use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::PrintableSlice;
use crate::{Brand, Cli};

use super::{LocatorId, RadarLocator};

mod command;
mod protocol;
mod report;
mod settings;

use protocol::{
    BEACON_ADDRESS, CONTROL_PREFIX, IMAGE_MARKER, IMG_MIN_SIZE, KEEPALIVE_PACKET, PIXEL_VALUES,
    RADAR_PORT, RESP_POWER, RESP_WARMUP, SPOKE_LEN, SPOKES, STATUS_PREFIX,
};

#[derive(Clone)]
struct KodenLocator {
    args: Cli,
}

impl RadarLocator for KodenLocator {
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

impl KodenLocator {
    fn new(args: Cli) -> Self {
        KodenLocator { args }
    }

    fn process_locator_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < 3 {
            return Ok(());
        }

        // Koden radars respond with control (&) or status (#) packets.
        // We detect the radar when we receive any valid response from it.
        let first = report[0];
        let is_koden = match first {
            CONTROL_PREFIX => {
                let cmd = report[1];
                cmd == RESP_POWER || cmd == RESP_WARMUP || cmd == b'e'
            }
            STATUS_PREFIX => {
                let cmd = report[1];
                // Model info, model code, MAC address, or keepalive ACK are
                // reliable indicators of a Koden radar.
                cmd == 0x4E || cmd == 0x72 || cmd == 0xA7 || cmd == 0xAB || cmd == 0xFF
            }
            b'{' => {
                // Image data frame — only if it has the full 4-byte marker
                report.len() >= IMG_MIN_SIZE && report[0..4] == IMAGE_MARKER
            }
            _ => false,
        };

        if !is_koden {
            return Ok(());
        }

        log::debug!(
            "Koden radar detected at {} via {} (packet: {})",
            from,
            nic_addr,
            PrintableSlice::new(&report[..report.len().min(16)])
        );

        self.found(*from, *nic_addr, radars, subsys);
        Ok(())
    }

    fn found(
        &self,
        radar_addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let spoke_data_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);
        let report_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);
        let send_command_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);

        let radar_info = RadarInfo::new(
            radars,
            &self.args,
            Brand::Koden,
            None, // serial number discovered later
            None, // no dual range
            PIXEL_VALUES,
            SPOKES,
            SPOKE_LEN,
            radar_addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            |id, tx| settings::new(id, tx, &self.args),
            false, // no doppler
            false, // not sparse spokes
        );

        if let Some(info) = radars.add(radar_info) {
            let report_name = info.key();
            info.start_forwarding_radar_messages_to_stdout(&subsys);

            let report_receiver =
                report::KodenReportReceiver::new(&self.args, radars.clone(), info);
            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Koden) {
        addresses.push(LocatorAddress::new(
            LocatorId::Koden,
            &BEACON_ADDRESS,
            Brand::Koden,
            vec![&KEEPALIVE_PACKET],
            Box::new(KodenLocator::new(args.clone())),
        ));
    }
}
