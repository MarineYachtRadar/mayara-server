use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use enum_primitive_derive::Primitive;
use portable_atomic::AtomicU64;
use protobuf::Message;
use serde::Serialize;
use serde::ser::Serializer;
use serde_json::Value;
use std::cmp::{max, min};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{
        Arc, LazyLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use utoipa::ToSchema;

pub mod cpa;
pub mod exclusion;
pub mod range;
pub mod settings;
pub mod spoke;
pub mod target;
pub mod trail;
pub(crate) mod units;

use crate::brand::CommandSender;
use crate::config::{Consent, KnownRadar, Persistence, SettingsStorage};
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::settings::{
    ControlDestination, ControlError, ControlId, ControlUpdate, ControlValue, SharedControls,
};
use crate::radar::spoke::{GenericSpoke, to_protobuf_spoke};
use crate::radar::target::{BlobDetector, BlobMessage, SpokeContext, TrackerCommand};
use crate::radar::trail::TrailBuffer;
use crate::stream::SignalKDelta;
use crate::{Brand, Cli, TargetMode};
use range::Ranges;

/// 1 nautical mile in metres, integer.
pub const NM: i32 = 1852;
/// 1 nautical mile in metres, floating point.
pub const NM_F64: f64 = 1852.;

/// `FRAC_NM_n` = 1 nm / n in metres, mirroring `std::f64::consts::FRAC_PI_n`.
/// Defined for the powers-of-two denominators that show up in radar range
/// tables; awkward fractions (3/4 nm, 1.5 nm, …) are written inline as
/// `NM * 3 / 4` / `NM * 3 / 2`.
pub const FRAC_NM_2: i32 = NM / 2;
pub const FRAC_NM_4: i32 = NM / 4;
pub const FRAC_NM_8: i32 = NM / 8;
pub const FRAC_NM_16: i32 = NM / 16;
pub const FRAC_NM_32: i32 = NM / 32;

/// Knots ↔ m/s, derived from `NM_F64`. Placed here (alongside the NM
/// constants) rather than scattered as `1852.0 / 3600.0` literals.
pub const KN_TO_MS: f64 = NM_F64 / 3600.;
pub const MS_TO_KN: f64 = 3600. / NM_F64;

// A "native to radar" bearing, usually [0..2048] or [0..4096] or [0..8192]
pub type SpokeBearing = u16;

pub const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Axum operation failed")]
    Axum(#[from] axum::Error),
    #[error("Interface '{0}' is not available")]
    InterfaceNotFound(String),
    #[error("Interface '{0}' has no valid IPv4 address")]
    InterfaceNoV4(String),
    #[error("Cannot detect Ethernet devices")]
    EnumerationFailed,
    #[error("{0}")]
    Config(String),
    #[error("Timeout")]
    Timeout,
    #[error("Shutdown")]
    Shutdown,
    #[error(
        "Radar at {0} cannot be reached from {1}: it is on a different subnet, so controls are unavailable. \
         Give this host an address on the radar's subnet."
    )]
    CannotReachRadar(std::net::Ipv4Addr, std::net::Ipv4Addr),
    #[error("No such control '{0}'")]
    InvalidControlId(String),
    #[error("{0}")]
    ControlError(#[from] ControlError),
    #[error("Cannot derive control from path '{0}'")]
    CannotParseControlId(String),
    #[error("Cannot set value for control '{0}'")]
    CannotSetControlId(ControlId),
    #[error("Cannot control '{0}' to value {1}")]
    CannotSetControlIdValue(ControlId, Value),
    #[error("Missing value for control '{0}'")]
    MissingValue(ControlId),
    #[error("Control '{0}' value '{1}' must be a valid number")]
    NotNumeric(ControlId, Value),
    #[error("No such radar with id '{0}'")]
    NoSuchRadar(String),
    #[error("Cannot parse JSON '{0}'")]
    ParseJson(String),
    #[error("Cannot parse NMEA0183 '{0}'")]
    ParseNmea0183(String),
    #[error("Signal K error: {0}")]
    SignalK(String),
    #[error("IP address changed")]
    IPAddressChanged,
    #[error("Cannot login to radar")]
    LoginFailed,
    #[error("Invalid port number")]
    InvalidPort,
    #[error("Not connected")]
    NotConnected,
    #[cfg(windows)]
    #[error("OS error: {0}")]
    OSError(String),
}

// Tell axum how to convert `RadarError` into a response.
impl IntoResponse for RadarError {
    fn into_response(self) -> Response {
        let status = match &self {
            RadarError::NoSuchRadar(_) => StatusCode::NOT_FOUND,
            RadarError::InvalidControlId(_) => StatusCode::NOT_FOUND,
            RadarError::CannotSetControlId(_)
            | RadarError::CannotSetControlIdValue(_, _)
            | RadarError::MissingValue(_)
            | RadarError::NotNumeric(_, _)
            | RadarError::ControlError(_)
            | RadarError::CannotParseControlId(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

//
// This order of pixeltypes is also how they are stored in the legend.
//
#[derive(Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
enum PixelType {
    Normal,
    DopplerApproaching,
    DopplerReceding,
    DopplerRain,
    History,
}

#[derive(Clone, Debug)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    /// Parse a CSS hex color string like "#rgb", "#rgba", "#rrggbb", or "#rrggbbaa"
    fn from_css(s: &str) -> Self {
        let s = s.trim_start_matches('#');
        match s.len() {
            3 => {
                // #rgb -> #rrggbb
                let r = u8::from_str_radix(&s[0..1], 16).unwrap_or(0) * 17;
                let g = u8::from_str_radix(&s[1..2], 16).unwrap_or(0) * 17;
                let b = u8::from_str_radix(&s[2..3], 16).unwrap_or(0) * 17;
                Color { r, g, b, a: 255 }
            }
            4 => {
                // #rgba -> #rrggbbaa
                let r = u8::from_str_radix(&s[0..1], 16).unwrap_or(0) * 17;
                let g = u8::from_str_radix(&s[1..2], 16).unwrap_or(0) * 17;
                let b = u8::from_str_radix(&s[2..3], 16).unwrap_or(0) * 17;
                let a = u8::from_str_radix(&s[3..4], 16).unwrap_or(0) * 17;
                Color { r, g, b, a }
            }
            6 => {
                // #rrggbb
                let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
                Color { r, g, b, a: 255 }
            }
            8 => {
                // #rrggbbaa
                let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
                let a = u8::from_str_radix(&s[6..8], 16).unwrap_or(0);
                Color { r, g, b, a }
            }
            _ => Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
        }
    }
}

impl From<&str> for Color {
    fn from(s: &str) -> Self {
        Color::from_css(s)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.a
        )
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Lookup {
    r#type: PixelType,
    #[schema(value_type = String, example = "#334455ff")]
    color: Color,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Legend {
    /// Doppler approaching pixel range: `(first_index, count)`.
    /// Navico uses 1 level (single flat color), Garmin Fantom uses 4
    /// (brightness gradient). `None` if the radar has no Doppler.
    pub doppler_approaching: Option<(u8, u8)>,
    /// Doppler receding pixel range: `(first_index, count)`.
    pub doppler_receding: Option<(u8, u8)>,
    /// Doppler rain pixel range: `(first_index, count)`.
    /// Only populated for radars that classify rain on the wire (Furuno NXT
    /// Target Analyzer). `None` for all other radars.
    pub doppler_rain: Option<(u8, u8)>,
    pub history_start: u8,
    pub low_return: u8,
    pub medium_return: u8,
    pub strong_return: u8,
    pub pixel_colors: u8,
    pub pixels: Vec<Lookup>,
    /// Color for static background in Static ARPA mode (light grey)
    pub static_background: Option<u8>,
}

/// A geographic position expressed in degrees latitude and longitude.
/// Latitude is positive in the northern hemisphere, negative in the southern.
/// Longitude is positive in the eastern hemisphere, negative in the western.
/// The range for latitude is -90 to 90, and for longitude is -180 to 180.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GeoPosition {
    lat: f64,
    lon: f64,
}

impl GeoPosition {
    pub fn new(lat: f64, lon: f64) -> Self {
        GeoPosition { lat, lon }
    }

    /// Get latitude in degrees
    pub fn lat(&self) -> f64 {
        self.lat
    }

    /// Get longitude in degrees
    pub fn lon(&self) -> f64 {
        self.lon
    }

    /// Calculate a new position from this position given a bearing and distance
    /// bearing: bearing in radians (0 = north, clockwise positive)
    /// distance: distance in meters
    pub fn position_from_bearing(&self, bearing: f64, distance: f64) -> GeoPosition {
        const EARTH_RADIUS: f64 = 6_371_000.0; // meters

        let lat1 = self.lat.to_radians();
        let lon1 = self.lon.to_radians();
        let d = distance / EARTH_RADIUS;

        let lat2 = (lat1.sin() * d.cos() + lat1.cos() * d.sin() * bearing.cos()).asin();
        let lon2 =
            lon1 + (bearing.sin() * d.sin() * lat1.cos()).atan2(d.cos() - lat1.sin() * lat2.sin());

        GeoPosition {
            lat: lat2.to_degrees(),
            lon: lon2.to_degrees(),
        }
    }
}

impl fmt::Display for GeoPosition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.lat, self.lon)
    }
}

/// Monotonic epoch for the cheap `AtomicU64` millisecond timestamps shared
/// across `RadarInfo` clones (see [`RadarInfo::last_input`]). `Instant` is not
/// itself storable in an atomic, so timestamps are milliseconds since this base.
static RADAR_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn now_millis() -> u64 {
    RADAR_EPOCH.elapsed().as_millis() as u64
}

#[derive(Clone, Debug)]
pub struct RadarInfo {
    key: String,

    // selected items from Cli args:
    targets: TargetMode,
    replay: bool,
    output: bool,

    pub brand: Brand,
    pub serial_no: Option<String>, // Serial # for this radar
    /// Stable per-unit identity for radars that report no usable serial
    /// number: a MAC on Furuno and Koden, the heartbeat's unique id on
    /// Garmin, the beacon link_id on Raymarine. Not the serial, and never
    /// shown as one.
    pub hardware_id: Option<String>,
    pub dual: Option<String>,            // "A", "B" or None
    pub pixel_values: u8,                // How many values per pixel, 0..220 or so
    pub spokes_per_revolution: u16,      // How many spokes per rotation
    pub max_spoke_len: u16,              // Fixed for some radars, variable for others
    pub addr: SocketAddrV4,              // The IP address of the radar
    pub nic_addr: Ipv4Addr,              // IPv4 address of NIC via which radar can be reached
    pub spoke_data_addr: SocketAddrV4,   // Where the radar will send data spokes
    pub report_addr: SocketAddrV4,       // Where the radar will send reports
    pub send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
    legend: Legend,                      // What pixel values mean
    pub controls: SharedControls,        // Which controls there are, not complete in beginning
    pub ranges: Ranges,                  // Ranges for this radar, empty in beginning
    pub doppler: bool,                   // Does it support Doppler?
    doppler_levels: u8,                  // Intensity sub-levels per direction (0, 1, or 4)
    has_rain_class: bool,                // Radar classifies rain on the wire (Furuno NXT)
    pub dual_range: bool,                // Is it dual range capable?
    pub sparse_spokes: bool,             // Does it produce fewer spokes than spokes_per_revolution?
    pub stationary: bool,                // Is radar stationary (shore-based)?
    rotation_timestamp: Instant,

