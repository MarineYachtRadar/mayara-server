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
use tokio::net::UdpSocket;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
#[cfg(feature = "pcap-replay")]
use std::path::Path;
#[cfg(feature = "pcap-replay")]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "pcap-replay")]
use std::time::Duration;

use tokio::sync::mpsc;
#[cfg(feature = "pcap-replay")]
use tokio::time::sleep;

#[cfg(feature = "pcap-replay")]
use crate::pcap::{self, PcapPacket};

/// A socket that can receive UDP packets from either a real network
/// socket or from the pcap replay dispatcher. Receivers use this
/// instead of `tokio::net::UdpSocket` directly.
pub(crate) enum RadarSocket {
    /// Real network UDP socket.
    Udp(UdpSocket),
    /// Pcap replay channel.
    Replay(ReplayReceiver),
}

impl RadarSocket {
    /// Receive a packet, matching the `UdpSocket::recv_buf_from` API.
    pub async fn recv_buf_from(
        &mut self,
        buf: &mut Vec<u8>,
    ) -> io::Result<(usize, SocketAddr)> {
        match self {
            RadarSocket::Udp(sock) => sock.recv_buf_from(buf).await,
            RadarSocket::Replay(rx) => rx.recv_buf_from(buf).await,
        }
    }
}


/// A packet received from replay, including the original source address.
#[derive(Debug, Clone)]
pub(crate) struct ReplayPacket {
    pub data: Vec<u8>,
    pub from: SocketAddrV4,
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
    channels: Mutex<HashMap<SocketAddrV4, Vec<mpsc::Sender<ReplayPacket>>>>,
}

/// Initialize the replay system with a pcap/nnd file. Called once at startup.
#[cfg(feature = "pcap-replay")]
pub fn init(path: &Path) -> io::Result<()> {
    let packets = pcap::parse_file(path)?;
    log::info!(
        "Replay: loaded {} UDP packets from {}",
        packets.len(),
        path.display()
    );
    REPLAY
        .set(Arc::new(ReplayState {
            packets,
            channels: Mutex::new(HashMap::new()),
        }))
        .map_err(|_| {
            io::Error::new(io::ErrorKind::AlreadyExists, "replay already initialized")
        })?;
    Ok(())
}

/// Returns true if pcap replay is active.
pub(crate) fn is_active() -> bool {
    #[cfg(feature = "pcap-replay")]
    { REPLAY.get().is_some() }
    #[cfg(not(feature = "pcap-replay"))]
    { false }
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
pub async fn run(realistic_timing: bool, repeat: bool) {
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

    let realistic_timing = realistic_timing
        && !INSTANT_TIMING.load(std::sync::atomic::Ordering::Relaxed);

    log::info!(
        "Replay: starting dispatcher ({} packets, timing={}, repeat={})",
        state.packets.len(),
        if realistic_timing { "realistic" } else { "instant" },
        repeat,
    );

    let mut first_pass = true;
    loop {
        let listeners_before: usize = state
            .channels
            .lock()
            .unwrap()
            .values()
            .map(|v| v.len())
            .sum();
        let mut prev_ts = Duration::ZERO;
        let mut sent = 0u64;
        let mut unrouted = 0u64;

        for pkt in &state.packets {
            if realistic_timing && pkt.timestamp > prev_ts {
                let delay = pkt.timestamp - prev_ts;
                sleep(delay).await;
            }
            prev_ts = pkt.timestamp;

            let channels = state.channels.lock().unwrap();
            if let Some(senders) = channels.get(&pkt.dst_addr) {
                let replay_pkt = ReplayPacket {
                    data: pkt.payload.clone(),
                    from: pkt.src_addr,
                };
                for tx in senders {
                    let _ = tx.try_send(replay_pkt.clone());
                }
                sent += 1;
            } else {
                unrouted += 1;
            }
        }

        log::info!(
            "Replay: dispatched {} packets ({} unrouted)",
            sent,
            unrouted
        );

        // After the first pass, wait for new listeners that may have
        // been registered by report receivers created during discovery,
        // then re-send so they get state reports (0xC403, 0xC409, etc.).
        if first_pass {
            first_pass = false;
            let wait_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                let listeners_now: usize = state
                    .channels
                    .lock()
                    .unwrap()
                    .values()
                    .map(|v| v.len())
                    .sum();
                if listeners_now > listeners_before {
                    // Give a short grace period for remaining listeners
                    sleep(Duration::from_millis(50)).await;
                    let listeners_final: usize = state
                        .channels
                        .lock()
                        .unwrap()
                        .values()
                        .map(|v| v.len())
                        .sum();
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
            let listeners_now = state.channels.lock().unwrap().len();
            if listeners_now > listeners_before {
                continue;
            }
        }

        if !repeat {
            break;
        }
        log::info!("Replay: restarting from beginning");
    }

    // Keep running so the program doesn't exit immediately
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
