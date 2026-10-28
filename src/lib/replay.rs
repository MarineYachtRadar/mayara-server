//! Pcap replay infrastructure.
//!
//! When `--replay <file.pcap>` is specified, this module:
//! 1. Parses the pcap file into a list of UDP packets
//! 2. Provides a global dispatcher that routes packets to registered
//!    listeners by destination address
//! 3. Receivers use `replay::create_listen()` which returns an mpsc
//!    receiver instead of a real socket
//!
//! The dispatcher supports two timing modes:
//! - **Realistic**: packets are sent with the original timing from the
//!   pcap timestamps (for `--replay <file>` interactive use)
//! - **Instant**: all packets are sent as fast as possible (for tests)

#[cfg(feature = "pcap-replay")]
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
#[cfg(feature = "pcap-replay")]
use std::path::Path;
use std::sync::Arc;
#[cfg(feature = "pcap-replay")]
use std::sync::{Mutex, OnceLock};
#[cfg(feature = "pcap-replay")]
use std::time::Duration;
use tokio::net::UdpSocket;

use tokio::sync::mpsc;
#[cfg(feature = "pcap-replay")]
use tokio::time::sleep;

#[cfg(feature = "pcap-replay")]
use crate::pcap::{self, PcapPacket};

/// A socket that can receive UDP packets from either a real network
/// socket or from the pcap replay dispatcher. Receivers use this
/// instead of `tokio::net::UdpSocket` directly.
pub(crate) enum RadarSocket {
    /// Real network UDP socket — sole owner of its file descriptor. This
    /// is the normal multicast / broadcast / unicast listen path used by
    /// every brand.
    Udp(UdpSocket),
    /// Real network UDP socket shared with another sender (currently only
    /// the Raymarine MFD-as-WiFi-AP unicast topology, where the command
    /// sender and the report receiver have to hold the same connected
    /// socket so the radar's replies don't get stolen by a separate
    /// command socket bound to the same host:port).
    SharedUdp(Arc<UdpSocket>),
    /// Pcap replay channel.
    Replay(ReplayReceiver),
}

impl RadarSocket {
    /// Receive a packet, matching the `UdpSocket::recv_buf_from` API.
    pub async fn recv_buf_from(&mut self, buf: &mut Vec<u8>) -> io::Result<(usize, SocketAddr)> {
        match self {
            RadarSocket::Udp(sock) => sock.recv_buf_from(buf).await,
            RadarSocket::SharedUdp(sock) => sock.recv_buf_from(buf).await,
            RadarSocket::Replay(rx) => rx.recv_buf_from(buf).await,
        }
    }
}

/// A packet received from replay, including the original source address.
#[derive(Debug, Clone)]
pub(crate) struct ReplayPacket {
    pub data: Vec<u8>,
    pub from: SocketAddrV4,
    /// When the packet was captured, since the Unix epoch.
    #[cfg(feature = "pcap-replay")]
    pub timestamp: std::time::Duration,
}

/// A replay receiver that can be used in place of a UDP socket.
/// Wraps an mpsc receiver of `ReplayPacket`.
pub(crate) struct ReplayReceiver {
    rx: mpsc::Receiver<ReplayPacket>,
}

impl ReplayReceiver {
    /// Receive a packet, mimicking `UdpSocket::recv_buf_from`.
    /// Returns `(length, source_address)`.
    pub async fn recv_buf_from(&mut self, buf: &mut Vec<u8>) -> io::Result<(usize, SocketAddr)> {
        match self.rx.recv().await {
            Some(pkt) => {
                #[cfg(feature = "pcap-replay")]
                capture_clock::received(pkt.timestamp);
                let len = pkt.data.len();
                buf.extend_from_slice(&pkt.data);
                Ok((len, SocketAddr::V4(pkt.from)))
            }
            None => Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "replay channel closed",
            )),
        }
    }
}

#[cfg(feature = "pcap-replay")]
pub(crate) use capture_clock::capture_position;
#[cfg(feature = "pcap-replay")]
pub use capture_clock::{NavFix, capture_time_ms, release_replay, use_capture_clock};

