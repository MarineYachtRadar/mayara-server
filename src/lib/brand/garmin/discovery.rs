//! Garmin CDM "V2 heartbeat" discovery (`0x038e`).
//!
//! Every Garmin marine device — including the radar — broadcasts a
//! 34-byte CDM heartbeat to multicast `239.254.2.2:50050` every 5
//! seconds. The body identifies the device by `product_id` and lists
//! the services it offers. We use it for two things:
//!
//! 1. **Identification.** The radar's `product_id` maps to a known
//!    model name (e.g. `0x06d0` → "GMR xHD"), which becomes the
//!    radar's display name in the API.
//! 2. **Stable serial.** The 16-bit product_id is the only stable
//!    identifier the protocol exposes; we hand it to `RadarInfo` so
//!    multi-radar setups have distinct keys.
//!
//! See `research/garmin/discovery-handshake.md` for the wire format
//! Garmin calls this the "V2 heartbeat" internally.
//!
//! ## Wire format
//!
//! ```text
//! Offset  Size  Field                Sample
//! +00     1     version_marker       0x02 (V2)
//! +01     1     padding              0x00
//! +02     2     product_id (LE)      0x06d0 (1744 = GMR xHD)
//! +04     1     simulator_mode       0x00
//! +05     1     product_subtype      0x05
//! +06     1     syc_group_id         0x02
//! +07     1     constant             0x01
//! +08     1     service_count        0x01
//! +09     3     padding              0x00 0x00 0x00
//! +0c     8*N   service_id_array     [class:1][inst:0][ver:2][rsv:0]+u32 id
//!         var   serialized tail      uptime/sequence counter
//! ```

#![allow(dead_code)]

use super::protocol::{
    CDM_OFFSET_PRODUCT_ID, CDM_OFFSET_PRODUCT_SUBTYPE, CDM_OFFSET_SIMULATOR_MODE,
    CDM_OFFSET_VERSION_MARKER,
};

/// Minimum number of bytes the CDM heartbeat body must contain (i.e.
/// after the 8-byte GMN header has been stripped) for us to extract
/// the product_id and subtype.
const MIN_CDM_BODY_LEN: usize = 12;

/// Decoded fields from a `0x038e` CDM heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CdmHeartbeat {
    /// `version_marker` byte. Should always be `2` for V2 heartbeats.
    pub version: u8,
    /// 16-bit product identifier. Maps to a model via [`product_name`].
    pub product_id: u16,
    /// `simulator_mode` byte (0 = real radar, non-zero = various
    /// simulator/replay modes).
    pub simulator_mode: u8,
    /// `product_subtype` byte (e.g. 5 for the captured GMR xHD).
    pub product_subtype: u8,
}

/// Parse the body of a `0x038e` heartbeat. `payload` must be the slice
/// **after** the 8-byte GMN header. Returns `None` if the body is too
/// short or has the wrong version marker.
pub(crate) fn parse(payload: &[u8]) -> Option<CdmHeartbeat> {
    if payload.len() < MIN_CDM_BODY_LEN {
        return None;
    }
    let version = payload[CDM_OFFSET_VERSION_MARKER];
    // Garmin only emits version 2 heartbeats. We accept it
    // strictly so a stray packet with the same multicast address but
    // a different format doesn't poison our state.
    if version != 2 {
        return None;
    }
    let product_id = u16::from_le_bytes(
        payload[CDM_OFFSET_PRODUCT_ID..CDM_OFFSET_PRODUCT_ID + 2]
            .try_into()
            .ok()?,
    );
    Some(CdmHeartbeat {
        version,
        product_id,
        simulator_mode: payload[CDM_OFFSET_SIMULATOR_MODE],
        product_subtype: payload[CDM_OFFSET_PRODUCT_SUBTYPE],
    })
}

/// Minimum body length for a `0x0392` product data response to contain
/// the device_name (30 bytes at +0x04) and device_alias (31 bytes at +0x23).
const MIN_PRODUCT_DATA_LEN: usize = 0x42;

/// `0x0392` — CDM product data response.
pub(crate) const MSG_CDM_PRODUCT_DATA: u32 = 0x0392;

/// `0x0391` — CDM product data request. Sent as a GMN packet to the
/// radar's IP on port 50050 to solicit a `0x0392` response containing
/// the factory model name and user-customizable alias.
pub(crate) const MSG_CDM_PRODUCT_DATA_REQUEST: u32 = 0x0391;

/// Decoded fields from a `0x0392` product data response.
#[derive(Debug, Clone)]
pub(crate) struct CdmProductData {
    /// Factory model name (e.g. "GMR Fantom 24"), up to 30 chars.
    pub device_name: String,
    /// User-customizable alias (e.g. "Bow Radar"), up to 31 chars.
    pub device_alias: String,
}

