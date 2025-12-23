use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_primitive_derive::Primitive;
use protobuf::Message;
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use std::str::FromStr;
use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio_graceful_shutdown::SubsystemHandle;

pub(crate) mod range;
pub(crate) mod spoke;
pub(crate) mod target;
pub(crate) mod trail;

use crate::config::Persistence;
use crate::locator::LocatorId;
use crate::protos::RadarMessage::RadarMessage;
use crate::settings::{ControlError, ControlUpdate, ControlValue, SharedControls};
use crate::{Brand, Session, TargetMode};
use range::{RangeDetection, Ranges};

pub(crate) const NAUTICAL_MILE: i32 = 1852; // 1 nautical mile in meters
pub(crate) const NAUTICAL_MILE_F64: f64 = 1852.; // 1 nautical mile in meters

// A "native to radar" bearing, usually [0..2048] or [0..4096] or [0..8192]
pub(crate) type SpokeBearing = u16;

pub(crate) const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("I/O operation failed")]
    Io(#[from] std::io::Error),
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
    #[error("{0}")]
    ControlError(#[from] ControlError),
    #[error("Cannot set value for control '{0}'")]
    CannotSetControlType(String),
    #[error("Missing value for control '{0}'")]
    MissingValue(String),
    #[error("No such radar with key '{0}'")]
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
    #[cfg(windows)]
    #[error("OS error: {0}")]
    OSError(String),
}

// Tell axum how to convert `RadarError` into a response.
impl IntoResponse for RadarError {
    fn into_response(self) -> Response {
        // Convert error to string to avoid infinite recursion
        // (the tuple impl calls into_response on self, which would recurse)
        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

//
// This order of pixeltypes is also how they are stored in the legend.
//
#[derive(Serialize, Clone, Debug)]
enum PixelType {
    Normal,
    TargetBorder,
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

#[derive(Clone, Debug, Serialize)]
pub struct Lookup {
    r#type: PixelType,
    color: Color,
}

#[derive(Clone, Debug)]
pub struct Legend {
    pub pixels: Vec<Lookup>,
    pub border: u8,
    pub doppler_approaching: u8,
    pub doppler_receding: u8,
    pub history_start: u8,
    pub strong_return: u8,
}

impl Serialize for Legend {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_map(Some(self.pixels.len()))?;
        for (n, value) in self.pixels.iter().enumerate() {
            let key = n.to_string();
            state.serialize_entry(&key, value)?;
        }
        state.end()
    }
}

/// A geographic position expressed in degrees latitude and longitude.
/// Latitude is positive in the northern hemisphere, negative in the southern.
/// Longitude is positive in the eastern hemisphere, negative in the western.
/// The range for latitude is -90 to 90, and for longitude is -180 to 180.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct GeoPosition {
    lat: f64,
    lon: f64,
}

impl GeoPosition {
    pub(crate) fn new(lat: f64, lon: f64) -> Self {
        GeoPosition { lat, lon }
    }
}

impl fmt::Display for GeoPosition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.lat, self.lon)
    }
}

#[derive(Clone, Debug)]
pub struct RadarInfo {
    session: Session,
    key: String,
    pub id: usize,
    pub(crate) locator_id: LocatorId,
    pub brand: Brand,
    pub(crate) serial_no: Option<String>, // Serial # for this radar
    pub(crate) which: Option<String>,     // "A", "B" or None
    pub(crate) pixel_values: u8,          // How many values per pixel, 0..220 or so
    pub spokes_per_revolution: u16,       // How many spokes per rotation
    pub max_spoke_len: u16,               // Fixed for some radars, variable for others
    pub(crate) addr: SocketAddrV4,        // The IP address of the radar
    pub(crate) nic_addr: Ipv4Addr,        // IPv4 address of NIC via which radar can be reached
    pub(crate) spoke_data_addr: SocketAddrV4, // Where the radar will send data spokes
    pub(crate) report_addr: SocketAddrV4, // Where the radar will send reports
    pub(crate) send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
    pub legend: Legend,                   // What pixel values mean
    pub controls: SharedControls,         // Which controls there are, not complete in beginning
    pub ranges: Ranges,                   // Ranges for this radar, empty in beginning
    pub(crate) range_detection: Option<RangeDetection>, // if Some, then ranges are flexible, detected and persisted
    pub(crate) doppler: bool,                           // Does it support Doppler?
    rotation_timestamp: Instant,

