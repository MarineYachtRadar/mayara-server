//! Raymarine NavDataMessage sender.
//!
//! The Quantum radar needs position/heading data from the MFD every
//! 100ms for Doppler processing and MARPA target tracking. Without
//! it, the radar cannot determine whether targets are approaching or
//! receding relative to the vessel's course.
//!
//! The message is 32 bytes: a 4-byte sub-ID, a 4-byte flags bitmask
//! indicating which fields are valid, then 6 × i32 navigation values.

use std::f64::consts::TAU;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::navdata::{get_cog, get_heading_true, get_position, get_sog};
use crate::radar::RadarError;

use super::protocol;

/// NavData is sent every 100ms.
const NAVDATA_INTERVAL: Duration = Duration::from_millis(protocol::NAVDATA_INTERVAL_MS);

// Flags indicating which fields are valid
const FLAG_HEADING: u32 = 0x01;
// FLAG_STW (0x02) is not used — speed through water is not available from Signal K nav data
const FLAG_COG: u32 = 0x04;
const FLAG_SOG: u32 = 0x08;
const FLAG_POSITION: u32 = 0x10;

/// Convert radians to the Raymarine 0.0001-radian fixed-point format.
fn radians_to_fixed(rad: f64) -> i32 {
    // Normalize to [0, 2π)
    let mut r = rad % TAU;
    if r < 0.0 {
        r += TAU;
    }
    (r * 10000.0) as i32
}

/// Run the NavData sender loop. Sends position/heading to the radar
/// every 100ms for as long as the subsystem is alive.
pub async fn run(subsys: SubsystemHandle, socket: UdpSocket) -> Result<(), RadarError> {
    let mut deadline = Instant::now() + NAVDATA_INTERVAL;

    loop {
        tokio::select! {
            _ = subsys.on_shutdown_requested() => {
                return Ok(());
            }
            _ = sleep_until(deadline) => {
                let msg = build_navdata_message();
                let _ = socket.send(&msg).await;
                deadline += NAVDATA_INTERVAL;
            }
        }
    }
}

fn build_navdata_message() -> Vec<u8> {
    let mut buf = vec![0u8; 32];
    buf[0..4].copy_from_slice(&protocol::NAVDATA_SUB_ID.to_le_bytes());

    let mut flags: u32 = 0;

    if let Some(heading) = get_heading_true() {
        flags |= FLAG_HEADING;
        let val = radians_to_fixed(heading);
        buf[8..12].copy_from_slice(&val.to_le_bytes());
    }

    // STW not commonly available via Signal K; skip for now
    // flags |= FLAG_STW;
    // buf[12..16].copy_from_slice(&stw.to_le_bytes());

    if let Some(cog) = get_cog() {
        flags |= FLAG_COG;
        let val = radians_to_fixed(cog);
        buf[16..20].copy_from_slice(&val.to_le_bytes());
    }

    if let Some(sog) = get_sog() {
        flags |= FLAG_SOG;
        // SOG is float × 10, as i32 (m/s × 10)
        let val = (sog * 10.0) as i32;
        buf[20..24].copy_from_slice(&val.to_le_bytes());
    }

    let (lat, lon) = get_position();
    if let (Some(lat), Some(lon)) = (lat, lon) {
        flags |= FLAG_POSITION;
        // Lat/lon in fixed-point format — the research says "from
        // CLatLong, rounded" which is likely degrees × 1e7 (standard
        // marine fixed-point), but this needs verification.
        let lat_fixed = (lat * 1e7) as i32;
        let lon_fixed = (lon * 1e7) as i32;
        buf[24..28].copy_from_slice(&lat_fixed.to_le_bytes());
        buf[28..32].copy_from_slice(&lon_fixed.to_le_bytes());
    }

    buf[4..8].copy_from_slice(&flags.to_le_bytes());
    buf
}
