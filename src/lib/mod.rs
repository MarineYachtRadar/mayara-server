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
pub mod process;
pub mod protos;
pub mod radar;
pub mod recording;
pub mod replay;
pub mod signalk;
pub mod stream;
pub mod telemetry;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PACKAGE: &str = env!("CARGO_PKG_NAME");
/// Version of the Signal K API mayara serves under `/signalk/v2` — the OpenAPI
/// `info.version` and the `version` of the `v2` endpoint on `/signalk`. Sourced
/// from `[package.metadata] api-version` in Cargo.toml.
pub const SIGNALK_RADAR_API_VERSION: &str = env!("SIGNALK_RADAR_API_VERSION");
/// Where this binary was built: `official` for one this project's CI
/// published, `local` for every other build. Determined by `build.rs`.
pub const BUILD: &str = env!("MAYARA_BUILD");

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

    /// Run as a helper process of a chart plotter such as OpenCPN, given its
    /// process id. The web server then listens on localhost only and is not
    /// advertised on mDNS, and mayara exits once that process is gone.
    #[arg(long, value_name = "PID")]
    pub parent: Option<u32>,

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

    /// Host name to claim on mDNS, so the GUI is reachable at
    /// `http://<name>.local:<port>/`. Defaults to `mayara`. Give each server on
    /// a network its own name; two claiming the same one contend for it, and
    /// the loser is renamed.
    #[arg(long, value_name = "NAME", conflicts_with = "no_mdns")]
    pub mdns_hostname: Option<String>,

    /// Never ask about, or send, anonymous usage stats. Mayara otherwise asks
    /// once in the GUI whether it may report that a radar delivered data and
    /// accepted a control change -- the mayara version, operating system,
    /// radar brand and model, and a random id for this install. Never a
    /// position, serial number or network address. `MAYARA_TELEMETRY=false`
    /// does the same; `MAYARA_TELEMETRY=true` answers yes without asking.
    #[arg(long, default_value_t = false)]
    pub no_telemetry: bool,

    /// Do not advertise on mDNS at all: no service and no host name, so
    /// `<name>.local` will not resolve to mayara and clients must be given its
    /// address. Use this when the machine already runs its own mDNS responder
    /// (Avahi, Bonjour) that should stay the sole authority.
    #[arg(long, default_value_t = false)]
    pub no_mdns: bool,
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

    /// Whether this build can look for this brand at all. Brands are cargo
    /// features, so a build can be missing one entirely -- and then no amount
    /// of correct network configuration will ever produce a radar.
    pub fn is_compiled_in(&self) -> bool {
        match self {
            Self::Navico => cfg!(feature = "navico"),
            Self::Furuno => cfg!(feature = "furuno"),
            Self::Garmin => cfg!(feature = "garmin"),
            Self::Koden => cfg!(feature = "koden"),
            Self::Raymarine => cfg!(feature = "raymarine"),
            Self::Emulator => cfg!(feature = "emulator"),
            Self::Playback => true,
        }
    }

    /// Every brand this build can look for, for a client that should not
    /// offer the user a radar this server could never find.
    pub fn compiled_in() -> Vec<Brand> {
        [
            Self::Navico,
            Self::Furuno,
            Self::Garmin,
            Self::Koden,
            Self::Raymarine,
        ]
        .into_iter()
        .filter(Self::is_compiled_in)
        .collect()
    }

    /// The brand a radar key belongs to. Keys are built as prefix + identity
    /// by `radar_key`, so a stored key still says which brand wrote it long
    /// after that radar was last seen.
    pub fn from_key(key: &str) -> Option<Brand> {
        [
            Self::Furuno,
            Self::Garmin,
            Self::Koden,
            Self::Navico,
            Self::Raymarine,
            Self::Emulator,
            Self::Playback,
        ]
        .into_iter()
        .find(|brand| key.starts_with(brand.to_prefix()))
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
    /// The link cannot carry radar traffic (Bluetooth, tunnel, PPP, ...) and is
    /// skipped even with `--allow-wifi`.
    LinkTypeIgnored,
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
    // Interface status: null (ok), "No IPv4 address", "Wireless ignored" or
    // "Link type ignored"
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

/// What the user says they are waiting to see.
///
/// Finer than `Brand`, because Raymarine's three product families need three
/// different networks: an RD picks its own `10/8` address from its MAC, a
/// Quantum on an Axiom network is a DHCP client on `198.18/21`, and a Quantum
/// on its own can be anywhere at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, ToSchema)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Expectation {
    Navico,
    Furuno,
    Garmin,
    Koden,
    RaymarineRd,
    RaymarineQuantumMfd,
    RaymarineQuantumStandalone,
}

