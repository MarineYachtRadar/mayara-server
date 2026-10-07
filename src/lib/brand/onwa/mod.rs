use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::network::match_ipv4;
use crate::radar::range::Ranges;
use crate::radar::{RadarInfo, SharedRadars};
use crate::{Brand, Cli};

use super::{LocatorId, RadarLocator};

mod command;
mod protocol;
mod report;
mod settings;

#[cfg(test)]
pub(crate) use settings::controls_for_every_model;

use protocol::{
    BEACON_ADDRESS, COMMAND_PORT, PIXEL_VALUES, RANGES, SPOKE_ADDRESS, SPOKE_LEN, SPOKES,
    STATE_ADDRESS, State, parse_state,
};

#[derive(Clone)]
struct OnwaLocator {
    args: Cli,
}

impl RadarLocator for OnwaLocator {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        // Only the state report names the radar; the gains report shares
        // its port.
        // A replay has no real interface to compare with.
        if let Some(state) = parse_state(message)
            && (self.args.is_replay() || on_radar_subnet(nic_addr, from.ip(), &state))
        {
            let mac: String = state.mac.iter().map(|b| format!("{:02x}", b)).collect();
            self.found(*from.ip(), &mac, *nic_addr, radars, subsys);
        }
        Ok(())
    }

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(Clone::clone(self))
    }
}

impl OnwaLocator {
    fn found(
        &self,
        radar_ip: Ipv4Addr,
        mac: &str,
        nic_addr: Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let mut info = RadarInfo::new(
            radars,
            &self.args,
            Brand::Onwa,
            None,
            Some(mac),
            None,
            PIXEL_VALUES,
            SPOKES,
            SPOKE_LEN,
            SocketAddrV4::new(radar_ip, COMMAND_PORT),
            nic_addr,
            SPOKE_ADDRESS,
            STATE_ADDRESS,
            SocketAddrV4::new(radar_ip, COMMAND_PORT),
            |id, tx| settings::new(id, tx, &self.args),
            false,
            // Longer ranges send a spoke every two or three tenths of a degree
            true,
        );
        info.set_ranges(Ranges::new_by_distance(&RANGES));
        info.controls.set_user_name(info.key());

        if let Some(info) = radars.add(info) {
            log::info!(
                "{}: ONWA radar at {} via {}",
                info.key(),
                radar_ip,
                nic_addr
            );
            let report_name = info.key();
            info.start_forwarding_radar_messages_to_stdout(subsys);

            let receiver = report::OnwaReportReceiver::new(&self.args, radars.clone(), info);
            subsys.start(SubsystemBuilder::new(
                report_name,
                async move |s: &mut SubsystemHandle| receiver.run(s).await,
            ));
        }
    }
}

/// The radar broadcasts to 255.255.255.255, which every interface's socket
/// hears. Commands go back by unicast, so only an address on the radar's own
/// subnet can reach it.
fn on_radar_subnet(nic_addr: &Ipv4Addr, radar_addr: &Ipv4Addr, state: &State) -> bool {
    match_ipv4(nic_addr, radar_addr, &state.netmask)
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Onwa) {
        addresses.push(LocatorAddress::new(
            LocatorId::Onwa,
            &BEACON_ADDRESS,
            Brand::Onwa,
            vec![],
            Box::new(OnwaLocator { args: args.clone() }),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(netmask: Ipv4Addr) -> State {
        State {
            mac: [0; 6],
            netmask,
            dead_sector: (0, 0),
            dead_sector_on: false,
            range_index: 0,
            echo_stretch: 0,
            interference_rejection: 0,
            noise_rejection: 0,
            sweep_timing: 0,
            transmit: false,
        }
    }

    #[test]
    fn radar_is_taken_only_on_its_own_subnet() {
        let radar = Ipv4Addr::new(223, 168, 1, 168);
        let state = state(Ipv4Addr::new(255, 255, 255, 0));
        assert!(on_radar_subnet(
            &Ipv4Addr::new(223, 168, 1, 200),
            &radar,
            &state
        ));
        assert!(!on_radar_subnet(
            &Ipv4Addr::new(10, 56, 0, 4),
            &radar,
            &state
        ));
    }
}
