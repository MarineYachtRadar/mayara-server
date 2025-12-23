//! # Mayara Server
//!
//! Marine radar server with REST API and WebSocket support.
//!
//! This crate provides a complete radar server that:
//! - Discovers radars on the local network
//! - Provides a REST API for radar control
//! - Streams radar data via WebSocket
//! - Serves a web-based radar display
//!
//! ## Architecture
//!
//! The server is built on top of [`mayara_core`] for platform-independent
//! protocol handling, with [`tokio`] providing the async runtime.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    mayara-server                        │
//! │  ┌─────────────┐  ┌─────────────┐  ┌──────────────────┐ │
//! │  │ REST API    │  │ WebSocket   │  │ Web UI (static)  │ │
//! │  │ (axum)      │  │ (spokes)    │  │ (rust-embed)     │ │
//! │  └──────┬──────┘  └──────┬──────┘  └──────────────────┘ │
//! │         │                │                              │
//! │         ▼                ▼                              │
//! │  ┌─────────────────────────────────────────────────────┐│
//! │  │              SharedRadars (Arc<RwLock>)             ││
//! │  │  - Radar discovery & lifecycle                      ││
//! │  │  - Control state                                    ││
//! │  │  - Spoke data buffers                               ││
//! │  └─────────────────────────────────────────────────────┘│
//! │         │                                               │
//! │         ▼                                               │
//! │  ┌─────────────────────────────────────────────────────┐│
//! │  │              TokioIoProvider                        ││
//! │  │  - TCP/UDP socket management                        ││
//! │  │  - Implements mayara_core::IoProvider               ││
//! │  └─────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Key Components
//!
//! - [`Session`] - Main application state container
//! - [`radar::SharedRadars`] - Thread-safe radar registry
//! - `locator::Locator` - Network radar discovery (internal)
//! - [`tokio_io::TokioIoProvider`] - Tokio-based I/O for mayara-core
//! - [`core_locator::CoreLocatorAdapter`] - Bridges core's locator to tokio
//!
//! ## REST API
//!
//! The server exposes a REST API (via the `web` module in `main.rs`):
//!
//! | Endpoint | Description |
//! |----------|-------------|
//! | `GET /api/v1/radars` | List discovered radars |
//! | `GET /api/v1/radars/{id}` | Get radar details |
//! | `GET /api/v1/radars/{id}/controls` | Get control values |
//! | `PUT /api/v1/radars/{id}/controls/{name}` | Set control value |
//! | `WS /api/v1/radars/{id}/spokes` | WebSocket spoke stream |
//!
//! ## Example: Starting the Server
//!
//! ```rust,no_run
//! use clap::Parser;
//! use mayara_server::{Cli, Session};
//! use tokio_graceful_shutdown::Toplevel;
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() {
//!     let args = Cli::parse_from(["mayara-server", "-p", "8080"]);
//!
//!     Toplevel::new(|s| async move {
//!         let session = Session::new(&s, args).await;
//!         // Start web server, etc.
//!     })
//!     .catch_signals()
//!     .handle_shutdown_requests(Duration::from_secs(5))
//!     .await
//!     .unwrap();
//! }
//! ```
//!
//! ## Feature Flags
//!
//! Enable/disable support for specific radar brands:
//!
//! - `furuno` - Furuno radar support (default)
//! - `navico` - Navico radar support (default)
//! - `raymarine` - Raymarine radar support (default)
//! - `garmin` - Garmin radar support (default)
//!
//! ## Command-Line Interface
//!
//! See [`Cli`] for all available options. Key options:
//!
//! - `-p, --port` - HTTP server port (default: 6502)
//! - `-v` - Increase verbosity (use multiple times)
//! - `--replay` - Replay mode for testing without radar hardware
//! - `--interface` - Limit discovery to specific network interface

extern crate tokio;

use clap::Parser;
use locator::Locator;
use miette::Result;
use radar::SharedRadars;
use serde::{Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
};
use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

pub mod brand;
pub mod config;
pub mod control_factory;
pub mod core_locator;
pub mod locator;
pub mod navdata;
pub mod network;
pub mod protos;
pub mod radar;
pub mod recording;
pub mod settings;
pub mod storage;
pub mod tokio_io;
pub mod util;

#[cfg(feature = "dev")]
pub mod debug;
use rust_embed::RustEmbed;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/web/"]
pub struct ProtoAssets;

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

    /// Replay mode, see below
    #[arg(short, long, default_value_t = false)]
    pub replay: bool,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    pub fake_errors: bool,

    /// Allow wifi mode
    #[arg(long, default_value_t = false)]
    pub allow_wifi: bool,

    /// Stationary mode
    #[arg(long, default_value_t = false)]
    pub stationary: bool,

    /// Multi-radar mode keeps locators running even when one radar is found
    #[arg(long, default_value_t = false)]
    pub multiple_radar: bool,

    /// Use legacy brand-specific locators (deprecated)
    ///
    /// This uses the old brand-specific RadarLocatorState implementations.
    /// Default is now the unified core locator from mayara-core.
    #[arg(long, default_value_t = false)]
    pub legacy_locator: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Brand {
    Furuno,
    Garmin,
    Navico,
    Raymarine,
    #[clap(skip)]
    Playback,
}

impl Into<Brand> for &str {
    fn into(self) -> Brand {
        match self.to_ascii_lowercase().as_str() {
            "furuno" => Brand::Furuno,
            "garmin" => Brand::Garmin,
            "navico" => Brand::Navico,
            "raymarine" => Brand::Raymarine,
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
            Self::Playback => write!(f, "Playback"),
        }
    }
}

// Note: InterfaceApi and related types are still used by web.rs for the /api/interface endpoint
// The RadarInterfaceApi.ip field and some impl methods are unused since the legacy locator
// was removed, but we keep them for the interface API endpoint functionality.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RadarInterfaceApi {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[allow(dead_code)] // deprecated_marked_for_delete: only used by legacy locator response
    #[serde(skip)]
    ip: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    listeners: Option<HashMap<Brand, String>>,
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct InterfaceId {
    name: String,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceApi {
    pub brands: HashSet<Brand>,
    pub(crate) interfaces: HashMap<InterfaceId, RadarInterfaceApi>,
}

// deprecated_marked_for_delete: RadarInterfaceApi::new() and InterfaceId::new() are only used by legacy locator
#[allow(dead_code)]
impl RadarInterfaceApi {
    fn new(
        status: Option<String>,
        ip: Option<Ipv4Addr>,
        listeners: Option<HashMap<Brand, String>>,
    ) -> Self {
        Self {
            status,
            ip,
            listeners,
        }
    }
}

#[allow(dead_code)]
impl InterfaceId {
    fn new(name: &str, address: Option<&IpAddr>) -> Self {
        Self {
            name: match address {
                Some(addr) => format!("{} ({})", name, addr),
                None => name.to_owned(),
            },
        }
    }
}

impl Serialize for InterfaceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.name.as_str())
    }
}

