extern crate tokio;

use clap::Parser;
use locator::Locator;
use miette::Result;
use radar::SharedRadars;
use radar::target::{BlobMessage, TrackerManager};
use serde::{Deserialize, Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
};
use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use utoipa::ToSchema;

pub mod ais;
pub mod brand;
pub mod config;
pub mod locator;
pub mod navdata;
pub mod network;
#[cfg(feature = "pcap-replay")]
pub(crate) mod nnd;
#[cfg(feature = "pcap-replay")]
pub mod pcap;
pub mod protos;
pub mod radar;
pub mod recording;
pub mod replay;
pub mod signalk;
pub mod stream;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PACKAGE: &str = env!("CARGO_PKG_NAME");
pub const SIGNALK_RADAR_API_VERSION: &str = env!("SIGNALK_RADAR_API_VERSION");

/// How often the static-position task re-broadcasts the current heading
/// so late-joining GUI clients can receive it.
const STATIC_NAV_REBROADCAST_INTERVAL_SECS: u64 = 2;

#[derive(clap::ValueEnum, Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetMode {
    #[default]
    Arpa,
    Trails,
    None,
}

#[derive(Parser, Clone, Debug)]
pub struct Cli {
    #[clap(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    /// Port for webserver
    #[arg(short, long, default_value_t = 6502)]
    pub port: u16,

    /// TLS certificate file (PEM format). Enables HTTPS when set with --tls-key.
    #[arg(long, requires = "tls_key")]
    pub tls_cert: Option<std::path::PathBuf>,

    /// TLS private key file (PEM format). Enables HTTPS when set with --tls-cert.
    #[arg(long, requires = "tls_cert")]
    pub tls_key: Option<std::path::PathBuf>,

    /// Limit radar location to a single interface
    #[arg(short, long)]
    pub interface: Option<String>,

    /// Limit radar location to a single brand
    #[arg(short, long)]
    pub brand: Option<Brand>,

    /// Target analysis mode
    #[arg(short, long, default_value_t, value_enum)]
    pub targets: TargetMode,

    /// Set navigation service address. Accepts either an interface name
    /// (restricts mDNS to that interface) or `<scheme>:<address>:<port>`.
    ///
    /// Schemes:
    /// - `tcp:` — plain TCP Signal K stream (anonymous only)
    /// - `udp:` — UDP listener for NMEA 0183 broadcasts
    /// - `ws:`  — WebSocket via Signal K discovery; supports `--signalk-token`
    /// - `wss:` — WebSocket Secure via Signal K discovery; supports
    ///   `--signalk-token`; requires `--accept-invalid-certs` for
    ///   self-signed certificates
    ///
    /// Authenticated Signal K servers can only be reached via `ws:` or `wss:`.
    /// The plain `tcp:` transport is strictly for anonymous setups.
    #[arg(short, long)]
    pub navigation_address: Option<String>,

    /// Use NMEA 0183 for navigation service instead of Signal K
    #[arg(long)]
    pub nmea0183: bool,

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    pub output: bool,

    /// Legacy replay mode (read-only controls, no beacon sending)
    #[arg(short, long, default_value_t = false)]
    pub replay: bool,

    /// Replay a pcap/nnd file through the full radar pipeline
    #[cfg(feature = "pcap-replay")]
    #[arg(long, value_name = "FILE")]
    pub pcap: Option<String>,

    /// Repeat pcap replay in a loop (only with --pcap)
    #[cfg(feature = "pcap-replay")]
    #[arg(long, default_value_t = false, requires = "pcap")]
    pub repeat: bool,

