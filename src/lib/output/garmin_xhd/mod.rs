//! Garmin xHD output bridge.
//!
//! Emulates a Garmin GMR xHD radar on the Garmin Marine Network so a Garmin
//! chartplotter can display the picture of any radar mayara supports. The
//! plotter discovers the emulated radar exactly as it would a real xHD, and
//! the controls it offers (range, gain, sea, rain, transmit) are translated
//! into mayara control changes on the source radar.
//!
//! Four tasks run concurrently:
//!
//! | Task      | Direction | Endpoint             | Contents                     |
//! |-----------|-----------|----------------------|------------------------------|
//! | `cdm`     | out       | `239.254.2.2:50050`  | discovery heartbeat `0x038e` |
//! | `status`  | out       | `239.254.2.0:50100`  | settings/state reports       |
//! | `spokes`  | out       | `239.254.2.0:50102`  | sweep data `0x0998`          |
//! | `command` | in        | `<local addr>:50101` | control changes from plotter |
//!
//! Only one radar can be bridged per mayara instance: the emulated radar owns
//! the fixed xHD multicast groups and ports, so a second one would interleave
//! its spokes with the first. The first radar discovered wins; use `--brand`
//! to steer that choice on a boat with several radars.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::Brand;
use crate::brand::garmin::protocol::GARMIN_NETMASK;
use crate::network;
use crate::radar::settings::{ControlId, SharedControls};
use crate::radar::{RadarInfo, SharedRadars};

mod cdm;
mod command;
mod convert;
mod spokes;
mod status;

use convert::nearest_xhd_range;

/// Base address of the Garmin Marine Network, `172.16.0.0/12` together with
/// [`GARMIN_NETMASK`].
const GARMIN_SUBNET: Ipv4Addr = Ipv4Addr::new(172, 16, 0, 0);

/// Range reported until the source radar tells us what it is really doing
/// (2 NM, an entry of the xHD range table).
const DEFAULT_RANGE_M: u32 = 3704;

/// How long the range the plotter asked for overrides the range the radar
/// reports. Long enough for a radar to act on the command and report back,
/// short enough that a command the radar ignored does not stick.
const RANGE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// Key of the radar being bridged. The xHD multicast groups and ports are
/// fixed, so exactly one radar can be emulated per process.
static BRIDGED_RADAR: OnceLock<String> = OnceLock::new();

/// Address the emulated radar announces itself from, so the locator can tell
/// mayara's own emulation apart from a real Garmin radar.
static EMULATED_SOURCE: OnceLock<Ipv4Addr> = OnceLock::new();

/// Whether `addr` is the emulated radar of this mayara. See
/// [`crate::locator::is_own_emulated_radar`], the only caller.
pub(crate) fn is_emulated_source(addr: &Ipv4Addr) -> bool {
    EMULATED_SOURCE.get() == Some(addr)
}

/// Gain mode as the enhanced protocol expresses it.
pub(super) const GAIN_MODE_MANUAL: u8 = 0;
pub(super) const GAIN_MODE_AUTO: u8 = 2;

/// Sea clutter mode as the enhanced protocol expresses it.
pub(super) const SEA_MODE_OFF: u8 = 0;
pub(super) const SEA_MODE_MANUAL: u8 = 1;
pub(super) const SEA_MODE_AUTO: u8 = 2;

/// Command acknowledgements that may queue up before the report stream gets
/// round to them. The plotter cannot change more settings at once than this.
const ECHO_DEPTH: usize = 32;

/// State the outgoing tasks share with the command listener.
pub(super) struct Shared {
    controls: SharedControls,
    /// Range the plotter last asked for, and when we stop insisting on it.
    /// Without this the report stream would keep announcing the old range for
    /// as long as the radar needs to act on the command, and the plotter would
    /// snap its range ring back to where it was.
    pending_range: Mutex<Option<(u32, Instant)>>,
    /// Command acknowledgements waiting to go out on the report stream, which
    /// owns the socket they have to be sent from.
    echo_tx: mpsc::Sender<Vec<u8>>,
}

