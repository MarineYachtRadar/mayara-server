//! One-shot mDNS browse for the diagnostics endpoint.
//!
//! Spins up a fresh [`ServiceDaemon`], issues `browse()` for a handful of
//! common service types, collects [`ServiceEvent::ServiceResolved`] events
//! for a short window, then shuts the daemon down. Used by
//! [`super::devices::collect_per_interface`] to attach hostnames to the
//! per-interface device list.
//!
//! The daemon is scoped to a caller-supplied set of interface names so the
//! browse does not leak across the host's other LANs (e.g. probing the
//! house Wi-Fi from a marine engine-room install).

use std::net::Ipv4Addr;
use std::time::Duration;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

#[derive(Debug, Clone)]
pub struct MdnsHit {
    pub ip: Ipv4Addr,
    /// e.g. `halo-bridge.local.`
    pub hostname: String,
    /// The service type we caught it on, e.g. `_workstation._tcp.local.`
    pub service: &'static str,
}

/// Service types worth browsing for "what's on this LAN" diagnostics.
/// Each one catches a different population of devices:
///
/// - `_workstation._tcp` — Avahi/Bonjour-published generic hosts (Linux/macOS)
/// - `_http._tcp` / `_https._tcp` — webservers, very common on MFDs/plotters
/// - `_signalk-tcp._tcp` / `_signalk-http._tcp` / `_signalk-https._tcp` —
///   Signal K servers, an obvious sibling on a radar net
/// - `_googlecast._tcp` — Chromecasts; useful "is the AV LAN even alive?"
/// - `_printer._tcp` / `_ipp._tcp` — printers / Bonjour print
/// - `_services._dns-sd._udp` — meta service enumerating advertised types
const SERVICE_TYPES: &[&str] = &[
    "_workstation._tcp.local.",
    "_http._tcp.local.",
    "_https._tcp.local.",
    "_signalk-tcp._tcp.local.",
    "_signalk-http._tcp.local.",
    "_signalk-https._tcp.local.",
    "_googlecast._tcp.local.",
    "_printer._tcp.local.",
    "_ipp._tcp.local.",
    "_services._dns-sd._udp.local.",
];

/// Browse mDNS for up to `duration` and return the resolved IPv4 hits.
///
/// `enabled_ifnames` are the NIC names (as `network-interface` reports
/// them) that the daemon should listen on; if empty, the daemon's
/// default behaviour is left alone (all interfaces). Any I/O error is
/// soft-failed — the diagnostics are best-effort.
pub async fn browse_one_shot(enabled_ifnames: &[String], duration: Duration) -> Vec<MdnsHit> {
    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            log::debug!("mdns_browse: ServiceDaemon::new failed: {}", e);
            return Vec::new();
        }
    };

    if !enabled_ifnames.is_empty() {
        // Default is to listen on everything — narrow first, then opt in
        // explicitly, so adjacent LANs the host happens to be on don't
        // appear in this diagnostic.
        let _ = mdns.disable_interface(IfKind::All);
        for name in enabled_ifnames {
            let _ = mdns.enable_interface(IfKind::Name(name.clone()));
        }
    }

    let deadline = Instant::now() + duration;
    let mut set: JoinSet<Vec<MdnsHit>> = JoinSet::new();

    for st in SERVICE_TYPES {
        let st: &'static str = st;
        let rx = match mdns.browse(st) {
            Ok(rx) => rx,
            Err(e) => {
                log::debug!("mdns_browse: browse({}) failed: {}", st, e);
                continue;
            }
        };
        set.spawn(async move {
            let mut hits = Vec::new();
            loop {
                tokio::select! {
                    _ = sleep_until(deadline) => return hits,
                    event = rx.recv_async() => {
                        match event {
                            Ok(ServiceEvent::ServiceResolved(info)) => {
                                let hostname = info.get_hostname().to_string();
                                for addr in info.get_addresses_v4() {
                                    hits.push(MdnsHit {
                                        ip: addr,
                                        hostname: hostname.clone(),
                                        service: st,
                                    });
                                }
                            }
                            Ok(_) => {}
                            Err(_) => return hits,
                        }
                    }
                }
            }
        });
    }

    let mut all = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(hits) = res {
            all.extend(hits);
        }
    }

    if let Ok(stop) = mdns.shutdown() {
        // Drain so the daemon thread exits cleanly. Bounded with a short
        // timeout so a wedged daemon never holds the endpoint past its
        // budget.
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::task::spawn_blocking(move || stop.recv()),
        )
        .await;
    }

    all
}
