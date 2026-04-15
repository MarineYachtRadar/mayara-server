extern crate tokio;

use clap::Parser;
use locator::Locator;
use miette::Result;
use radar::SharedRadars;
use radar::target::{BlobMessage, TrackerManager};
use serde::{Serialize, Serializer};
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
pub mod pcap;
pub mod replay;
pub mod network;
pub mod protos;
pub mod radar;
pub mod recording;
pub mod stream;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PACKAGE: &str = env!("CARGO_PKG_NAME");
pub const SIGNALK_RADAR_API_VERSION: &str = env!("SIGNALK_RADAR_API_VERSION");

/// How often the static-position task re-broadcasts the current heading
/// so late-joining GUI clients can receive it.
const STATIC_NAV_REBROADCAST_INTERVAL_SECS: u64 = 2;

#[derive(clap::ValueEnum, Clone, Default, Debug, PartialEq, Serialize)]
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

    /// Set navigation service address, either
    /// - Nothing: all interfaces will search via MDNS
    /// - An interface name: only that interface will seach for via MDNS
    /// - `udp-listen:ipv4-address:port` = listen on (broadcast) address at given port
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

    /// Replay a pcap file through the full radar pipeline
    #[arg(long, value_name = "FILE")]
    pub pcap: Option<String>,

    /// Repeat pcap replay in a loop (only with --pcap)
    #[arg(long, default_value_t = false, requires = "pcap")]
    pub repeat: bool,

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

    /// Pass AIS targets from Signal K server to GUI clients
    #[arg(long, default_value_t = false)]
    pub pass_ais: bool,

    /// Use emulator radar instead of real radar discovery
    #[arg(long, default_value_t = false)]
    pub emulator: bool,

    /// Merge targets from multiple radars into a single shared target list
    #[arg(long, default_value_t = false)]
    pub merge_targets: bool,
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
        self.replay || self.pcap.is_some()
    }

    /// Returns the pcap file path if `--pcap <file>` was specified.
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
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash, ToSchema)]
pub enum Brand {
    Furuno,
    Garmin,
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
            Self::Navico => "nav",
            Self::Raymarine => "ray",
            Self::Emulator => "emu",
            Self::Playback => "play",
        }
    }
}

impl Into<Brand> for &str {
    fn into(self) -> Brand {
        match self.to_ascii_lowercase().as_str() {
            "furuno" => Brand::Furuno,
            "garmin" => Brand::Garmin,
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
pub struct InterfaceApi {
    /// Set of radar brands that have been compiled into this server
    #[schema(example = json!(["Navico", "Furuno"]))]
    brands: HashSet<Brand>,
    /// Map of network interface name to its radar listener information
    #[schema(value_type = HashMap<String, RadarInterfaceApi>)]
    interfaces: HashMap<InterfaceId, RadarInterfaceApi>,
}

impl Default for InterfaceApi {
    fn default() -> Self {
        InterfaceApi {
            brands: HashSet::new(),
            interfaces: HashMap::new(),
        }
    }
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
            |subsys| async move {
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
            navdata::set_position(Some(static_pos.lat), Some(static_pos.lon));
            navdata::set_heading_true(Some(heading_rad), "static");
            navdata::set_sog(Some(0.0));
            navdata::set_cog(Some(heading_rad));

            subsystem.start(SubsystemBuilder::new(
                "Static Navigation",
                |subsys| async move {
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

    // Initialize AIS vessel store if pass_ais is enabled
    if args.pass_ais {
        navdata::init_ais_store(radars.get_sk_client_tx());

        // Start background task to check for AIS vessel timeouts (every 30 seconds)
        subsystem.start(SubsystemBuilder::new("AIS Timeout", |subsys| async move {
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
        }));

        // Start background task to flush pending AIS broadcasts (every 50ms)
        // This coalesces rapid updates into single broadcasts
        subsystem.start(SubsystemBuilder::new(
            "AIS Broadcast",
            |subsys| async move {
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
    }

    let locator = Locator::new(args.clone(), radars.clone());

    let (tx_ip_change, _rx_ip_change) = broadcast::channel(1);
    let mut navdata = navdata::NavigationData::new(args.clone());

    let rx_ip_change_clone = tx_ip_change.subscribe();
    subsystem.start(SubsystemBuilder::new("NavData", |subsys| async move {
        navdata.run(subsys, rx_ip_change_clone).await
    }));
    let tx_interface_request_clone = tx_interface_request.clone();
    subsystem.start(SubsystemBuilder::new("Locator", |subsys| {
        locator.run(subsys, tx_ip_change, tx_interface_request_clone)
    }));

    // Start pcap replay dispatcher after the locator (which registers listeners)
    if replay::is_active() {
        let repeat = args.repeat;
        subsystem.start(SubsystemBuilder::new("PcapReplay", move |subsys| async move {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("PcapReplay shutdown requested");
                },
                _ = replay::run(true, repeat) => {}
            }
            Ok::<(), miette::Report>(())
        }));
    }

    (radars, tx_interface_request)
}