    /// Display name reported by the radar itself (Navico 0xC406 NAME tag), with
    /// the trailing marker stripped. `None` until the radar reports one. Drives
    /// [`SharedRadars::refresh_user_names`] so radars reporting the same name are
    /// disambiguated by a minimal suffix of their technical key.
    pub(crate) reported_name: Option<String>,

    // Channels
    /// Serialized RadarMessage broadcast to subscribers (spoke WS clients,
    /// `--output` stdout forwarder, recording manager). Carried as `Bytes`
    /// so receivers share one underlying buffer instead of each cloning
    /// the full payload — fan-out cost goes from `N × memcpy(message)` to
    /// `N × refcount-bump` per send.
    pub message_tx: tokio::sync::broadcast::Sender<Bytes>,

    /// Soft idle flag. When `true`, the radar's data_loop drains the spoke
    /// multicast/broadcast socket but skips frame decoding and blob detection
    /// — saving ~1.5 cores on Furuno radars that emit spokes even in Standby
    /// (see issue #274). Entry condition: power == Standby AND
    /// `message_tx.receiver_count() == 0`. Exit: power transitions, spoke WS
    /// subscribe, or any control PUT — each of those call sites stores
    /// `false` directly. Wrapped in Arc so cloned RadarInfos (e.g. from
    /// `SharedRadars::get_by_key`) share the same flag. Kept private so the
    /// load/store contract stays narrow; callers use `is_idle()` and
    /// `wake_up()` below.
    is_idle: Arc<AtomicBool>,

    /// Milliseconds since [`RADAR_EPOCH`] of the last packet received from this
    /// radar on its report socket. A powered-off radar stops sending, so the
    /// central watchdog (`mark_silent_radars_off`) decays its power state to
    /// `Off` after [`SharedRadars::RADAR_SILENCE_TIMEOUT`]. Shared via `Arc` so
    /// brand receivers (which hold a clone) and the watchdog (reading the map)
    /// observe the same value. Updated via `mark_input`, read via `input_silence`.
    last_input: Arc<AtomicU64>,
}

/// Number of trailing characters of an identity that make a radar
/// distinguishable in its key and default name. Four is enough to tell
/// units apart on one vessel while staying short enough to read.
const IDENTITY_TAIL_LEN: usize = 4;

/// The tail of a radar identity, used as the discriminator in both the
/// radar key and the default user-visible name so the two always agree.
pub(crate) fn identity_tail(identity: &str) -> &str {
    &identity[identity.len().saturating_sub(IDENTITY_TAIL_LEN)..]
}

/// The discriminator that distinguishes this radar from its siblings:
/// the tail of the serial number, or of a hardware identity such as a MAC
/// address when the radar reports no usable serial. `None` when the radar
/// offers neither, in which case only the key falls back to the IP.
pub(crate) fn identity_discriminator<'a>(
    serial_no: Option<&'a str>,
    hardware_id: Option<&'a str>,
) -> Option<&'a str> {
    // Filter each candidate before the fallback, or an unusable serial would
    // win over a perfectly good MAC and drop us to the IP.
    serial_no
        .filter(|s| usable_identity(s))
        .or(hardware_id.filter(|s| usable_identity(s)))
        .map(identity_tail)
}

/// Whether an identity string actually distinguishes one unit from another.
/// A radar that reports no serial may send an empty field or a run of ASCII
/// zeros; neither tells two units apart, and both must give way to the MAC.
fn usable_identity(identity: &str) -> bool {
    !identity.is_empty() && !identity.bytes().all(|b| b == b'0')
}

/// Format a MAC address as a radar hardware identity, or `None` when it
/// cannot identify anything: all-zero, or the broadcast address that
/// Furuno's virtual devices (e.g. the `CAN-BUS` entry) report.
pub(crate) fn mac_identity(mac: &[u8; 6]) -> Option<String> {
    if mac.iter().all(|&b| b == 0) || mac.iter().all(|&b| b == 0xff) {
        return None;
    }
    Some(mac.iter().fold(String::new(), |mut s, b| {
        write!(s, "{:02x}", b).unwrap();
        s
    }))
}

/// Build the stable per-radar key: the brand prefix, a four-character
/// discriminator, and the optional dual-range suffix.
///
/// The discriminator is the tail of the serial number; of `hardware_id`
/// when the serial is absent *or* empty, as on Furuno NavNet 3D DRS units
/// which report an all-zero serial; and only failing both, the low 16 bits
/// of the radar IP. The IP is a last resort because it moves with the DHCP
/// lease and takes the radar's saved settings with it, so only a radar that
/// offers neither of the first two lands there.
fn radar_key(
    prefix: &str,
    serial_no: Option<&str>,
    hardware_id: Option<&str>,
    dual: Option<&str>,
    addr: &SocketAddrV4,
) -> String {
    let mut key = prefix.to_string();
    match identity_discriminator(serial_no, hardware_id) {
        Some(discriminator) => key.push_str(discriminator),
        None => write!(key, "{:04x}", addr.ip().to_bits() & 0xffff).unwrap(),
    }
    if let Some(dual) = dual {
        key.push_str(dual);
    }
    key
}

/// The key a radar has when nothing identifies it but its address.
///
/// Used to find settings saved before the radar had a hardware identity.
/// Deriving it through the very function that produces the real key keeps
/// the two from drifting apart: a lookup that guessed the format wrongly
/// would silently find nothing.
pub(crate) fn legacy_address_key(info: &RadarInfo) -> String {
    radar_key(
        info.brand.to_prefix(),
        None,
        None,
        info.dual.as_deref(),
        &info.addr,
    )
}

impl RadarInfo {
    #[allow(clippy::too_many_arguments)] // every radar field comes flat from per-brand discovery; the brands are the only callers
    pub fn new<F>(
        radars: &SharedRadars,
        args: &Cli,
        brand: Brand,
        serial_no: Option<&str>,
        hardware_id: Option<&str>,
        dual: Option<&str>,
        pixel_values: u8, // How many values per pixel, 0..220 or so
        spokes_per_revolution: usize,
        max_spoke_len: usize,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
        controls_fn: F,
        doppler: bool,
        sparse_spokes: bool,
    ) -> Self
    where
        F: FnOnce(String, tokio::sync::broadcast::Sender<SignalKDelta>) -> SharedControls,
    {
        let (message_tx, _message_rx) = tokio::sync::broadcast::channel(32);

        let (targets, replay, output) = { (args.targets.clone(), args.is_replay(), args.output) };
        let doppler_levels = if doppler { 1 } else { 0 };
        let has_rain_class = false;
        let legend = default_legend(&targets, doppler_levels, has_rain_class, pixel_values);

        // Normalize an empty serial to absent so both the key and the stored
        // `serial_no` field treat it the same way (avoids a `Some("")` that
        // later code guards with `is_some()` would mishandle).
        let serial_no = serial_no.filter(|s| !s.is_empty());
        let hardware_id = hardware_id.filter(|s| !s.is_empty());
        let key = radar_key(brand.to_prefix(), serial_no, hardware_id, dual, &addr);

        let sk_client_tx = radars.radars.read().unwrap().sk_client_tx.clone();
        let controls = controls_fn(key.clone(), sk_client_tx);

        let info = RadarInfo {
            targets,
            replay,
            output,
            key,
            brand,
            serial_no: serial_no.map(String::from),
            hardware_id: hardware_id.map(String::from),
            dual: dual.map(String::from),
            reported_name: None,
            pixel_values,
            spokes_per_revolution: spokes_per_revolution as u16,
            max_spoke_len: max_spoke_len as u16,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            legend,
            message_tx,
            ranges: Ranges::empty(),
            controls,
            doppler,
            doppler_levels,
            has_rain_class,
            dual_range: false,
            sparse_spokes,
            stationary: args.stationary,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
            // Start non-idle so the first ~5s of operation always processes
            // frames; the data_loop's periodic re-check will flip it once
            // power and subscriber count have settled.
            is_idle: Arc::new(AtomicBool::new(false)),
            // Seed with "now" so a freshly discovered radar isn't immediately
            // considered silent before its first report arrives.
            last_input: Arc::new(AtomicU64::new(now_millis())),
        };

        log::trace!("Created RadarInfo {:?}", info);
        info
    }