/// Type alias for the shared debug hub.
#[cfg(feature = "dev")]
pub type SharedDebugHub = Arc<debug::DebugHub>;

pub struct SessionInner {
    pub args: Cli,
    pub tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
    pub radars: Option<SharedRadars>,
    /// Locator status from core (updated by CoreLocatorAdapter)
    pub locator_status: mayara_core::LocatorStatus,
    /// Debug hub for protocol analysis (only available with dev feature)
    #[cfg(feature = "dev")]
    pub debug_hub: Option<SharedDebugHub>,
}

#[derive(Clone)]
pub struct Session {
    pub inner: Arc<RwLock<SessionInner>>,
}

impl Session {
    pub fn read(
        &self,
    ) -> Result<RwLockReadGuard<'_, SessionInner>, PoisonError<RwLockReadGuard<'_, SessionInner>>>
    {
        self.inner.read()
    }

    pub fn write(
        &self,
    ) -> Result<RwLockWriteGuard<'_, SessionInner>, PoisonError<RwLockWriteGuard<'_, SessionInner>>>
    {
        self.inner.write()
    }

    #[cfg(test)]
    pub fn new_fake() -> Self {
        // This does not actually start anything - only use for testing
        Self::new_base(Cli::parse_from(["my_program"]))
    }

    fn new_base(args: Cli) -> Self {
        let (tx_interface_request, _) = broadcast::channel(10);
        let selfref = Session {
            inner: Arc::new(RwLock::new(SessionInner {
                args,
                tx_interface_request,
                radars: None,
                locator_status: mayara_core::LocatorStatus::default(),
                #[cfg(feature = "dev")]
                debug_hub: Some(Arc::new(debug::DebugHub::new())),
            })),
        };
        selfref
    }

    pub async fn new(subsystem: &SubsystemHandle, args: Cli) -> Self {
        let session = Self::new_base(args);

        let radars = SharedRadars::new(session.clone());

        session.write().unwrap().radars = Some(radars.clone());

        let use_legacy_locator = session.read().unwrap().args.legacy_locator;
        let locator = Locator::new(session.clone(), radars);

        // deprecated_marked_for_delete: Legacy locator used tx_ip_change and tx_interface_request
        // let (tx_ip_change, rx_ip_change) = mpsc::channel(1);
        let (_tx_ip_change, rx_ip_change) = mpsc::channel(1);
        let mut navdata = navdata::NavigationData::new(session.clone());

        subsystem.start(SubsystemBuilder::new("NavData", |subsys| async move {
            navdata.run(subsys, rx_ip_change).await
        }));
        // deprecated_marked_for_delete: Legacy locator used tx_interface_request
        // let tx_interface_request = session.write().unwrap().tx_interface_request.clone();

        // deprecated_marked_for_delete: Legacy locator code removed
        // if use_legacy_locator {
        //     log::warn!("Using legacy locator (--legacy-locator flag) - deprecated");
        //     subsystem.start(SubsystemBuilder::new("Locator", |subsys| {
        //         locator.run(subsys, tx_ip_change, tx_interface_request)
        //     }));
        // } else {
        //     ...
        // }

        // Use the unified core locator (default and only option now)
        if use_legacy_locator {
            log::error!(
                "--legacy-locator flag is no longer supported, legacy code has been commented out"
            );
            log::warn!("Falling back to unified core locator");
        }
        log::info!("Using unified core locator");
        subsystem.start(SubsystemBuilder::new("Locator", |subsys| {
            locator.run_with_core_locator(subsys)
        }));

        session
    }

    pub fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    pub fn args(&self) -> Cli {
        let args = { self.read().unwrap().args.clone() };
        args
    }

    /// Get the debug hub for protocol analysis (only available with dev feature).
    #[cfg(feature = "dev")]
    pub fn debug_hub(&self) -> Option<SharedDebugHub> {
        self.read().unwrap().debug_hub.clone()
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session {{ }}")
    }
}