    /// Replay at most this many seconds of pcap content, then exit (only
    /// with --pcap, conflicts with --repeat). Useful for time-bounded
    /// reproducible profiling. Exits earlier if the file ends first.
    #[cfg(feature = "pcap-replay")]
    #[arg(
        long,
        value_name = "SECONDS",
        requires = "pcap",
        conflicts_with = "repeat"
    )]
    pub pcap_max_time: Option<u32>,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    pub fake_errors: bool,

    /// Allow wifi mode
    #[arg(long, default_value_t = false)]
    pub allow_wifi: bool,

    /// Stationary mode for shore-based radar
    #[arg(long, default_value_t = false)]
    pub stationary: bool,

    /// Static position for stationary radar: latitude longitude heading
    /// Example: --static-position 52.3676 4.9041 45.0
    #[arg(long, value_names = ["LAT", "LON", "HEADING"], num_args = 3)]
    pub static_position: Option<Vec<f64>>,

    /// Multi-radar mode keeps locators running even when one radar is found
    #[arg(long, default_value_t = false)]
    pub multiple_radar: bool,

    /// Output OpenAPI specification to stdout and exit
    #[arg(long, default_value_t = false)]
    pub openapi: bool,

    /// Automatically put detected radars into transmit mode
    #[arg(long, default_value_t = false)]
    pub transmit: bool,

    /// Accept invalid TLS certificates (e.g. self-signed) when connecting to Signal K
    #[arg(long, default_value_t = false)]
    pub accept_invalid_certs: bool,

    /// Signal K bearer token for authenticating to the upstream `ws:`/`wss:`
    /// server. Sent as `?token=...` on WebSocket and as
    /// `Authorization: Bearer ...` on REST discovery and AIS-store seeding.
    /// Conflicts with `--signalk-token-file`. Has no effect on `tcp:` or
    /// `udp:` transports.
    #[arg(long, conflicts_with = "signalk_token_file")]
    pub signalk_token: Option<String>,

    /// File containing a Signal K bearer token (single line, trailing
    /// whitespace trimmed). Re-read at startup only. Use this instead of
    /// `--signalk-token` to keep the token out of the process argv.
    #[arg(long, conflicts_with = "signalk_token")]
    pub signalk_token_file: Option<std::path::PathBuf>,

    /// Use emulator radar instead of real radar discovery
    #[arg(long, default_value_t = false)]
    pub emulator: bool,

    /// Merge targets from multiple radars into a single shared target list
    #[arg(long, default_value_t = false)]
    pub merge_targets: bool,

    /// Disable permessage-deflate compression on outbound WebSocket streams
    /// (spoke and Signal K delta). Trades bandwidth for CPU — useful on LAN
    /// deployments where compression cost exceeds the bandwidth benefit.
    #[arg(long, default_value_t = false)]
    pub no_websocket_compression: bool,
}

/// Static position data (latitude, longitude, heading)
#[derive(Clone, Copy, Debug)]
pub struct StaticPosition {
    pub lat: f64,
    pub lon: f64,
    pub heading: f64,
}

impl Cli {
    /// Returns true if any replay mode is active (pcap or legacy).
    pub fn is_replay(&self) -> bool {
        #[cfg(feature = "pcap-replay")]
        if self.pcap.is_some() {
            return true;
        }
        self.replay
    }

    /// Returns the pcap file path if `--pcap <file>` was specified.
    #[cfg(feature = "pcap-replay")]
    pub fn pcap_file(&self) -> Option<&str> {
        self.pcap.as_deref()
    }

    /// Get the static position if specified
    pub fn get_static_position(&self) -> Option<StaticPosition> {
        self.static_position.as_ref().and_then(|v| {
            if v.len() == 3 {
                Some(StaticPosition {
                    lat: v[0],
                    lon: v[1],
                    heading: v[2],
                })
            } else {
                None
            }
        })
    }

    /// Resolve the upstream Signal K bearer token by precedence:
    /// `--signalk-token` > `--signalk-token-file` > env `MAYARA_SIGNALK_TOKEN` > none.
    /// The file is read once; outer whitespace is trimmed. An
    /// empty/whitespace-only value resolves to `None` so misconfigured
    /// deployments don't silently send blank tokens. Embedded control
    /// characters (including `\r`/`\n`) cause an `InvalidData` error so a
    /// token can never inject extra HTTP headers downstream.
    pub fn resolved_signalk_token(&self) -> std::io::Result<Option<String>> {
        self.resolved_signalk_token_with_env(|k| std::env::var(k).ok())
    }

