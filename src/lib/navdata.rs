use atomic_float::AtomicF64;
use futures_util::future::select_ok;
use mdns_sd::{Error, IfKind, ServiceDaemon, ServiceEvent};
use nmea_parser::*;
use serde_json::Value;
use std::{
    collections::HashSet,
    future::Future,
    io::ErrorKind,
    net::SocketAddr,
    pin::Pin,
    sync::{
        OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncBufReadExt, net::UdpSocket, time::sleep};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio::{io::BufReader, sync::broadcast::Receiver};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::{
    Cli,
    ais::AisVesselStore,
    radar::{GeoPosition, RadarError},
    stream::SignalKDelta,
};

static HEADING_TRUE: AtomicF64 = AtomicF64::new(f64::NAN);
static POSITION_VALID: AtomicBool = AtomicBool::new(false);
static POSITION_LAT: AtomicF64 = AtomicF64::new(f64::NAN);
static POSITION_LON: AtomicF64 = AtomicF64::new(f64::NAN);
static COG: AtomicF64 = AtomicF64::new(f64::NAN);
static SOG: AtomicF64 = AtomicF64::new(f64::NAN);

/// Broadcast sender for navigation updates to GUI clients
static NAV_BROADCAST_TX: OnceLock<tokio::sync::broadcast::Sender<SignalKDelta>> = OnceLock::new();

/// Initialize the navigation broadcast sender (called once at startup)
pub fn init_nav_broadcast(tx: tokio::sync::broadcast::Sender<SignalKDelta>) {
    let _ = NAV_BROADCAST_TX.set(tx);
}

/// Broadcast a navigation update to subscribed clients
fn broadcast_nav_update(path: &str, value: f64, source: &str) {
    if let Some(tx) = NAV_BROADCAST_TX.get() {
        let mut delta = SignalKDelta::new();
        delta.add_navigation_update(path, value, source);
        // Ignore send errors (no subscribers)
        let _ = tx.send(delta);
    }
}

/// Own-ship context detected from Signal K server (when pass_ais is enabled)
static OWN_SHIP_CONTEXT: OnceLock<RwLock<Option<String>>> = OnceLock::new();

/// AIS vessel store (when pass_ais is enabled)
static AIS_STORE: OnceLock<std::sync::Arc<AisVesselStore>> = OnceLock::new();

/// Get the own-ship context if detected
fn get_own_ship_context() -> Option<String> {
    OWN_SHIP_CONTEXT
        .get()
        .and_then(|lock| lock.read().ok())
        .and_then(|guard| guard.clone())
}

///// Set the own-ship context (detected from first message after vessels.self subscription)
fn set_own_ship_context(context: &str) {
    let lock = OWN_SHIP_CONTEXT.get_or_init(|| RwLock::new(None));
    if let Ok(mut guard) = lock.write() {
        if guard.is_none() {
            log::info!("Own-ship context set to: {}", context);
            *guard = Some(context.to_string());
        }
    }
}

/// Initialize the AIS vessel store (called once at startup when pass_ais is enabled)
pub fn init_ais_store(tx: tokio::sync::broadcast::Sender<SignalKDelta>) {
    let store = AisVesselStore::new(tx);
    let _ = AIS_STORE.set(store);
}

/// Get a reference to the AIS vessel store
pub fn get_ais_store() -> Option<&'static std::sync::Arc<AisVesselStore>> {
    AIS_STORE.get()
}

/// Update AIS vessel data from Signal K message
fn update_ais_vessel(context: &str, updates: &Value) {
    if let Some(store) = AIS_STORE.get() {
        store.update(context, updates);
    }
}

///
/// Get the heading in radians [0..2*PI>
///
pub(crate) fn get_heading_true() -> Option<f64> {
    let heading = HEADING_TRUE.load(Ordering::Acquire);
    if !heading.is_nan() {
        return Some(heading);
    }
    return None;
}