    // Channels
    pub message_tx: tokio::sync::broadcast::Sender<Vec<u8>>, // Serialized RadarMessage
}

impl RadarInfo {
    pub fn new(
        session: Session,
        locator_id: LocatorId,
        brand: Brand,
        serial_no: Option<&str>,
        which: Option<&str>,
        pixel_values: u8, // How many values per pixel, 0..220 or so
        spokes_per_revolution: usize,
        max_spoke_len: usize,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
        controls: SharedControls,
        doppler: bool,
    ) -> Self {
        // Large buffer to handle high-latency connections (VPN, slow networks)
        // A 4G radar sends ~64 frames/second with 32 spokes each = ~2048 spokes/sec
        // With 200ms latency, we need ~400+ messages buffered to avoid lag
        let (message_tx, _message_rx) = tokio::sync::broadcast::channel(1024);

        let legend = default_legend(session.clone(), false, pixel_values);

        let info = RadarInfo {
            session,
            key: {
                let mut key = brand.to_string();

                if let Some(serial_no) = serial_no {
                    key.push_str("-");
                    key.push_str(serial_no);
                } else {
                    write!(key, "-{}", &addr).unwrap();
                }

                if let Some(which) = which {
                    key.push_str("-");
                    key.push_str(which);
                }
                key
            },
            id: usize::MAX,
            locator_id,
            brand,
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
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
            range_detection: None,
            controls,
            doppler,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
        };

        log::debug!("Created RadarInfo {:?}", info);
        info
    }

    pub fn all_clients_rx(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        self.controls.all_clients_rx()
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        self.controls.control_update_subscribe()
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn pixel_values(&self) -> u8 {
        self.pixel_values
    }

    pub fn set_doppler(&mut self, doppler: bool) {
        if doppler != self.doppler {
            self.legend = default_legend(self.session.clone(), doppler, self.pixel_values);
            log::info!("Doppler changed to {}", doppler);
        }
        self.doppler = doppler;
    }

    pub fn set_pixel_values(&mut self, pixel_values: u8) {
        if pixel_values != self.pixel_values {
            self.legend = default_legend(self.session.clone(), self.doppler, pixel_values);
            log::info!("Pixel_values changed to {}", pixel_values);
        }
        self.pixel_values = pixel_values;
    }

    pub fn set_rotation_length(&mut self, millis: u32) -> u32 {
        let diff = millis as f64;
        let rpm = format!("{:.0}", (600_000. / diff));

        log::debug!(
            "{}: rotation speed elapsed {} = {} RPM",
            self.key,
            diff,
            rpm
        );

        if diff < 10000. && diff > 300. {
            let _ = self.controls.set_string("rotation_speed", rpm);
            diff as u32
        } else {
            0
        }
    }

    pub fn full_rotation(&mut self) -> u32 {
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
            let _ = self.controls.set_string("rotation_speed", rpm);
            diff as u32
        } else {
            0
        }
    }

    pub(crate) fn set_ranges(&mut self, ranges: Ranges) -> Result<(), RadarError> {
        self.controls.set_valid_ranges("range", &ranges)?;
        self.ranges = ranges;
        Ok(())
    }