/// Replays a capture on its own clock rather than the wall clock.
///
/// A tracker measures speed as distance over spoke time, and spoke time is
/// the wall clock, so a capture replayed faster than it was recorded makes
/// every target appear to move that much faster. With the capture clock on,
/// each spoke is stamped with the time its packet was captured, and own-ship
/// position follows the voyage the capture was recorded on, so a replay at
/// any speed tracks as the original did.
#[cfg(feature = "pcap-replay")]
mod capture_clock {
    use std::cell::Cell;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    /// One own-ship fix of the voyage a capture was recorded on.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct NavFix {
        /// Milliseconds since the Unix epoch.
        pub time_ms: u64,
        pub lat: f64,
        pub lon: f64,
        /// Course over ground, radians true.
        pub cog: f64,
        /// Speed over ground, m/s.
        pub sog: f64,
    }

    /// The voyage, in time order; set once the capture clock is on.
    static VOYAGE: OnceLock<Vec<NavFix>> = OnceLock::new();

    /// How much of the capture's start is replayed to discover the radars.
    static DISCOVERY: OnceLock<Duration> = OnceLock::new();

    /// Whether the caller has configured the radars and wants the capture.
    static RELEASED: AtomicBool = AtomicBool::new(false);

    /// When own-ship navigation was last moved along the voyage.
    static NAV_SET_MS: AtomicU64 = AtomicU64::new(0);

    /// Own ship moves less than a metre in this time at sailing speeds.
    const NAV_STEP_MS: u64 = 100;

    thread_local! {
        /// Capture time of the packet this thread received last. Receivers
        /// stamp a spoke synchronously after receiving its frame, so this is
        /// the frame's own time.
        static PACKET_TIME_MS: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// Stamps spokes with capture time and sails own ship along `voyage`,
    /// which may be empty when the capture holds its own navigation.
    ///
    /// Until [`release_replay`], only the first `discovery` of the capture
    /// plays, over and over: long enough for the radars to be found and to
    /// report what they are, and short enough to hold no spoke, so the
    /// caller can set their controls before any spoke is tracked. After it
    /// the capture plays once, from the start, so the clock never runs
    /// backwards while there are spokes.
    pub fn use_capture_clock(voyage: Vec<NavFix>, discovery: Duration) {
        let _ = VOYAGE.set(voyage);
        let _ = DISCOVERY.set(discovery);
    }

    /// Lets the capture play, once the radars are set up.
    pub fn release_replay() {
        RELEASED.store(true, Ordering::Release);
    }

    /// The discovery window, when the capture clock is on.
    pub(super) fn discovery() -> Option<Duration> {
        DISCOVERY.get().copied()
    }

    pub(super) fn released() -> bool {
        RELEASED.load(Ordering::Acquire)
    }

    /// The capture time of the packet being handled on this thread, when the
    /// capture clock is on.
    pub fn capture_time_ms() -> Option<u64> {
        VOYAGE.get()?;
        PACKET_TIME_MS.get()
    }

    /// Own ship when the packet being handled on this thread was captured.
    pub(crate) fn capture_position() -> Option<(f64, f64)> {
        let fix = fix_at(VOYAGE.get()?, PACKET_TIME_MS.get()?)?;
        Some((fix.lat, fix.lon))
    }

    pub(super) fn received(timestamp: Duration) {
        let Some(voyage) = VOYAGE.get() else {
            return;
        };
        let now = timestamp.as_millis() as u64;
        PACKET_TIME_MS.set(Some(now));
        let last = NAV_SET_MS.load(Ordering::Relaxed);
        if now.abs_diff(last) >= NAV_STEP_MS
            && NAV_SET_MS
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            && let Some(fix) = fix_at(voyage, now)
        {
            crate::navdata::set_position(Some(fix.lat), Some(fix.lon), "replay");
            crate::navdata::set_cog(Some(fix.cog));
            crate::navdata::set_sog(Some(fix.sog));
        }
    }

    /// Own ship at `time_ms`: position interpolated between the fixes either
    /// side, course and speed from the one before.
    pub(super) fn fix_at(voyage: &[NavFix], time_ms: u64) -> Option<NavFix> {
        let after = voyage.partition_point(|f| f.time_ms <= time_ms);
        let before = voyage.get(after.checked_sub(1)?)?;
        let Some(next) = voyage.get(after) else {
            return Some(*before);
        };
        let span = (next.time_ms - before.time_ms) as f64;
        let f = (time_ms - before.time_ms) as f64 / span;
        Some(NavFix {
            time_ms,
            lat: before.lat + (next.lat - before.lat) * f,
            lon: before.lon + (next.lon - before.lon) * f,
            ..*before
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn fix(time_ms: u64, lat: f64) -> NavFix {
            NavFix {
                time_ms,
                lat,
                lon: 177.0,
                cog: 1.0,
                sog: 2.5,
            }
        }

        #[test]
        fn own_ship_is_interpolated_between_fixes() {
            let voyage = [fix(1000, -17.0), fix(2000, -17.1)];
            assert_eq!(fix_at(&voyage, 500), None, "before the voyage");
            assert!((fix_at(&voyage, 1500).unwrap().lat + 17.05).abs() < 1e-12);
            assert_eq!(fix_at(&voyage, 2500).unwrap().lat, -17.1, "after it");
        }
    }
}

// --- pcap-replay feature: global state and init/run ---

#[cfg(feature = "pcap-replay")]
static REPLAY: OnceLock<Arc<ReplayState>> = OnceLock::new();

#[cfg(feature = "pcap-replay")]
static INSTANT_TIMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Force instant timing for replay. Call before `run()` in tests.
#[cfg(feature = "pcap-replay")]
pub fn set_instant_timing() {
    INSTANT_TIMING.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "pcap-replay")]
struct ReplayState {
    packets: Vec<PcapPacket>,
    /// When the first packet was captured, since the Unix epoch.
    start: Duration,
    channels: Mutex<HashMap<SocketAddrV4, Vec<mpsc::Sender<ReplayPacket>>>>,
}

#[cfg(feature = "pcap-replay")]
impl ReplayState {
    /// How many listeners are registered, across every address.
    ///
    /// Counting addresses instead — `channels.len()` — misses a listener that
    /// joins a group another one already holds, and that is the ordinary case:
    /// discovery hands a radar the groups the locator is already listening on.
    fn listener_count(&self) -> usize {
        self.channels
            .lock()
            .unwrap()
            .values()
            .map(|v| v.len())
            .sum()
    }
}

/// Initialize the replay system with a pcap/nnd file. Called once at startup.
#[cfg(feature = "pcap-replay")]
pub fn init(path: &Path) -> io::Result<()> {
    let (packets, start) = pcap::parse_file_anchored(path)?;
    log::info!(
        "Replay: loaded {} UDP packets from {}",
        packets.len(),
        path.display()
    );
    REPLAY
        .set(Arc::new(ReplayState {
            packets,
            start,
            channels: Mutex::new(HashMap::new()),
        }))
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "replay already initialized"))?;
    Ok(())
}