    /// Inner resolver that takes an env source. Lets tests exercise the
    /// env-var fallback branch without mutating the process environment
    /// (which is `unsafe` on Unix and not actually serialized by
    /// `serial_test`).
    fn resolved_signalk_token_with_env<F>(&self, env: F) -> std::io::Result<Option<String>>
    where
        F: FnOnce(&str) -> Option<String>,
    {
        fn sanitize(raw: &str) -> std::io::Result<Option<String>> {
            let t = raw.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if t.chars().any(char::is_control) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Signal K token contains control characters",
                ));
            }
            Ok(Some(t.to_string()))
        }
        if let Some(t) = self.signalk_token.as_deref() {
            return sanitize(t);
        }
        if let Some(path) = self.signalk_token_file.as_deref() {
            let raw = std::fs::read_to_string(path)?;
            return sanitize(&raw);
        }
        match env("MAYARA_SIGNALK_TOKEN") {
            Some(t) => sanitize(&t),
            None => Ok(None),
        }
    }
}

/// Resolve the Signal K bearer token from CLI/env and install it into the
/// process-global slot so downstream WS/REST connect paths can read it.
/// Designed to be called once during startup from the binary entry point.
pub fn install_signalk_token(args: &Cli) -> std::io::Result<()> {
    let token = args.resolved_signalk_token()?;
    if token.is_some() {
        log::info!("Signal K bearer token loaded");
    }
    signalk::set_signalk_token(token);
    Ok(())
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash, ToSchema)]
pub enum Brand {
    Furuno,
    Garmin,
    Koden,
    Navico,
    Raymarine,
    Emulator,
    Playback,
}

impl Brand {
    pub fn to_prefix(&self) -> &'static str {
        match self {
            Self::Furuno => "fur",
            Self::Garmin => "gar",
            Self::Koden => "kod",
            Self::Navico => "nav",
            Self::Raymarine => "ray",
            Self::Emulator => "emu",
            Self::Playback => "play",
        }
    }
}

impl From<&str> for Brand {
    fn from(val: &str) -> Self {
        match val.to_ascii_lowercase().as_str() {
            "furuno" => Brand::Furuno,
            "garmin" => Brand::Garmin,
            "koden" => Brand::Koden,
            "navico" => Brand::Navico,
            "raymarine" => Brand::Raymarine,
            "emulator" => Brand::Emulator,
            "playback" => Brand::Playback,
            _ => panic!("Invalid brand"),
        }
    }
}

impl Serialize for Brand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Furuno => serializer.serialize_str("Furuno"),
            Self::Garmin => serializer.serialize_str("Garmin"),
            Self::Koden => serializer.serialize_str("Koden"),
            Self::Navico => serializer.serialize_str("Navico"),
            Self::Raymarine => serializer.serialize_str("Raymarine"),
            Self::Emulator => serializer.serialize_str("Emulator"),
            Self::Playback => serializer.serialize_str("Playback"),
        }
    }
}

impl std::fmt::Display for Brand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Furuno => write!(f, "Furuno"),
            Self::Garmin => write!(f, "Garmin"),
            Self::Koden => write!(f, "Koden"),
            Self::Navico => write!(f, "Navico"),
            Self::Raymarine => write!(f, "Raymarine"),
            Self::Emulator => write!(f, "Emulator"),
            Self::Playback => write!(f, "Playback"),
        }
    }
}

#[derive(Serialize, Clone, ToSchema)]
enum InterfaceStatus {
    Ok,
    NoIPv4Address,
    WirelessIgnored,
}

/// Information about a network interface and its radar listeners
#[derive(Serialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(example = json!({
    "ip": "192.168.1.100",
    "netmask": "255.255.255.0",
    "listeners": {
        "Furuno": "No match for 172.31.255.255",
        "Navico": "Active",
        "Raymarine": "Listening"
    }
}))]
struct RadarInterfaceApi {
    // Interface status: null (ok), "No IPv4 address" or "Wireless ignored"
    status: InterfaceStatus,
    /// IPv4 address assigned to this interface
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, example = "192.168.1.100")]
    ip: Option<Ipv4Addr>,
    /// Network mask for this interface
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, example = "255.255.255.0")]
    netmask: Option<Ipv4Addr>,
    /// Map of radar brand to listener status message
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<HashMap<String, String>>, example = json!({"Navico": "Active"}))]
    listeners: Option<HashMap<Brand, String>>,
}

