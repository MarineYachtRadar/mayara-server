//! # Mayara Core
//!
//! Platform-independent radar protocol library for marine radar systems.
//!
//! This crate contains pure parsing and protocol logic with **zero I/O dependencies**,
//! making it suitable for any platform including WebAssembly (WASM).
//!
//! ## Architecture
//!
//! `mayara-core` is designed to be the shared foundation between:
//! - **`mayara-server`**: Native server with REST API and WebSocket support
//! - **`mayara-signalk-wasm`**: WASM plugin for SignalK integration
//!
//! All platform-specific I/O is abstracted through the [`IoProvider`] trait,
//! allowing the same radar logic to run on any platform.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  mayara-core (platform-independent, no tokio/async deps)   │
//! │  ├── protocol/     (wire format parsing & formatting)      │
//! │  ├── models/       (radar model capabilities)              │
//! │  ├── capabilities/ (control definitions)                   │
//! │  ├── connection/   (state machine)                         │
//! │  └── IoProvider    (abstracts TCP/UDP I/O)                 │
//! └─────────────────────────────────────────────────────────────┘
//!                 ▲                           ▲
//!    ┌────────────┴────────────┐   ┌─────────┴─────────┐
//!    │  mayara-server          │   │ mayara-signalk    │
//!    │  (TokioIoProvider)      │   │ (WasmIoProvider)  │
//!    └─────────────────────────┘   └───────────────────┘
//! ```
//!
//! ## Supported Radars
//!
//! | Brand     | Models                                |
//! |-----------|---------------------------------------|
//! | Furuno    | DRS4D-NXT, DRS6A-NXT, FAR-21x7, etc  |
//! | Navico    | BR24, 3G, 4G, HALO series            |
//! | Raymarine | Quantum, RD series                   |
//! | Garmin    | xHD series                           |
//!
//! ## Key Modules
//!
//! - [`protocol`] - Wire protocol parsing and command formatting
//! - [`models`] - Radar model database with per-model capabilities
//! - [`capabilities`] - Control definitions (gain, range, filters, etc.)
//! - [`connection`] - Connection state machine with backoff logic
//! - [`io`] - Platform-agnostic I/O trait ([`IoProvider`])
//! - [`locator`] - Radar discovery abstraction
//! - [`arpa`] - Automatic Radar Plotting Aid (target tracking)
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
//! ## Example: Parsing a Furuno Beacon
//!
//! ```rust,no_run
//! use mayara_core::protocol::furuno;
//!
//! let packet: &[u8] = &[0u8; 32]; // Real packet from network
//! match furuno::parse_beacon_response(packet, "172.31.6.1") {
//!     Ok(discovery) => println!("Found radar: {}", discovery.name),
//!     Err(e) => println!("Parse error: {}", e),
//! }
//! ```
//!
//! ## Example: Using Connection State Machine
//!
//! ```rust
//! use mayara_core::{ConnectionManager, ConnectionState};
//!
//! let mut conn = ConnectionManager::new();
//! assert_eq!(conn.state(), ConnectionState::Disconnected);
//!
//! // Transition through states
//! conn.start_connecting(0);
//! assert!(conn.is_connecting());
//!
//! conn.connected(100);
//! assert!(conn.can_send());
//! ```
//!
//! ## Example: Control Dispatch
//!
//! ```rust,no_run
//! use mayara_core::protocol::furuno::dispatch;
//!
//! // Format a control command for the wire
//! if let Some(cmd) = dispatch::format_control_command("gain", 50, false) {
//!     // Send `cmd` to radar over TCP
//! }
//!
//! // Parse a response from the radar
//! let response = "$PFEC,GPrar,1,50*XX";
//! if let Some(update) = dispatch::parse_control_response(response) {
//!     // Handle the control update
//! }
//! ```

pub mod arpa;
pub mod brand;
pub mod capabilities;
pub mod connection;
pub mod controllers;
pub mod dual_range;
pub mod engine;
pub mod error;
pub mod guard_zones;
pub mod io;
pub mod locator;
pub mod models;
pub mod protocol;
pub mod radar;
pub mod state;
pub mod trails;

// Re-export commonly used types
pub use brand::Brand;
pub use capabilities::ControlId;
pub use connection::{ConnectionManager, ConnectionState, ReceiveSocketType};
pub use controllers::{
    ControllerEvent, ControllerState, FurunoController, GarminController, GarminControllerState,
    NavicoController, NavicoControllerState, NavicoModel, RaymarineController,
    RaymarineControllerState, RaymarineVariant,
};
pub use engine::{ManagedRadar, RadarController, RadarEngine};
pub use error::ParseError;
pub use io::{IoError, IoProvider, TcpSocketHandle, UdpSocketHandle};
pub use locator::{BrandStatus, DiscoveredRadar, LocatorEvent, LocatorStatus, RadarLocator};
pub use state::{ControlValueState, PowerState, RadarState};
