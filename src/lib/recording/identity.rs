//! Persisted radar identity for `.mrr` recordings.
//!
//! Captures everything needed to reconstruct a `RadarInfo` whose legend and
//! brand-specific shape match the source radar's at record time. The recorder
//! serializes this struct into the capabilities JSON of the file; the player
//! deserializes it and replays the spoke stream against a faithful copy of
//! the original radar.

use serde::{Deserialize, Serialize};

use crate::TargetMode;
use crate::radar::RadarInfo;

/// Identity of the source radar at record time. Stored as the capabilities
/// JSON inside an `.mrr` file. Field set is the minimum required for the
/// player to reproduce the same `RadarInfo` shape — adding fields is
/// backwards-compatible (deserializer treats them as optional) but bumping
/// `MRR_VERSION` is required if old data becomes meaningless.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingIdentity {
    /// Numeric `Brand` id, mirroring `recorder::brand_to_id`. JSON-numeric
    /// rather than the brand's string form so we don't need a `Deserialize`
    /// impl on `Brand`.
    pub brand_id: u32,
    pub spokes_per_revolution: u16,
    pub max_spoke_len: u16,
    pub pixel_values: u8,
    /// Doppler intensity sub-levels per direction. 0 = no Doppler, 1 = single
    /// flat color (Navico), 4 = brightness gradient (Garmin), 16 = NXT bands.
    pub doppler_levels: u8,
    /// Whether the wire format encodes a rain Doppler class (Furuno NXT).
    pub has_rain_class: bool,
    /// Doppler enabled flag (separate from `doppler_levels` because
    /// `RadarInfo::doppler` and `doppler_levels` are independently set).
    pub doppler: bool,
    /// Target tracking mode at record time.
    pub targets: TargetMode,
    pub dual_range: bool,
    /// Dual-radar side identifier ("A" or "B" for Furuno NXT dual range,
    /// `None` for single-side units). Captured so the playback radar's key
    /// matches the source side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual: Option<String>,
    pub sparse_spokes: bool,
    pub stationary: bool,
    /// Source radar's serial number, if any. Used to keep the playback radar
    /// key visually identifiable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_no: Option<String>,
    /// User-defined name carried over so playback shows the same label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    /// Detected model name from the source radar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
}

impl RecordingIdentity {
    /// Snapshot the source radar's identity at record time.
    pub fn from_radar_info(info: &RadarInfo) -> Self {
        let user_name = {
            let name = info.controls.user_name();
            if name.is_empty() || name == info.key() {
                None
            } else {
                Some(name)
            }
        };
        let model_name = info.controls.model_name();
        Self {
            brand_id: crate::recording::recorder::brand_to_id(info.brand),
            spokes_per_revolution: info.spokes_per_revolution,
            max_spoke_len: info.max_spoke_len,
            pixel_values: info.pixel_values,
            doppler_levels: info.doppler_levels(),
            has_rain_class: info.has_rain_class(),
            doppler: info.doppler,
            targets: info.targets(),
            dual_range: info.dual_range,
            dual: info.dual.clone(),
            sparse_spokes: info.sparse_spokes,
            stationary: info.stationary,
            serial_no: info.serial_no.clone(),
            user_name,
            model_name,
        }
    }

    /// Serialize to JSON bytes for writing into the `.mrr` capabilities slot.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("RecordingIdentity always serializes")
    }

    /// Parse from the capabilities slot of an `.mrr` file.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> RecordingIdentity {
        RecordingIdentity {
            brand_id: 1,
            spokes_per_revolution: 8192,
            max_spoke_len: 1024,
            pixel_values: 252,
            doppler_levels: 16,
            has_rain_class: true,
            doppler: true,
            targets: TargetMode::Arpa,
            dual_range: true,
            dual: Some("A".to_string()),
            sparse_spokes: true,
            stationary: false,
            serial_no: Some("PB-fixture".to_string()),
            user_name: Some("Test Radar".to_string()),
            model_name: Some("DRS4D-NXT".to_string()),
        }
    }

    #[test]
    fn json_roundtrip_preserves_all_fields() {
        let original = sample_identity();
        let bytes = original.to_json();
        let decoded = RecordingIdentity::from_json(&bytes).unwrap();

        assert_eq!(decoded.brand_id, original.brand_id);
        assert_eq!(
            decoded.spokes_per_revolution,
            original.spokes_per_revolution
        );
        assert_eq!(decoded.max_spoke_len, original.max_spoke_len);
        assert_eq!(decoded.pixel_values, original.pixel_values);
        assert_eq!(decoded.doppler_levels, original.doppler_levels);
        assert_eq!(decoded.has_rain_class, original.has_rain_class);
        assert_eq!(decoded.doppler, original.doppler);
        assert_eq!(decoded.targets, original.targets);
        assert_eq!(decoded.dual_range, original.dual_range);
        assert_eq!(decoded.dual, original.dual);
        assert_eq!(decoded.sparse_spokes, original.sparse_spokes);
        assert_eq!(decoded.stationary, original.stationary);
        assert_eq!(decoded.serial_no, original.serial_no);
        assert_eq!(decoded.user_name, original.user_name);
        assert_eq!(decoded.model_name, original.model_name);
    }

    #[test]
    fn omitted_optional_fields_deserialize_as_none() {
        // An identity with all Option fields absent must round-trip cleanly.
        let mut identity = sample_identity();
        identity.dual = None;
        identity.serial_no = None;
        identity.user_name = None;
        identity.model_name = None;

        let bytes = identity.to_json();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // skip_serializing_if = "Option::is_none" must elide them entirely.
        assert!(json.get("dual").is_none());
        assert!(json.get("serialNo").is_none());
        assert!(json.get("userName").is_none());
        assert!(json.get("modelName").is_none());

        let decoded = RecordingIdentity::from_json(&bytes).unwrap();
        assert_eq!(decoded.dual, None);
        assert_eq!(decoded.serial_no, None);
        assert_eq!(decoded.user_name, None);
        assert_eq!(decoded.model_name, None);
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compat() {
        // Future versions may add fields; the current parser must still
        // accept them by ignoring extras rather than rejecting the file.
        let json = br#"{
            "brandId": 1,
            "spokesPerRevolution": 2048,
            "maxSpokeLen": 1024,
            "pixelValues": 64,
            "dopplerLevels": 0,
            "hasRainClass": false,
            "doppler": false,
            "targets": "none",
            "dualRange": false,
            "sparseSpokes": false,
            "stationary": false,
            "futureField": 42
        }"#;
        let decoded = RecordingIdentity::from_json(json).expect("should parse");
        assert_eq!(decoded.brand_id, 1);
        assert_eq!(decoded.spokes_per_revolution, 2048);
    }
}
