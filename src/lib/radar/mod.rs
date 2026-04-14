use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_primitive_derive::Primitive;
use protobuf::Message;
use serde::Serialize;
use serde::ser::Serializer;
use serde_json::Value;
use std::cmp::{max, min};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
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
use crate::config::Persistence;
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

pub const NAUTICAL_MILE: i32 = 1852; // 1 nautical mile in meters
pub const NAUTICAL_MILE_F64: f64 = 1852.; // 1 nautical mile in meters

// A "native to radar" bearing, usually [0..2048] or [0..4096] or [0..8192]
pub type SpokeBearing = u16;

pub const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("I/O operation failed")]
    Io(#[from] std::io::Error),
    #[error("Axum operation failed")]
    Axum(#[from] axum::Error),
    #[error("Interface '{0}' is not available")]
    InterfaceNotFound(String),
    #[error("Interface '{0}' has no valid IPv4 address")]
    InterfaceNoV4(String),
    #[error("Cannot detect Ethernet devices")]
    EnumerationFailed,
    #[error("Timeout")]
    Timeout,
    #[error("Shutdown")]
    Shutdown,
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

#[derive(Clone, Debug)]
pub struct RadarInfo {
    key: String,

    // selected items from Cli args:
    targets: TargetMode,
    replay: bool,
    output: bool,

    pub brand: Brand,
    pub serial_no: Option<String>,       // Serial # for this radar
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
    pub doppler: bool,      // Does it support Doppler?
    doppler_levels: u8,     // Intensity sub-levels per direction (0, 1, or 4)
    pub dual_range: bool,                               // Is it dual range capable?
    pub sparse_spokes: bool, // Does it produce fewer spokes than spokes_per_revolution?
    pub stationary: bool,    // Is radar stationary (shore-based)?
    rotation_timestamp: Instant,

    // Channels
    pub message_tx: tokio::sync::broadcast::Sender<Vec<u8>>, // Serialized RadarMessage
}

impl RadarInfo {
    pub fn new<F>(
        radars: &SharedRadars,
        args: &Cli,
        brand: Brand,
        serial_no: Option<&str>,
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

        let (targets, replay, output) = {
            (
                args.targets.clone(),
                args.is_replay(),
                args.output.clone(),
            )
        };
        let doppler_levels = if doppler { 1 } else { 0 };
        let legend = default_legend(&targets, doppler_levels, pixel_values);

        let mut key = brand.to_prefix().to_string();
        if let Some(serial_no) = serial_no {
            key.push_str(&serial_no[serial_no.len().saturating_sub(4)..]);
        } else {
            write!(key, "{:04x}", addr.ip().to_bits() & 0xffff).unwrap();
        }
        if let Some(dual) = dual {
            key.push_str(dual);
        }

        let sk_client_tx = radars.radars.read().unwrap().sk_client_tx.clone();
        let controls = controls_fn(key.clone(), sk_client_tx);

        let info = RadarInfo {
            targets,
            replay,
            output,
            key,
            brand,
            serial_no: serial_no.map(String::from),
            dual: dual.map(String::from),
            pixel_values,
            spokes_per_revolution: spokes_per_revolution as u16,
            max_spoke_len: max_spoke_len as u16,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            legend: legend,
            message_tx,
            ranges: Ranges::empty(),
            controls,
            doppler,
            doppler_levels,
            dual_range: false,
            sparse_spokes,
            stationary: args.stationary,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
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
            self.legend =
                default_legend(&self.targets, self.doppler_levels, self.pixel_values);
            log::debug!("Doppler changed to {}", doppler);
        }
    }

    /// Set the number of Doppler intensity sub-levels per direction.
    /// Garmin Fantom uses 4 (brightness gradient); Navico uses 1 (flat).
    pub fn set_doppler_levels(&mut self, levels: u8) {
        self.doppler = levels > 0;
        self.doppler_levels = levels;
        self.legend =
            default_legend(&self.targets, self.doppler_levels, self.pixel_values);
        log::debug!(
            "Doppler levels changed to {} (doppler={})",
            levels,
            self.doppler
        );
    }

    pub fn set_pixel_values(&mut self, pixel_values: u8) {
        if pixel_values != self.pixel_values {
            self.legend =
                default_legend(&self.targets, self.doppler_levels, pixel_values);
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
        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");

        // Send the message to all receivers, normally the web client(s)
        // We send raw bytes to avoid encoding overhead in each web client.
        // This strategy will change when clients want different protocols.
        match self.message_tx.send(bytes) {
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

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }
    }

    async fn forward_output(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
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
                            std::io::stdout().write_all(&r).unwrap_or_else(|_| { subsys.request_shutdown(); });
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

    pub(crate) fn pixel_colors(&self) -> u8 {
        self.legend.pixel_colors
    }
}

impl Display for RadarInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Radar {} brand {}", &self.key, &self.brand)?;
        if let Some(which) = &self.dual {
            write!(f, " {}", which)?;
        }
        if let Some(serial_no) = &self.serial_no {
            write!(f, " [{}]", serial_no)?;
        }
        write!(
            f,
            " at {} via {} data {} report {} send {}",
            &self.addr.ip(),
            &self.nic_addr,
            &self.spoke_data_addr,
            &self.report_addr,
            &self.send_command_addr
        )
    }
}

/// Capacity of the Signal K client broadcast channel. Sized to absorb a
/// full revolution's worth of target updates plus control value changes
/// without dropping messages, even when multiple WebSocket clients are
/// connected. A single lag event is now recoverable (see v2 stream
/// handler), but larger headroom reduces the chance of visible gaps.
const SK_CLIENT_CHANNEL_CAPACITY: usize = 128;

#[derive(Clone)]
pub struct SharedRadars {
    radars: Arc<RwLock<Radars>>,
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

        let is_new = radars.info.get(&key).is_none();
        if is_new {
            // Set any previously detected model and ranges
            radars
                .persistent_data
                .update_info_from_persistence(&mut new_info);

            log::info!(
                "Found radar: key '{}' name '{}' with {} ranges",
                &new_info.key,
                new_info.controls.user_name(),
                new_info.ranges.len()
            );
            radars.info.insert(key, new_info.clone());
            Some(new_info)
        } else {
            None
        }
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
            .iter()
            .map(|(_k, v)| v)
            .filter(|i| i.ranges.len() > 0)
            .map(|v| v.clone())
            .collect()
    }

    pub fn have_active(&self) -> bool {
        let radars = self.radars.read().unwrap();
        radars
            .info
            .iter()
            .map(|(_k, v)| v)
            .filter(|i| i.ranges.len() > 0)
            .count()
            > 0
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
        for (_, info) in radars.info.iter() {
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
        for (_, info) in radars.info.iter() {
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
            if let Some(status) = info.controls.get_status() {
                if status == Power::Standby {
                    log::info!("Requesting transmit mode for radar '{}'", key);
                    let control_value = ControlValue::new(
                        ControlId::Power,
                        serde_json::Value::Number(serde_json::Number::from(2)), // 2 = Transmit
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
}

#[derive(Clone, Debug)]
struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub persistent_data: Persistence,
    sk_client_tx: tokio::sync::broadcast::Sender<SignalKDelta>,
    blob_tx: Option<mpsc::Sender<BlobMessage>>,
    tracker_command_tx: Option<mpsc::Sender<TrackerCommand>>,
}

#[derive(Debug, PartialEq)]
pub enum Power {
    Off,
    Standby,
    Transmit,
    Preparing,
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
                _ => match n.as_f64() {
                    Some(0.) => Ok(Power::Off),
                    Some(1.) => Ok(Power::Standby),
                    Some(2.) => Ok(Power::Transmit),
                    Some(3.) => Ok(Power::Preparing),
                    _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
                },
            },
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "0" | "off" => Ok(Power::Off),
                "1" | "standby" => Ok(Power::Standby),
                "2" | "transmit" => Ok(Power::Transmit),
                "3" | "preparing" => Ok(Power::Preparing),
                _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
            },
            _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
        }
    }
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
fn default_legend(targets: &TargetMode, doppler_levels: u8, pixel_values: u8) -> Legend {
    let mut legend = Legend {
        pixels: Vec::new(),
        pixel_colors: 0,
        history_start: 0,
        doppler_approaching: None,
        doppler_receding: None,
        strong_return: 0,
        medium_return: 0,
        low_return: 0,
        static_background: None,
    };

    let doppler_total = doppler_levels * 2; // approaching + receding

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

#[cfg(test)]
mod tests {
    use super::RadarError;
    use super::default_legend;
    use axum::response::IntoResponse;

    #[test]
    fn legend() {
        let targets = crate::TargetMode::Arpa;
        let legend = default_legend(&targets, 1, 16);
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
                        // Send to hardware if the brand supports it (e.g. Furuno)
                        if let Some(command_sender) = command_sender {
                            match command_sender
                                .set_control(&cv, &self.info.controls)
                                .await
                            {
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
                    ControlId::ArpaDetectMaxSpeed => {
                        let value = cv.as_value()?;
                        let result = self
                            .info
                            .controls
                            .set_value(&cv.id, value)
                            .map(|_| ())
                            .map_err(|e| RadarError::ControlError(e));
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
                    | ControlId::ExclusionRect4
                    | ControlId::DopplerAutoTrack => {
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
                }
            }
        }

        Ok(())
    }

    pub fn new_spoke_message(&mut self) {
        self.spoke_message = Some(RadarMessage::new());
        self.spoke_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap();
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
            .map(|r| exclusion::rect_to_internal(r))
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
                for i in 0..generic_spoke.len() {
                    if generic_spoke[i] >= max_value {
                        log::error!(
                            "{}: Spoke contains value {} which is >= {}",
                            self.key,
                            generic_spoke[i],
                            max_value
                        );
                        generic_spoke[i] = 0;
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

                if !completed_blobs.is_empty() {
                    if let Some(ref blob_tx) = self.blob_tx {
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
            }

            // Extract heading from spoke data and broadcast for GUI
            if let Some(bearing) = spoke.bearing {
                let heading_spokes = (bearing as i32 - spoke.angle as i32)
                    .rem_euclid(self.info.spokes_per_revolution as i32)
                    as f64;
                let heading_rad =
                    heading_spokes / self.info.spokes_per_revolution as f64 * std::f64::consts::TAU;
                crate::navdata::set_heading_true(Some(heading_rad), &self.key);
            }

            // Always broadcast spoke to clients
            let mut spoke = spoke;
            self.trails
                .update_trails(&mut spoke, &self.info.legend, &self.info.controls);
            message.spokes.push(spoke);

            if angle < self.prev_angle {
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
        if let Some(message) = self.spoke_message.take() {
            if !message.spokes.is_empty() {
                self.info.broadcast_radar_message(message);
            }
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
                log::error!("{}: {}", self.key, e.to_string());
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
        self.set(control_id, value.into(), None, None)
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
                log::error!("{}: {}", self.key, e.to_string());
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
                log::error!("{}: {}", self.key, e.to_string());
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
                log::error!("{}: {}", self.key, e.to_string());
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
                log::error!("{}: {}", self.key, e.to_string());
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
        let heading_spokes = (bearing as i32 - spoke.angle as i32)
            .rem_euclid(spokes_per_revolution as i32) as f64;
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
