//! mDNS advertisement of the mayara web server.
//!
//! Publishes `_mayara-http._tcp.local.` so clients (browsers, Signal K
//! servers, plotters) can find mayara without being told its IP address.
//! The service is registered under the hostname `mayara.local.`, so the GUI
//! is also reachable at `http://mayara.local:<port>/` from any host with an
//! mDNS resolver.
//!
//! The daemon probes both names before announcing them, so a second mayara
//! on the same LAN is renamed rather than clashing with the first.

use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::{SIGNALK_RADAR_API_VERSION, VERSION};

/// DNS-SD service type; the name is within the 15-character limit of RFC 6763.
const SERVICE_TYPE: &str = "_mayara-http._tcp.local.";
const INSTANCE_NAME: &str = "mayara";
const HOSTNAME: &str = "mayara.local.";

/// How long to wait for the daemon to multicast the goodbye packets.
const UNREGISTER_TIMEOUT: Duration = Duration::from_millis(500);

/// A live mDNS registration for the web server. Unregisters on drop.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Announce a web server listening on `port`; `tls` selects the scheme
    /// clients should use.
    pub fn start(port: u16, tls: bool) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let info = service_info(port, tls)?;
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;

        Ok(Advertiser { daemon, fullname })
    }

    /// The instance name as registered, e.g. `mayara._mayara-http._tcp.local.`.
    pub fn fullname(&self) -> &str {
        &self.fullname
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
fn service_info(port: u16, tls: bool) -> Result<ServiceInfo, mdns_sd::Error> {
    let properties = [
        ("version", VERSION),
        ("api", SIGNALK_RADAR_API_VERSION),
        ("path", "/signalk"),
        ("scheme", if tls { "https" } else { "http" }),
    ];

    Ok(ServiceInfo::new(
        SERVICE_TYPE,
        INSTANCE_NAME,
        HOSTNAME,
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
        let info = service_info(6502, false).unwrap();

        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_fullname(), "mayara._mayara-http._tcp.local.");
        assert_eq!(info.get_hostname(), HOSTNAME);
        assert_eq!(info.get_port(), 6502);
        assert!(info.is_addr_auto());
    }

    #[test]
    fn service_info_advertises_scheme_and_versions() {
        let plain = service_info(6502, false).unwrap();
        assert_eq!(plain.get_property_val_str("scheme"), Some("http"));
        assert_eq!(plain.get_property_val_str("version"), Some(VERSION));
        assert_eq!(
            plain.get_property_val_str("api"),
            Some(SIGNALK_RADAR_API_VERSION)
        );
        assert_eq!(plain.get_property_val_str("path"), Some("/signalk"));

        let tls = service_info(443, true).unwrap();
        assert_eq!(tls.get_property_val_str("scheme"), Some("https"));
    }
}