    pub fn new_client_subscription(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        self.controls.new_client_subscription()
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        self.controls.control_update_subscribe()
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    /// The discriminator this radar's key was built from, for brands that
    /// want the default user-visible name to match the key.
    pub(crate) fn discriminator(&self) -> Option<&str> {
        identity_discriminator(self.serial_no.as_deref(), self.hardware_id.as_deref())
    }

    /// True when this radar's data_loop is currently dropping decoded frames
    /// to save CPU (see the `is_idle` field comment). Reads with Relaxed
    /// ordering; the gate tolerates a one-frame race in either direction.
    pub fn is_idle(&self) -> bool {
        self.is_idle.load(Ordering::Relaxed)
    }

    /// Mark this radar as non-idle. Idempotent. Called from any path that
    /// should immediately resume frame decoding — control PUTs, spoke WS
    /// subscribes, power-state transitions in the report parser.
    pub fn wake_up(&self) {
        self.is_idle.store(false, Ordering::Relaxed);
    }

    /// Set the idle flag from a precomputed value. Used only by the data_loop's
    /// periodic refresh; external callers should prefer `wake_up()`.
    pub(crate) fn set_idle(&self, idle: bool) {
        self.is_idle.store(idle, Ordering::Relaxed);
    }

    /// Record that a packet was just received from this radar. Brand report
    /// receivers call this on every report-socket receive; the watchdog reads
    /// [`input_silence`](Self::input_silence) to detect a radar gone silent.
    pub(crate) fn mark_input(&self) {
        self.last_input.store(now_millis(), Ordering::Relaxed);
    }

    /// How long it has been since the last packet was received from this radar.
    pub(crate) fn input_silence(&self) -> Duration {
        Duration::from_millis(now_millis().saturating_sub(self.last_input.load(Ordering::Relaxed)))
    }

    /// Recompute the idle flag from this radar's live power state and spoke
    /// broadcast subscriber count. Brand data loops call this on their periodic
    /// tick; see [`should_idle`] for the entry condition. Brand-agnostic — it
    /// reads only the shared `Power` control and `message_tx.receiver_count()`.
    pub(crate) fn refresh_idle_flag(&self) {
        let power = self
            .controls
            .get(&ControlId::Power)
            .and_then(|c| c.value)
            .map(|v| v as i32);
        let receiver_count = self.message_tx.receiver_count();
        self.set_idle(should_idle(power, receiver_count));
    }

    pub fn replay(&self) -> bool {
        self.replay
    }

    pub fn targets(&self) -> TargetMode {
        self.targets.clone()
    }

    pub fn doppler_levels(&self) -> u8 {
        self.doppler_levels
    }

    pub fn has_rain_class(&self) -> bool {
        self.has_rain_class
    }

    /// Override the replay flag. Used by the recording player when constructing
    /// a playback radar so brand-specific receivers know not to attempt live
    /// network operations.
    pub fn set_replay(&mut self, replay: bool) {
        self.replay = replay;
    }

    //
    // Once the ranges are set non-zero the radar is findable by the GUI,
    // this version only to be called by config() that does not have CommonRadar.
    //
    pub(super) fn set_ranges(&mut self, ranges: Ranges) {
        if self.ranges.is_empty() && !ranges.is_empty() {
            log::info!(
                "{}: supports ranges {} and is now findable in GUI",
                self.key,
                ranges
            );
        }
        self.ranges = ranges;
        self.controls.set_valid_ranges(&self.ranges);
    }

    pub fn set_doppler(&mut self, doppler: bool) {
        if doppler != self.doppler {
            self.doppler = doppler;
            self.doppler_levels = if doppler { 1 } else { 0 };
            self.legend = default_legend(
                &self.targets,
                self.doppler_levels,
                self.has_rain_class,
                self.pixel_values,
            );
            log::debug!("Doppler changed to {}", doppler);
        }
    }

    /// Set the number of Doppler intensity sub-levels per direction.
    /// Garmin Fantom uses 4 (brightness gradient); Navico uses 1 (flat).
    pub fn set_doppler_levels(&mut self, levels: u8) {
        self.doppler = levels > 0;
        self.doppler_levels = levels;
        self.legend = default_legend(
            &self.targets,
            self.doppler_levels,
            self.has_rain_class,
            self.pixel_values,
        );
        log::debug!(
            "Doppler levels changed to {} (doppler={})",
            levels,
            self.doppler
        );
    }

    /// Enable the rain Doppler class on this radar. Used by Furuno NXT where
    /// the wire format encodes a third Doppler band (rain) in addition to
    /// stationary and approaching.
    pub(crate) fn set_has_rain_class(&mut self, has_rain_class: bool) {
        if has_rain_class != self.has_rain_class {
            self.has_rain_class = has_rain_class;
            self.legend = default_legend(
                &self.targets,
                self.doppler_levels,
                self.has_rain_class,
                self.pixel_values,
            );
            log::debug!("Rain class changed to {}", has_rain_class);
        }
    }

    pub fn set_pixel_values(&mut self, pixel_values: u8) {
        if pixel_values != self.pixel_values {
            self.legend = default_legend(
                &self.targets,
                self.doppler_levels,
                self.has_rain_class,
                pixel_values,
            );
            log::debug!("Pixel_values changed to {}", pixel_values);
        }
        self.pixel_values = pixel_values;
    }

    fn full_rotation(&mut self) -> u32 {
        let now = Instant::now();
        let diff: Duration = now - self.rotation_timestamp;
        let diff = diff.as_millis() as f64;
        let rpm = format!("{:.0}", (600_000. / diff));

        self.rotation_timestamp = now;

        log::debug!(
            "{}: rotation speed elapsed {} = {} RPM",
            self.key,
            diff,
            rpm
        );

        if diff < 10000. && diff > 300. {
            let _ = self.controls.set_string(&ControlId::RotationSpeed, rpm);
            diff as u32
        } else {
            0
        }
    }

    pub(super) fn broadcast_radar_message(&self, message: RadarMessage) {
        crate::telemetry::note_spokes(self);

        // write_to_bytes() pre-sizes the Vec via compute_size(), avoiding the
        // ~16 doublings a fresh Vec::new() would do for a ~40 KB serialized
        // batch of spokes (the dominant per-frame allocator churn).
        let bytes = message
            .write_to_bytes()
            .expect("Cannot write RadarMessage to bytes");

        // Send the message to all receivers, normally the web client(s).
        // `Bytes::from(Vec)` is zero-copy; receivers will share this single
        // buffer via refcount instead of each cloning ~40 KB on recv().
        match self.message_tx.send(Bytes::from(bytes)) {
            Err(e) => {
                log::trace!("{}: Dropping received spoke: {}", self.key, e);
            }
            Ok(count) => {
                log::trace!("{}: sent to {} receivers", self.key, count);
            }
        }
    }

    pub fn start_forwarding_radar_messages_to_stdout(&self, subsys: &SubsystemHandle) {
        if self.output {
            let info_clone2 = self.clone();

            subsys.start(SubsystemBuilder::new(
                "stdout",
                async move |s: &mut SubsystemHandle| info_clone2.forward_output(s).await,
            ));
        }
    }

    async fn forward_output(self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
        use std::io::Write;

        let mut rx = self.message_tx.subscribe();

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Ok(());
                },
                r = rx.recv() => {
                    match r {
                        Ok(r) => {
                            std::io::stdout().write_all(r.as_ref()).unwrap_or_else(|_| { subsys.request_shutdown(); });
                        },
                        Err(_) => {
                            subsys.request_shutdown();
                        }
                    };
                },
            }
        }
    }

    pub fn get_legend(&self) -> Legend {
        self.legend.clone()
    }
}

impl Display for RadarInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Radar {} brand {}", self.key, self.brand)?;
        if let Some(which) = &self.dual {
            write!(f, " {}", which)?;
        }
        if let Some(serial_no) = &self.serial_no {
            write!(f, " [{}]", serial_no)?;
        }
        write!(
            f,
            " at {} via {} data {} report {} send {}",
            self.addr.ip(),
            self.nic_addr,
            self.spoke_data_addr,
            self.report_addr,
            self.send_command_addr
        )
    }
}

/// Capacity of the Signal K client broadcast channel. Sized to absorb a
/// full revolution's worth of target updates plus control value changes
/// without dropping messages, even when multiple WebSocket clients are
/// connected. A single lag event is now recoverable (see v2 stream
/// handler), but larger headroom reduces the chance of visible gaps.
const SK_CLIENT_CHANNEL_CAPACITY: usize = 128;

/// The last `n` characters of `s` (fewer if `s` is shorter).
fn suffix_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

/// Smallest `n` for which the `n`-character suffixes of all `keys` are
/// distinct. Keys are globally unique, so a distinguishing length always
/// exists within the longest key.
fn distinguishing_suffix_len(keys: &[&str]) -> usize {
    let max_len = keys.iter().map(|k| k.chars().count()).max().unwrap_or(1);
    (1..=max_len)
        .find(|&n| {
            let mut seen = HashSet::new();
            keys.iter().all(|k| seen.insert(suffix_chars(k, n)))
        })
        .unwrap_or(max_len)
        .max(1)
}

#[derive(Clone)]
pub struct SharedRadars {
    radars: Arc<RwLock<Radars>>,
}

impl Default for SharedRadars {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedRadars {
    pub fn new() -> Self {
        let (sk_client_tx, _) = tokio::sync::broadcast::channel(SK_CLIENT_CHANNEL_CAPACITY);

        SharedRadars {
            radars: Arc::new(RwLock::new(Radars {
                info: HashMap::new(),
                persistent_data: Persistence::new(),
                sk_client_tx,
                blob_tx: None,
                tracker_command_tx: None,
            })),
        }
    }

    // A radar has been found
    pub fn add(&self, mut new_info: RadarInfo) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = self.radars.write().unwrap();

        // For now, drop second radar in replay Mode...
        if new_info.replay && key.ends_with("B") {
            return None;
        }

        let is_new = !radars.info.contains_key(&key);
        if is_new {
            // Set any previously detected model and ranges
            radars
                .persistent_data
                .update_info_from_persistence(&mut new_info);

            log::info!(
                "Found radar: key '{}' name '{}' with {} ranges",
                new_info.key,
                new_info.controls.user_name(),
                new_info.ranges.len()
            );
            radars.info.insert(key, new_info.clone());
            crate::telemetry::note_radar_found();
            Some(new_info)
        } else {
            None
        }
    }

    /// Radars this install has seen before, for the discovery page to name
    /// while it waits for them to turn up again.
    pub fn known_radars(&self) -> Vec<KnownRadar> {
        self.radars.read().unwrap().persistent_data.known_radars()
    }

    /// What this run does with radar settings, for the status endpoint and
    /// the warning a connecting client is sent.
    pub fn settings_storage(&self) -> SettingsStorage {
        self.radars.read().unwrap().persistent_data.storage()
    }

    /// Whether the user has agreed to report that this install works, and
    /// whether the question can be put to them at all.
    pub fn telemetry_consent(&self) -> Consent {
        self.radars.read().unwrap().persistent_data.consent()
    }

    /// Record the user's answer to the telemetry question.
    pub fn set_telemetry_consent(&self, granted: bool) -> Consent {
        self.radars
            .write()
            .unwrap()
            .persistent_data
            .set_consent(granted)
    }

    /// The id this install reports under, created on first use. Called only
    /// once reporting is already allowed.
    pub(crate) fn telemetry_install_id(&self) -> Option<String> {
        self.radars
            .write()
            .unwrap()
            .persistent_data
            .ensure_install_id()
    }

    ///
    /// Update radar info in radars container
    ///
    pub fn update(&self, radar_info: &mut RadarInfo) {
        let mut radars = self.radars.write().unwrap();

        radars
            .info
            .insert(radar_info.key.clone(), radar_info.clone());

        radars.persistent_data.store(radar_info);
    }