///
/// Set the heading in radians [0..2*PI>
///
pub(crate) fn set_heading_true(heading: Option<f64>, source: &str) {
    use std::f64::consts::TAU;

    if let Some(h) = heading {
        assert!(
            h > -TAU && h < 2.0 * TAU,
            "set_heading_true: heading {h} rad ({} deg) from '{source}' is out of range",
            h.to_degrees()
        );
        let h = h.rem_euclid(TAU);

        let old = HEADING_TRUE.swap(h, Ordering::AcqRel);
        // Only broadcast if value changed significantly (> 0.001 rad ~ 0.06 deg)
        if (old - h).abs() > 0.001 || old.is_nan() {
            broadcast_nav_update("navigation.headingTrue", h, source);
        }
    } else {
        HEADING_TRUE.store(f64::NAN, Ordering::Release);
    }
}

/// Force broadcast the current heading value (for emulator to ensure GUI receives heading)
pub(crate) fn broadcast_heading(source: &str) {
    let h = HEADING_TRUE.load(Ordering::Acquire);
    if !h.is_nan() {
        broadcast_nav_update("navigation.headingTrue", h, source);
    }
}

pub fn get_radar_position() -> Option<GeoPosition> {
    if POSITION_VALID.load(Ordering::Acquire) {
        let lat = POSITION_LAT.load(Ordering::Acquire);
        let lon = POSITION_LON.load(Ordering::Acquire);
        return Some(GeoPosition::new(lat, lon));
    }
    return None;
}

pub(crate) fn get_position() -> (Option<f64>, Option<f64>) {
    if POSITION_VALID.load(Ordering::Acquire) {
        let lat = POSITION_LAT.load(Ordering::Acquire);
        let lon = POSITION_LON.load(Ordering::Acquire);
        log::trace!("navdata::get_position() -> lat={}, lon={}", lat, lon);
        return (Some(lat), Some(lon));
    }
    return (None, None);
}

pub(crate) fn set_position(lat: Option<f64>, lon: Option<f64>) {
    if let (Some(lat), Some(lon)) = (lat, lon) {
        log::trace!("navdata::set_position(lat={}, lon={})", lat, lon);
        POSITION_LAT.store(lat, Ordering::Release);
        POSITION_LON.store(lon, Ordering::Release);
        POSITION_VALID.store(true, Ordering::Release);
    } else {
        POSITION_VALID.store(false, Ordering::Release);
        return;
    }
}

pub(crate) fn get_cog() -> Option<f64> {
    let cog = COG.load(Ordering::Acquire);
    if !cog.is_nan() {
        return Some(cog);
    }
    return None;
}

pub(crate) fn set_cog(cog: Option<f64>) {
    use std::f64::consts::TAU;

    if let Some(c) = cog {
        assert!(
            c > -TAU && c < 2.0 * TAU,
            "set_cog: COG {c} rad ({} deg) is out of range",
            c.to_degrees()
        );
        COG.store(c.rem_euclid(TAU), Ordering::Release);
    } else {
        COG.store(f64::NAN, Ordering::Release);
    }
}

pub(crate) fn get_sog() -> Option<f64> {
    let sog = SOG.load(Ordering::Acquire);
    if !sog.is_nan() {
        return Some(sog);
    }
    return None;
}

pub(crate) fn set_sog(sog: Option<f64>) {
    if let Some(s) = sog {
        SOG.store(s, Ordering::Release);
    } else {
        SOG.store(f64::NAN, Ordering::Release);
    }
}

/// The hostname of the devices we are searching for.
const SIGNAL_K_SERVICE_NAME: &'static str = "_signalk-tcp._tcp.local.";
const NMEA0183_SERVICE_NAME: &'static str = "_nmea-0183._tcp.local.";

/// Subscription for own-ship navigation data only
const SUBSCRIBE_SELF: &'static str = "{\"context\":\"vessels.self\",\"subscribe\":[{\"path\":\"navigation.headingTrue\"},{\"path\":\"navigation.position\"},{\"path\":\"navigation.speedOverGround\"},{\"path\":\"navigation.courseOverGroundTrue\"}]}\r\n";

/// Additional subscription for all vessels (sent after own-ship context is known)
const SUBSCRIBE_ALL: &'static str =
    "{\"context\":\"vessels.*\",\"subscribe\":[{\"path\":\"*\"}]}\r\n";