    pub(crate) fn broadcast_radar_message(&self, message: RadarMessage) {
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

    ///
    ///  forward_output is activated in all starts of radars when cli args.output
    ///  is true:
    ///
    ///  if args.output {
    ///      subsys.start(SubsystemBuilder::new(data_name, move |s| {
    ///          info.forward_output(s)
    ///      }));
    ///  }
    ///

    pub async fn forward_output(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
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
}

impl Display for RadarInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Radar {} locator {} brand {}",
            &self.id,
            &self.locator_id.as_str(),
            &self.brand
        )?;
        if let Some(which) = &self.which {
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

#[derive(Clone)]
pub struct SharedRadars {
    session: Session,
    radars: Arc<RwLock<Radars>>,
}

impl SharedRadars {
    pub fn new(session: Session) -> Self {
        SharedRadars {
            session,
            radars: Arc::new(RwLock::new(Radars {
                info: HashMap::new(),
                persistent_data: Persistence::new(),
            })),
        }
    }

    // A radar has been found
    pub(crate) fn located(&self, mut new_info: RadarInfo) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = self.radars.write().unwrap();

        // For now, drop second radar in replay Mode...
        if self.session.read().unwrap().args.replay && key.ends_with("-B") {
            return None;
        }

        let max_radar_id = radars.info.iter().map(|(_, i)| i.id).max().unwrap_or(0);
        let max_persist_id = radars
            .persistent_data
            .config
            .radars
            .iter()
            .map(|(_, i)| i.id)
            .max()
            .unwrap_or(0);
        let max_id = std::cmp::max(max_radar_id, max_persist_id);

        let is_new = radars.info.get(&key).is_none();
        if is_new {
            // Set any previously detected model and ranges
            radars
                .persistent_data
                .update_info_from_persistence(&mut new_info);

            if new_info.id == usize::MAX {
                new_info.id = max_id + 1;
            }

            log::debug!("key '{}' info {:?}", &new_info.key, new_info);
            log::info!(
                "Found radar: key '{}' id {} name '{}'",
                &new_info.key,
                new_info.id,
                new_info
                    .controls
                    .user_name()
                    .unwrap_or_else(|| new_info.key.clone())
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
    pub fn update(&self, radar_info: &RadarInfo) {
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

    pub fn get_by_id(&self, key: &str) -> Option<RadarInfo> {
        let radars = self.radars.read().unwrap();
        for info in radars.info.iter() {
            // Check standard radar ID format
            let id = format!("radar-{}", info.1.id);
            if id == key {
                return Some(info.1.clone());
            }
            // Check for playback radar ID (playback-filename format)
            if key.starts_with("playback-") {
                // For playback radars, check if the key contains the base filename
                // The internal key is like "Furuno-Playback-filename", and we're looking for "playback-filename"
                let playback_suffix = &key[9..]; // Strip "playback-" prefix
                if info.0.contains(&format!("Playback-{}", playback_suffix)) {
                    return Some(info.1.clone());
                }
            }
        }
        None
    }

    /// Get radar by internal key (e.g., "Playback-filename" or "Furuno-serial-A")
    pub fn get_by_key(&self, key: &str) -> Option<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars.info.get(key).cloned()
    }

    pub fn remove(&self, key: &str) {
        let mut radars = self.radars.write().unwrap();

        radars.info.remove(key);
    }

    ///
    /// Update radar info in radars container
    ///
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

    // deprecated_marked_for_delete: Only used by legacy locator (reply_with_interface_state)
    #[allow(dead_code)]
    pub(crate) fn is_active_radar(&self, brand: &Brand, ip: &Ipv4Addr) -> bool {
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

    /// Update Furuno radar model when received from UDP model report
    #[cfg(feature = "furuno")]
    pub fn update_furuno_model(&self, key: &str, model_name: &str) {
        use crate::brand::furuno::settings;

        let mut radars = self.radars.write().unwrap();
        if let Some(info) = radars.info.get_mut(key) {
            let model = model_name_to_radar_model(model_name);
            log::info!(
                "{}: Updating model from UDP report: {} -> {:?}",
                key,
                model_name,
                model
            );
            // Use "unknown" for firmware since we don't have it from UDP
            settings::update_when_model_known(info, model, "unknown");
        }
    }

    /// Update radar info from a core RadarDiscovery (e.g., when model info arrives).
    ///
    /// This finds the existing radar by address and updates its model/serial.
    pub fn update_from_discovery(&self, discovery: &mayara_core::radar::RadarDiscovery) {
        use mayara_core::Brand as CoreBrand;

        // Extract IP from discovery address (which may be "ip:port" or just "ip")
        let discovery_ip = discovery
            .address
            .split(':')
            .next()
            .unwrap_or(&discovery.address);

        // Find radar by matching address
        let matching_key = {
            let radars = self.radars.read().unwrap();
            radars
                .info
                .iter()
                .find(|(_, info)| {
                    let info_ip = info.addr.ip().to_string();
                    info_ip == discovery_ip
                })
                .map(|(key, _)| key.clone())
        };

        if let Some(key) = matching_key {
            // Update model if provided - delegate to brand-specific handlers
            if let Some(ref model_name) = discovery.model {
                match discovery.brand {
                    #[cfg(feature = "furuno")]
                    CoreBrand::Furuno => {
                        self.update_furuno_model(&key, model_name);
                    }
                    #[cfg(feature = "navico")]
                    CoreBrand::Navico => {
                        self.update_navico_model(&key, model_name);
                    }
                    #[cfg(feature = "raymarine")]
                    CoreBrand::Raymarine => {
                        // Raymarine model settings applied at discovery time
                        log::debug!(
                            "{}: Raymarine model update ignored (applied at discovery)",
                            key
                        );
                    }
                    _ => {
                        log::debug!(
                            "{}: No model update handler for brand {:?}",
                            key,
                            discovery.brand
                        );
                    }
                }
            }

            // Update serial if provided
            if let Some(ref serial) = discovery.serial_number {
                let mut radars = self.radars.write().unwrap();
                if let Some(info) = radars.info.get_mut(&key) {
                    if info.serial_no.as_ref() != Some(serial) {
                        log::info!(
                            "{}: Updating serial from discovery: {:?} -> {}",
                            key,
                            info.serial_no,
                            serial
                        );
                        info.serial_no = Some(serial.clone());
                    }
                }
            }
        } else {
            log::warn!(
                "update_from_discovery: No radar found for address {}",
                discovery_ip
            );
        }
    }

    /// Update Navico radar model when received from discovery update
    #[cfg(feature = "navico")]
    pub fn update_navico_model(&self, key: &str, model_name: &str) {
        use crate::brand::navico::Model;

        let mut radars = self.radars.write().unwrap();
        if let Some(info) = radars.info.get_mut(key) {
            let current_model = info.controls.model_name();
            if current_model.as_deref() != Some(model_name) {
                log::info!(
                    "{}: Updating Navico model: {:?} -> {}",
                    key,
                    current_model,
                    model_name
                );
                info.controls.set_model_name(model_name.to_string());

                // Apply model-specific control updates
                let model = Model::from_name(model_name);
                if model != Model::Unknown {
                    crate::brand::navico::update_controls_for_model(info, model);
                }
            }
        }
    }
}

/// Convert model name string to RadarModel enum (for Furuno)
#[cfg(feature = "furuno")]
fn model_name_to_radar_model(name: &str) -> crate::brand::furuno::RadarModel {
    use crate::brand::furuno::RadarModel;
    match name {
        "FAR-21x7" => RadarModel::FAR21x7,
        "DRS" => RadarModel::DRS,
        "FAR-14x7" => RadarModel::FAR14x7,
        "DRS4DL" => RadarModel::DRS4DL,
        "FAR-3000" => RadarModel::FAR3000,
        "DRS4D-NXT" => RadarModel::DRS4DNXT,
        "DRS6A-NXT" => RadarModel::DRS6ANXT,
        "DRS6A-XCLASS" => RadarModel::DRS6AXCLASS,
        "FAR-15x3" => RadarModel::FAR15x3,
        "FAR-14x6" => RadarModel::FAR14x6,
        "DRS12A-NXT" => RadarModel::DRS12ANXT,
        "DRS25A-NXT" => RadarModel::DRS25ANXT,
        _ => RadarModel::Unknown,
    }
}

#[derive(Clone, Debug)]
struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub persistent_data: Persistence,
}

pub struct Statistics {
    pub broken_packets: usize,
    pub missing_spokes: usize,  // this revolution
    pub received_spokes: usize, // this revolution
    pub total_rotations: usize, // total number of revolutions
}

impl Statistics {
    pub fn new() -> Self {
        Statistics {
            broken_packets: 0,
            missing_spokes: 0,
            received_spokes: 0,
            total_rotations: 0,
        }
    }

    pub fn full_rotation(&mut self, key: &str) {
        self.total_rotations += 1;
        log::debug!(
            "{}: Full rotation #{},  {} spokes received and {} missing spokes {} broken packets",
            key,
            self.total_rotations,
            self.received_spokes,
            self.missing_spokes,
            self.broken_packets
        );
        self.received_spokes = 0;
        self.missing_spokes = 0;
        self.broken_packets = 0;
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Status {
    Off,
    Standby,
    Transmit,
    Preparing,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" | "0" => Ok(Status::Off),
            "standby" | "1" => Ok(Status::Standby),
            "transmit" | "2" => Ok(Status::Transmit),
            "preparing" | "3" => Ok(Status::Preparing),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

// The actual values are not arbitrary: these are the exact values as reported
// by HALO radars, simplifying the navico::report code.
#[derive(Copy, Clone, Debug, Primitive)]
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
const TRANSPARENT: u8 = 0;
const OPAQUE: u8 = 255;

fn default_legend(session: Session, doppler: bool, pixel_values: u8) -> Legend {
    let mut legend = Legend {
        pixels: Vec::new(),
        history_start: 255,
        border: 255,
        doppler_approaching: 255,
        doppler_receding: 255,
        strong_return: 255,
    };

    let mut pixel_values = pixel_values;
    if pixel_values > 255 - 32 - 2 {
        pixel_values = 255 - 32 - 2;
    }

    if pixel_values == 0 {
        return legend;
    }

    let pixels_with_color = pixel_values - 1;
    let two_thirds = (pixels_with_color * 2) / 3;
    legend.strong_return = two_thirds as u8;

    // No return is black/transparent
    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: TRANSPARENT,
        },
    });

    // Use continuous color blending for better visual distinction,
    // especially important for low pixel counts like Navico's 16 values.
    // Color progression: dark blue -> cyan -> green -> yellow -> red
    // This gives more visual distinction than the previous thirds-based approach.
    const MIN_INTENSITY: f64 = 85.0; // Start at 1/3 intensity for visibility
    const MAX_INTENSITY: f64 = 255.0;

    for v in 1..pixel_values {
        // Normalize v to 0.0 - 1.0 range
        let t = (v - 1) as f64 / (pixels_with_color - 1).max(1) as f64;

        // Compute RGB using smooth transitions through the color spectrum
        // This creates: blue (0.0) -> cyan (0.25) -> green (0.5) -> yellow (0.75) -> red (1.0)
        let (r, g, b) = if t < 0.25 {
            // Blue to Cyan: increase green
            let local_t = t / 0.25;
            (0.0, local_t, 1.0)
        } else if t < 0.5 {
            // Cyan to Green: decrease blue
            let local_t = (t - 0.25) / 0.25;
            (0.0, 1.0, 1.0 - local_t)
        } else if t < 0.75 {
            // Green to Yellow: increase red
            let local_t = (t - 0.5) / 0.25;
            (local_t, 1.0, 0.0)
        } else {
            // Yellow to Red: decrease green
            let local_t = (t - 0.75) / 0.25;
            (1.0, 1.0 - local_t, 0.0)
        };

        // Apply intensity scaling
        let scale = |c: f64| -> u8 { (MIN_INTENSITY + (MAX_INTENSITY - MIN_INTENSITY) * c) as u8 };

        legend.pixels.push(Lookup {
            r#type: PixelType::Normal,
            color: Color {
                r: if r > 0.0 { scale(r) } else { 0 },
                g: if g > 0.0 { scale(g) } else { 0 },
                b: if b > 0.0 { scale(b) } else { 0 },
                a: OPAQUE,
            },
        });
    }

    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: OPAQUE,
        },
    });

    if session.read().unwrap().args.targets == TargetMode::Arpa {
        legend.border = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::TargetBorder,
            color: Color {
                r: 200,
                g: 200,
                b: 200,
                a: OPAQUE,
            },
        });
    }

    if doppler {
        legend.doppler_approaching = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerApproaching,
            color: Color {
                // Purple
                r: 255,
                g: 0,
                b: 255,
                a: OPAQUE,
            },
        });
        legend.doppler_receding = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerReceding,
            color: Color {
                // Green
                r: 0x00,
                g: 0xff,
                b: 0x00,
                a: OPAQUE,
            },
        });
    }

    if session.read().unwrap().args.targets != TargetMode::None {
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
    use super::default_legend;

    #[test]
    fn legend() {
        let session = crate::Session::new_fake();
        let legend = default_legend(session.clone(), true, 16);
        let json = serde_json::to_string_pretty(&legend).unwrap();
        println!("{}", json);
    }
}