/// Whether this host's network can carry the radar the user is waiting for.
#[derive(Serialize, Debug, PartialEq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCheck {
    /// False only when we can say something is definitely wrong. Searching
    /// continues either way; a radar mayara cannot name a requirement for is
    /// not a radar mayara has given up on.
    pub met: bool,
    /// What this radar needs of the network, in the user's terms.
    pub requirement: String,
    /// What this host has, or what it is missing.
    pub finding: String,
    /// What to do about it. Absent when there is nothing to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl Expectation {
    /// What this radar asks of the network, in the user's terms.
    fn requirement_text(&self) -> String {
        match self.required_network() {
            Some((_, _, human)) => {
                format!("This radar can only be reached from a host in {}.", human)
            }
            None => "This radar announces itself by multicast or broadcast and needs no particular address on this host.".to_string(),
        }
    }

    /// The network this radar has to be reachable on, as a prefix and the
    /// wording a user would recognise. `None` for radars that impose nothing
    /// mayara can check.
    fn required_network(&self) -> Option<(Ipv4Addr, u8, &'static str)> {
        match self {
            // A DRS only answers a host inside its own fixed subnet.
            Self::Furuno => Some((Ipv4Addr::new(172, 31, 0, 0), 16, "172.31.x.x")),
            // 172.16.0.0/12, the whole marine range Garmin uses.
            Self::Garmin => Some((Ipv4Addr::new(172, 16, 0, 0), 12, "172.16.x.x - 172.31.x.x")),
            // An RD self-assigns 10.<three bytes of its MAC> and will never
            // answer a host outside that /8.
            Self::RaymarineRd => Some((Ipv4Addr::new(10, 0, 0, 0), 8, "10.x.x.x")),
            // The network an Axiom insists on, and hands out DHCP leases from.
            Self::RaymarineQuantumMfd => {
                Some((Ipv4Addr::new(198, 18, 0, 0), 21, "198.18.0.x - 198.18.7.x"))
            }
            // Navico and Koden find each other by multicast and broadcast;
            // a standalone Quantum can be on any address its DHCP server hands
            // out, which is why mayara cannot check it (and does not yet
            // support it).
            Self::Navico | Self::Koden | Self::RaymarineQuantumStandalone => None,
        }
    }

    /// The brand this expectation belongs to. Raymarine's three families are
    /// one brand as far as discovery is concerned.
    pub fn brand(&self) -> Brand {
        match self {
            Self::Navico => Brand::Navico,
            Self::Furuno => Brand::Furuno,
            Self::Garmin => Brand::Garmin,
            Self::Koden => Brand::Koden,
            Self::RaymarineRd | Self::RaymarineQuantumMfd | Self::RaymarineQuantumStandalone => {
                Brand::Raymarine
            }
        }
    }

    /// Judge this host's addresses against what the radar needs.
    pub fn check(&self, interfaces: &InterfaceApi) -> NetworkCheck {
        self.check_supported(interfaces, self.brand().is_compiled_in())
    }

    fn check_supported(&self, interfaces: &InterfaceApi, supported: bool) -> NetworkCheck {
        let addresses = interfaces.ipv4_addresses();

        // A network the radar could be reached on means nothing if this build
        // has no code to look for it. Saying the network is fine would send
        // the user checking cables for a radar that can never appear.
        if !supported {
            return NetworkCheck {
                met: false,
                requirement: self.requirement_text(),
                finding: format!(
                    "This build of mayara has no {} support compiled in, so it will never find one however the network is set up.",
                    self.brand()
                ),
                remedy: Some(format!(
                    "Use a mayara build with {} support -- the official builds include every brand.",
                    self.brand()
                )),
            };
        }

        if *self == Self::RaymarineQuantumStandalone {
            return NetworkCheck {
                met: false,
                requirement: "A Quantum with no MFD accepts an address from whatever DHCP server it finds, so there is no fixed network to check.".to_string(),
                finding: "Mayara does not support a standalone Quantum yet: it can only find one that is part of a Raymarine network with an MFD.".to_string(),
                remedy: Some("Connect the Quantum to a Raymarine MFD network, or follow the issue for standalone support.".to_string()),
            };
        }

        // Being confidently wrong is worse than saying nothing: with no
        // interface list to read, "your host has no address" would send the
        // user reconfiguring a network that is fine.
        if !interfaces.is_known() {
            return NetworkCheck {
                met: true,
                requirement: self.requirement_text(),
                finding: "Mayara could not read this host's network interfaces just now, so it cannot check them.".to_string(),
                remedy: None,
            };
        }

        let Some((network, prefix, human)) = self.required_network() else {
            return NetworkCheck {
                met: true,
                requirement: self.requirement_text(),
                finding: if addresses.is_empty() {
                    "This host has no usable IPv4 address at all, so nothing can be found."
                        .to_string()
                } else {
                    "Nothing about this host's addresses prevents it from being found."
                        .to_string()
                },
                remedy: (addresses.is_empty())
                    .then(|| "Connect this computer to the radar network by wired Ethernet and give it an IPv4 address.".to_string()),
            };
        };

        let matching: Vec<&(String, Ipv4Addr)> = addresses
            .iter()
            .filter(|(_, ip)| in_network(*ip, network, prefix))
            .collect();

        if let Some((interface, ip)) = matching.first() {
            NetworkCheck {
                met: true,
                requirement: self.requirement_text(),
                // Naming the interface matters: a container bridge or a
                // virtual interface can sit in the same range while carrying
                // no radar traffic at all, and on a containerised install
                // that is more likely than not.
                finding: format!(
                    "{} has {}, which is in that range — check that {} is the interface your radar is wired to.",
                    interface, ip, interface
                ),
                remedy: None,
            }
        } else {
            let has = if addresses.is_empty() {
                "This host has no usable IPv4 address at all.".to_string()
            } else {
                format!(
                    "This host has {}, none of which is in that range.",
                    addresses
                        .iter()
                        .map(|(interface, ip)| format!("{} on {}", ip, interface))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };

            NetworkCheck {
                met: false,
                requirement: self.requirement_text(),
                finding: has,
                remedy: Some(format!(
                    "Give the interface the radar is wired to an address in {}, then reload this page.",
                    human
                )),
            }
        }
    }
}

