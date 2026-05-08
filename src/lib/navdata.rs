use atomic_float::AtomicF64;
use futures_util::{SinkExt, StreamExt, future::select_ok};
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
use tokio_tungstenite::tungstenite::Message;

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

/// Clear the own-ship context. Called on every Signal K reconnection so a
/// roam to a different server (with a different vessel URN) cannot silently
/// misroute AIS traffic to the stale context.
fn reset_own_ship_context() {
    if let Some(lock) = OWN_SHIP_CONTEXT.get() {
        if let Ok(mut guard) = lock.write() {
            if guard.is_some() {
                log::debug!("Clearing own-ship context on reconnect");
                *guard = None;
            }
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

const NMEA0183_SERVICE_NAME: &str = "_nmea-0183._tcp.local.";

/// Subscription for own-ship navigation data only
const SUBSCRIBE_SELF: &'static str = "{\"context\":\"vessels.self\",\"subscribe\":[{\"path\":\"navigation.headingTrue\"},{\"path\":\"navigation.position\"},{\"path\":\"navigation.speedOverGround\"},{\"path\":\"navigation.courseOverGroundTrue\"}]}\r\n";

/// Additional subscription for all vessels (sent after own-ship context is known)
const SUBSCRIBE_ALL: &'static str =
    "{\"context\":\"vessels.*\",\"subscribe\":[{\"path\":\"*\"}]}\r\n";

/// A Signal K subscription the transport layer should send in response to an
/// incoming message. Both TCP and WebSocket receive loops share this decision
/// logic via `NavigationData::signalk_actions_for_line`.
#[derive(Clone, Copy, Debug)]
enum SignalKSubscription {
    /// Initial own-ship subscription; sent once we receive the hello message.
    OwnShip,
    /// All-vessels subscription for AIS forwarding; sent once we know the
    /// own-ship context (when `--pass-ais` is set).
    AllVessels,
}

impl SignalKSubscription {
    fn payload(self) -> &'static str {
        match self {
            Self::OwnShip => SUBSCRIBE_SELF,
            Self::AllVessels => SUBSCRIBE_ALL,
        }
    }
}

enum ConnectionType {
    Mdns,
    Udp(SocketAddr),
    Tcp(SocketAddr),
    /// WebSocket connection; bool indicates TLS (wss) vs plain (ws)
    Ws(SocketAddr, bool),
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
                        match parts[0].to_ascii_lowercase().as_str() {
                            "udp" => return ConnectionType::Udp(addr),
                            "tcp" => return ConnectionType::Tcp(addr),
                            "ws" => return ConnectionType::Ws(addr, false),
                            "wss" => return ConnectionType::Ws(addr, true),
                            _ => {} // fallthrough to panic below
                        }
                    }
                }
            }
        }
        panic!(
            "Interface must be either interface name (no :) or <connection>:<address>:<port> with <connection> one of `udp`, `tcp`, `ws` or `wss`."
        );
    }
}

use crate::signalk::WsStream;

enum Stream {
    Tcp(TcpStream),
    Udp(UdpSocket),
    WebSocket(WsStream, String),
}