impl Shared {
    fn new(controls: SharedControls) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (echo_tx, echo_rx) = mpsc::channel(ECHO_DEPTH);
        (
            Self {
                controls,
                pending_range: Mutex::new(None),
                echo_tx,
            },
            echo_rx,
        )
    }

    /// Acknowledge a command by having the report stream send `packet`.
    fn echo(&self, packet: Vec<u8>) {
        if self.echo_tx.try_send(packet).is_err() {
            log::warn!("Garmin xHD: dropped a command acknowledgement");
        }
    }

    /// Value of a numeric control on the source radar.
    fn control(&self, id: ControlId) -> Option<f64> {
        self.controls.get(&id).and_then(|c| c.value)
    }

    /// Whether a control is in automatic mode.
    fn control_auto(&self, id: ControlId) -> bool {
        self.controls
            .get(&id)
            .and_then(|c| c.auto)
            .unwrap_or_default()
    }

    /// Range to report to the plotter, always an entry of the xHD range table.
    fn range_m(&self) -> u32 {
        // A radar that has not reported a range yet leaves the control at
        // zero, which is not a range the plotter can be told about.
        let radar_range = self
            .control(ControlId::Range)
            .filter(|&meters| meters > 0.0)
            .map(|meters| nearest_xhd_range(meters as u32));

        let mut pending = self.pending_range.lock().unwrap();
        if let Some((wanted, until)) = *pending {
            if radar_range == Some(wanted) || Instant::now() >= until {
                *pending = None;
            } else {
                return wanted;
            }
        }
        radar_range.unwrap_or(DEFAULT_RANGE_M)
    }

    fn set_pending_range(&self, range_m: u32) {
        *self.pending_range.lock().unwrap() =
            Some((range_m, Instant::now() + RANGE_CONFIRM_TIMEOUT));
    }
}

/// TLV packet: 4-byte message id, 4-byte payload length, payload.
fn packet(msg_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + payload.len());
    p.extend_from_slice(&msg_id.to_le_bytes());
    p.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    p.extend_from_slice(payload);
    p
}

fn packet_u8(msg_id: u32, value: u8) -> Vec<u8> {
    packet(msg_id, &[value])
}

fn packet_u16(msg_id: u32, value: u16) -> Vec<u8> {
    packet(msg_id, &value.to_le_bytes())
}

fn packet_u32(msg_id: u32, value: u32) -> Vec<u8> {
    packet(msg_id, &value.to_le_bytes())
}

/// Create one of the outgoing multicast sockets.
///
/// The socket binds the well-known port it also sends to: the plotter
/// identifies the stream by its source port, and an ephemeral one makes it
/// ignore the traffic. The outgoing interface is pinned rather than left to
/// the routing table, which would send the traffic out of the default route
/// on a host whose Garmin network is not the one it reaches the world by. The
/// TTL keeps the traffic on this segment.
///
/// Multicast loopback is left on, so a display on the same host — a second
/// mayara, or an OpenCPN with radar_pi — sees the emulated radar just as one
/// across the network does. mayara's own locator ignores what mayara sends,
/// which is what keeps it from discovering the radar it is emulating.
fn multicast_send(dest: &SocketAddr, local_addr: Ipv4Addr) -> io::Result<UdpSocket> {
    let SocketAddr::V4(dest) = dest else {
        unreachable!("Garmin multicast addresses are IPv4");
    };
    let socket = network::create_connected_send(dest, &local_addr)?;
    let options = socket2::SockRef::from(&socket);
    options.set_multicast_if_v4(&local_addr)?;
    options.set_multicast_ttl_v4(1)?;
    Ok(socket)
}

/// Whether `ip` is part of the Garmin Marine Network.
fn in_garmin_network(ip: Ipv4Addr) -> bool {
    network::match_ipv4(&ip, &GARMIN_SUBNET, &GARMIN_NETMASK)
}

/// Local address to emulate the radar from.
///
/// Preferred is an address on the same NIC as the source radar: that is how a
/// single-NIC installation is set up, and it cannot pick up a container bridge
/// address by accident. Failing that, any address in `172.16.0.0/16` qualifies
/// — the range Garmin devices actually use, whereas the `172.17`–`172.31` half
/// of the `/12` is where Docker and friends live.
fn local_addr(radar_nic: Ipv4Addr) -> Option<Ipv4Addr> {
    fn addresses(iface: &NetworkInterface) -> impl Iterator<Item = Ipv4Addr> + '_ {
        iface.addr.iter().filter_map(|a| match a.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
    }

    let interfaces = NetworkInterface::show().ok()?;

    let radar_interface = interfaces
        .iter()
        .find(|iface| addresses(iface).any(|ip| ip == radar_nic));
    if let Some(iface) = radar_interface
        && let Some(ip) = addresses(iface).find(|ip| in_garmin_network(*ip))
    {
        return Some(ip);
    }

    let garmin_prefix = &GARMIN_SUBNET.octets()[..2];
    interfaces
        .iter()
        .flat_map(addresses)
        .find(|ip| ip.octets()[..2] == *garmin_prefix)
}