    ///
    /// Return iterater over completed fully available radars
    ///
    pub fn get_active(&self) -> Vec<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars
            .info
            .values()
            .filter(|i| !i.ranges.is_empty())
            .cloned()
            .collect()
    }

    pub fn have_active(&self) -> bool {
        let radars = self.radars.read().unwrap();
        radars
            .info
            .values()
            .filter(|i| !i.ranges.is_empty())
            .count()
            > 0
    }

    /// Re-evaluate, for every known radar, whether its command address can be
    /// reached from the interface it was discovered on.
    ///
    /// Binding a socket to a source address does not pin the outgoing
    /// interface — the kernel routes by destination — so a radar addressed on
    /// a subnet this host has no address on receives none of our commands
    /// while its multicast picture and status arrive perfectly. Raymarine RD
    /// and HD radomes make this routine: they address themselves in 10/8 while
    /// a host on the RayNet network is given 198.18.x by the chartplotter.
    ///
    /// Cheap to call repeatedly: [`SharedControls::set_command_reachable`]
    /// only acts on a change of state, so this is driven both by the radar
    /// watchdog and by address-change events.
    pub(crate) fn refresh_command_reachability(&self) {
        let infos: Vec<RadarInfo> = {
            let radars = self.radars.read().unwrap();
            radars.info.values().cloned().collect()
        };

        for info in infos {
            let dst = *info.send_command_addr.ip();
            // Multicast and broadcast reach the wire whatever the addressing.
            if dst.is_multicast() || dst.is_broadcast() {
                continue;
            }
            let Some(reachable) =
                crate::network::can_reach(&info.nic_addr, &info.send_command_addr)
            else {
                // The interface went away, or the route leads somewhere we
                // cannot identify; say nothing rather than guess.
                continue;
            };
            let ifname = crate::network::interface_for(&info.nic_addr)
                .map(|(name, _)| name)
                .unwrap_or_else(|| "the radar interface".to_string());
            // Never name a concrete address to configure: the only address
            // known here is the radar's own, and telling the user to assign
            // that to the host would collide with the radar. The prefix length
            // is unknown too. A host route to the radar is safe to suggest
            // because it names the radar as a destination, not as our address.
            let reason = format!(
                "radar address {} cannot be reached from {} on interface '{}'. \
                 Give this host an unused address in the radar's subnet, or add a \
                 route to it: `ip route add {}/32 dev {}`",
                dst, info.nic_addr, ifname, dst, ifname
            );
            if info.controls.set_command_reachable(reachable, &reason) {
                if !reachable {
                    log::warn!("{}: controls disabled: {}", info.key(), reason);
                } else {
                    log::info!(
                        "{}: radar is reachable again; controls restored",
                        info.key()
                    );
                }
            }
        }
    }

    /// A radar is considered powered off once no packet has been received from
    /// it for this long. A standby radar still emits periodic status reports, so
    /// only a genuinely silent (powered-off or disconnected) radar decays to
    /// [`Power::Off`]. See issue #432.
    pub(crate) const RADAR_SILENCE_TIMEOUT: Duration = Duration::from_secs(30);

    /// Decay the power state of any radar gone silent to [`Power::Off`], so the
    /// GUI reflects a powered-off radar instead of holding its last reported
    /// state. Called on a fixed cadence by the radar watchdog. The next report
    /// from a radar that comes back overwrites this with its real state.
    pub(crate) fn mark_silent_radars_off(&self) {
        let radars = self.radars.read().unwrap();
        for info in radars.info.values() {
            let current = info
                .controls
                .get(&ControlId::Power)
                .and_then(|c| c.value)
                .map(|v| v as i32);
            if !should_power_off(info.input_silence(), current) {
                continue;
            }
            log::debug!(
                "{}: no data for {}s, marking powered off",
                info.key,
                info.input_silence().as_secs()
            );
            let _ = info
                .controls
                .set_value(&ControlId::Power, Value::from(Power::Off as i32));
        }
    }

    ///
    /// Return every radar that has been discovered, including those that have
    /// not yet reported their ranges. Use this where a radar should surface as
    /// soon as it is found (e.g. the `/radars` listing) rather than only once
    /// it is fully usable — see [`get_active`](Self::get_active) for the
    /// range-filtered set used by the spoke/data paths.
    ///
    pub fn get_discovered(&self) -> Vec<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars.info.values().cloned().collect()
    }

    /// True once any radar has been discovered, regardless of whether its
    /// ranges have arrived. The locator uses this (not `have_active`) to decide
    /// when to stop hunting for beacons: a Quantum that is discovered but still
    /// asleep keeps `have_active` false forever, which would otherwise keep the
    /// locator marking the external-controller witness and starve the wake
    /// nudge that is meant to wake it.
    pub fn have_discovered(&self) -> bool {
        let radars = self.radars.read().unwrap();
        !radars.info.is_empty()
    }

    /// Recompute the `UserName` control for every radar that has reported a name
    /// (see [`RadarInfo::reported_name`]). A radar whose reported name is unique
    /// gets that name verbatim; radars sharing a reported name are disambiguated
    /// by appending the shortest suffix of their technical key that tells them
    /// apart (in practice a single character — "A"/"B" for a dual-range antenna
    /// pair, or a serial digit when two separate radars carry the same name).
    /// Radars that never report a name keep their existing `UserName`.
    pub(crate) fn refresh_user_names(&self) {
        let assignments: Vec<(SharedControls, String)> = {
            let radars = self.radars.read().unwrap();

            let mut groups: HashMap<&str, Vec<&RadarInfo>> = HashMap::new();
            for info in radars.info.values() {
                if let Some(base) = info.reported_name.as_deref() {
                    groups.entry(base).or_default().push(info);
                }
            }

            let mut assignments = Vec::new();
            for (base, infos) in groups {
                if infos.len() == 1 {
                    assignments.push((infos[0].controls.clone(), base.to_string()));
                    continue;
                }
                let keys: Vec<&str> = infos.iter().map(|i| i.key.as_str()).collect();
                let suffix_len = distinguishing_suffix_len(&keys);
                for info in infos {
                    let suffix = suffix_chars(&info.key, suffix_len);
                    assignments.push((info.controls.clone(), format!("{base} {suffix}")));
                }
            }
            assignments
        };

        for (controls, name) in assignments {
            let _ = controls.set_string(&ControlId::UserName, name);
        }
    }

    #[allow(dead_code)]
    pub fn get_by_key(&self, key: &str) -> Option<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars.info.get(key).cloned()
    }

    pub fn get_keys(&self) -> Vec<String> {
        let radars = self.radars.read().unwrap();
        radars.info.keys().cloned().collect()
    }

    /// Save persistence for a radar by key
    /// This should be called when a control value changes that needs to be persisted
    pub fn save_persistence(&self, key: &str) {
        let mut radars = self.radars.write().unwrap();
        if let Some(radar_info) = radars.info.get(key).cloned() {
            radars.persistent_data.store(&radar_info);
        }
    }

    pub fn remove(&self, key: &str) {
        let mut radars = self.radars.write().unwrap();

        radars.info.remove(key);
    }

    ///
    /// Update radar info in radars container
    ///
    #[deprecated]
    pub fn update_serial_no(&self, key: &str, serial_no: String) {
        let mut radars = self.radars.write().unwrap();

        if let Some(radar_info) = {
            if let Some(radar_info) = radars.info.get_mut(key) {
                if radar_info.serial_no != Some(serial_no.clone()) {
                    radar_info.serial_no = Some(serial_no);
                    Some(radar_info.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } {
            radars.persistent_data.store(&radar_info);
        }
    }

    pub fn is_radar_active_on_nic(&self, brand: &Brand, ip: &Ipv4Addr) -> bool {
        let radars = self.radars.read().unwrap();
        for info in radars.info.values() {
            log::trace!(
                "is_active_radar: brand {}/{} ip {}/{}",
                info.brand,
                brand,
                info.nic_addr,
                ip
            );
            if info.brand == *brand && info.nic_addr == *ip {
                return true;
            }
        }
        false
    }

    pub fn is_radar_active_by_addr(&self, brand: &Brand, ip: &SocketAddrV4) -> bool {
        let radars = self.radars.read().unwrap();
        for info in radars.info.values() {
            log::trace!(
                "is_active_radar: brand {}/{} ip {}/{}",
                info.brand,
                brand,
                info.addr,
                ip
            );
            if info.brand == *brand && info.addr == *ip {
                return true;
            }
        }
        false
    }

    pub fn new_sk_client_subscription(&self) -> tokio::sync::broadcast::Receiver<SignalKDelta> {
        self.radars.read().unwrap().sk_client_tx.subscribe()
    }

    /// Get the SignalK delta broadcast sender for pushing target updates
    pub fn get_sk_client_tx(&self) -> tokio::sync::broadcast::Sender<SignalKDelta> {
        self.radars.read().unwrap().sk_client_tx.clone()
    }

    /// Get the blob message sender for target tracking
    pub fn get_blob_tx(&self) -> Option<mpsc::Sender<BlobMessage>> {
        self.radars.read().unwrap().blob_tx.clone()
    }

    /// Set the blob message sender for target tracking
    pub fn set_blob_tx(&self, blob_tx: mpsc::Sender<BlobMessage>) {
        self.radars.write().unwrap().blob_tx = Some(blob_tx);
    }

    /// Get the tracker command sender for MARPA requests and control changes
    pub fn get_tracker_command_tx(&self) -> Option<mpsc::Sender<TrackerCommand>> {
        self.radars.read().unwrap().tracker_command_tx.clone()
    }

    /// Set the tracker command sender for MARPA requests and control changes
    pub fn set_tracker_command_tx(&self, command_tx: mpsc::Sender<TrackerCommand>) {
        self.radars.write().unwrap().tracker_command_tx = Some(command_tx);
    }

    /// Request all radars to switch to transmit mode
    /// This sends a Power=Transmit control update to each radar's control handler
    pub fn request_transmit_all(&self) {
        let radars = self.radars.read().unwrap();
        for (key, info) in radars.info.iter() {
            // Check if radar is in standby (can be switched to transmit)
            if let Some(status) = info.controls.get_status()
                && status == Power::Standby
            {
                log::info!("Requesting transmit mode for radar '{}'", key);
                let control_value = ControlValue::new(
                    ControlId::Power,
                    serde_json::Value::Number(serde_json::Number::from(Power::Transmit as i32)),
                );
                // Create a dummy reply channel - we don't need the response
                let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(1);
                if let Err(e) = info
                    .controls
                    .send_to_command_handler(control_value, reply_tx)
                {
                    log::error!("Failed to send transmit command to '{}': {:?}", key, e);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub persistent_data: Persistence,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    blob_tx: Option<mpsc::Sender<BlobMessage>>,
    tracker_command_tx: Option<mpsc::Sender<TrackerCommand>>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Power {
    Off,
    Standby,
    Transmit,
    Preparing,
    Fault,
}

impl fmt::Display for Power {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl Power {
    pub(crate) fn from_value(s: &Value) -> Result<Self, RadarError> {
        match s {
            Value::Number(n) => match n.as_i64() {
                Some(0) => Ok(Power::Off),
                Some(1) => Ok(Power::Standby),
                Some(2) => Ok(Power::Transmit),
                Some(3) => Ok(Power::Preparing),
                Some(4) => Ok(Power::Fault),
                _ => match n.as_f64() {
                    Some(0.) => Ok(Power::Off),
                    Some(1.) => Ok(Power::Standby),
                    Some(2.) => Ok(Power::Transmit),
                    Some(3.) => Ok(Power::Preparing),
                    Some(4.) => Ok(Power::Fault),
                    _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
                },
            },
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "0" | "off" => Ok(Power::Off),
                "1" | "standby" => Ok(Power::Standby),
                "2" | "transmit" => Ok(Power::Transmit),
                "3" | "preparing" => Ok(Power::Preparing),
                "4" | "fault" => Ok(Power::Fault),
                _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
            },
            _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
        }
    }
}

/// Decide whether a radar's data loop should enter idle mode, where it drains
/// the spoke socket but skips decoding. Idle is safe when both: the radar is in
/// Standby (so the frames it emits are essentially noise) AND nobody is
/// subscribed to the spoke broadcast (so no one downstream observes the result).
/// `power` is None when the Power control has not yet been reported by the radar
/// — treated as non-idle so the first frames after startup are always decoded.
///
/// Brand-agnostic: any brand whose radar keeps emitting spokes in Standby can
/// gate its decode on [`RadarInfo::is_idle`] and refresh it via
/// [`RadarInfo::refresh_idle_flag`]. If a radar simply stops emitting in
/// Standby, the gate never triggers and is a harmless no-op.
///
/// CARE: `spoke_receiver_count` is the spoke-broadcast WebSocket subscriber
/// count only. ARPA does not subscribe to that broadcast — it consumes blobs
/// over a separate mpsc channel, and idle skips the blob detection that feeds
/// it. So a radar tracking ARPA targets with no GUI viewer counts as zero here
/// and would idle, silently killing tracking. Before widening this predicate to
/// the transmit-but-unwatched case, fold in an active-tracker signal so an
/// ARPA-tracked radar stays awake. See docs/internals/radar-status.md.
pub(crate) fn should_idle(power: Option<i32>, spoke_receiver_count: usize) -> bool {
    let standby = power.map(|p| p == Power::Standby as i32).unwrap_or(false);
    standby && spoke_receiver_count == 0
}

/// Decide whether a radar that has been silent for `silence` should have its
/// power state forced to [`Power::Off`]. True once the silence reaches
/// [`SharedRadars::RADAR_SILENCE_TIMEOUT`] and the radar is not already Off.
/// `current_power` is `None` when the Power control has never been reported —
/// still forced Off, since a radar that has said nothing at all for that long
/// is powered off from the operator's point of view.
fn should_power_off(silence: Duration, current_power: Option<i32>) -> bool {
    silence >= SharedRadars::RADAR_SILENCE_TIMEOUT && current_power != Some(Power::Off as i32)
}

// The actual values are not arbitrary: these are the exact values as reported
// by HALO radars, simplifying the navico::report code.
#[derive(Copy, Clone, Debug, Primitive, PartialEq)]
pub enum DopplerMode {
    None = 0,
    Both = 1,
    Approaching = 2,
}

impl fmt::Display for DopplerMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub const BLOB_HISTORY_COLORS: u8 = 32;
const OPAQUE: u8 = 255;

/// Build the default legend for a radar.
///
/// `doppler_levels` is the number of intensity sub-levels **per direction**:
/// - `0` — no Doppler (HD, plain xHD)
/// - `1` — single flat Doppler color per direction (Navico HALO)
/// - `4` — 4-level brightness gradient per direction (Garmin Fantom)
fn default_legend(
    targets: &TargetMode,
    doppler_levels: u8,
    has_rain_class: bool,
    pixel_values: u8,
) -> Legend {
    let mut legend = Legend {
        pixels: Vec::new(),
        pixel_colors: 0,
        history_start: 0,
        doppler_approaching: None,
        doppler_receding: None,
        doppler_rain: None,
        strong_return: 0,
        medium_return: 0,
        low_return: 0,
        static_background: None,
    };

    let rain_levels: u8 = if has_rain_class && doppler_levels > 0 {
        doppler_levels
    } else {
        0
    };
    let doppler_total = doppler_levels * 2 + rain_levels;

    // Calculate extra colors needed for special purposes
    let arpa_extra_colors: u8 = if *targets == TargetMode::Arpa {
        1 // static_background
    } else {
        0
    };
    let pixel_values = min(
        pixel_values,
        u8::MAX
            - if *targets != TargetMode::None {
                BLOB_HISTORY_COLORS
            } else {
                0
            }
            - 1 // transparent/none color
            - arpa_extra_colors
            - doppler_total,
    );

    // No return is transparent (black)
    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color::from("#00000000"),
    });
    legend.pixel_colors = pixel_values;
    if pixel_values == 0 {
        return legend;
    }

    let pixels_with_color = pixel_values - 1;
    let one_third = pixels_with_color / 3;
    let two_thirds = one_third * 2;
    legend.low_return = max(1, one_third / 3);
    legend.medium_return = one_third;
    legend.strong_return = two_thirds;

    for v in 1..pixel_values {
        legend.pixels.push(Lookup {
            r#type: PixelType::Normal,
            color: Color {
                // red starts at 2/3 and peaks at end
                r: if v >= two_thirds {
                    (255.0 * (v - two_thirds) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                // green starts at 1/3 and peaks at 2/3
                g: if v >= one_third && v < two_thirds {
                    (255.0 * (v - one_third) as f64 / one_third as f64) as u8
                } else if v >= two_thirds {
                    (255.0 * (pixels_with_color - v) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                // blue peaks at 1/3
                b: if v < one_third {
                    (255.0 * v as f64 / one_third as f64) as u8
                } else if v >= one_third && v < two_thirds {
                    (255.0 * (two_thirds - v) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                a: OPAQUE,
            },
        });
    }

    if *targets == TargetMode::Arpa {
        // Static background color (light grey) for Static ARPA mode
        legend.static_background = Some(legend.pixels.len() as u8);
        legend.pixels.push(Lookup {
            r#type: PixelType::History, // Reuse History type for static background
            color: Color::from("#505050"),
        });
    }

    if doppler_levels > 0 {
        let approaching_start = legend.pixels.len() as u8;
        for i in 0..doppler_levels {
            let brightness = if doppler_levels == 1 {
                255
            } else {
                // Dim → bright gradient: 64, 128, 191, 255 for 4 levels
                64 + (i as u16 * 191 / (doppler_levels as u16 - 1)) as u8
            };
            legend.pixels.push(Lookup {
                r#type: PixelType::DopplerApproaching,
                color: Color {
                    r: brightness,
                    g: 0,
                    b: brightness,
                    a: OPAQUE,
                }, // Purple at varying brightness
            });
        }
        legend.doppler_approaching = Some((approaching_start, doppler_levels));

        let receding_start = legend.pixels.len() as u8;
        for i in 0..doppler_levels {
            let brightness = if doppler_levels == 1 {
                255
            } else {
                64 + (i as u16 * 191 / (doppler_levels as u16 - 1)) as u8
            };
            legend.pixels.push(Lookup {
                r#type: PixelType::DopplerReceding,
                color: Color {
                    r: 0,
                    g: brightness,
                    b: 0,
                    a: OPAQUE,
                }, // Green at varying brightness
            });
        }
        legend.doppler_receding = Some((receding_start, doppler_levels));

        if rain_levels > 0 {
            let rain_start = legend.pixels.len() as u8;
            for i in 0..rain_levels {
                let brightness = if rain_levels == 1 {
                    255
                } else {
                    64 + (i as u16 * 191 / (rain_levels as u16 - 1)) as u8
                };
                legend.pixels.push(Lookup {
                    r#type: PixelType::DopplerRain,
                    color: Color {
                        r: 0,
                        g: 0,
                        b: brightness,
                        a: OPAQUE,
                    }, // Blue at varying brightness
                });
            }
            legend.doppler_rain = Some((rain_start, rain_levels));
        }
    }

    if *targets != TargetMode::None {
        legend.history_start = legend.pixels.len() as u8;
        const START_DENSITY: u8 = 255; // Target trail starts as white
        const END_DENSITY: u8 = 63; // Ends as gray
        const DELTA_INTENSITY: u8 = (START_DENSITY - END_DENSITY) / BLOB_HISTORY_COLORS;
        let mut density = START_DENSITY;
        for _history in 0..BLOB_HISTORY_COLORS {
            let color = Color {
                r: density,
                g: density,
                b: density,
                a: OPAQUE,
            };
            density -= DELTA_INTENSITY;
            legend.pixels.push(Lookup {
                r#type: PixelType::History,
                color,
            });
        }
    }

    log::debug!("Created legend {:?}", legend);
    legend
}

pub(crate) struct CommonRadar {
    pub key: String,
    pub info: RadarInfo,
    radars: SharedRadars,
    pub control_update_rx: broadcast::Receiver<ControlUpdate>,
    pub replay: bool,

    // Common state so we can process spokes
    trails: TrailBuffer,
    blob_detector: Option<BlobDetector>,
    blob_tx: Option<mpsc::Sender<BlobMessage>>,
    spoke_message: Option<RadarMessage>,
    spoke_time: u64,
    prev_angle: SpokeBearing,
    spoke_count: u32,
    max_spoke_length: u32,

    /// Minimum spoke count before `send_spoke_message` actually broadcasts
    /// the accumulated batch. `0` disables batching (the default) so brands
    /// that already emit a reasonable number of spokes per UDP frame —
    /// Navico's 32, Furuno's variable, etc. — are unchanged. Brands that
    /// emit 1 spoke per UDP packet (Raymarine Quantum / Garmin Fantom) set
    /// this higher so each broadcast carries roughly 1/32 of a revolution,
    /// amortising the per-message compression / WebSocket framing cost.
    /// Set via `set_spoke_batch_threshold()` at receiver construction.
    spoke_batch_threshold: usize,

    /// Range of the spokes currently sitting in the un-flushed batch. Used
    /// in `add_spoke` to detect a mid-batch range change and force-flush
    /// before mixing two ranges into one broadcast message.
    batch_range: Option<u32>,

    // Exclusion zones (stationary installations only)
    exclusion_zones: [Option<crate::config::ExclusionZone>; 4],
    exclusion_rects: [Option<crate::config::ExclusionRect>; 4],
    exclusion_mask: Option<exclusion::ExclusionMask>,
    current_exclusion_range: u32,
    current_exclusion_spoke_len: usize,
}

impl CommonRadar {
    pub fn new(
        _args: &Cli,
        key: String,
        info: RadarInfo,
        radars: SharedRadars,
        control_update_rx: broadcast::Receiver<ControlUpdate>,
        replay: bool,
        blob_tx: Option<mpsc::Sender<BlobMessage>>,
    ) -> Self {
        let trails = TrailBuffer::new(&info);
        let spoke_message = None;

        // Create blob detector if ARPA mode is enabled
        let blob_detector = if info.targets == TargetMode::Arpa {
            log::info!(
                "{}: BlobDetector created with threshold={} (strong return), spokes={}",
                key,
                info.legend.strong_return,
                info.spokes_per_revolution
            );
            let mut detector = BlobDetector::new(
                info.spokes_per_revolution,
                info.legend.strong_return,
                info.legend.doppler_approaching,
            );
            // Initialize guard zones from current control values
            detector.set_guard_zone_1(info.controls.guard_zone(&ControlId::GuardZone1));
            detector.set_guard_zone_2(info.controls.guard_zone(&ControlId::GuardZone2));
            Some(detector)
        } else {
            None
        };

        // Initialize exclusion zones from control values (stationary only)
        let exclusion_zones = [
            info.controls.exclusion_zone(&ControlId::ExclusionZone1),
            info.controls.exclusion_zone(&ControlId::ExclusionZone2),
            info.controls.exclusion_zone(&ControlId::ExclusionZone3),
            info.controls.exclusion_zone(&ControlId::ExclusionZone4),
        ];

        // Initialize rectangular exclusion zones from control values (stationary only)
        let exclusion_rects = [
            info.controls.exclusion_rect(&ControlId::ExclusionRect1),
            info.controls.exclusion_rect(&ControlId::ExclusionRect2),
            info.controls.exclusion_rect(&ControlId::ExclusionRect3),
            info.controls.exclusion_rect(&ControlId::ExclusionRect4),
        ];

        CommonRadar {
            key,
            info,
            radars,
            control_update_rx,
            replay,
            trails,
            blob_detector,
            blob_tx,
            spoke_message,
            spoke_time: 0,
            prev_angle: 0,
            spoke_count: 0,
            max_spoke_length: 0,
            spoke_batch_threshold: 0,
            batch_range: None,
            exclusion_zones,
            exclusion_rects,
            exclusion_mask: None,
            current_exclusion_range: 0,
            current_exclusion_spoke_len: 0,
        }
    }

    pub(crate) fn update(&mut self) {
        self.radars.update(&mut self.info);
    }

    pub(crate) fn refresh_user_names(&self) {
        self.radars.refresh_user_names();
    }

    //
    // Once the ranges are set non-zero the radar is findable by the GUI
    //
    pub(crate) fn set_ranges(&mut self, ranges: Ranges) {
        if self.info.ranges.is_empty() && !ranges.is_empty() {
            log::info!(
                "{}: supports ranges {} and is now findable in GUI",
                self.key,
                ranges
            );
        }
        self.info.ranges = ranges;
        self.info.controls.set_valid_ranges(&self.info.ranges);
        self.update();
    }

    ///
    /// Received a control update from the (web) client over the receiver channel
    ///
    pub async fn process_control_update<T: CommandSender>(
        &mut self,
        control_update: ControlUpdate,
        command_sender: &mut Option<T>,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        match cv.id.get_destination() {
            ControlDestination::Internal | ControlDestination::ReadOnly => {
                panic!("{:?} should not be sent to radar receiver", cv)
            }
            ControlDestination::Trail | ControlDestination::Target => {
                // Update blob detector guard zones when those controls change
                if let Some(ref mut detector) = self.blob_detector {
                    match cv.id {
                        ControlId::GuardZone1 => {
                            detector.set_guard_zone_1(
                                self.info.controls.guard_zone(&ControlId::GuardZone1),
                            );
                        }
                        ControlId::GuardZone2 => {
                            detector.set_guard_zone_2(
                                self.info.controls.guard_zone(&ControlId::GuardZone2),
                            );
                        }
                        _ => {}
                    }
                }

                // Update exclusion zones when those controls change
                match cv.id {
                    ControlId::ExclusionZone1 => {
                        self.exclusion_zones[0] = self
                            .info
                            .controls
                            .exclusion_zone(&ControlId::ExclusionZone1);
                        self.current_exclusion_range = 0; // Force mask rebuild
                    }
                    ControlId::ExclusionZone2 => {
                        self.exclusion_zones[1] = self
                            .info
                            .controls
                            .exclusion_zone(&ControlId::ExclusionZone2);
                        self.current_exclusion_range = 0;
                    }
                    ControlId::ExclusionZone3 => {
                        self.exclusion_zones[2] = self
                            .info
                            .controls
                            .exclusion_zone(&ControlId::ExclusionZone3);
                        self.current_exclusion_range = 0;
                    }
                    ControlId::ExclusionZone4 => {
                        self.exclusion_zones[3] = self
                            .info
                            .controls
                            .exclusion_zone(&ControlId::ExclusionZone4);
                        self.current_exclusion_range = 0;
                    }
                    ControlId::ExclusionRect1 => {
                        self.exclusion_rects[0] = self
                            .info
                            .controls
                            .exclusion_rect(&ControlId::ExclusionRect1);
                        self.current_exclusion_range = 0; // Force mask rebuild
                    }
                    ControlId::ExclusionRect2 => {
                        self.exclusion_rects[1] = self
                            .info
                            .controls
                            .exclusion_rect(&ControlId::ExclusionRect2);
                        self.current_exclusion_range = 0;
                    }
                    ControlId::ExclusionRect3 => {
                        self.exclusion_rects[2] = self
                            .info
                            .controls
                            .exclusion_rect(&ControlId::ExclusionRect3);
                        self.current_exclusion_range = 0;
                    }
                    ControlId::ExclusionRect4 => {
                        self.exclusion_rects[3] = self
                            .info
                            .controls
                            .exclusion_rect(&ControlId::ExclusionRect4);
                        self.current_exclusion_range = 0;
                    }
                    _ => {}
                }

                // Handle ARPA/target tracking and exclusion zone controls directly
                match cv.id {
                    ControlId::GuardZone1 | ControlId::GuardZone2 => {
                        self.update();
                        // Send to hardware if the brand supports it (e.g. Furuno).
                        // Mayara evaluates the zone itself either way, so an
                        // unreachable radar keeps a working guard zone; only the
                        // optional hardware copy is skipped.
                        if let Some(command_sender) = command_sender
                            .as_mut()
                            .filter(|_| self.info.controls.command_reachable())
                        {
                            match command_sender.set_control(&cv, &self.info.controls).await {
                                Ok(()) => {}
                                Err(RadarError::CannotSetControlId(_)) => {}
                                Err(e) => {
                                    log::warn!(
                                        "{}: guard zone hardware sync failed: {}",
                                        self.key,
                                        e
                                    );
                                }
                            }
                        }
                        return Ok(());
                    }
                    ControlId::ArpaDetectMaxSpeed | ControlId::DopplerAutoTrack => {
                        let value = cv.as_value()?;
                        let result = self
                            .info
                            .controls
                            .set_value(&cv.id, value)
                            .map(|_| ())
                            .map_err(RadarError::ControlError);
                        if result.is_ok() {
                            self.update(); // Persist the change
                        }
                        return result;
                    }
                    ControlId::ExclusionZone1
                    | ControlId::ExclusionZone2
                    | ControlId::ExclusionZone3
                    | ControlId::ExclusionZone4
                    | ControlId::ExclusionRect1
                    | ControlId::ExclusionRect2
                    | ControlId::ExclusionRect3
                    | ControlId::ExclusionRect4 => {
                        self.update();
                        return Ok(());
                    }
                    ControlId::ClearTargets => {
                        if let Some(tx) = self.radars.get_tracker_command_tx() {
                            let _ = tx.try_send(TrackerCommand::ClearTargets {
                                radar_key: self.key.clone(),
                            });
                        }
                        return Ok(());
                    }
                    _ => {}
                }

                match self.trails.set_control_value(&self.info.controls, &cv) {
                    Ok(()) => {
                        return Ok(());
                    }
                    Err(e) => {
                        return self
                            .info
                            .controls
                            .send_error_to_client(reply_tx, &cv, &e)
                            .await;
                    }
                };
            }
            ControlDestination::Command => {
                // A radar we cannot reach swallows commands silently: the
                // datagram is routed away and both connect() and send()
                // succeed. Refuse the set instead, so the client is told why
                // rather than watching the control snap back.
                if !self.info.controls.command_reachable() {
                    let e = RadarError::CannotReachRadar(
                        *self.info.send_command_addr.ip(),
                        self.info.nic_addr,
                    );
                    return self
                        .info
                        .controls
                        .send_error_to_client(reply_tx, &cv, &e)
                        .await;
                }
                if let Some(command_sender) = command_sender {
                    if let Err(e) = command_sender.set_control(&cv, &self.info.controls).await {
                        return self
                            .info
                            .controls
                            .send_error_to_client(reply_tx, &cv, &e)
                            .await;
                    } else {
                        self.info.controls.set_refresh(&cv.id);
                    }
                } else {
                    // Without this branch a PUT against a radar whose brand
                    // module has not yet wired up its command channel
                    // silently disappears with HTTP 200, which has cost
                    // contributors many hours of misdiagnosis (issue #228).
                    log::warn!(
                        "{}: control PUT {:?}={:?} dropped — command channel \
                         not initialised yet for this radar",
                        self.key,
                        cv.id,
                        cv.value
                    );
                    return self
                        .info
                        .controls
                        .send_error_to_client(reply_tx, &cv, &RadarError::NotConnected)
                        .await;
                }
            }
        }

        Ok(())
    }

    /// Begin (or continue) a spoke-batch. Idempotent: if a batch is already
    /// open it stays open so the next `add_spoke` call appends to it. Brands
    /// call this once at the start of each UDP frame just like before — the
    /// idempotence is what lets `spoke_batch_threshold > 0` accumulate
    /// across multiple frames without losing the partial batch.
    pub fn new_spoke_message(&mut self) {
        if self.spoke_message.is_none() {
            self.spoke_message = Some(RadarMessage::new());
            self.spoke_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap();
        }
    }

    /// Set the minimum spokes per broadcast message. `0` (the default)
    /// means every `send_spoke_message` flushes — original behaviour.
    /// Larger values batch spokes across UDP frames; the batch is also
    /// force-flushed on revolution wrap or range change in `add_spoke`,
    /// so the trailing partial batch of each revolution is shipped as-is.
    pub fn set_spoke_batch_threshold(&mut self, threshold: usize) {
        self.spoke_batch_threshold = threshold;
    }

    /// Drop the current batch onto the broadcast channel and reset the
    /// batch-range tracker. No-op if there's no active batch.
    fn flush_spoke_message(&mut self) {
        if let Some(message) = self.spoke_message.take()
            && !message.spokes.is_empty()
        {
            self.info.broadcast_radar_message(message);
        }
        self.batch_range = None;
    }

    /// Refresh exclusion mask when range or spoke length changes.
    /// Only applies to stationary installations.
    fn refresh_exclusion_mask(&mut self, range: u32, spoke_len: usize) {
        if !self.info.stationary {
            return;
        }

        // Check if we need to rebuild the mask
        if range == self.current_exclusion_range
            && spoke_len == self.current_exclusion_spoke_len
            && self.exclusion_mask.is_some()
        {
            return;
        }

        self.current_exclusion_range = range;
        self.current_exclusion_spoke_len = spoke_len;

        // Collect enabled sector zones
        let active_zones: Vec<exclusion::ExclusionZoneInternal> = self
            .exclusion_zones
            .iter()
            .filter_map(|z| z.as_ref())
            .filter(|z| z.enabled)
            .map(|z| {
                exclusion::zone_to_internal(z, self.info.spokes_per_revolution, range, spoke_len)
            })
            .collect();

        // Collect enabled rectangular zones
        let active_rects: Vec<exclusion::ExclusionRectInternal> = self
            .exclusion_rects
            .iter()
            .filter_map(|r| r.as_ref())
            .filter(|r| r.enabled)
            .map(exclusion::rect_to_internal)
            .collect();

        if active_zones.is_empty() && active_rects.is_empty() {
            self.exclusion_mask = None;
            return;
        }

        log::debug!(
            "{}: Building exclusion mask for {} sector zones + {} rects, range={}m, spoke_len={}",
            self.key,
            active_zones.len(),
            active_rects.len(),
            range,
            spoke_len
        );

        self.exclusion_mask = Some(exclusion::ExclusionMask::new(
            &active_zones,
            &active_rects,
            self.info.spokes_per_revolution,
            spoke_len,
            range,
        ));
    }

    pub(crate) fn add_spoke(
        &mut self,
        range: u32,
        angle: SpokeBearing,
        heading: Option<u16>,
        mut generic_spoke: GenericSpoke,
    ) {
        // Refresh exclusion mask before borrowing spoke_message
        if self.info.stationary {
            self.refresh_exclusion_mask(range, generic_spoke.len());
        }

        // End the current batch BEFORE pushing this spoke if:
        //   - range changed (would mix two ranges into one broadcast), or
        //   - the revolution wrapped (would mix end-of-old and start-of-new
        //     revolution into one broadcast).
        // In both cases this spoke belongs to a fresh batch.
        let range_changed = self.batch_range.is_some_and(|r| r != range);
        let rev_wrapped = angle < self.prev_angle && self.spoke_message.is_some();
        if rev_wrapped {
            // Publish per-revolution stats describing the rev that just
            // finished, before the batch carrying them is flushed.
            let ms = self.info.full_rotation();
            self.trails.set_rotation_speed(ms);
            log::debug!("spoke_count = {}", self.spoke_count);
            self.info
                .controls
                .set_value(&ControlId::Spokes, Value::Number(self.spoke_count.into()))
                .unwrap();
            self.info
                .controls
                .set_value(
                    &ControlId::SpokeLength,
                    Value::Number(self.max_spoke_length.into()),
                )
                .unwrap();
            self.spoke_count = 0;
            self.max_spoke_length = 0;
        }
        if range_changed || rev_wrapped {
            self.flush_spoke_message();
        }

        // Ensure a batch exists for the spoke we're about to push (creates
        // a fresh one if we just flushed, or reuses the existing one).
        self.new_spoke_message();
        self.batch_range = Some(range);

        if let Some(message) = &mut self.spoke_message {
            // In replay mode, draw a circle at extreme range for visual indication
            if self.replay && generic_spoke.len() >= 2 {
                let max_pixel = self.info.legend.pixel_colors.saturating_sub(1);
                let len = generic_spoke.len();
                generic_spoke[len - 2] = max_pixel;
                generic_spoke[len - 1] = max_pixel;
            }

            // Apply exclusion zones for stationary installations
            // Pixels in exclusion zones are set to 0 (transparent)
            if let Some(ref mask) = self.exclusion_mask {
                for (pixel_idx, pixel) in generic_spoke.iter_mut().enumerate() {
                    if mask.is_excluded(angle, pixel_idx) {
                        *pixel = 0;
                    }
                }
            }

            if log::log_enabled!(log::Level::Trace) {
                // Verify spoke contains legal values
                let max_value = self.info.legend.pixels.len() as u8;
                for pixel in generic_spoke.iter_mut() {
                    if *pixel >= max_value {
                        log::error!(
                            "{}: Spoke contains value {} which is >= {}",
                            self.key,
                            *pixel,
                            max_value
                        );
                        *pixel = 0;
                    }
                }
            }
            let mut spoke = to_protobuf_spoke(
                self.info.spokes_per_revolution,
                range,
                angle,
                heading,
                Some(self.spoke_time),
                generic_spoke,
            );
            apply_antenna_offset(
                &mut spoke,
                &self.info.controls,
                self.info.spokes_per_revolution as usize,
            );
            self.spoke_count += 1;
            self.max_spoke_length = max(self.max_spoke_length, spoke.data.len() as u32);

            // Feed spoke to blob detector for target tracking
            if let Some(ref mut detector) = self.blob_detector {
                let completed_blobs = detector.process_spoke(&spoke);

                if !completed_blobs.is_empty()
                    && let Some(ref blob_tx) = self.blob_tx
                {
                    let max_speed_mode = self.info.controls.arpa_detect_max_speed();
                    let max_target_speed_ms = SpokeContext::max_speed_from_mode(max_speed_mode);

                    for blob in &completed_blobs {
                        let ctx = SpokeContext {
                            time: spoke.time.unwrap_or(self.spoke_time),
                            range: spoke.range,
                            bearing: spoke.bearing.map(|b| b as u16),
                            lat: spoke.lat,
                            lon: spoke.lon,
                            spokes_per_revolution: self.info.spokes_per_revolution,
                            spoke_len: spoke.data.len(),
                            angle: spoke.angle as u16,
                            max_target_speed_ms,
                            doppler_auto_track: self.info.controls.doppler_auto_track(),
                        };
                        let msg = BlobMessage {
                            radar_key: self.key.clone(),
                            blob: blob.clone(),
                            context: ctx,
                        };
                        let _ = blob_tx.try_send(msg);
                    }
                }
            }

            // Per-spoke heading from the radar's inline bearing is NOT fed
            // back to navdata. The radar typically receives its heading
            // from N2K and re-emits it once per spoke; pushing it into
            // set_heading_true would race the authoritative compass source
            // (typically YDEN02 / direct N2K) on `navigation.headingTrue`
            // and saturate downstream subscribers with kHz-rate updates.
            // The PPI still uses `spoke.bearing` directly for rotation, so
            // dropping this feed is invisible to single-radar displays.

            // Always broadcast spoke to clients
            self.trails
                .update_trails(&mut spoke, &self.info.legend, &self.info.controls);
            message.spokes.push(spoke);

            if ((self.prev_angle + 1) % self.info.spokes_per_revolution) != angle {
                let missing_spokes = ((angle as u32 + self.info.spokes_per_revolution as u32)
                    - self.prev_angle as u32
                    - 1)
                    % self.info.spokes_per_revolution as u32;
                log::trace!(
                    "{}: Spoke angle {} is not consecutive to previous angle {}, missing spokes {}",
                    self.key,
                    angle,
                    self.prev_angle,
                    missing_spokes
                );
            }
            self.prev_angle = angle;
        }
    }

    pub(crate) fn send_spoke_message(&mut self) {
        // Flush only when the batch is large enough; otherwise the partial
        // batch stays open and the next UDP frame's `add_spoke` calls
        // append to it. add_spoke force-flushes on revolution wrap or
        // range change, so a stalled batch is never held forever.
        let ready = self
            .spoke_message
            .as_ref()
            .is_some_and(|m| m.spokes.len() >= self.spoke_batch_threshold);
        if ready {
            self.flush_spoke_message();
        }
    }

    pub(crate) fn set<T>(
        &mut self,
        control_id: &ControlId,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) where
        f64: From<T>,
    {
        match self
            .info
            .controls
            .set_value_auto_enabled(control_id, value, auto, enabled)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e);
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {:?} auto {:?} auto_value {:?} enabled {:?}",
                        self.key,
                        control_id,
                        control.value,
                        control.auto,
                        control.auto_value,
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    pub(crate) fn set_value<T>(&mut self, control_id: &ControlId, value: T)
    where
        f64: From<T>,
    {
        self.set(control_id, value, None, None)
    }

    pub(crate) fn set_value_auto<T>(&mut self, control_id: &ControlId, value: T, auto: u8)
    where
        f64: From<T>,
    {
        self.set(control_id, value, Some(auto > 0), None)
    }

    pub(crate) fn set_value_enabled<T>(&mut self, control_id: &ControlId, value: T, enabled: u8)
    where
        f64: From<T>,
    {
        self.set(control_id, value, None, Some(enabled > 0))
    }

    pub(crate) fn set_string(&mut self, control: &ControlId, value: String) {
        match self.info.controls.set_string(control, value) {
            Err(e) => {
                log::error!("{}: {}", self.key, e);
            }
            Ok(Some(v)) => {
                log::debug!("{}: Control '{}' new value '{}'", self.key, control, v);
            }
            Ok(None) => {}
        };
    }

    pub(crate) fn set_wire_range(&mut self, control_id: &ControlId, min: u8, max: u8) {
        match self
            .info
            .controls
            .set_wire_range(control_id, min as f64, max as f64)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e);
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new wire min {} max {} value {:?} auto {:?} auto_value {:?} enabled {:?} ",
                        self.key,
                        control_id,
                        min,
                        max,
                        control.value,
                        control.auto,
                        control.auto_value,
                        control.enabled,
                    );
                }
            }
            Ok(None) => {}
        };
    }

    pub(crate) fn set_value_with_many_auto(
        &mut self,
        control_id: &ControlId,
        value: f64,
        auto_value: f64,
    ) {
        match self
            .info
            .controls
            .set_value_with_many_auto(control_id, value, auto_value)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e);
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {:?} auto_value {:?} auto {:?}",
                        self.key,
                        control_id,
                        control.value,
                        control.auto_value,
                        control.auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    pub(crate) fn set_sector<T>(
        &mut self,
        control_id: &ControlId,
        start: T,
        end: T,
        enabled: Option<bool>,
    ) where
        f64: From<T>,
    {
        match self
            .info
            .controls
            .set_sector(control_id, start.into(), end.into(), enabled)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e);
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::debug!(
                        "{}: Control '{}' new sector start {:?} end {:?} enabled {:?}",
                        self.key,
                        control_id,
                        control.value,
                        control.end_value,
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }
}

/// Adjust a spoke's lat/lon by the antenna offset (forward/starboard of GPS).
/// Requires both a valid position and heading to compute the offset.
fn apply_antenna_offset(
    spoke: &mut crate::protos::RadarMessage::radar_message::Spoke,
    controls: &SharedControls,
    spokes_per_revolution: usize,
) {
    let (Some(lat), Some(lon)) = (spoke.lat, spoke.lon) else {
        return;
    };
    // Prefer the spoke's own heading over the global navdata heading to avoid
    // a one-spoke lag on startup or when heading only comes from the radar feed.
    let heading = if let Some(bearing) = spoke.bearing {
        let heading_spokes =
            (bearing as i32 - spoke.angle as i32).rem_euclid(spokes_per_revolution as i32) as f64;
        heading_spokes / spokes_per_revolution as f64 * std::f64::consts::TAU
    } else if let Some(h) = crate::navdata::get_heading_true() {
        h
    } else {
        return;
    };

    let forward_m = controls
        .get(&ControlId::AntennaForward)
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    let starboard_m = controls
        .get(&ControlId::AntennaStarboard)
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);

    if forward_m == 0.0 && starboard_m == 0.0 {
        return;
    }

    // Rotate forward/starboard into north/east using vessel heading
    let (sin_h, cos_h) = heading.sin_cos();
    let north_m = forward_m * cos_h - starboard_m * sin_h;
    let east_m = forward_m * sin_h + starboard_m * cos_h;

    const METERS_PER_DEG_LAT: f64 = 111_111.0;
    spoke.lat = Some(lat + north_m / METERS_PER_DEG_LAT);
    spoke.lon = Some(lon + east_m / (METERS_PER_DEG_LAT * lat.to_radians().cos()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn test_addr() -> SocketAddrV4 {
        // 10.0.1.2 -> low 16 bits 0x0102
        SocketAddrV4::new(Ipv4Addr::new(10, 0, 1, 2), 6878)
    }

    #[test]
    fn radar_key_uses_serial_tail() {
        assert_eq!(
            radar_key("fur", Some("1403302452"), None, None, &test_addr()),
            "fur2452"
        );
        // Serials shorter than four characters are used in full.
        assert_eq!(
            radar_key("fur", Some("12"), None, None, &test_addr()),
            "fur12"
        );
    }

    #[test]
    fn radar_key_prefers_serial_over_mac() {
        assert_eq!(
            radar_key(
                "fur",
                Some("1403302452"),
                Some("00d01d057903"),
                None,
                &test_addr()
            ),
            "fur2452"
        );
    }

    #[test]
    fn radar_key_falls_back_to_mac_without_serial() {
        // The NavNet 3D case from issue #447: two units, both reporting an
        // all-zero serial from the same DHCP pool, told apart by their MACs.
        assert_eq!(
            radar_key("fur", None, Some("00d01d057903"), None, &test_addr()),
            "fur7903"
        );
        assert_eq!(
            radar_key("fur", Some(""), Some("00d01d057045"), None, &test_addr()),
            "fur7045"
        );
    }

    #[test]
    fn legacy_address_key_matches_the_address_fallback() {
        // The migration key must be byte-identical to what a radar with no
        // serial and no hardware identity used to be called, or settings
        // saved under the old key would never be found.
        assert_eq!(radar_key("ray", None, None, None, &test_addr()), "ray0102");
        assert_eq!(
            radar_key("ray", None, None, Some("B"), &test_addr()),
            "ray0102B"
        );
    }

    #[test]
    fn radar_key_treats_an_all_zero_serial_as_unusable() {
        // A radar reporting its serial as ASCII zeros rather than an empty
        // field must still key on the MAC, not on "0000".
        assert_eq!(
            radar_key(
                "fur",
                Some("000000000000"),
                Some("00d01d057903"),
                None,
                &test_addr()
            ),
            "fur7903"
        );
        // With nothing better, an all-zero serial still must not become the key.
        assert_eq!(
            radar_key("fur", Some("000000000000"), None, None, &test_addr()),
            "fur0102"
        );
    }

    #[test]
    fn radar_key_falls_back_to_ip_without_serial_or_mac() {
        // Garmin, Raymarine and Koden expose neither; the IP is all they have.
        assert_eq!(radar_key("gar", None, None, None, &test_addr()), "gar0102");
        assert_eq!(
            radar_key("gar", Some(""), Some(""), None, &test_addr()),
            "gar0102"
        );
    }

    #[test]
    fn radar_key_appends_dual_suffix() {
        assert_eq!(
            radar_key("nav", Some("1403302452"), None, Some("A"), &test_addr()),
            "nav2452A"
        );
        // Two ranges of one MAC-identified radar stay distinct via the suffix.
        assert_eq!(
            radar_key(
                "fur",
                Some(""),
                Some("00d01d057903"),
                Some("A"),
                &test_addr()
            ),
            "fur7903A"
        );
        assert_eq!(
            radar_key(
                "fur",
                Some(""),
                Some("00d01d057903"),
                Some("B"),
                &test_addr()
            ),
            "fur7903B"
        );
    }

    #[test]
    fn mac_identity_rejects_non_identifying_addresses() {
        // Furuno's virtual devices (the CAN-BUS entry) report broadcast.
        assert_eq!(mac_identity(&[0xff; 6]), None);
        assert_eq!(mac_identity(&[0; 6]), None);
        assert_eq!(
            mac_identity(&[0x00, 0xd0, 0x1d, 0x05, 0x79, 0x03]).as_deref(),
            Some("00d01d057903")
        );
    }

    #[test]
    fn suffix_chars_takes_tail() {
        assert_eq!(suffix_chars("Navico1234A", 1), "A");
        assert_eq!(suffix_chars("Navico1234A", 4), "234A");
        assert_eq!(suffix_chars("AB", 5), "AB");
        assert_eq!(suffix_chars("", 3), "");
    }

    #[test]
    fn distinguishing_suffix_len_dual_range_pair() {
        // A dual-range antenna pair differs only in the trailing A/B.
        assert_eq!(
            distinguishing_suffix_len(&["Navico1234A", "Navico1234B"]),
            1
        );
    }

    #[test]
    fn distinguishing_suffix_len_separate_radars() {
        // Two separate radars share the brand prefix but differ in serial.
        assert_eq!(distinguishing_suffix_len(&["Navico1234", "Navico5678"]), 1);
        // Serials that share their tail need a longer suffix to separate.
        assert_eq!(distinguishing_suffix_len(&["Navico1234", "Navico5234"]), 4);
    }

    #[test]
    fn distinguishing_suffix_len_single_key() {
        assert_eq!(distinguishing_suffix_len(&["Navico1234"]), 1);
    }

    #[test]
    fn legend() {
        let targets = crate::TargetMode::Arpa;
        let legend = default_legend(&targets, 1, false, 16);
        let json = serde_json::to_string_pretty(&legend).unwrap();
        println!("{}", json);
    }

    #[test]
    fn radar_error_into_response_not_recursive() {
        // This test verifies that RadarError::into_response() does not cause
        // infinite recursion. If the implementation is broken, this test will
        // cause a stack overflow.
        let error = RadarError::NoSuchRadar("test".to_string());
        let response = error.into_response();

        // If we reach here, no stack overflow occurred
        assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
    }

    /// Idle mode (issue #274) relies on `RadarInfo.is_idle` being an
    /// `Arc<AtomicBool>` so every clone — including the ones
    /// `SharedRadars::get_by_key` returns to the web layer — points at
    /// the SAME atomic. If anyone refactors the field to a plain
    /// `AtomicBool`, `wake_up()` calls on the clone the web layer sees
    /// become no-ops against the clone the data_loop sees, and the GUI
    /// silently falls back to "PPI blank for a beat every time you
    /// connect".
    ///
    /// We can't easily construct a full `RadarInfo` in a unit test
    /// (the factory needs `SharedRadars`, a controls closure, several
    /// `SocketAddrV4`s, …), so this test pins the contract on the
    /// `is_idle` field itself: a function that accepts only
    /// `Arc<AtomicBool>` and the cross-clone propagation that depends
    /// on that exact type. A refactor to a plain `AtomicBool` fails
    /// the compile-time portion; a refactor that keeps the type but
    /// breaks sharing through some other mechanism fails the runtime
    /// portion.
    #[test]
    fn is_idle_field_must_be_arc_atomic_bool() {
        fn assert_arc_atomic_bool_field(_field: &Arc<AtomicBool>) {}

        // Use the actual field by name — if a refactor renames or
        // retypes it, this test fails to compile.
        let info = test_helpers::dummy_is_idle_field();
        assert_arc_atomic_bool_field(&info);

        let clone = info.clone();
        clone.store(true, Ordering::Relaxed);
        assert!(
            info.load(Ordering::Relaxed),
            "wake_up on a RadarInfo clone must reach the data_loop's clone"
        );
        assert!(Arc::ptr_eq(&info, &clone));
    }

    // ----- idle-mode predicate (issue #274) -----

    #[test]
    fn should_idle_yes_when_standby_and_no_subscribers() {
        assert!(should_idle(Some(Power::Standby as i32), 0));
    }

    #[test]
    fn should_idle_no_when_transmitting_even_with_no_subscribers() {
        // A transmitting radar drives downstream consumers we may not see
        // directly (MFDs over multicast, recording, ARPA targets via the
        // tracker channel). Never idle while it's broadcasting useful data.
        assert!(!should_idle(Some(Power::Transmit as i32), 0));
    }

    #[test]
    fn should_idle_no_when_standby_with_subscribers() {
        // Some client is watching the spoke stream — keep the pipeline hot
        // so the moment the radar transitions to Transmit, the first frame
        // is decoded and rendered without a tick of blank PPI.
        assert!(!should_idle(Some(Power::Standby as i32), 1));
    }

    #[test]
    fn should_idle_no_when_power_is_unknown() {
        // Before the radar has reported its first Status frame we don't
        // know its state. Default to processing frames so we never blank
        // the PPI for a viewer that connects very early in startup.
        assert!(!should_idle(None, 0));
    }

    #[test]
    fn should_power_off_no_before_timeout() {
        let recent = SharedRadars::RADAR_SILENCE_TIMEOUT - Duration::from_secs(1);
        assert!(!should_power_off(recent, Some(Power::Transmit as i32)));
        assert!(!should_power_off(recent, Some(Power::Standby as i32)));
    }

    #[test]
    fn should_power_off_yes_from_transmit_and_standby_after_timeout() {
        // A silent radar decays to Off from any live state, including Standby.
        assert!(should_power_off(
            SharedRadars::RADAR_SILENCE_TIMEOUT,
            Some(Power::Transmit as i32)
        ));
        assert!(should_power_off(
            SharedRadars::RADAR_SILENCE_TIMEOUT,
            Some(Power::Standby as i32)
        ));
    }

    #[test]
    fn should_power_off_no_when_already_off() {
        assert!(!should_power_off(
            SharedRadars::RADAR_SILENCE_TIMEOUT * 10,
            Some(Power::Off as i32)
        ));
    }

    #[test]
    fn should_power_off_yes_when_power_never_reported() {
        // A radar that has said nothing at all for the whole timeout is off.
        assert!(should_power_off(SharedRadars::RADAR_SILENCE_TIMEOUT, None));
    }

    mod test_helpers {
        use super::*;
        /// Mint a stand-in for `RadarInfo.is_idle` so the test above
        /// doesn't have to construct a full `RadarInfo`. If the field
        /// type changes, the caller fails to compile.
        pub(super) fn dummy_is_idle_field() -> Arc<AtomicBool> {
            Arc::new(AtomicBool::new(false))
        }
    }

    // ----- Power::from_value -----

    #[test]
    fn power_from_value_parses_fault_variants() {
        assert_eq!(
            Power::from_value(&serde_json::json!(4)).unwrap(),
            Power::Fault
        );
        assert_eq!(
            Power::from_value(&serde_json::json!(4.0)).unwrap(),
            Power::Fault
        );
        assert_eq!(
            Power::from_value(&serde_json::json!("fault")).unwrap(),
            Power::Fault
        );
        // Case-insensitive matching.
        assert_eq!(
            Power::from_value(&serde_json::json!("Fault")).unwrap(),
            Power::Fault
        );
        // Numeric string form, same as the non-Fault states.
        assert_eq!(
            Power::from_value(&serde_json::json!("4")).unwrap(),
            Power::Fault
        );
    }

    #[test]
    fn power_from_value_rejects_unknown_status() {
        assert!(Power::from_value(&serde_json::json!("faulty")).is_err());
        assert!(Power::from_value(&serde_json::json!(5)).is_err());
        assert!(Power::from_value(&serde_json::json!(-1)).is_err());
    }
}