pub(crate) struct NavigationData {
    args: Cli,
    nmea0183_mode: bool,
    pass_ais: bool,
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
                what: "NMEA0183",
                nmea_parser: Some(NmeaParser::new()),
            },
            false => NavigationData {
                args,
                nmea0183_mode: false,
                pass_ais,
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
            // Clear per-session state so a reconnect to a different Signal K
            // server cannot inherit a stale own-ship URN from the previous one.
            reset_own_ship_context();

            match self
                .find_service(&subsys, &mut rx_ip_change, &navigation_address)
                .await
            {
                Ok(Stream::Tcp(stream)) => {
                    log::info!(
                        "Listening to {} data via TCP from {}",
                        self.what,
                        stream
                            .peer_addr()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|_| "<unknown>".to_string())
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
                    log::info!(
                        "Listening to {} data via UDP from {}",
                        self.what,
                        socket
                            .local_addr()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|_| "<unknown>".to_string())
                    );
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
                Ok(Stream::WebSocket(ws, url)) => {
                    log::info!("Listening to {} data via WebSocket from {}", self.what, url);
                    match self.receive_ws_loop(ws, &subsys).await {
                        Err(RadarError::Shutdown) => {
                            log::debug!("{} receive_ws_loop shutdown", self.what);
                            return Ok(());
                        }
                        e => {
                            log::debug!(
                                "{} receive_ws_loop restart on result {:?}",
                                self.what,
                                e
                            );
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
            ConnectionType::Ws(addr, tls) => self.find_signalk_ws_service(subsys, addr, tls).await,
        }
    }

    async fn find_mdns_service(
        &self,
        subsys: &SubsystemHandle,
        rx_ip_change: &mut Receiver<()>,
        interface: &Option<String>,
    ) -> Result<Stream, RadarError> {
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

        if self.nmea0183_mode {
            return self
                .find_mdns_nmea0183(&mdns, subsys, rx_ip_change)
                .await;
        }

        let r = crate::signalk::find_mdns_service(
            &mdns,
            subsys,
            rx_ip_change,
            self.args.accept_invalid_certs,
        )
        .await
        .map(signalk_connection_to_stream);

        if let Ok(r3) = mdns.shutdown() {
            if let Ok(r3) = r3.recv() {
                log::debug!("mdns_shutdown: {:?}", r3);
            }
        }
        r
    }

    /// mDNS discovery for NMEA 0183 services (plain TCP)
    async fn find_mdns_nmea0183(
        &self,
        mdns: &ServiceDaemon,
        subsys: &SubsystemHandle,
        rx_ip_change: &mut Receiver<()>,
    ) -> Result<Stream, RadarError> {
        let mut known_addresses: HashSet<SocketAddr> = HashSet::new();
        let locator = mdns
            .browse(NMEA0183_SERVICE_NAME)
            .expect("Failed to browse for NMEA0183 service");

        log::debug!("NMEA0183 find_mdns_service (re)start");

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
                event = locator.recv_async() => {
                    match event {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            log::debug!("Resolved NMEA0183 service: {}", info.get_fullname());
                            let port = info.get_port();
                            for a in info.get_addresses() {
                                known_addresses.insert(SocketAddr::new(a.to_ip_addr(), port));
                            }
                        },
                        _ => {
                            continue;
                        }
                    }

                    match connect_first(known_addresses.clone()).await {
                        Ok(stream) => {
                            r = Ok(Stream::Tcp(stream));
                            break;
                        }
                        Err(_e) => {}
                    }
                },
            }
        }

        if let Ok(r3) = mdns.shutdown() {
            if let Ok(r3) = r3.recv() {
                log::debug!("mdns_shutdown: {:?}", r3);
            }
        }
        r
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

    async fn find_signalk_ws_service(
        &self,
        subsys: &SubsystemHandle,
        addr: SocketAddr,
        use_tls: bool,
    ) -> Result<Stream, RadarError> {
        crate::signalk::find_explicit_service(subsys, addr, use_tls, self.args.accept_invalid_certs)
            .await
            .map(signalk_connection_to_stream)
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
            stream
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "<unknown>".to_string())
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
                                if let Err(e) = self.parse_nmea0183(&line) {
                                    log::warn!("{}", e);
                                }
                            } else {
                                for action in self.signalk_actions_for_line(&line) {
                                    self.send_subscription_tcp(&mut write_half, action).await?;
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

    /// Process an incoming Signal K message and return the subscriptions the
    /// transport layer should send in response. Shared between the TCP and
    /// WebSocket receive loops so hello-detection and AIS-expansion logic
    /// lives in one place. Delta parsing side effects (heading, position,
    /// AIS updates, own-ship context) happen inside `parse_signalk`.
    fn signalk_actions_for_line(&self, line: &str) -> Vec<SignalKSubscription> {
        if line.starts_with("{\"name\":") {
            log::debug!("{} received hello, will subscribe", self.what);
            return vec![SignalKSubscription::OwnShip];
        }

        let had_own_ship = get_own_ship_context().is_some();

        if let Err(e) = parse_signalk(line, self.pass_ais) {
            log::trace!("{} parse error: {}", self.what, e);
        }

        if self.pass_ais && !had_own_ship && get_own_ship_context().is_some() {
            log::info!("Own-ship context established, expanding subscription to all vessels");
            return vec![SignalKSubscription::AllVessels];
        }

        Vec::new()
    }

    async fn send_subscription_tcp(
        &self,
        stream: &mut tokio::net::tcp::WriteHalf<'_>,
        subscription: SignalKSubscription,
    ) -> Result<(), RadarError> {
        let payload = subscription.payload();
        log::info!("Sending SignalK subscription: {}", payload.trim());
        stream
            .write_all(payload.as_bytes())
            .await
            .map_err(RadarError::Io)
    }

    async fn receive_ws_loop(
        &mut self,
        ws: WsStream,
        subsys: &SubsystemHandle,
    ) -> Result<(), RadarError> {
        log::info!("{} WebSocket receive_loop started", self.what);

        let (mut write, mut read) = ws.split();

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{} receive_ws_loop shutdown", self.what);
                    let _ = write.close().await;
                    return Ok(());
                },
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            log::trace!("{} <- {}", self.what, text);
                            for action in self.signalk_actions_for_line(&text) {
                                let payload = action.payload().trim();
                                log::info!("Sending SignalK subscription: {}", payload);
                                write
                                    .send(Message::Text(payload.into()))
                                    .await
                                    .map_err(|e| RadarError::SignalK(e.to_string()))?;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            log::warn!("{} WebSocket connection closed", self.what);
                            return Ok(());
                        }
                        Some(Ok(_)) => {
                            // Ignore binary, ping, pong frames
                        }
                        Some(Err(e)) => {
                            log::warn!("{} WebSocket error: {}", self.what, e);
                            return Err(RadarError::SignalK(e.to_string()));
                        }
                    }
                }
            }
        }
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
    let v: Value = serde_json::from_str(s).map_err(|e| {
        log::warn!("Unable to parse SK message '{}'", s);
        RadarError::ParseJson(e.to_string())
    })?;

    let context = v["context"].as_str();
    let updates = &v["updates"];

    // When pass_ais is enabled, handle own-ship detection and AIS forwarding.
    if pass_ais {
        if let Some(ctx) = context {
            let own_ship = get_own_ship_context();

            if own_ship.is_none() {
                // First message after subscribing to vessels.self establishes the
                // own-ship context. Continue processing this message as nav data.
                set_own_ship_context(ctx);
                log::info!("Own-ship context detected: {}", ctx);
            } else if let Some(own_ship_ctx) = own_ship {
                let is_own_ship = own_ship_ctx == ctx || ctx == "vessels.self";
                if !is_own_ship {
                    // This delta is for another vessel; route to AIS store.
                    update_ais_vessel(ctx, updates);
                    return Ok(());
                }
            }
        }
    }

    // Process own-ship navigation data. The Signal K delta format allows
    // multiple updates per message and multiple values per update; process
    // every one rather than just `updates[0].values[0]`.
    let Some(updates_array) = updates.as_array() else {
        return Ok(());
    };

    for update in updates_array {
        // Extract source: prefer $source, then source.label, then source.type.
        let source = update["$source"]
            .as_str()
            .or_else(|| update["source"]["label"].as_str())
            .or_else(|| update["source"]["type"].as_str())
            .unwrap_or("signalk");

        let Some(values_array) = update["values"].as_array() else {
            continue;
        };

        for values_entry in values_array {
            apply_signalk_value(values_entry, source);
        }
    }

    Ok(())
}

/// Apply a single `{path, value}` entry from a Signal K delta to the local
/// navigation state.
fn apply_signalk_value(values_entry: &Value, source: &str) {
    let Some(path) = values_entry["path"].as_str() else {
        return;
    };
    let value = &values_entry["value"];
    log::trace!("parse_signalk: path = '{}', value = {:?}", path, value);
    match path {
        "navigation.position" => {
            set_position(value["latitude"].as_f64(), value["longitude"].as_f64());
        }
        "navigation.headingTrue" => {
            set_heading_true(value.as_f64(), source);
        }
        "navigation.speedOverGround" => {
            set_sog(value.as_f64());
        }
        "navigation.courseOverGroundTrue" => {
            set_cog(value.as_f64());
        }
        _ => {
            log::trace!("Ignored path '{}'", path);
        }
    }
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

fn signalk_connection_to_stream(conn: crate::signalk::Connection) -> Stream {
    use crate::signalk::Connection;
    match conn {
        Connection::Tcp(s) => Stream::Tcp(s),
        Connection::WebSocket(ws, url) => Stream::WebSocket(ws, url),
    }
}
