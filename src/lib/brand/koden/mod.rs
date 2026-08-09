use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};
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
    BEACON_ADDRESS, CMD_MAC_ADDRESS, CONTROL_PREFIX, IMAGE_MARKER, IMG_MIN_SIZE, KEEPALIVE_PACKET,
    MAC_ADDRESS_REQUEST, PACKET_END, PIXEL_VALUES, RADAR_PORT, RESP_POWER, RESP_WARMUP, SPOKE_LEN,
    SPOKES, STATUS_PREFIX,
};

/// Length of a `0xA7` MAC address response: prefix, command, six bytes
/// of MAC, terminator.
const MAC_RESPONSE_LEN: usize = 9;

/// How long to keep asking a radar for its MAC before giving up and
/// keying it on its address.
///
/// No Koden radar has been available to test against, so a unit that
/// never answers must still appear: the identity is an improvement when
/// it arrives, never a precondition for the radar being usable.
const MAC_REQUEST_BUDGET: Duration = Duration::from_secs(5);

/// Minimum gap between MAC requests to one radar. Without it every
/// accepted packet would provoke one, and Koden image frames arrive
/// continuously.
const MAC_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

/// What to key a radar on, once [`KodenLocator::identity_for`] has looked
/// at what the radar has told us so far.
#[derive(Debug, PartialEq, Eq)]
enum Identity {
    /// The radar reported this MAC; key on it.
    Known(String),
    /// Nothing yet, and it is time to ask again.
    Ask,
    /// Nothing yet; an answer may still arrive, so do not ask again.
    Wait,
    /// The radar never answered. Key on its address, as before.
    UseAddress,
}