/// Network interface identifier (e.g., "eth0 (192.168.1.100)")
///
/// Serialized as a string: "name (ip)" when IP is present, or just "name" when not.
#[derive(Clone, Eq, PartialEq, Hash)]
struct InterfaceId {
    name: String,
    ip: Option<Ipv4Addr>,
}

/// API response containing network interface information for radar detection
#[derive(Serialize, Clone, ToSchema)]
#[schema(example = json!({
    "brands": ["Navico", "Furuno"],
    "interfaces": {
        "en0 (192.168.1.100)": {
            "status": "Ok",
            "ip": "192.168.1.100",
            "netmask": "255.255.255.0",
            "listeners": {
                "Navico": "Active",
                "Furuno": "No match for 172.31.255.255"
            }
        },
        "en1": {
            "status": "WirelessIgnored"
        }
    }
}))]
#[derive(Default)]
pub struct InterfaceApi {
    /// Set of radar brands that have been compiled into this server
    #[schema(example = json!(["Navico", "Furuno"]))]
    brands: HashSet<Brand>,
    /// Map of network interface name to its radar listener information
    #[schema(value_type = HashMap<String, RadarInterfaceApi>)]
    interfaces: HashMap<InterfaceId, RadarInterfaceApi>,
}

impl RadarInterfaceApi {
    fn new(
        status: InterfaceStatus,
        ip: Option<Ipv4Addr>,
        netmask: Option<Ipv4Addr>,
        listeners: Option<HashMap<Brand, String>>,
    ) -> Self {
        Self {
            status,
            ip,
            netmask,
            listeners,
        }
    }
}

impl InterfaceId {
    fn new(name: &str, ip: Option<Ipv4Addr>) -> Self {
        Self {
            name: name.to_owned(),
            ip,
        }
    }
}

impl Serialize for InterfaceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.ip {
            Some(ip) => serializer.serialize_str(&format!("{} ({})", self.name, ip)),
            None => serializer.serialize_str(self.name.as_str()),
        }
    }
}