/// Parse the body of a `0x0392` product data response. `payload` is the
/// slice after the 8-byte GMN header.
pub(crate) fn parse_product_data(payload: &[u8]) -> Option<CdmProductData> {
    if payload.len() < MIN_PRODUCT_DATA_LEN {
        return None;
    }
    let device_name = crate::util::c_string(&payload[0x04..0x04 + 30])?;
    let device_alias = crate::util::c_string(&payload[0x23..0x23 + 31])?;
    Some(CdmProductData {
        device_name: device_name.to_string(),
        device_alias: device_alias.to_string(),
    })
}

/// Build a minimal `0x0391` request packet (just the 8-byte GMN header,
/// no payload). The radar responds with a `0x0392` on the same port.
pub(crate) fn build_product_data_request() -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&MSG_CDM_PRODUCT_DATA_REQUEST.to_le_bytes());
    // payload_len = 0
    buf
}

/// `0x0393` — Set device alias. The MFD sends this to rename a device
/// on the Garmin Marine Network. Payload: 30-byte alias string, NUL-padded.
/// Sent to the device's IP on the CDM control port (50051).
pub(crate) const MSG_CDM_SET_ALIAS: u32 = 0x0393;

/// CDM control port used for alias-set and other CDM write operations.
pub(crate) const CDM_CONTROL_PORT: u16 = 50051;

/// Build a `0x0393` set-alias packet. `alias` is truncated to 30 bytes
/// and NUL-padded.
pub(crate) fn build_set_alias(alias: &str) -> Vec<u8> {
    let alias_bytes = alias.as_bytes();
    let copy_len = alias_bytes.len().min(30);
    let payload_len: u32 = 32; // 30 chars + 2 padding/null bytes
    let mut buf = vec![0u8; 8 + payload_len as usize];
    buf[0..4].copy_from_slice(&MSG_CDM_SET_ALIAS.to_le_bytes());
    buf[4..8].copy_from_slice(&payload_len.to_le_bytes());
    buf[8..8 + copy_len].copy_from_slice(&alias_bytes[..copy_len]);
    // Rest is already zeroed (NUL padding)
    buf
}

/// Map a Garmin marine `product_id` to a human-readable model name.
/// Sourced from `research/garmin/radar-detection.md`. Returns `None`
/// for unknown IDs (the caller should
/// fall back to a generic "Garmin xHD" / "Garmin HD" label).
pub(crate) fn product_name(product_id: u16) -> Option<&'static str> {
    Some(match product_id {
        0x010f => "GMR 18",
        0x0195 => "GMR 24 HD",
        0x01fd => "GMR 18 HD",
        0x021d => "GMR (legacy)",
        0x0263 => "GMR (legacy)",
        0x06d0 => "GMR xHD",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CDM heartbeat body captured from a GMR xHD radar in
    /// `radar-recordings/garmin/garmin_xhd.pcap`. Sourced from
    /// `research/garmin/discovery-handshake.md:74`.
    const SAMPLE_BODY: [u8; 26] = [
        0x02, 0x00, // version=2, padding
        0xd0, 0x06, // product_id = 0x06d0
        0x00, // simulator_mode = 0
        0x05, // product_subtype = 5
        0x02, // syc_group_id = 2
        0x01, // constant
        0x01, 0x00, 0x00, 0x00, // service_count + padding
        0x01, 0x00, 0x02, 0x00, // service class/inst/version/reserved
        0xa0, 0x0a, 0xd4, 0x08, // service_id = 0x08d40aa0
        0x01, 0x04, 0x9b, 0x05, 0x00, 0x00, // tail (sequence)
    ];

    #[test]
    fn parse_captured_xhd_heartbeat() {
        let hb = parse(&SAMPLE_BODY).expect("should parse");
        assert_eq!(hb.version, 2);
        assert_eq!(hb.product_id, 0x06d0);
        assert_eq!(hb.simulator_mode, 0);
        assert_eq!(hb.product_subtype, 5);
        assert_eq!(product_name(hb.product_id), Some("GMR xHD"));
    }

    #[test]
    fn parse_returns_none_on_short_body() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 11]).is_none());
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let mut body = SAMPLE_BODY;
        body[CDM_OFFSET_VERSION_MARKER] = 1;
        assert!(parse(&body).is_none());
    }

    #[test]
    fn product_name_known_models() {
        assert_eq!(product_name(0x06d0), Some("GMR xHD"));
        assert_eq!(product_name(0x01fd), Some("GMR 18 HD"));
        assert_eq!(product_name(0x0195), Some("GMR 24 HD"));
        assert_eq!(product_name(0x9999), None);
    }
}