#[derive(Clone)]
struct KodenLocator {
    args: Cli,
    /// The MAC each radar reported for itself, from its `0xA7` response.
    /// A Koden radar is keyed on this rather than on its address, which
    /// changes with the DHCP lease and takes the radar's saved settings
    /// with it.
    macs: HashMap<Ipv4Addr, String>,
    /// When each radar was first asked for its MAC, and when it was last
    /// asked, so requests stay bounded and a radar that never answers is
    /// still registered.
    asked: HashMap<Ipv4Addr, (Instant, Instant)>,
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
        KodenLocator {
            args,
            macs: HashMap::new(),
            asked: HashMap::new(),
        }
    }

    /// Record the MAC from a `0xA7` response. Returns whether it was new,
    /// so the caller only logs a radar's identity once.
    ///
    /// Only a complete, correctly terminated response is trusted: a
    /// truncated or malformed one would yield a hardware id that is not
    /// the radar's, and two radars could end up sharing it.
    fn record_mac(&mut self, radar_addr: &Ipv4Addr, report: &[u8]) -> bool {
        if report.len() != MAC_RESPONSE_LEN || report[MAC_RESPONSE_LEN - 1] != PACKET_END {
            log::debug!("{}: ignoring a malformed MAC response", radar_addr);
            return false;
        }
        let mac: String = report[2..8].iter().map(|b| format!("{:02x}", b)).collect();
        self.macs.insert(*radar_addr, mac).is_none()
    }

    /// What to key a radar on, and whether it is time to ask it for its
    /// MAC again.
    ///
    /// Deciding is separate from asking: this touches only local state, so
    /// it can be exercised without putting a datagram on the network.
    ///
    /// A Koden radar will tell us its own MAC, and it answers on the port
    /// discovery already listens on. Waiting briefly for that is worth it,
    /// because an address-derived key moves with the DHCP lease. But only
    /// briefly: no Koden unit has been available to test against, so one
    /// that never answers must still appear.
    fn identity_for(&mut self, radar_addr: &SocketAddrV4) -> Identity {
        if let Some(mac) = self.macs.get(radar_addr.ip()) {
            return Identity::Known(mac.clone());
        }

        let now = Instant::now();
        let (first, last) = self
            .asked
            .entry(*radar_addr.ip())
            .or_insert((now, now - MAC_REQUEST_INTERVAL));

        if now.duration_since(*first) >= MAC_REQUEST_BUDGET {
            log::info!(
                "{}: no MAC reported, keying the radar on its address",
                radar_addr
            );
            return Identity::UseAddress;
        }
        if now.duration_since(*last) >= MAC_REQUEST_INTERVAL {
            *last = now;
            return Identity::Ask;
        }
        Identity::Wait
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

        if first == STATUS_PREFIX
            && report[1] == CMD_MAC_ADDRESS
            && self.record_mac(from.ip(), report)
        {
            log::debug!("{}: Koden radar reported its MAC", from);
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
        &mut self,
        radar_addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) {
        let spoke_data_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);
        let report_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);
        let send_command_addr = SocketAddrV4::new(*radar_addr.ip(), RADAR_PORT);

        let hardware_id = match self.identity_for(&radar_addr) {
            Identity::Known(mac) => Some(mac),
            Identity::Ask => {
                request_mac(&radar_addr);
                return;
            }
            Identity::Wait => return,
            Identity::UseAddress => None,
        };

        let radar_info = RadarInfo::new(
            radars,
            &self.args,
            Brand::Koden,
            None, // serial number discovered later
            hardware_id.as_deref(),
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
            info.start_forwarding_radar_messages_to_stdout(subsys);

            let report_receiver =
                report::KodenReportReceiver::new(&self.args, radars.clone(), info);
            subsys.start(SubsystemBuilder::new(
                report_name,
                async move |s: &mut SubsystemHandle| report_receiver.run(s).await,
            ));
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

/// Ask a radar for its MAC address. Fire-and-forget: the answer arrives
/// as a `0xA7` status packet on the port the locator already listens on,
/// and the radar is asked again on its next packet if it does not reply.
fn request_mac(radar_addr: &SocketAddrV4) {
    if let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        let _ = socket.set_nonblocking(true);
        let _ = socket.send_to(&MAC_ADDRESS_REQUEST, radar_addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const RADAR: Ipv4Addr = Ipv4Addr::new(172, 31, 3, 12);

    fn locator() -> KodenLocator {
        KodenLocator::new(Cli::parse_from(["mayara-server"]))
    }

    /// A `0xA7` response: prefix, command, the radar's own six MAC bytes,
    /// terminator. The identity must come out in the same compact hex form
    /// other brands use, so keys look alike however they were obtained.
    #[test]
    fn mac_response_yields_the_radar_identity() {
        let mut locator = locator();
        let report = [
            STATUS_PREFIX,
            CMD_MAC_ADDRESS,
            0x00,
            0x0e,
            0xc6,
            0x12,
            0x34,
            0x56,
            0x0d,
        ];

        assert!(locator.record_mac(&RADAR, &report), "first MAC is new");
        assert_eq!(
            locator.macs.get(&RADAR).map(String::as_str),
            Some("000ec6123456")
        );
        assert!(
            !locator.record_mac(&RADAR, &report),
            "the same MAC again is not news"
        );
    }

    /// A truncated response must leave the radar unidentified rather than
    /// produce a short identity that could collide with another unit's.
    #[test]
    fn truncated_mac_response_is_ignored() {
        let mut locator = locator();
        let short = [STATUS_PREFIX, CMD_MAC_ADDRESS, 0x00, 0x0e, 0xc6];

        assert!(!locator.record_mac(&RADAR, &short));
        assert!(locator.macs.is_empty());
    }

    /// Malformed responses must not become an identity: a wrong hardware
    /// id is worse than none, since two radars could share it.
    #[test]
    fn malformed_mac_responses_are_refused() {
        let mut locator = locator();
        let good = [
            STATUS_PREFIX,
            CMD_MAC_ADDRESS,
            0x00,
            0x0e,
            0xc6,
            0x12,
            0x34,
            0x56,
            PACKET_END,
        ];

        let mut no_terminator = good;
        no_terminator[MAC_RESPONSE_LEN - 1] = 0x00;
        assert!(!locator.record_mac(&RADAR, &no_terminator));

        let mut overlong = good.to_vec();
        overlong.push(0x00);
        assert!(!locator.record_mac(&RADAR, &overlong));

        assert!(locator.macs.is_empty(), "nothing malformed may be cached");
        assert!(
            locator.record_mac(&RADAR, &good),
            "a clean response is taken"
        );
    }

    /// A radar that answers is keyed on the MAC it reported.
    #[test]
    fn a_radar_that_reports_its_mac_is_keyed_on_it() {
        let mut locator = locator();
        let addr = SocketAddrV4::new(RADAR, RADAR_PORT);
        let response = [
            STATUS_PREFIX,
            CMD_MAC_ADDRESS,
            0x00,
            0x0e,
            0xc6,
            0x12,
            0x34,
            0x56,
            PACKET_END,
        ];

        assert_eq!(locator.identity_for(&addr), Identity::Ask);
        assert!(locator.record_mac(&RADAR, &response));
        assert_eq!(
            locator.identity_for(&addr),
            Identity::Known("000ec6123456".to_string())
        );
    }

    /// A radar that never answers must still appear. No Koden unit has
    /// been available to test against, so the address fallback is what
    /// keeps an unresponsive one usable.
    #[test]
    fn a_silent_radar_falls_back_to_its_address() {
        let mut locator = locator();
        let addr = SocketAddrV4::new(RADAR, RADAR_PORT);

        assert_eq!(locator.identity_for(&addr), Identity::Ask);

        // Exhaust the budget rather than sleeping through it.
        let expired = Instant::now() - MAC_REQUEST_BUDGET - Duration::from_secs(1);
        locator.asked.insert(RADAR, (expired, expired));

        assert_eq!(
            locator.identity_for(&addr),
            Identity::UseAddress,
            "an unresponsive radar is keyed on its address"
        );
    }

    /// Koden image frames arrive continuously, so a request per accepted
    /// packet would be a flood. Only the first asks; the rest wait.
    #[test]
    fn mac_requests_are_rate_limited() {
        let mut locator = locator();
        let addr = SocketAddrV4::new(RADAR, RADAR_PORT);

        assert_eq!(locator.identity_for(&addr), Identity::Ask);
        for _ in 0..50 {
            assert_eq!(
                locator.identity_for(&addr),
                Identity::Wait,
                "no further request inside the interval"
            );
        }
    }
}