/// Start the bridge for `info`, unless another radar is already being bridged
/// or the host has no address on the Garmin Marine Network.
pub(crate) fn spawn(info: &RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    let key = info.key();

    if info.brand == Brand::Garmin {
        log::info!("{key}: Garmin xHD output not started: radar is a Garmin already");
        return;
    }

    let Some(local_addr) = local_addr(info.nic_addr) else {
        log::error!(
            "{key}: Garmin xHD output not started: no address on the Garmin Marine Network \
             ({GARMIN_SUBNET}/12) found; give the radar NIC a second address in 172.16.x.x"
        );
        return;
    };

    let bridged = BRIDGED_RADAR.get_or_init(|| key.clone());
    if bridged != &key {
        log::info!("{key}: Garmin xHD output not started: already emulating {bridged}");
        return;
    }

    log::info!("{key}: Garmin xHD output emulating a GMR xHD on {local_addr}");
    let _ = EMULATED_SOURCE.set(local_addr);

    let (shared, echo_rx) = Shared::new(info.controls.clone());
    let shared = Arc::new(shared);

    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/CDM"),
        async move |s: &mut SubsystemHandle| cdm::run(local_addr, s).await,
    ));

    let status_shared = shared.clone();
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Status"),
        async move |s: &mut SubsystemHandle| {
            status::run(local_addr, status_shared, echo_rx, s).await
        },
    ));

    let spokes_shared = shared.clone();
    let spokes_radars = radars.clone();
    let spokes_key = key.clone();
    let message_rx = info.message_tx.subscribe();
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Spokes"),
        async move |s: &mut SubsystemHandle| {
            spokes::run(
                local_addr,
                spokes_key,
                spokes_radars,
                message_rx,
                spokes_shared,
                s,
            )
            .await
        },
    ));

    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Command"),
        async move |s: &mut SubsystemHandle| command::run(local_addr, shared, s).await,
    ));
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::Cli;
    use clap::Parser as _;
    use std::collections::HashMap;

    /// Controls of a radar that offers everything the bridge can translate.
    pub(super) fn controls() -> SharedControls {
        use crate::radar::settings::{HAS_AUTO_NOT_ADJUSTABLE, new_auto, new_numeric};

        let mut controls = HashMap::new();
        new_numeric(ControlId::Range, 0., 100_000.).build(&mut controls);
        for id in [ControlId::Gain, ControlId::Sea, ControlId::Rain] {
            new_auto(id, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
        }
        SharedControls::new(
            "test".to_string(),
            tokio::sync::broadcast::Sender::new(1),
            &Cli::parse_from(["mayara"]),
            controls,
        )
    }

    #[test]
    fn range_falls_back_until_the_radar_reports_one() {
        let (shared, _echo_rx) = Shared::new(controls());
        assert_eq!(shared.range_m(), DEFAULT_RANGE_M);
    }

    #[test]
    fn reported_range_is_snapped_to_the_xhd_table() {
        let (shared, _echo_rx) = Shared::new(controls());
        shared.controls.set(&ControlId::Range, 5000., None).unwrap();
        assert_eq!(shared.range_m(), 5556);
    }

    #[test]
    fn the_range_the_plotter_asked_for_holds_until_the_radar_confirms() {
        let (shared, _echo_rx) = Shared::new(controls());
        shared.controls.set(&ControlId::Range, 3704., None).unwrap();

        shared.set_pending_range(11112);
        assert_eq!(shared.range_m(), 11112, "the radar has not acted yet");

        shared
            .controls
            .set(&ControlId::Range, 11112., None)
            .unwrap();
        assert_eq!(shared.range_m(), 11112);

        // Once confirmed, the radar is back in charge of what is reported.
        shared.controls.set(&ControlId::Range, 3704., None).unwrap();
        assert_eq!(shared.range_m(), 3704);
    }

    #[test]
    fn a_range_the_radar_never_takes_stops_being_reported() {
        let (shared, _echo_rx) = Shared::new(controls());
        shared.controls.set(&ControlId::Range, 3704., None).unwrap();

        *shared.pending_range.lock().unwrap() = Some((11112, Instant::now()));
        assert_eq!(shared.range_m(), 3704);
    }

    #[test]
    fn packets_carry_message_id_length_and_value() {
        assert_eq!(packet_u8(0x0919, 1), vec![0x19, 0x09, 0, 0, 1, 0, 0, 0, 1]);
        assert_eq!(
            packet_u16(0x0925, 0x1234),
            vec![0x25, 0x09, 0, 0, 2, 0, 0, 0, 0x34, 0x12]
        );
        assert_eq!(
            packet_u32(0x091e, 3704),
            vec![0x1e, 0x09, 0, 0, 4, 0, 0, 0, 0x78, 0x0e, 0, 0]
        );
    }

    #[test]
    fn garmin_network_covers_the_whole_slash_12() {
        assert!(in_garmin_network(Ipv4Addr::new(172, 16, 2, 0)));
        assert!(in_garmin_network(Ipv4Addr::new(172, 31, 1, 1)));
        assert!(!in_garmin_network(Ipv4Addr::new(172, 15, 255, 255)));
        assert!(!in_garmin_network(Ipv4Addr::new(192, 168, 1, 1)));
    }
}