/// Returns true if pcap replay is active.
pub(crate) fn is_active() -> bool {
    #[cfg(feature = "pcap-replay")]
    {
        REPLAY.get().is_some()
    }
    #[cfg(not(feature = "pcap-replay"))]
    {
        false
    }
}

/// Create a replay receiver for the given multicast/listen address.
/// Returns `None` if replay is not active.
pub(crate) fn create_listen(addr: &SocketAddrV4) -> Option<ReplayReceiver> {
    #[cfg(feature = "pcap-replay")]
    {
        let state = REPLAY.get()?;
        let (tx, rx) = mpsc::channel(512);
        state
            .channels
            .lock()
            .unwrap()
            .entry(*addr)
            .or_default()
            .push(tx);
        log::debug!("Replay: registered listener for {}", addr);
        Some(ReplayReceiver { rx })
    }
    #[cfg(not(feature = "pcap-replay"))]
    {
        let _ = addr;
        None
    }
}

#[cfg(feature = "pcap-replay")]
/// Start the replay dispatcher. Call this after all sockets/listeners
/// have been registered.
///
/// `realistic_timing`: if true, sleep between packets to match pcap
/// timestamps. If false, send all packets as fast as possible.
/// `repeat`: if true, loop the pcap file indefinitely.
/// `max_time`: if `Some(n)`, stop replay after `n` seconds of pcap
/// content (whichever comes first: max_time elapsed or end of file),
/// then return. Caller is expected to trigger shutdown.
pub async fn run(realistic_timing: bool, repeat: bool, max_time: Option<u32>) {
    let state = match REPLAY.get() {
        Some(s) => s.clone(),
        None => return,
    };

    // Wait for at least one listener to register before dispatching.
    // The Locator subsystem runs concurrently and registers channels
    // via create_listen() as it creates sockets.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !state.channels.lock().unwrap().is_empty() {
            // Give a short grace period for remaining listeners
            sleep(Duration::from_millis(50)).await;
            break;
        }
        if tokio::time::Instant::now() > deadline {
            log::warn!("Replay: no listeners registered after 5s, dispatching anyway");
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let realistic_timing =
        realistic_timing && !INSTANT_TIMING.load(std::sync::atomic::Ordering::Relaxed);

    if let Some(discovery) = capture_clock::discovery() {
        run_on_capture_clock(&state, discovery, realistic_timing).await;
        return;
    }

    log::info!(
        "Replay: starting dispatcher ({} packets, timing={}, repeat={})",
        state.packets.len(),
        if realistic_timing {
            "realistic"
        } else {
            "instant"
        },
        repeat,
    );

    let mut first_pass = true;
    loop {
        let listeners_before = state.listener_count();
        let mut prev_ts = Duration::ZERO;
        let mut sent = 0u64;
        let mut unrouted = 0u64;
        let mut dropped = 0u64;

        let first_ts = state
            .packets
            .first()
            .map(|p| p.timestamp)
            .unwrap_or_default();
        let max_pcap_ts = max_time.map(|n| first_ts + Duration::from_secs(n.into()));

        for pkt in &state.packets {
            if let Some(limit) = max_pcap_ts
                && pkt.timestamp > limit
            {
                log::info!("Replay: reached --pcap-max-time, stopping");
                break;
            }
            if realistic_timing && pkt.timestamp > prev_ts {
                let delay = pkt.timestamp - prev_ts;
                sleep(delay).await;
            } else {
                // Instant timing never awaits anything, so on a current-thread
                // runtime — which is what `#[tokio::test]` builds — the
                // dispatcher would hold the thread from the moment it starts
                // and starve every task waiting on what it sends.
                tokio::task::yield_now().await;
            }
            prev_ts = pkt.timestamp;

            let channels = state.channels.lock().unwrap();
            if let Some(senders) = channels.get(&pkt.dst_addr) {
                let replay_pkt = ReplayPacket {
                    data: pkt.payload.clone(),
                    from: pkt.src_addr,
                    timestamp: state.start + pkt.timestamp,
                };
                for tx in senders {
                    if tx.try_send(replay_pkt.clone()).is_err() {
                        dropped += 1;
                    }
                }
                sent += 1;
            } else {
                unrouted += 1;
            }
        }

        log::info!(
            "Replay: dispatched {} packets ({} unrouted, {} dropped by a full receiver)",
            sent,
            unrouted,
            dropped
        );

        // After the first pass, wait for new listeners that may have
        // been registered by report receivers created during discovery,
        // then re-send so they get state reports (0xC403, 0xC409, etc.).
        if first_pass {
            first_pass = false;
            let wait_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                let listeners_now = state.listener_count();
                if listeners_now > listeners_before {
                    // Give a short grace period for remaining listeners
                    sleep(Duration::from_millis(50)).await;
                    let listeners_final = state.listener_count();
                    log::info!(
                        "Replay: {} new listeners registered (total {}), re-sending",
                        listeners_final - listeners_before,
                        listeners_final,
                    );
                    break;
                }
                if tokio::time::Instant::now() > wait_deadline {
                    log::debug!(
                        "Replay: no new listeners after 2s (before={}, now={})",
                        listeners_before,
                        listeners_now,
                    );
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
            if state.listener_count() > listeners_before {
                continue;
            }
        }

        if !repeat {
            break;
        }
        log::info!("Replay: restarting from beginning");
    }

    if max_time.is_some() {
        // Give receivers a moment to process the last dispatched packets
        // before returning to the caller (which will request shutdown).
        sleep(Duration::from_secs(1)).await;
        return;
    }

    // Keep running so the program doesn't exit immediately
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

/// Plays the start of the capture until released, then all of it once.
#[cfg(feature = "pcap-replay")]
async fn run_on_capture_clock(state: &ReplayState, discovery: Duration, realistic_timing: bool) {
    let Some(first_ts) = state.packets.first().map(|p| p.timestamp) else {
        return;
    };
    let window = state
        .packets
        .partition_point(|p| p.timestamp < first_ts + discovery);
    log::info!(
        "Replay: discovering radars in the first {} packets of {}",
        window,
        state.packets.len()
    );
    while !capture_clock::released() {
        dispatch(state, &state.packets[..window], false).await;
        sleep(Duration::from_millis(20)).await;
    }
    log::info!("Replay: playing the capture on its own clock");
    dispatch(state, &state.packets, realistic_timing).await;
    // Give receivers a moment to process the last dispatched packets before
    // returning to the caller, which will request shutdown.
    sleep(Duration::from_secs(1)).await;
}

/// How long a full receiver may hold up the capture before its packet is
/// dropped: a listener nobody reads must not stall the replay for good.
#[cfg(feature = "pcap-replay")]
const RECEIVER_PATIENCE: Duration = Duration::from_secs(1);

/// Sends `packets` to their listeners, waiting for any that are behind: on
/// the capture clock a replay runs as fast as its receivers keep up, and
/// loses nothing. Drops are logged.
#[cfg(feature = "pcap-replay")]
async fn dispatch(state: &ReplayState, packets: &[PcapPacket], realistic_timing: bool) {
    let mut prev_ts = packets.first().map(|p| p.timestamp).unwrap_or_default();
    let mut dropped = 0u64;
    for pkt in packets {
        if realistic_timing && pkt.timestamp > prev_ts {
            sleep(pkt.timestamp - prev_ts).await;
        } else {
            tokio::task::yield_now().await;
        }
        prev_ts = pkt.timestamp;
        let senders = state.channels.lock().unwrap().get(&pkt.dst_addr).cloned();
        let Some(senders) = senders else {
            continue;
        };
        let replay_pkt = ReplayPacket {
            data: pkt.payload.clone(),
            from: pkt.src_addr,
            timestamp: state.start + pkt.timestamp,
        };
        for tx in senders.iter().filter(|tx| !tx.is_closed()) {
            let sent = tokio::time::timeout(RECEIVER_PATIENCE, tx.send(replay_pkt.clone())).await;
            if sent.is_err() {
                dropped += 1;
            }
        }
    }
    if dropped > 0 {
        log::warn!(
            "Replay: {} packets dropped by a receiver that fell behind",
            dropped
        );
    }
}

#[cfg(all(test, feature = "pcap-replay"))]
mod tests {
    use super::*;
    use std::fs;

    fn state_with(listeners: &[(&str, usize)]) -> ReplayState {
        let mut channels: HashMap<SocketAddrV4, Vec<mpsc::Sender<ReplayPacket>>> = HashMap::new();
        for (addr, count) in listeners {
            let senders = (0..*count).map(|_| mpsc::channel(1).0).collect();
            channels.insert(addr.parse().expect("an address"), senders);
        }
        ReplayState {
            packets: Vec::new(),
            start: Duration::ZERO,
            channels: Mutex::new(channels),
        }
    }

    /// Listeners are counted, not the addresses they listen on. Counting
    /// addresses is what the dispatcher used to compare against, and it misses
    /// the listener a radar registers on a group the locator already holds —
    /// so the re-send pass that exists to give that radar the reports and
    /// spokes replayed before it existed never happens.
    #[test]
    fn listeners_sharing_an_address_are_counted_separately() {
        let state = state_with(&[("236.6.7.5:6878", 3)]);
        assert_eq!(
            state.listener_count(),
            3,
            "three listeners on one group are three listeners"
        );
    }

    #[test]
    fn listeners_are_counted_across_addresses() {
        let state = state_with(&[("236.6.7.5:6878", 2), ("236.6.7.8:6679", 1)]);
        assert_eq!(state.listener_count(), 3);
    }

    #[test]
    fn no_listeners_is_none() {
        assert_eq!(state_with(&[]).listener_count(), 0);
    }

    /// End to end: a listener that joins a group another listener already
    /// holds must still get the capture replayed to it.
    ///
    /// This is the shape of a real replay session. The locator is listening on
    /// a group when the dispatcher starts; discovery then hands the radar the
    /// same group, and the radar's listener appears part way through. The
    /// second pass exists to give that listener what it missed, and counting
    /// addresses rather than listeners skipped it, because the group was
    /// already in the map.
    ///
    /// The only test in this binary that touches the replay global, which is
    /// set once per process.
    #[tokio::test]
    async fn a_listener_joining_a_known_group_still_receives_the_capture() {
        let group: SocketAddrV4 = "239.9.9.9:5555".parse().expect("an address");
        let dir = std::env::temp_dir().join(format!("mayara-replay-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        let capture = dir.join("capture.pcap");
        crate::pcap::write_file(
            &capture,
            &[PcapPacket {
                timestamp: Duration::ZERO,
                src_addr: "10.0.0.1:1234".parse().expect("an address"),
                dst_addr: group,
                payload: vec![0x01, 0x02, 0x03],
            }],
        )
        .expect("write capture");

        init(&capture).expect("init replay");
        set_instant_timing();

        // The locator, listening before the dispatcher starts.
        let _locator = create_listen(&group).expect("replay is active");
        tokio::spawn(run(false, false, None));

        // The radar's listener, arriving on the group the locator already
        // holds — after the first pass has been and gone.
        sleep(Duration::from_millis(100)).await;
        let mut radar = create_listen(&group).expect("replay is active");

        let got = tokio::time::timeout(Duration::from_secs(5), radar.rx.recv()).await;
        assert!(
            matches!(got, Ok(Some(_))),
            "the second pass never came: a listener joining a known group \
             received nothing"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