enum ConnectionType {
    Mdns,
    Udp(SocketAddr),
    Tcp(SocketAddr),
}

impl ConnectionType {
    fn parse(interface: &Option<String>) -> ConnectionType {
        match interface {
            None => {
                return ConnectionType::Mdns;
            }
            Some(interface) => {
                let parts: Vec<&str> = interface.splitn(2, ':').collect();
                if parts.len() == 1 {
                    return ConnectionType::Mdns;
                } else if parts.len() == 2 {
                    if let Ok(addr) = parts[1].parse() {
                        // Dump if illegal address
                        match parts[0].to_ascii_lowercase().as_str() {
                            "udp" => return ConnectionType::Udp(addr),
                            "tcp" => return ConnectionType::Tcp(addr),
                            _ => {} // fallthrough to panic below
                        }
                    }
                }
            }
        }
        panic!(
            "Interface must be either interface name (no :) or <connection>:<address>:<port> with <connection> one of `udp` or `tcp`."
        );
    }
}

#[derive(Debug)]
enum Stream {
    Tcp(TcpStream),
    Udp(UdpSocket),
}

pub(crate) struct NavigationData {
    args: Cli,
    nmea0183_mode: bool,
    pass_ais: bool,
    service_name: &'static str,
    what: &'static str,
    nmea_parser: Option<NmeaParser>,
}

impl NavigationData {
    pub(crate) fn new(args: Cli) -> Self {
        let nmea0183 = args.nmea0183;
        let pass_ais = args.pass_ais;
        match nmea0183 {
            true => NavigationData {
                args,
                nmea0183_mode: true,
                pass_ais,
                service_name: NMEA0183_SERVICE_NAME,
                what: "NMEA0183",
                nmea_parser: Some(NmeaParser::new()),
            },
            false => NavigationData {
                args,
                nmea0183_mode: false,
                pass_ais,
                service_name: SIGNAL_K_SERVICE_NAME,
                what: "Signal K",
                nmea_parser: None,
            },
        }
    }

