//! Garmin xHD output bridge.
//!
//! Emulates a Garmin GMR xHD radar on the local network, allowing a Garmin
//! GPSMAP chartplotter to display spoke data from any radar source that
//! mayara supports (Furuno, Navico, emulator, …).
//!
//! Four async tasks run concurrently:
//! - CDM heartbeat  → 239.254.2.2:50050   (radar discovery)
//! - Status stream  → 239.254.2.0:50100   (settings/state broadcast)
//! - Spoke sender   → 239.254.2.0:50102   (sweep data)
//! - Command listener ← unicast :50101    (plotter → radar controls)

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::oneshot;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use network_interface::{NetworkInterface, NetworkInterfaceConfig};

use crate::radar::settings::ControlId;
use crate::radar::{Power, RadarInfo};

mod cdm;
mod command;
pub(super) mod convert;
mod spokes;
mod status;

/// Garmin Marine Network subnet for auto-detection: 172.16.0.0/12.
const GARMIN_NET: u32 = 0xac10_0000;
const GARMIN_MASK: u32 = 0xfff0_0000;

/// State shared between the status broadcaster and command listener.
pub(super) struct SharedState {
    /// Current range in meters (snapped to nearest xHD table value).
    pub range_m: u32,
    /// Spoke thread ignores range updates until this instant has passed.
    pub range_lock_until: Instant,
    /// Whether the radar is transmitting.
    pub transmitting: bool,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            range_m: 3704, // 2 NM default
            range_lock_until: Instant::now(),
            transmitting: true,
        }
    }
}

/// Detect the Garmin Marine Network IP (172.16.0.0/12) on the same physical
/// NIC as `nic_addr`. This avoids picking a Docker bridge address when the
/// host has multiple 172.16/12 interfaces.
fn detect_garmin_ip(nic_addr: Ipv4Addr) -> Option<Ipv4Addr> {
    let ifaces = NetworkInterface::show().ok()?;
    // First pass: find the NIC that has nic_addr, then return its first GMN IP.
    for iface in &ifaces {
        let has_nic_addr = iface
            .addr
            .iter()
            .any(|a| a.ip() == std::net::IpAddr::V4(nic_addr));
        if !has_nic_addr {
            continue;
        }
        for addr in &iface.addr {
            if let std::net::IpAddr::V4(ip) = addr.ip() {
                let n = u32::from(ip);
                if (n & GARMIN_MASK) == GARMIN_NET {
                    return Some(ip);
                }
            }
        }
    }
    // Fallback: any GMN IP on any interface (original behaviour).
    for iface in &ifaces {
        for addr in &iface.addr {
            if let std::net::IpAddr::V4(ip) = addr.ip() {
                let n = u32::from(ip);
                if (n & GARMIN_MASK) == GARMIN_NET {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// Spawn all four xHD output tasks as graceful-shutdown subsystems.
pub(crate) fn spawn(radar: &RadarInfo, subsys: &SubsystemHandle) {
    let local_ip = match detect_garmin_ip(radar.nic_addr) {
        Some(ip) => ip,
        None => {
            log::info!(
                "{}: GarminXhd output: no interface in 172.16.0.0/12 found — skipping",
                radar.key()
            );
            return;
        }
    };

    log::info!("{}: GarminXhd output starting on {}", radar.key(), local_ip);

    let controls = radar.controls.clone();

    // Read initial transmit state from controls; default to false (standby) if unknown.
    let transmitting = controls
        .get(&ControlId::Power)
        .and_then(|c| c.value)
        .map(|v| v as u32 == Power::Transmit as u32)
        .unwrap_or(false);
    let state = Arc::new(Mutex::new(SharedState {
        transmitting,
        ..SharedState::default()
    }));
    let brand = radar.brand;
    let spokes_per_rev = radar.spokes_per_revolution as u32;
    let message_rx = radar.message_tx.subscribe();
    let key = radar.key();

    // CDM heartbeat
    let (cdm_stop_tx, cdm_stop_rx) = oneshot::channel::<()>();
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/CDM"),
        async move |s: &mut SubsystemHandle| {
            tokio::select! {
                biased;
                _ = s.on_shutdown_requested() => { let _ = cdm_stop_tx.send(()); }
                _ = cdm::run(local_ip, cdm_stop_rx) => {}
            }
            Ok::<(), miette::Report>(())
        },
    ));

    // Status stream
    let (status_stop_tx, status_stop_rx) = oneshot::channel::<()>();
    let state_s = Arc::clone(&state);
    let controls_s = controls.clone();
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Status"),
        async move |s: &mut SubsystemHandle| {
            tokio::select! {
                biased;
                _ = s.on_shutdown_requested() => { let _ = status_stop_tx.send(()); }
                _ = status::run(local_ip, state_s, controls_s, status_stop_rx) => {}
            }
            Ok::<(), miette::Report>(())
        },
    ));

    // Spoke sender
    let (spokes_stop_tx, spokes_stop_rx) = oneshot::channel::<()>();
    let state_sp = Arc::clone(&state);
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Spokes"),
        async move |s: &mut SubsystemHandle| {
            tokio::select! {
                biased;
                _ = s.on_shutdown_requested() => { let _ = spokes_stop_tx.send(()); }
                _ = spokes::run(local_ip, brand, spokes_per_rev, message_rx, state_sp, spokes_stop_rx) => {}
            }
            Ok::<(), miette::Report>(())
        },
    ));

    // Command listener
    let (cmd_stop_tx, cmd_stop_rx) = oneshot::channel::<()>();
    let state_c = Arc::clone(&state);
    subsys.start(SubsystemBuilder::new(
        format!("{key}/GarminXhd/Command"),
        async move |s: &mut SubsystemHandle| {
            tokio::select! {
                biased;
                _ = s.on_shutdown_requested() => { let _ = cmd_stop_tx.send(()); }
                _ = command::run(local_ip, state_c, controls, cmd_stop_rx) => {}
            }
            Ok::<(), miette::Report>(())
        },
    ));
}
