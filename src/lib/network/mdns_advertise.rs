//! mDNS advertisement of the mayara web server.
//!
//! Publishes `_mayara-http._tcp.local.` so clients (browsers, Signal K
//! servers, plotters) can find mayara without being told its IP address.
//! By default the service is registered under the host name `mayara.local.`,
//! so the GUI is also reachable at `http://mayara.local:<port>/` from any host
//! with an mDNS resolver.
//!
//! The daemon probes both names before announcing them, so a second mayara
//! on the same LAN is renamed rather than clashing with the first. Give each
//! server its own name (`--mdns-hostname`) and they never contend at all.
//!
//! Claiming a host name means owning its address records, which a machine that
//! already runs its own responder (Avahi, Bonjour) also does for its own name.
//! A service registration cannot opt out of that while staying advertised —
//! mdns-sd announces nothing for a service with no addresses — so the way to
//! leave a resident responder as the sole authority is `--no-mdns`, which
//! skips the advertisement entirely.

use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::{SIGNALK_RADAR_API_VERSION, VERSION};

/// DNS-SD service type; the name is within the 15-character limit of RFC 6763.
const SERVICE_TYPE: &str = "_mayara-http._tcp.local.";
const INSTANCE_NAME: &str = "mayara";
/// Host name claimed unless `--mdns-hostname` overrides it.
pub const DEFAULT_HOSTNAME: &str = "mayara";
const LOCAL_DOMAIN: &str = ".local.";

/// Turn a bare name into a fully qualified mDNS host name, accepting anything
/// a user is likely to type: `radar`, `radar.local` or `radar.local.`.
fn qualify(name: &str) -> String {
    let bare = name
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(".local")
        .trim_end_matches('.');
    format!("{}{}", bare, LOCAL_DOMAIN)
}

/// How long to wait for the daemon to multicast the goodbye packets.
const UNREGISTER_TIMEOUT: Duration = Duration::from_millis(500);

/// A live mDNS registration for the web server. Unregisters on drop.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    hostname: String,
}

impl Advertiser {
    /// Announce a web server listening on `port`; `tls` selects the scheme
    /// clients should use and `hostname` the name claimed for it.
    pub fn start(port: u16, tls: bool, hostname: &str) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let info = service_info(port, tls, hostname)?;
        let fullname = info.get_fullname().to_string();
        let hostname = info.get_hostname().to_string();
        daemon.register(info)?;

        Ok(Advertiser {
            daemon,
            fullname,
            hostname,
        })
    }

    /// The instance name as registered, e.g. `mayara._mayara-http._tcp.local.`.
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// The host name the service points at, e.g. `mayara.local.`.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        // Wait for the goodbye packets so clients drop the service straight
        // away instead of waiting out the record TTL.
        if let Ok(status) = self.daemon.unregister(&self.fullname) {
            let _ = status.recv_timeout(UNREGISTER_TIMEOUT);
        }
        let _ = self.daemon.shutdown();
    }
}

/// The addresses are left to the daemon: `enable_addr_auto` fills in every
/// host address and keeps the records in step as interfaces come and go.
fn service_info(port: u16, tls: bool, hostname: &str) -> Result<ServiceInfo, mdns_sd::Error> {
    let properties = [
        ("version", VERSION),
        ("api", SIGNALK_RADAR_API_VERSION),
        ("path", "/signalk"),
        ("scheme", if tls { "https" } else { "http" }),
    ];

    Ok(ServiceInfo::new(
        SERVICE_TYPE,
        INSTANCE_NAME,
        &qualify(hostname),
        (),
        port,
        &properties[..],
    )?
    .enable_addr_auto())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_info_names_and_port() {
        let info = service_info(6502, false, DEFAULT_HOSTNAME).unwrap();

        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_fullname(), "mayara._mayara-http._tcp.local.");
        assert_eq!(info.get_hostname(), "mayara.local.");
        assert_eq!(info.get_port(), 6502);
        assert!(info.is_addr_auto());
    }

    #[test]
    fn service_info_advertises_scheme_and_versions() {
        let plain = service_info(6502, false, DEFAULT_HOSTNAME).unwrap();
        assert_eq!(plain.get_property_val_str("scheme"), Some("http"));
        assert_eq!(plain.get_property_val_str("version"), Some(VERSION));
        assert_eq!(
            plain.get_property_val_str("api"),
            Some(SIGNALK_RADAR_API_VERSION)
        );
        assert_eq!(plain.get_property_val_str("path"), Some("/signalk"));

        let tls = service_info(443, true, DEFAULT_HOSTNAME).unwrap();
        assert_eq!(tls.get_property_val_str("scheme"), Some("https"));
    }

    #[test]
    fn overridden_name_is_claimed_however_it_was_typed() {
        for given in ["radar", "radar.local", "radar.local.", "  radar  "] {
            let info = service_info(6502, false, given).unwrap();
            assert_eq!(info.get_hostname(), "radar.local.", "given={given}");
            // The address records are what make the name resolve, so an
            // overridden name must still carry them.
            assert!(info.is_addr_auto(), "given={given}");
        }
    }

    #[test]
    fn qualify_appends_the_local_domain_exactly_once() {
        assert_eq!(qualify("radar"), "radar.local.");
        assert_eq!(qualify("radar."), "radar.local.");
        assert_eq!(qualify("radar.local"), "radar.local.");
        assert_eq!(qualify("radar.local."), "radar.local.");
    }

    #[test]
    fn a_service_without_addresses_is_never_announced() {
        // Guards the reason `--no-mdns` skips the advertiser wholesale rather
        // than registering a service that claims no host name: mdns-sd drops
        // such a service on every interface ("no valid addrs") and it silently
        // never appears on the network.
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            INSTANCE_NAME,
            &qualify(DEFAULT_HOSTNAME),
            (),
            6502,
            &[][..] as &[(&str, &str)],
        )
        .unwrap();

        assert!(info.get_addresses().is_empty());
        assert!(!info.is_addr_auto());
    }
}