    pub(crate) async fn run(
        &mut self,
        subsys: SubsystemHandle,
        rx_ip_change: Receiver<()>,
    ) -> Result<(), Error> {
        // In NND replay mode, consume NMEA sentences from the replay channel
        // instead of connecting to a live TCP/UDP source.
        #[cfg(feature = "pcap-replay")]
        if crate::replay::is_active() {
            if let Some(mut rx) = crate::replay::create_listen(&crate::nnd::NMEA_REPLAY_ADDRESS) {
                // Ensure we have an NMEA parser even if --nmea0183 wasn't passed
                if self.nmea_parser.is_none() {
                    self.nmea_parser = Some(NmeaParser::new());
                }
                log::info!("NavData: listening for NMEA replay packets");
                let mut buf = Vec::with_capacity(1024);
                loop {
                    tokio::select! { biased;
                        _ = subsys.on_shutdown_requested() => {
                            return Ok(());
                        },
                        result = rx.recv_buf_from(&mut buf) => {
                            match result {
                                Ok((len, _from)) => {
                                    if let Ok(text) = std::str::from_utf8(&buf[..len]) {
                                        for line in text.lines() {
                                            let trimmed = line.trim();
                                            if trimmed.starts_with('$') || trimmed.starts_with('!') {
                                                if let Err(e) = self.parse_nmea0183(trimmed) {
                                                    log::warn!("NMEA replay: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    buf.clear();
                                }
                                Err(_) => return Ok(()),
                            }
                        }
                    }
                }
            }
        }

        log::debug!("{} run_loop (re)start", self.what);
        let mut rx_ip_change = rx_ip_change;
        let navigation_address = self.args.navigation_address.clone();

        loop {
            match self
                .find_service(&subsys, &mut rx_ip_change, &navigation_address)
                .await
            {
                Ok(Stream::Tcp(stream)) => {
                    log::info!(
                        "Listening to {} data from {}",
                        self.what,
                        stream.peer_addr().unwrap()
                    );
                    match self.receive_loop(stream, &subsys).await {
                        Err(RadarError::Shutdown) => {
                            log::debug!("{} receive_loop shutdown", self.what);
                            return Ok(());
                        }
                        e => {
                            log::debug!("{} receive_loop restart on result {:?}", self.what, e);
                        }
                    }
                }
                Ok(Stream::Udp(socket)) => {
                    log::info!("Listening to {} data via UDP", self.what);
                    match self.receive_udp_loop(socket, &subsys).await {
                        Err(RadarError::Shutdown) => {
                            log::debug!("{} receive_loop shutdown", self.what);
                            return Ok(());
                        }
                        e => {
                            log::debug!("{} receive_loop restart on result {:?}", self.what, e);
                        }
                    }
                }
                Err(e) => match e {
                    RadarError::Shutdown => {
                        log::debug!("{} run_loop shutdown", self.what);
                        return Ok(());
                    }
                    e => {
                        log::debug!("{} find_service restart on result {:?}", self.what, e);
                    }
                },
            }
        }
    }

    async fn find_service(
        &self,
        subsys: &SubsystemHandle,
        rx_ip_change: &mut Receiver<()>,
        interface: &Option<String>,
    ) -> Result<Stream, RadarError> {
        let connection_type = ConnectionType::parse(interface);
        match connection_type {
            ConnectionType::Mdns => {
                self.find_mdns_service(subsys, rx_ip_change, interface)
                    .await
            }
            ConnectionType::Tcp(addr) => self.find_tcp_service(subsys, addr).await,
            ConnectionType::Udp(addr) => self.find_udp_service(subsys, addr).await,
        }
    }

    async fn find_mdns_service(
        &self,
        subsys: &SubsystemHandle,
        rx_ip_change: &mut Receiver<()>,
        interface: &Option<String>,
    ) -> Result<Stream, RadarError> {
        let mut known_addresses: HashSet<SocketAddr> = HashSet::new();

        let mdns = ServiceDaemon::new().expect("Failed to create daemon");

        if interface.is_some() {
            let _ = mdns.disable_interface(IfKind::All);
            let navigation_address = self
                .args
                .navigation_address
                .as_ref()
                .unwrap()
                .to_string()
                .clone();
            let _ = mdns.enable_interface(IfKind::Name(navigation_address));
        }
        let tcp_locator = mdns.browse(self.service_name).expect(&format!(
            "Failed to browse for {} service",
            self.service_name
        ));

        log::debug!("SignalK find_service (re)start");

        let r: Result<Stream, RadarError>;
        loop {
            let s = &subsys;
            tokio::select! { biased;
                _ = s.on_shutdown_requested() => {
                    r = Err(RadarError::Shutdown);
                    break;
                },
                _ = rx_ip_change.recv() => {
                    log::debug!("rx_ip_change");
                    r = Err(RadarError::IPAddressChanged);
                    break;
                },
                event = tcp_locator.recv_async() => {
                    match event {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            log::debug!("Resolved a new {} service: {}", self.what, info.get_fullname());
                            let addr = info.get_addresses();
                            let port = info.get_port();

                            for a in addr {
                                known_addresses.insert(SocketAddr::new(a.to_ip_addr(), port));
                            }
                        },
                        _ => {
                            continue;
                        }
                    }

                }
            }

            let stream = connect_first(known_addresses.clone()).await;
            match stream {
                Ok(stream) => {
                    log::info!(
                        "Listening to {} data from {}",
                        self.what,
                        stream.peer_addr().unwrap()
                    );

                    r = Ok(Stream::Tcp(stream));
                    break;
                }
                Err(_e) => {} // Just loop
            }
        }

        log::debug!("find_service(...,'{}') = {:?}", self.service_name, r);
        if let Ok(r3) = mdns.shutdown() {
            if let Ok(r3) = r3.recv() {
                log::debug!("mdns_shutdown: {:?}", r3);
            }
        }
        return r;
    }

    async fn find_tcp_service(
        &self,
        subsys: &SubsystemHandle,
        addr: SocketAddr,
    ) -> Result<Stream, RadarError> {
        log::debug!("TCP find_service {} (re)start", self.what);

        loop {
            let s = &subsys;

            tokio::select! { biased;
                _ = s.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                stream = connect_to_socket(addr) => {
                    match stream {
                        Ok(stream) => {
                            log::info!(
                                "Receiving {} data from {}",
                                self.what,
                                stream.peer_addr().unwrap()
                            );
                            return Ok(Stream::Tcp(stream));
                        }
                        Err(e) => {
                            log::trace!("Failed to connect {} to {addr}: {e}", self.what);
                            sleep(Duration::from_millis(1000)).await;
                        }
                    }
                }
            }
        }
    }

    async fn find_udp_service(
        &self,
        subsys: &SubsystemHandle,
        addr: SocketAddr,
    ) -> Result<Stream, RadarError> {
        log::debug!("UDP find_service (re)start");

        loop {
            let s = &subsys;

            tokio::select! { biased;
                _ = s.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                stream = UdpSocket::bind(addr) => {
                    match stream {
                        Ok(stream) => {
                            log::info!(
                                "Receiving {} data from {}",
                                self.what,
                                stream.local_addr().unwrap()
                            );
                            return Ok(Stream::Udp(stream));
                        }
                        Err(e) => {
                            log::trace!("Failed to bind {} to {addr}: {e}", self.what);
                            sleep(Duration::from_millis(1000)).await;
                        }
                    }
                }
            }
        }
    }

    // Loop until we get an error, then just return the error
    // or Ok if we are to shutdown.
    async fn receive_loop(
        &mut self,
        mut stream: TcpStream,
        subsys: &SubsystemHandle,
    ) -> Result<(), RadarError> {
        log::info!(
            "{} receive_loop started for {}",
            self.what,
            stream.peer_addr().unwrap()
        );
        let (read_half, mut write_half) = stream.split();
        let mut lines = BufReader::new(read_half).lines();

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{} receive_loop shutdown", self.what);
                    return Ok(());
                },
                r = lines.next_line() => {
                    match r {
                        Ok(Some(line)) => {
                            log::trace!("{} <- {}", self.what, line);
                            if self.nmea0183_mode {
                                // We are in NMEA0183 mode, so we need to parse
                                // the data we get.
                                match self.parse_nmea0183(&line) {
                                    Err(e) => { log::warn!("{}", e)}
                                    Ok(_) => { }
                                }
                            } else {
                                // We are in SignalK mode, so we need to subscribe
                                // to the data we want.
                                if line.starts_with("{\"name\":") {
                                    log::debug!("{} sending subscription", self.what);
                                    self.send_subscription(&mut write_half).await?;
                                }
                                else {
                                    // Check if we need to send AIS subscription
                                    // (when pass_ais is enabled and we just learned the own-ship context)
                                    let had_own_ship = get_own_ship_context().is_some();

                                    match parse_signalk(&line, self.pass_ais) {
                                        Err(e) => { log::trace!("{} parse error: {}", self.what, e)}
                                        Ok(_) => { }
                                    }

                                    // If pass_ais is enabled and we just learned the own-ship context,
                                    // send the expanded subscription for all vessels
                                    if self.pass_ais && !had_own_ship && get_own_ship_context().is_some() {
                                        log::info!("Own-ship context established, expanding subscription to all vessels");
                                        self.send_ais_subscription(&mut write_half).await?;
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            log::warn!("{} connection closed by server", self.what);
                            return Ok(());
                        }
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
            }
        }
    }

    async fn send_subscription(
        &self,
        stream: &mut tokio::net::tcp::WriteHalf<'_>,
    ) -> Result<(), RadarError> {
        // Always start with SUBSCRIBE_SELF to get own-ship navigation data
        // and to learn the own-ship context
        let bytes: &[u8] = SUBSCRIBE_SELF.as_bytes();
        log::info!("Sending SignalK subscription: {}", SUBSCRIBE_SELF.trim());
        let result = stream.write_all(bytes).await.map_err(|e| RadarError::Io(e));
        match &result {
            Ok(_) => log::debug!("Subscription sent successfully"),
            Err(e) => log::warn!("Failed to send subscription: {:?}", e),
        }
        result
    }

    /// Send the expanded subscription for all vessels (AIS data)
    async fn send_ais_subscription(
        &self,
        stream: &mut tokio::net::tcp::WriteHalf<'_>,
    ) -> Result<(), RadarError> {
        let bytes: &[u8] = SUBSCRIBE_ALL.as_bytes();
        log::info!("Sending AIS subscription: {}", SUBSCRIBE_ALL.trim());
        let result = stream.write_all(bytes).await.map_err(|e| RadarError::Io(e));
        match &result {
            Ok(_) => log::debug!("AIS subscription sent successfully"),
            Err(e) => log::warn!("Failed to send AIS subscription: {:?}", e),
        }
        result
    }

    // Loop until we get an error, then just return the error
    // or Ok if we are to shutdown.
    async fn receive_udp_loop(
        &mut self,
        socket: UdpSocket,
        subsys: &SubsystemHandle,
    ) -> Result<(), RadarError> {
        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{} receive_loop shutdown", self.what);
                    return Ok(());
                },
                r = socket.readable() => {
                    match r {
                        Ok(()) => {
                            let mut buf = [0; 2000];
                            let r = socket.try_recv(&mut buf);
                            match r {
                                Ok(len) => {
                                    self.process_udp_buf(&buf[..len]);
                                },
                                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {},
                                Err(e) => { log::warn!("{}", e)}
                            }
                        }

                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
            }
        }
    }

    fn process_udp_buf(&mut self, buf: &[u8]) {
        if let Ok(data) = String::from_utf8(buf.to_vec()) {
            for line in data.lines() {
                match if self.nmea0183_mode {
                    self.parse_nmea0183(line)
                } else {
                    parse_signalk(&line, self.pass_ais)
                } {
                    Err(e) => {
                        log::warn!("{}", e)
                    }
                    Ok(_) => {}
                }
            }
        }
    }

    fn parse_nmea0183(&mut self, s: &str) -> Result<(), RadarError> {
        let parser = self.nmea_parser.as_mut().unwrap();

        match parser.parse_sentence(s) {
            Ok(ParsedMessage::Rmc(rmc)) => {
                set_position(rmc.latitude, rmc.longitude);
            }
            Ok(ParsedMessage::Gll(gll)) => {
                set_position(gll.latitude, gll.longitude);
            }
            Ok(ParsedMessage::Hdt(hdt)) => {
                set_heading_true(
                    hdt.heading_true.map(|h| h.to_radians()),
                    "nmea0183",
                );
            }
            Ok(ParsedMessage::Vtg(vtg)) => {
                set_cog(vtg.cog_true.map(|c| c.to_radians()));
                let sog = vtg
                    .sog_kph
                    .or_else(|| vtg.sog_knots.map(|k| k * 1.852))
                    .map(|s| s * 3.6); // convert to m/s
                set_sog(sog);
            }

            Err(e) => match e {
                ParseError::UnsupportedSentenceType(_) => {}
                ParseError::CorruptedSentence(e2) => {
                    return Err(RadarError::ParseNmea0183(format!("{s}: {e2}")));
                }
                ParseError::InvalidSentence(e2) => {
                    return Err(RadarError::ParseNmea0183(format!("{s}: {e2}")));
                }
            },
            _ => {}
        }
        Ok(())
    }
}

//  {"context":"vessels.urn:mrn:imo:mmsi:244060807","updates":
//   [{"source":{"sentence":"GLL","talker":"BM","type":"NMEA0183","label":"canboat-merrimac"},
//     "$source":"canboat-merrimac.BM","timestamp":"2024-10-01T09:11:36.000Z",
//     "values":[{"path":"navigation.position","value":{"longitude":5.428445,"latitude":53.180205}}]}]}

fn parse_signalk(s: &str, pass_ais: bool) -> Result<(), RadarError> {
    log::trace!("parse_signalk: parsing '{}'", s);
    match serde_json::from_str::<Value>(s) {
        Ok(v) => {
            log::trace!("parse_signalk: parsed JSON successfully");
            let context = v["context"].as_str();
            let updates = &v["updates"];

            // When pass_ais is enabled, handle own-ship detection and AIS forwarding
            if pass_ais {
                if let Some(ctx) = context {
                    let own_ship = get_own_ship_context();

                    if own_ship.is_none() {
                        // First message after subscribing to vessels.self establishes own-ship
                        // The context will be the actual vessel URN (e.g., vessels.urn:mrn:imo:mmsi:244060807)
                        set_own_ship_context(ctx);
                        log::info!("Own-ship context detected: {}", ctx);
                        // Continue to process this as navigation data
                    } else if let Some(own_ship_ctx) = own_ship {
                        // We know which vessel is own-ship, check if this is a different vessel
                        let is_own_ship = own_ship_ctx == ctx || ctx == "vessels.self";
                        if !is_own_ship {
                            // This is an AIS target - update the vessel store
                            update_ais_vessel(ctx, updates);
                            return Ok(());
                        }
                    }
                }
            }

            // Process own-ship navigation data
            let update = &updates[0];
            // Extract source from upstream SignalK message
            // Try $source first (more specific), then source, fall back to "signalk"
            let source = update["$source"]
                .as_str()
                .or_else(|| update["source"]["label"].as_str())
                .or_else(|| update["source"]["type"].as_str())
                .unwrap_or("signalk");
            let values = &update["values"][0];
            {
                log::trace!("parse_signalk: values = {:?}", values);

                if let (Some(path), value) = (values["path"].as_str(), &values["value"]) {
                    log::trace!("parse_signalk: path = '{}', value = {:?}", path, value);
                    match path {
                        "navigation.position" => {
                            log::trace!(
                                "parse_signalk: position lat={:?} lon={:?}",
                                value["latitude"].as_f64(),
                                value["longitude"].as_f64()
                            );
                            set_position(value["latitude"].as_f64(), value["longitude"].as_f64());
                            return Ok(());
                        }
                        "navigation.headingTrue" => {
                            set_heading_true(value.as_f64(), source);
                            return Ok(());
                        }
                        "navigation.speedOverGround" => {
                            set_sog(value.as_f64());
                            return Ok(());
                        }
                        "navigation.courseOverGroundTrue" => {
                            set_cog(value.as_f64());
                            return Ok(());
                        }
                        _ => {
                            return Err(RadarError::ParseJson(format!("Ignored path '{}'", path)));
                        }
                    }
                } else {
                    log::trace!("parse_signalk: no path or value found in values");
                }
            }
        }
        Err(e) => {
            log::warn!("Unable to parse SK message '{}'", s);
            return Err(RadarError::ParseJson(e.to_string()));
        }
    }
    return Err(RadarError::ParseJson(format!(
        "Insufficient fields in '{}'",
        s
    )));
}

async fn connect_to_socket(address: SocketAddr) -> Result<TcpStream, RadarError> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| RadarError::Io(e))?;
    log::debug!("Connected to {}", address);
    Ok(stream)
}

///
/// Take an interable of SocketAddr and return a TCP stream to the first socket that connects.
///
async fn connect_first<I>(addresses: I) -> Result<TcpStream, RadarError>
where
    I: IntoIterator<Item = SocketAddr>,
{
    // Create a collection of connection futures
    // Since the life time of the stream must outlive this function,
    // and we create async closures on the stack, we must add a lot
    // of syntactic sugar so the compiler doesn't grumble.
    // Future<....> says that it is async, e.g. first call returns a future.
    // It resolves to Output = ... and is Send.
    // Box<> places this on the heap, not stack.
    // Pin<> makes sure it doesn't move or get invalid as an object.
    // Vec<> so we can store a list of these.
    let futures: Vec<Pin<Box<dyn Future<Output = Result<TcpStream, RadarError>> + Send>>> =
        addresses
            .into_iter()
            .map(|address| {
                log::debug!("Connecting to {}", address);
                Box::pin(connect_to_socket(address)) as Pin<Box<dyn Future<Output = _> + Send>>
            })
            .collect();

    // Use select_ok to return the first successful connection
    match select_ok(futures).await {
        Ok((stream, _)) => {
            log::debug!("First successful connection: {:?}", stream);
            Ok(stream)
        }
        Err(e) => {
            log::debug!("All connections failed: {}", e);
            Err(e)
        }
    }
}