/// Whether an address falls inside a network, by prefix length.
fn in_network(ip: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(ip) & mask == u32::from(network) & mask
}

impl InterfaceApi {
    /// Whether the interface list could be read at all. Empty means the
    /// locator did not answer (or is not running, as under `--emulator`) --
    /// not that the host has no addresses.
    pub fn is_known(&self) -> bool {
        !self.interfaces.is_empty()
    }

    /// Every usable IPv4 address this host has, with the interface it is on.
    /// Loopback is left out: a radar never answers there.
    pub fn ipv4_addresses(&self) -> Vec<(String, Ipv4Addr)> {
        let mut found: Vec<(String, Ipv4Addr)> = self
            .interfaces
            .iter()
            .filter_map(|(id, interface)| {
                let ip = interface.ip?;
                (!ip.is_loopback()).then(|| (id.name.clone(), ip))
            })
            .collect();
        found.sort();
        found
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
    //
    // Once the HTTP base is known, wait a bounded extra window for the WS
    // task to latch `OWN_SHIP_CONTEXT` from the `vessels.self` subscription
    // before seeding. The REST tree contains the operator's own ship under
    // its MMSI URN just like any other vessel, and the seed function uses
    // the latched context to skip it. If the operator runs without an
    // upstream Signal K (no own-ship will ever latch), the timeout still
    // lets the seed proceed so an offline GUI overlay isn't blocked.
    let accept_invalid_certs = args.accept_invalid_certs;
    subsystem.start(SubsystemBuilder::new(
        "AIS Seed",
        async move |subsys: &mut SubsystemHandle| {
            let own_ship_wait_deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
                }
                if crate::signalk::get_upstream_http_base().is_some() {
                    let own_ship_latched = navdata::get_own_ship_context().is_some();
                    let deadline_passed = tokio::time::Instant::now() >= own_ship_wait_deadline;
                    if own_ship_latched || deadline_passed {
                        navdata::seed_ais_from_upstream(accept_invalid_certs).await;
                        break;
                    }
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
                            let removed_count = store.check_timeouts();
                            if removed_count > 0 {
                                log::debug!("Dropped {} timed-out AIS vessels", removed_count);
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

    let (tx_ip_change, _rx_ip_change) = broadcast::channel(1);

    // Decay a radar's power state to Off once it stops sending (issue #432): a
    // powered-off radar goes silent, so without this its GUI icon would hold the
    // last state it reported (standby/transmit) forever.
    //
    // The same loop re-checks whether each radar can still be commanded. That
    // depends on this host's addressing, so it is driven by address changes as
    // well as by the tick — the tick alone would still catch it, but only after
    // a delay, and a radar discovered between ticks needs evaluating too.
    let watchdog_radars = radars.clone();
    let mut watchdog_rx_ip_change = tx_ip_change.subscribe();
    subsystem.start(SubsystemBuilder::new(
        "Radar Watchdog",
        async move |subsys: &mut SubsystemHandle| {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! { biased;
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("Radar watchdog shutdown");
                        break;
                    },
                    _ = watchdog_rx_ip_change.recv() => {
                        log::debug!("Radar watchdog: address change, re-checking reachability");
                        watchdog_radars.refresh_command_reachability();
                    },
                    _ = interval.tick() => {
                        watchdog_radars.mark_silent_radars_off();
                        watchdog_radars.refresh_command_reachability();
                    }
                }
            }
            Ok::<(), miette::Report>(())
        },
    ));

    let locator = Locator::new(args.clone(), radars.clone());
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
mod expectation_tests {
    use super::*;

    fn interfaces(addresses: &[(&str, &str)]) -> InterfaceApi {
        let mut api = InterfaceApi::default();
        for (name, ip) in addresses {
            let ip: Ipv4Addr = ip.parse().unwrap();
            api.interfaces.insert(
                InterfaceId::new(name, Some(ip)),
                RadarInterfaceApi::new(InterfaceStatus::Ok, Some(ip), None, None),
            );
        }
        api
    }

    /// A host whose interfaces were read and simply carry no IPv4 address --
    /// which is a different thing from not having been able to read them.
    fn interfaces_without_addresses() -> InterfaceApi {
        let mut api = InterfaceApi::default();
        api.interfaces.insert(
            InterfaceId::new("eth0", None),
            RadarInterfaceApi::new(InterfaceStatus::NoIPv4Address, None, None, None),
        );
        api
    }

    /// A DRS answers nobody outside its own subnet, so a host that is not in
    /// it will wait forever without being told why.
    #[test]
    fn furuno_needs_its_own_subnet() {
        let wrong = Expectation::Furuno.check(&interfaces(&[("eth0", "192.168.1.10")]));
        assert!(!wrong.met);
        assert!(wrong.finding.contains("192.168.1.10"), "name what is there");
        assert!(wrong.remedy.is_some(), "say what to do about it");

        let right = Expectation::Furuno.check(&interfaces(&[("eth0", "172.31.3.100")]));
        assert!(right.met);
        assert_eq!(right.remedy, None);
        assert!(
            right.finding.contains("eth0"),
            "name the interface: a container bridge can sit in the range and carry nothing"
        );
    }

    /// Garmin's range is the whole of 172.16/12, not just 172.16.x.
    #[test]
    fn garmin_accepts_the_whole_marine_range() {
        assert!(
            Expectation::Garmin
                .check(&interfaces(&[("eth0", "172.16.2.5")]))
                .met
        );
        assert!(
            Expectation::Garmin
                .check(&interfaces(&[("eth0", "172.30.9.9")]))
                .met
        );
        assert!(
            !Expectation::Garmin
                .check(&interfaces(&[("eth0", "172.32.0.1")]))
                .met
        );
    }

    /// An RD self-assigns 10.<three bytes of its MAC>, so the host has to be
    /// somewhere in 10/8 to hear it at all.
    #[test]
    fn a_raymarine_rd_needs_a_ten_network() {
        assert!(
            Expectation::RaymarineRd
                .check(&interfaces(&[("eth0", "10.56.0.1")]))
                .met
        );

        let wrong = Expectation::RaymarineRd.check(&interfaces(&[("eth0", "192.168.1.10")]));
        assert!(!wrong.met);
        assert!(wrong.requirement.contains("10.x.x.x"));
    }

    /// An Axiom network is 198.18.0.0/21 -- 198.18.8.x is already outside it.
    #[test]
    fn a_quantum_with_an_mfd_needs_the_axiom_network() {
        assert!(
            Expectation::RaymarineQuantumMfd
                .check(&interfaces(&[("eth0", "198.18.0.5")]))
                .met
        );
        assert!(
            Expectation::RaymarineQuantumMfd
                .check(&interfaces(&[("eth0", "198.18.7.254")]))
                .met
        );
        assert!(
            !Expectation::RaymarineQuantumMfd
                .check(&interfaces(&[("eth0", "198.18.8.1")]))
                .met,
            "/21 stops at 198.18.7.255"
        );
    }

    /// Mayara cannot find one yet, and saying so beats letting the user wait.
    #[test]
    fn a_standalone_quantum_is_reported_as_unsupported() {
        let check =
            Expectation::RaymarineQuantumStandalone.check(&interfaces(&[("eth0", "192.168.1.10")]));

        assert!(!check.met);
        assert!(check.finding.contains("does not support"));
    }

    /// Being unable to read the interfaces changes nothing about a radar
    /// mayara cannot find at all, so that answer has to come first.
    #[test]
    fn a_standalone_quantum_is_unsupported_even_with_no_interface_list() {
        let check = Expectation::RaymarineQuantumStandalone.check(&InterfaceApi::default());

        assert!(!check.met);
        assert!(
            check.finding.contains("does not support"),
            "not 'could not read the interfaces', which invites the user to keep waiting"
        );
    }

    /// Navico is found by multicast from any address, so the answer is "your
    /// network is not the problem" -- unless there is no address at all.
    #[test]
    fn navico_asks_nothing_of_the_address() {
        let check = Expectation::Navico.check(&interfaces(&[("eth0", "192.168.1.10")]));
        assert!(check.met);
        assert_eq!(check.remedy, None);

        let nothing = Expectation::Navico.check(&interfaces_without_addresses());
        assert!(nothing.met, "still worth searching");
        assert!(nothing.remedy.is_some(), "but say the host has no address");
    }

    /// Loopback is not a radar network and must not count as a match.
    #[test]
    fn loopback_is_not_an_address_a_radar_can_use() {
        let check = Expectation::RaymarineRd.check(&interfaces(&[("lo", "127.0.0.1")]));
        assert!(!check.met);
    }

    /// Under `--emulator`, or if the locator does not answer, there is no
    /// interface list. Telling the user their host has no address would send
    /// them reconfiguring a network that is fine.
    #[test]
    fn an_unreadable_interface_list_is_not_reported_as_a_missing_address() {
        let check = Expectation::Furuno.check(&InterfaceApi::default());

        assert!(check.met, "nothing is known to be wrong");
        assert!(check.finding.contains("could not read"));
        assert_eq!(check.remedy, None);
        assert!(
            check.requirement.contains("172.31.x.x"),
            "the requirement is still worth stating"
        );
    }

    /// Brands are cargo features. A build without one will never produce that
    /// radar, and telling the user their network is fine would have them
    /// checking cables for something that cannot arrive. Tested through the
    /// injected flag, because the test build has every brand compiled in.
    #[test]
    fn a_brand_this_build_cannot_look_for_is_said_so_plainly() {
        let check =
            Expectation::Garmin.check_supported(&interfaces(&[("eth0", "172.16.2.5")]), false);

        assert!(
            !check.met,
            "a correct network cannot save an absent locator"
        );
        assert!(check.finding.contains("no Garmin support"));
        assert!(check.remedy.is_some());

        // The same host, with the brand present, is fine.
        let supported =
            Expectation::Garmin.check_supported(&interfaces(&[("eth0", "172.16.2.5")]), true);
        assert!(supported.met);
    }

    /// Raymarine's three families are one brand to discovery, so all three
    /// stand or fall with the same feature.
    #[test]
    fn every_raymarine_expectation_belongs_to_the_raymarine_brand() {
        for expectation in [
            Expectation::RaymarineRd,
            Expectation::RaymarineQuantumMfd,
            Expectation::RaymarineQuantumStandalone,
        ] {
            assert_eq!(expectation.brand(), Brand::Raymarine);
        }
        assert_eq!(Expectation::Navico.brand(), Brand::Navico);
    }

    #[test]
    fn an_expectation_is_named_the_way_the_url_spells_it() {
        use std::str::FromStr;
        assert_eq!(
            Expectation::from_str("raymarine-quantum-mfd").unwrap(),
            Expectation::RaymarineQuantumMfd
        );
        assert_eq!(
            Expectation::from_str("FURUNO").unwrap(),
            Expectation::Furuno
        );
        assert!(Expectation::from_str("nonsense").is_err());
    }
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
