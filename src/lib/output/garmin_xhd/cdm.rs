//! Discovery heartbeat of the emulated xHD radar (`239.254.2.2:50050`).
//!
//! Every Garmin marine device announces itself with a CDM "V2 heartbeat", and
//! a plotter only offers a radar it has heard one from. The emulated radar
//! announces itself as a GMR xHD, quickly at first so it shows up on a plotter
//! that was already switched on, then at the leisurely rate a real one uses.

use std::net::Ipv4Addr;
use std::time::Duration;

use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::garmin::protocol::{CDM_HEARTBEAT_ADDRESS, MSG_CDM_HEARTBEAT};
use crate::radar::RadarError;

use super::multicast_send;

/// Product id of the GMR xHD, which is what makes a plotter treat the
/// announcement as coming from a radar it knows how to drive.
const PRODUCT_ID_XHD: u16 = 0x06d0;

/// Remaining identity fields, as a real GMR xHD sends them.
const PRODUCT_SUBTYPE: u8 = 5;
const SYC_GROUP_ID: u8 = 6;
const SERVICE_CLASS: u8 = 1;
const SERVICE_INSTANCE: u8 = 0;
const SERVICE_VERSION: u16 = 2;
const SERVICE_ID: u32 = 0x08d4_0aa0;

/// `version_marker`: this is a V2 heartbeat, the only version mayara's own
/// discovery accepts and the only one seen in captures.
const VERSION_MARKER: u8 = 2;

/// `simulator_mode`: zero for a real radar rather than a simulated one. The
/// emulated radar claims to be real, because to the plotter it is.
const SIMULATOR_MODE: u8 = 0;

/// Byte at +07, constant in every capture and of unknown meaning.
const CONSTANT_07: u8 = 1;

/// The announcement lists the services the device offers; a radar offers one.
const SERVICE_COUNT: u8 = 1;

/// Tag and length of the sequence counter in the serialized tail.
const TAIL_TAG_SEQUENCE: u8 = 1;
const TAIL_LEN_SEQUENCE: u8 = 4;

/// Padding runs: one byte after the version marker, three before the service
/// entries.
const PAD_AFTER_VERSION: [u8; 1] = [0];
const PAD_BEFORE_SERVICES: [u8; 3] = [0; 3];

/// Heartbeats sent at the fast rate before settling into the slow one.
const FAST_HEARTBEATS: u32 = 30;
const FAST_INTERVAL: Duration = Duration::from_secs(1);
const SLOW_INTERVAL: Duration = Duration::from_secs(5);

/// Build the `0x038e` announcement. The layout is documented in
/// `research/garmin/discovery-handshake.md` and in
/// `brand::garmin::discovery`, which parses it.
fn heartbeat(sequence: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(26);
    body.push(VERSION_MARKER);
    body.extend_from_slice(&PAD_AFTER_VERSION);
    body.extend_from_slice(&PRODUCT_ID_XHD.to_le_bytes());
    body.push(SIMULATOR_MODE);
    body.push(PRODUCT_SUBTYPE);
    body.push(SYC_GROUP_ID);
    body.push(CONSTANT_07);
    body.push(SERVICE_COUNT);
    body.extend_from_slice(&PAD_BEFORE_SERVICES);

    body.push(SERVICE_CLASS);
    body.push(SERVICE_INSTANCE);
    body.extend_from_slice(&SERVICE_VERSION.to_le_bytes());
    body.extend_from_slice(&SERVICE_ID.to_le_bytes());

    // Serialized tail: one tagged field holding the sequence counter.
    body.push(TAIL_TAG_SEQUENCE);
    body.push(TAIL_LEN_SEQUENCE);
    body.extend_from_slice(&sequence.to_le_bytes());

    super::packet(MSG_CDM_HEARTBEAT, &body)
}

pub(super) async fn run(
    local_addr: Ipv4Addr,
    subsys: &mut SubsystemHandle,
) -> Result<(), RadarError> {
    let socket = match multicast_send(&CDM_HEARTBEAT_ADDRESS, local_addr) {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("Garmin xHD heartbeat: cannot open socket: {e}");
            return Ok(());
        }
    };

    let mut sequence: u32 = 0;
    loop {
        if let Err(e) = socket.send(&heartbeat(sequence)).await {
            log::warn!("Garmin xHD heartbeat: send failed: {e}");
        }
        sequence = sequence.wrapping_add(1);

        let interval = if sequence < FAST_HEARTBEATS {
            FAST_INTERVAL
        } else {
            SLOW_INTERVAL
        };

        tokio::select! {
            biased;
            _ = subsys.on_shutdown_requested() => return Ok(()),
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brand::garmin::discovery;

    #[test]
    fn heartbeat_identifies_the_radar_as_a_gmr_xhd() {
        let packet = heartbeat(7);
        assert_eq!(
            u32::from_le_bytes(packet[0..4].try_into().unwrap()),
            MSG_CDM_HEARTBEAT
        );

        let parsed = discovery::parse(&packet[8..]).expect("valid heartbeat");
        assert_eq!(parsed.product_id, PRODUCT_ID_XHD);
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.simulator_mode, 0);
        assert_eq!(parsed.product_subtype, PRODUCT_SUBTYPE);
        assert_eq!(discovery::product_name(parsed.product_id), Some("GMR xHD"));
    }
}
