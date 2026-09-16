//! Raymarine NavDataMessage sender.
//!
//! The Quantum radar needs position/heading data from the MFD every
//! 100ms for Doppler processing and MARPA target tracking. Without
//! it, the radar cannot determine whether targets are approaching or
//! receding relative to the vessel's course.
//!
//! The message is 32 bytes: a 4-byte sub-ID, a 4-byte flags bitmask
//! indicating which fields are valid, then 6 × i32 navigation values.

use deku::DekuWrite;
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
pub(super) async fn run(subsys: &mut SubsystemHandle, socket: UdpSocket) -> Result<(), RadarError> {
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

/// The 32-byte message. A field the vessel has no value for is sent as zero
/// and its flag left clear, which is what `flags` is for.
#[derive(DekuWrite, Debug, Default, PartialEq)]
#[deku(endian = "little")]
struct NavDataMessage {
    sub_id: u32,
    flags: u32,
    heading: i32,
    /// Speed through water, which Signal K does not give us; always zero.
    stw: i32,
    cog: i32,
    sog: i32,
    latitude: i32,
    longitude: i32,
}

fn build_navdata_message() -> Vec<u8> {
    let mut msg = NavDataMessage {
        sub_id: protocol::NAVDATA_SUB_ID,
        ..Default::default()
    };

    if let Some(heading) = get_heading_true() {
        msg.flags |= FLAG_HEADING;
        msg.heading = radians_to_fixed(heading);
    }

    if let Some(cog) = get_cog() {
        msg.flags |= FLAG_COG;
        msg.cog = radians_to_fixed(cog);
    }

    if let Some(sog) = get_sog() {
        msg.flags |= FLAG_SOG;
        // SOG is float × 10, as i32 (m/s × 10)
        msg.sog = (sog * 10.0) as i32;
    }

    let (lat, lon) = get_position();
    if let (Some(lat), Some(lon)) = (lat, lon) {
        msg.flags |= FLAG_POSITION;
        // Lat/lon in fixed-point format — the research says "from
        // CLatLong, rounded" which is likely degrees × 1e7 (standard
        // marine fixed-point), but this needs verification.
        msg.latitude = (lat * 1e7) as i32;
        msg.longitude = (lon * 1e7) as i32;
    }

    crate::util::encode(&msg)
}

#[cfg(test)]
mod tests {
    use super::{
        FLAG_COG, FLAG_HEADING, FLAG_POSITION, FLAG_SOG, NavDataMessage, radians_to_fixed,
    };
    use crate::util::encode;

    const NAVDATA_MESSAGE_LENGTH: usize = 32;

    /// Nothing pinned this message's bytes before. The radar reads the flags
    /// word to decide which of the six values to believe, so the flags and
    /// the values have to agree about where they are.
    #[test]
    fn navdata_message_layout() {
        let msg = NavDataMessage {
            sub_id: 0x2800_0018,
            flags: FLAG_HEADING | FLAG_COG | FLAG_SOG | FLAG_POSITION,
            heading: radians_to_fixed(std::f64::consts::PI),
            stw: 0,
            cog: 1000,
            sog: 55,
            latitude: 523_456_789,
            longitude: -43_210_987,
        };

        let bytes = encode(&msg);

        assert_eq!(bytes.len(), NAVDATA_MESSAGE_LENGTH);
        assert_eq!(bytes[0..4], 0x2800_0018u32.to_le_bytes());
        assert_eq!(bytes[4..8], 0x1du32.to_le_bytes()); // heading|cog|sog|position
        assert_eq!(bytes[8..12], 31415i32.to_le_bytes()); // π in 0.0001 rad
        assert_eq!(bytes[12..16], 0i32.to_le_bytes()); // speed through water
        assert_eq!(bytes[16..20], 1000i32.to_le_bytes());
        assert_eq!(bytes[20..24], 55i32.to_le_bytes());
        assert_eq!(bytes[24..28], 523_456_789i32.to_le_bytes());
        assert_eq!(bytes[28..32], (-43_210_987i32).to_le_bytes());
    }

    /// A vessel with no data at all still sends a well-formed message: the
    /// radar is told that none of the values mean anything.
    #[test]
    fn navdata_message_without_any_data_flags_nothing() {
        let bytes = encode(&NavDataMessage {
            sub_id: 0x2800_0018,
            ..Default::default()
        });

        assert_eq!(bytes.len(), NAVDATA_MESSAGE_LENGTH);
        assert_eq!(bytes[4..8], 0u32.to_le_bytes());
        assert!(bytes[8..].iter().all(|&b| b == 0));
    }
}