pub async fn start_session(
    subsystem: &SubsystemHandle,
    args: Cli,
) -> (
    SharedRadars,
    broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
) {
    let radars = SharedRadars::new();
    let (tx_interface_request, _) = broadcast::channel(10);

    // Initialize target tracker manager if ARPA mode is enabled
    if args.targets == TargetMode::Arpa {
        let (blob_tx, blob_rx) = mpsc::channel::<BlobMessage>(512);
        radars.set_blob_tx(blob_tx);

        let sk_client_tx = radars.get_sk_client_tx();
        let (tracker_manager, command_tx) = TrackerManager::new(args.merge_targets, sk_client_tx);
        radars.set_tracker_command_tx(command_tx);

        subsystem.start(SubsystemBuilder::new(
            "TrackerManager",
            async move |subsys: &mut SubsystemHandle| {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("TrackerManager shutdown requested");
                    },
                    _ = tracker_manager.run(blob_rx) => {}
                }
                Ok::<(), miette::Report>(())
            },
        ));
    }

    // Initialize navigation broadcast sender so navdata can push updates to GUI clients
    navdata::init_nav_broadcast(radars.get_sk_client_tx());

    // Seed navigation data from --static-position (for shore-based installations
    // without a connected Signal K/NMEA navigation source). Mirrors the emulator
    // pattern: set atomics once, then periodically re-broadcast so late-joining
    // GUI clients receive the current heading/position.
    if let Some(static_pos) = args.get_static_position() {
        if static_pos.lat.is_finite()
            && static_pos.lon.is_finite()
            && static_pos.heading.is_finite()
            && (-90.0..=90.0).contains(&static_pos.lat)
            && (-180.0..=180.0).contains(&static_pos.lon)
        {
            let heading_rad = static_pos.heading.to_radians();
            navdata::set_position(Some(static_pos.lat), Some(static_pos.lon), "static");
            navdata::set_heading_true(Some(heading_rad), "static");
            navdata::set_sog(Some(0.0));
            navdata::set_cog(Some(heading_rad));

            subsystem.start(SubsystemBuilder::new(
                "Static Navigation",
                async move |subsys: &mut SubsystemHandle| {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                        STATIC_NAV_REBROADCAST_INTERVAL_SECS,
                    ));
                    loop {
                        tokio::select! { biased;
                            _ = subsys.on_shutdown_requested() => break,
                            _ = interval.tick() => {
                                navdata::broadcast_heading("static");
                            }
                        }
                    }
                    Ok::<(), miette::Report>(())
                },
            ));
        } else {
            log::warn!(
                "--static-position ignored: values out of range \
                 (lat={}, lon={}, heading={}). \
                 Expected lat ∈ [-90,90], lon ∈ [-180,180], finite heading.",
                static_pos.lat,
                static_pos.lon,
                static_pos.heading
            );
        }
    }

    // AIS support is unconditional: the GUI subscribes to vessels.* on
    // Signal K via the heading WebSocket, and standalone mayara mirrors
    // upstream `vessels.*` into the local AIS store so the same
    // subscription works against either backend.
    navdata::init_ais_store(radars.get_sk_client_tx());

    // Seed the AIS store from the upstream Signal K REST snapshot as
    // soon as discovery resolves an HTTP URL. Polls until either shutdown
    // or discovery resolves; the WS task and discovery race startup and
    // discovery may take arbitrarily long on a quiet network.
    let accept_invalid_certs = args.accept_invalid_certs;
    subsystem.start(SubsystemBuilder::new(
        "AIS Seed",
        async move |subsys: &mut SubsystemHandle| {
            loop {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
                }
                if crate::signalk::get_upstream_http_base().is_some() {
                    navdata::seed_ais_from_upstream(accept_invalid_certs).await;
                    break;
                }
            }
            Ok::<(), miette::Report>(())
        },
    ));

    // Check for AIS vessel timeouts every 30 seconds.
    subsystem.start(SubsystemBuilder::new(
        "AIS Timeout",
        async move |subsys: &mut SubsystemHandle| {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("AIS timeout task shutdown");
                        break;
                    },
                    _ = interval.tick() => {
                        if let Some(store) = navdata::get_ais_store() {
                            let lost_count = store.check_timeouts();
                            if lost_count > 0 {
                                log::debug!("Marked {} AIS vessels as Lost", lost_count);
                            }
                        }
                    }
                }
            }
            Ok::<(), miette::Report>(())
        },
    ));

    // Coalesce rapid AIS deltas into 50 ms broadcast batches.
    subsystem.start(SubsystemBuilder::new(
        "AIS Broadcast",
        async move |subsys: &mut SubsystemHandle| {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("AIS broadcast task shutdown");
                        break;
                    },
                    _ = interval.tick() => {
                        if let Some(store) = navdata::get_ais_store() {
                            store.flush_pending_broadcasts();
                        }
                    }
                }
            }
            Ok::<(), miette::Report>(())
        },
    ));

    let locator = Locator::new(args.clone(), radars.clone());

    let (tx_ip_change, _rx_ip_change) = broadcast::channel(1);
    let mut navdata = navdata::NavigationData::new(args.clone());

    let rx_ip_change_clone = tx_ip_change.subscribe();
    subsystem.start(SubsystemBuilder::new(
        "NavData",
        async move |subsys: &mut SubsystemHandle| navdata.run(subsys, rx_ip_change_clone).await,
    ));
    let tx_interface_request_clone = tx_interface_request.clone();
    subsystem.start(SubsystemBuilder::new(
        "Locator",
        async move |subsys: &mut SubsystemHandle| {
            locator
                .run(subsys, tx_ip_change, tx_interface_request_clone)
                .await
        },
    ));

    // Start pcap replay dispatcher after the locator (which registers listeners)
    #[cfg(feature = "pcap-replay")]
    if replay::is_active() {
        let repeat = args.repeat;
        let max_time = args.pcap_max_time;
        subsystem.start(SubsystemBuilder::new(
            "PcapReplay",
            async move |subsys: &mut SubsystemHandle| {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("PcapReplay shutdown requested");
                    },
                    _ = replay::run(true, repeat, max_time) => {
                        if max_time.is_some() {
                            log::info!("Replay complete, requesting shutdown");
                            subsys.request_shutdown();
                        }
                    }
                }
                Ok::<(), miette::Report>(())
            },
        ));
    }

    (radars, tx_interface_request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse_cli(args: &[&str]) -> Cli {
        let mut full = vec!["mayara-server"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    #[test]
    fn token_literal_takes_precedence_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tok");
        std::fs::write(&file, "from-file\n").unwrap();
        let cli = Cli {
            signalk_token: Some("from-literal".to_string()),
            signalk_token_file: None, // clap conflict precludes both at once
            ..parse_cli(&[])
        };
        assert_eq!(
            cli.resolved_signalk_token().unwrap().as_deref(),
            Some("from-literal")
        );
    }

    #[test]
    fn token_file_is_read_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tok");
        std::fs::write(&file, "  eyJabc.def\n\n").unwrap();
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: Some(file),
            ..parse_cli(&[])
        };
        assert_eq!(
            cli.resolved_signalk_token().unwrap().as_deref(),
            Some("eyJabc.def")
        );
    }

    #[test]
    fn empty_token_file_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tok");
        std::fs::write(&file, "   \n").unwrap();
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: Some(file),
            ..parse_cli(&[])
        };
        assert!(cli.resolved_signalk_token().unwrap().is_none());
    }

    #[test]
    fn missing_token_file_returns_io_error() {
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: Some(std::path::PathBuf::from("/nonexistent/path/tok")),
            ..parse_cli(&[])
        };
        assert!(cli.resolved_signalk_token().is_err());
    }

    #[test]
    fn token_with_embedded_control_char_is_rejected() {
        let cli = Cli {
            signalk_token: Some("abc\r\nX-Injected: yes".to_string()),
            signalk_token_file: None,
            ..parse_cli(&[])
        };
        let err = cli.resolved_signalk_token().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn token_file_with_embedded_control_char_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tok");
        std::fs::write(&file, "abc\r\nX-Injected: yes\n").unwrap();
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: Some(file),
            ..parse_cli(&[])
        };
        let err = cli.resolved_signalk_token().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    // Env-var fallback tests exercise the inner resolver with an
    // injected env source. Avoids `std::env::set_var`, which is
    // `unsafe` on Unix (it requires no other threads be reading the
    // environment concurrently — a contract tests can't honor).

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_with(value: &str) -> impl FnOnce(&str) -> Option<String> + '_ {
        move |k| {
            if k == "MAYARA_SIGNALK_TOKEN" {
                Some(value.to_string())
            } else {
                None
            }
        }
    }

    #[test]
    fn token_env_var_is_read_and_trimmed() {
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: None,
            ..parse_cli(&[])
        };
        let env = env_with("  eyJenv.abc  \n");
        let resolved = cli.resolved_signalk_token_with_env(env);
        assert_eq!(resolved.unwrap().as_deref(), Some("eyJenv.abc"));
    }

    #[test]
    fn empty_env_var_resolves_to_none() {
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: None,
            ..parse_cli(&[])
        };
        let env = env_with("   \t  ");
        assert!(cli.resolved_signalk_token_with_env(env).unwrap().is_none());
    }

    #[test]
    fn env_var_with_control_char_is_rejected() {
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: None,
            ..parse_cli(&[])
        };
        let env = env_with("abc\nX-Injected: yes");
        let err = cli.resolved_signalk_token_with_env(env).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn missing_env_var_resolves_to_none() {
        let cli = Cli {
            signalk_token: None,
            signalk_token_file: None,
            ..parse_cli(&[])
        };
        assert!(
            cli.resolved_signalk_token_with_env(no_env)
                .unwrap()
                .is_none()
        );
    }
}
