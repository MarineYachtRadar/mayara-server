//! AIS vessel tracking and storage module
//!
//! This module maintains a store of AIS vessels received from Signal K,
//! accumulating data over time and broadcasting updates to WebSocket clients.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::stream::SignalKDelta;

/// Timeout after which a vessel is marked as "Lost" (3 minutes)
const AIS_TIMEOUT: Duration = Duration::from_secs(180);

/// GPS position
#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
}

/// Vessel dimensions and GPS antenna offset
#[derive(Clone, Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Dimensions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beam: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_bow: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_center: Option<f64>,
}

impl Dimensions {
    /// Returns true if at least one dimension field is known
    fn has_any(&self) -> bool {
        self.length.is_some()
            || self.beam.is_some()
            || self.from_bow.is_some()
            || self.from_center.is_some()
    }
}

/// AIS vessel data accumulated over time
#[derive(Clone, Debug)]
pub struct AisVessel {
    pub mmsi: String,
    pub name: Option<String>,
    pub position: Option<Position>,
    dimensions: Dimensions,
    pub heading: Option<f64>,
    pub cog: Option<f64>,
    pub sog: Option<f64>,
    pub status: String,
    last_update: Instant,
}

/// Serializable view of AisVessel (only includes dimensions when known)
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AisVesselApi {
    pub mmsi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Dimensions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cog: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sog: Option<f64>,
    pub status: String,
}

impl From<&AisVessel> for AisVesselApi {
    fn from(vessel: &AisVessel) -> Self {
        AisVesselApi {
            mmsi: vessel.mmsi.clone(),
            name: vessel.name.clone(),
            position: vessel.position.clone(),
            dimensions: if vessel.dimensions.has_any() {
                Some(vessel.dimensions.clone())
            } else {
                None
            },
            heading: vessel.heading,
            cog: vessel.cog,
            sog: vessel.sog,
            status: vessel.status.clone(),
        }
    }
}

impl AisVessel {
    fn new(mmsi: String) -> Self {
        AisVessel {
            mmsi,
            name: None,
            position: None,
            dimensions: Dimensions::default(),
            heading: None,
            cog: None,
            sog: None,
            status: "Active".to_string(),
            last_update: Instant::now(),
        }
    }

    /// Update vessel fields from Signal K update values
    /// Returns true if any field changed
    fn update_from_signalk(&mut self, updates: &Value) -> bool {
        let mut changed = false;
        self.last_update = Instant::now();

        if let Some(updates_array) = updates.as_array() {
            for update in updates_array {
                if let Some(values) = update.get("values").and_then(|v| v.as_array()) {
                    for val in values {
                        let path = val.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let value = &val["value"];

                        match path {
                            "navigation.position" => {
                                if let (Some(lat), Some(lon)) = (
                                    value.get("latitude").and_then(|v| v.as_f64()),
                                    value.get("longitude").and_then(|v| v.as_f64()),
                                ) {
                                    let new_pos = Some(Position {
                                        latitude: lat,
                                        longitude: lon,
                                    });
                                    if self.position != new_pos {
                                        self.position = new_pos;
                                        changed = true;
                                    }
                                }
                            }
                            "navigation.headingTrue" => {
                                if let Some(heading) = value.as_f64()
                                    && self.heading != Some(heading)
                                {
                                    self.heading = Some(heading);
                                    changed = true;
                                }
                            }
                            "navigation.courseOverGroundTrue" => {
                                if let Some(cog) = value.as_f64()
                                    && self.cog != Some(cog)
                                {
                                    self.cog = Some(cog);
                                    changed = true;
                                }
                            }
                            "navigation.speedOverGround" => {
                                if let Some(sog) = value.as_f64()
                                    && self.sog != Some(sog)
                                {
                                    self.sog = Some(sog);
                                    changed = true;
                                }
                            }
                            "design.length" => {
                                if let Some(length) = value.get("overall").and_then(|v| v.as_f64())
                                    && self.dimensions.length != Some(length)
                                {
                                    self.dimensions.length = Some(length);
                                    changed = true;
                                }
                            }
                            "design.beam" => {
                                if let Some(beam) = value.as_f64()
                                    && self.dimensions.beam != Some(beam)
                                {
                                    self.dimensions.beam = Some(beam);
                                    changed = true;
                                }
                            }
                            "sensors.ais.fromBow" => {
                                if let Some(from_bow) = value.as_f64()
                                    && self.dimensions.from_bow != Some(from_bow)
                                {
                                    self.dimensions.from_bow = Some(from_bow);
                                    changed = true;
                                }
                            }
                            "sensors.ais.fromCenter" => {
                                if let Some(from_center) = value.as_f64()
                                    && self.dimensions.from_center != Some(from_center)
                                {
                                    self.dimensions.from_center = Some(from_center);
                                    changed = true;
                                }
                            }
                            "" => {
                                // Empty path contains vessel name and other metadata
                                if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                                    let new_name = Some(name.to_string());
                                    if self.name != new_name {
                                        self.name = new_name;
                                        changed = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        changed
    }
}

/// Delay before broadcasting vessel updates to coalesce rapid updates
const BROADCAST_DELAY: Duration = Duration::from_millis(100);

/// Store for AIS vessels, indexed by MMSI
pub struct AisVesselStore {
    vessels: RwLock<HashMap<String, AisVessel>>,
    /// Vessels pending broadcast (MMSI -> scheduled broadcast time)
    pending_broadcast: RwLock<HashMap<String, Instant>>,
    broadcast_tx: broadcast::Sender<SignalKDelta>,
}

impl AisVesselStore {
    pub fn new(tx: broadcast::Sender<SignalKDelta>) -> Arc<Self> {
        Arc::new(Self {
            vessels: RwLock::new(HashMap::new()),
            pending_broadcast: RwLock::new(HashMap::new()),
            broadcast_tx: tx,
        })
    }

    /// Remove a vessel from the store. Used when the own-ship context is
    /// established after the REST seed has already loaded the operator's
    /// own boat as if it were an AIS target — otherwise the PPI overlay
    /// would render own-ship on top of itself.
    pub fn evict(&self, context: &str) -> bool {
        let Some(mmsi) = Self::extract_mmsi(context) else {
            return false;
        };
        let Ok(mut vessels) = self.vessels.write() else {
            return false;
        };
        let removed = vessels.remove(&mmsi).is_some();
        if removed {
            log::info!("Evicted MMSI {} from AIS store (own ship)", mmsi);
        }
        removed
    }

    /// Extract MMSI from context like "vessels.urn:mrn:imo:mmsi:227334400"
    fn extract_mmsi(context: &str) -> Option<String> {
        // Look for pattern "mmsi:" followed by digits
        if let Some(idx) = context.find("mmsi:") {
            let start = idx + 5;
            let mmsi: String = context[start..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !mmsi.is_empty() {
                return Some(mmsi);
            }
        }
        None
    }

    /// Update vessel data from Signal K message
    /// Returns true if the vessel data changed
    pub fn update(&self, context: &str, updates: &Value) -> bool {
        let mmsi = match Self::extract_mmsi(context) {
            Some(m) => m,
            None => {
                log::debug!("Could not extract MMSI from context: {}", context);
                return false;
            }
        };

        let mut vessels = match self.vessels.write() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let vessel = vessels
            .entry(mmsi.clone())
            .or_insert_with(|| AisVessel::new(mmsi.clone()));

        let changed = vessel.update_from_signalk(updates);

        if changed {
            // Schedule a delayed broadcast instead of broadcasting immediately
            self.schedule_broadcast(&mmsi);
        }

        changed
    }

    /// Schedule a vessel for delayed broadcast
    /// If already scheduled, keep the existing schedule time to coalesce updates
    fn schedule_broadcast(&self, mmsi: &str) {
        if let Ok(mut pending) = self.pending_broadcast.write() {
            // Only schedule if not already pending
            pending
                .entry(mmsi.to_string())
                .or_insert_with(|| Instant::now() + BROADCAST_DELAY);
        }
    }

    /// Flush pending broadcasts that are due
    /// Returns the number of vessels broadcast
    pub fn flush_pending_broadcasts(&self) -> usize {
        let now = Instant::now();
        let mut to_broadcast = Vec::new();

        // Find vessels due for broadcast
        {
            let pending = match self.pending_broadcast.read() {
                Ok(p) => p,
                Err(_) => return 0,
            };
            for (mmsi, scheduled_time) in pending.iter() {
                if now >= *scheduled_time {
                    to_broadcast.push(mmsi.clone());
                }
            }
        }

        if to_broadcast.is_empty() {
            return 0;
        }

        // Remove from pending and broadcast
        {
            let mut pending = match self.pending_broadcast.write() {
                Ok(p) => p,
                Err(_) => return 0,
            };
            for mmsi in &to_broadcast {
                pending.remove(mmsi);
            }
        }

        // Broadcast vessels
        let vessels = match self.vessels.read() {
            Ok(v) => v,
            Err(_) => return 0,
        };
        for mmsi in &to_broadcast {
            if let Some(vessel) = vessels.get(mmsi) {
                self.broadcast_vessel(vessel);
            }
        }

        to_broadcast.len()
    }

    /// Get all active vessels (for initial connection)
    pub fn get_all_active(&self) -> Vec<AisVesselApi> {
        let vessels = match self.vessels.read() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        vessels
            .values()
            .filter(|v| v.status == "Active")
            .map(AisVesselApi::from)
            .collect()
    }

    /// Check for timed-out vessels, mark as Lost and broadcast
    /// Returns the number of vessels marked as Lost
    pub fn check_timeouts(&self) -> usize {
        let now = Instant::now();
        let mut lost_vessels = Vec::new();

        {
            let vessels = match self.vessels.read() {
                Ok(v) => v,
                Err(_) => return 0,
            };
            for (mmsi, vessel) in vessels.iter() {
                if vessel.status == "Active" && now.duration_since(vessel.last_update) > AIS_TIMEOUT
                {
                    lost_vessels.push(mmsi.clone());
                }
            }
        }

        if !lost_vessels.is_empty() {
            let mut vessels = match self.vessels.write() {
                Ok(v) => v,
                Err(_) => return 0,
            };
            for mmsi in &lost_vessels {
                if let Some(vessel) = vessels.get_mut(mmsi) {
                    vessel.status = "Lost".to_string();
                    // Lost status is broadcast immediately (no delay needed)
                    self.broadcast_vessel(vessel);
                }
            }
            // Remove lost vessels from the store
            for mmsi in &lost_vessels {
                vessels.remove(mmsi);
            }
        }

        lost_vessels.len()
    }

    /// Broadcast a vessel update to all connected clients
    fn broadcast_vessel(&self, vessel: &AisVessel) {
        let api = AisVesselApi::from(vessel);
        let path = format!("vessels.{}", vessel.mmsi);

        let delta = SignalKDelta::for_ais_vessel(&path, &api);

        let _ = self.broadcast_tx.send(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_mmsi() {
        assert_eq!(
            AisVesselStore::extract_mmsi("vessels.urn:mrn:imo:mmsi:227334400"),
            Some("227334400".to_string())
        );
        assert_eq!(
            AisVesselStore::extract_mmsi("vessels.urn:mrn:imo:mmsi:538011344"),
            Some("538011344".to_string())
        );
        assert_eq!(AisVesselStore::extract_mmsi("vessels.self"), None);
        assert_eq!(AisVesselStore::extract_mmsi("invalid"), None);
    }

    #[test]
    fn test_extract_mmsi_edge_cases() {
        // Empty string
        assert_eq!(AisVesselStore::extract_mmsi(""), None);
        // mmsi: without digits
        assert_eq!(AisVesselStore::extract_mmsi("mmsi:"), None);
        // mmsi: with non-digit suffix
        assert_eq!(AisVesselStore::extract_mmsi("mmsi:abc"), None);
        // Multiple mmsi: patterns (takes first)
        assert_eq!(
            AisVesselStore::extract_mmsi("mmsi:123mmsi:456"),
            Some("123".to_string())
        );
    }

    #[test]
    fn test_dimensions_has_any() {
        let empty = Dimensions::default();
        assert!(!empty.has_any());

        let with_length = Dimensions {
            length: Some(12.0),
            ..Default::default()
        };
        assert!(with_length.has_any());

        let with_beam = Dimensions {
            beam: Some(4.0),
            ..Default::default()
        };
        assert!(with_beam.has_any());

        let with_from_bow = Dimensions {
            from_bow: Some(7.0),
            ..Default::default()
        };
        assert!(with_from_bow.has_any());

        let with_from_center = Dimensions {
            from_center: Some(-1.0),
            ..Default::default()
        };
        assert!(with_from_center.has_any());

        let full = Dimensions {
            length: Some(12.0),
            beam: Some(4.0),
            from_bow: Some(7.0),
            from_center: Some(-1.0),
        };
        assert!(full.has_any());
    }

    #[test]
    fn test_ais_vessel_new() {
        let vessel = AisVessel::new("123456789".to_string());
        assert_eq!(vessel.mmsi, "123456789");
        assert!(vessel.name.is_none());
        assert!(vessel.position.is_none());
        assert!(vessel.cog.is_none());
        assert!(vessel.sog.is_none());
        assert_eq!(vessel.status, "Active");
        assert!(!vessel.dimensions.has_any());
    }

    #[test]
    fn test_update_from_signalk_position() {
        let mut vessel = AisVessel::new("123456789".to_string());
        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {
                    "latitude": 52.3676,
                    "longitude": 4.9041
                }
            }]
        }]);

        let changed = vessel.update_from_signalk(&updates);
        assert!(changed);
        assert_eq!(
            vessel.position,
            Some(Position {
                latitude: 52.3676,
                longitude: 4.9041
            })
        );

        // Same position should not report change
        let changed = vessel.update_from_signalk(&updates);
        assert!(!changed);
    }

    #[test]
    fn test_update_from_signalk_cog_sog() {
        let mut vessel = AisVessel::new("123456789".to_string());
        let updates = json!([{
            "values": [
                {"path": "navigation.courseOverGroundTrue", "value": 1.5},
                {"path": "navigation.speedOverGround", "value": 5.14}
            ]
        }]);

        let changed = vessel.update_from_signalk(&updates);
        assert!(changed);
        assert_eq!(vessel.cog, Some(1.5));
        assert_eq!(vessel.sog, Some(5.14));
    }

    #[test]
    fn test_update_from_signalk_dimensions() {
        let mut vessel = AisVessel::new("123456789".to_string());
        let updates = json!([{
            "values": [
                {"path": "design.length", "value": {"overall": 25.0}},
                {"path": "design.beam", "value": 6.0},
                {"path": "sensors.ais.fromBow", "value": 15.0},
                {"path": "sensors.ais.fromCenter", "value": -1.5}
            ]
        }]);

        let changed = vessel.update_from_signalk(&updates);
        assert!(changed);
        assert_eq!(vessel.dimensions.length, Some(25.0));
        assert_eq!(vessel.dimensions.beam, Some(6.0));
        assert_eq!(vessel.dimensions.from_bow, Some(15.0));
        assert_eq!(vessel.dimensions.from_center, Some(-1.5));
    }

    #[test]
    fn test_update_from_signalk_name() {
        let mut vessel = AisVessel::new("123456789".to_string());
        let updates = json!([{
            "values": [{
                "path": "",
                "value": {"name": "TEST VESSEL"}
            }]
        }]);

        let changed = vessel.update_from_signalk(&updates);
        assert!(changed);
        assert_eq!(vessel.name, Some("TEST VESSEL".to_string()));

        // Same name should not report change
        let changed = vessel.update_from_signalk(&updates);
        assert!(!changed);
    }

    #[test]
    fn test_update_from_signalk_accumulates_data() {
        let mut vessel = AisVessel::new("123456789".to_string());

        // First update: position only
        let updates1 = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);
        vessel.update_from_signalk(&updates1);

        // Second update: name only
        let updates2 = json!([{
            "values": [{
                "path": "",
                "value": {"name": "MY VESSEL"}
            }]
        }]);
        vessel.update_from_signalk(&updates2);

        // Both should be present
        assert_eq!(
            vessel.position,
            Some(Position {
                latitude: 52.0,
                longitude: 4.0
            })
        );
        assert_eq!(vessel.name, Some("MY VESSEL".to_string()));
    }

    #[test]
    fn test_update_from_signalk_invalid_data() {
        let mut vessel = AisVessel::new("123456789".to_string());

        // Empty updates array
        let updates = json!([]);
        let changed = vessel.update_from_signalk(&updates);
        assert!(!changed);

        // Missing values key
        let updates = json!([{"foo": "bar"}]);
        let changed = vessel.update_from_signalk(&updates);
        assert!(!changed);

        // Invalid position (missing latitude)
        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"longitude": 4.0}
            }]
        }]);
        let changed = vessel.update_from_signalk(&updates);
        assert!(!changed);
        assert!(vessel.position.is_none());
    }

    #[test]
    fn test_ais_vessel_api_from_vessel() {
        let mut vessel = AisVessel::new("123456789".to_string());
        vessel.name = Some("TEST".to_string());
        vessel.position = Some(Position {
            latitude: 52.0,
            longitude: 4.0,
        });
        vessel.cog = Some(1.5);
        vessel.sog = Some(5.0);

        let api = AisVesselApi::from(&vessel);
        assert_eq!(api.mmsi, "123456789");
        assert_eq!(api.name, Some("TEST".to_string()));
        assert_eq!(
            api.position,
            Some(Position {
                latitude: 52.0,
                longitude: 4.0
            })
        );
        assert_eq!(api.cog, Some(1.5));
        assert_eq!(api.sog, Some(5.0));
        assert_eq!(api.status, "Active");
        // No dimensions set, so should be None
        assert!(api.dimensions.is_none());
    }

    #[test]
    fn test_ais_vessel_api_includes_dimensions_when_set() {
        let mut vessel = AisVessel::new("123456789".to_string());
        vessel.dimensions.length = Some(25.0);

        let api = AisVesselApi::from(&vessel);
        assert!(api.dimensions.is_some());
        assert_eq!(api.dimensions.as_ref().unwrap().length, Some(25.0));
    }

    #[test]
    fn test_ais_vessel_api_serialization() {
        let mut vessel = AisVessel::new("227334400".to_string());
        vessel.name = Some("DOMICIL".to_string());
        vessel.position = Some(Position {
            latitude: 9.51919,
            longitude: -78.64894,
        });
        vessel.cog = Some(3.632);
        vessel.sog = Some(0.0);
        vessel.dimensions.length = Some(12.0);
        vessel.dimensions.beam = Some(4.0);

        let api = AisVesselApi::from(&vessel);
        let json = serde_json::to_value(&api).unwrap();

        assert_eq!(json["mmsi"], "227334400");
        assert_eq!(json["name"], "DOMICIL");
        assert_eq!(json["position"]["latitude"], 9.51919);
        assert_eq!(json["position"]["longitude"], -78.64894);
        assert_eq!(json["cog"], 3.632);
        assert_eq!(json["sog"], 0.0);
        assert_eq!(json["status"], "Active");
        assert_eq!(json["dimensions"]["length"], 12.0);
        assert_eq!(json["dimensions"]["beam"], 4.0);
        // camelCase serialization
        assert!(json.get("fromBow").is_none()); // Should be in dimensions
    }

    #[test]
    fn test_ais_vessel_api_omits_none_fields() {
        let vessel = AisVessel::new("123456789".to_string());
        let api = AisVesselApi::from(&vessel);
        let json = serde_json::to_value(&api).unwrap();

        // These should not be present in JSON
        assert!(json.get("name").is_none());
        assert!(json.get("position").is_none());
        assert!(json.get("dimensions").is_none());
        assert!(json.get("cog").is_none());
        assert!(json.get("sog").is_none());
        // These should always be present
        assert!(json.get("mmsi").is_some());
        assert!(json.get("status").is_some());
    }

    #[test]
    fn test_store_update_creates_vessel() {
        let (tx, _rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        let changed = store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);
        assert!(changed);

        let active = store.get_all_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].mmsi, "123456789");
    }

    #[test]
    fn test_store_update_invalid_context() {
        let (tx, _rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        // vessels.self has no MMSI
        let changed = store.update("vessels.self", &updates);
        assert!(!changed);

        let active = store.get_all_active();
        assert!(active.is_empty());
    }

    #[test]
    fn test_store_get_all_active_filters_lost() {
        let (tx, _rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        // Add two vessels
        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);
        store.update("vessels.urn:mrn:imo:mmsi:111111111", &updates);
        store.update("vessels.urn:mrn:imo:mmsi:222222222", &updates);

        // Mark one as lost manually
        {
            let mut vessels = store.vessels.write().unwrap();
            if let Some(v) = vessels.get_mut("111111111") {
                v.status = "Lost".to_string();
            }
        }

        let active = store.get_all_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].mmsi, "222222222");
    }

    #[test]
    fn test_store_schedules_broadcast_on_change() {
        let (tx, _rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        let changed = store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);
        assert!(changed);

        // Verify vessel is scheduled for broadcast
        let pending = store.pending_broadcast.read().unwrap();
        assert!(pending.contains_key("123456789"));
    }

    #[test]
    fn test_store_flush_broadcasts_after_delay() {
        let (tx, mut rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);

        // Before flush, nothing should be broadcast yet
        assert!(rx.try_recv().is_err());

        // Manually set the scheduled time to the past to simulate delay elapsed
        {
            let mut pending = store.pending_broadcast.write().unwrap();
            if let Some(time) = pending.get_mut("123456789") {
                *time = Instant::now() - Duration::from_millis(1);
            }
        }

        // Now flush should broadcast
        let count = store.flush_pending_broadcasts();
        assert_eq!(count, 1);

        // Should have received a broadcast
        let delta = rx.try_recv();
        assert!(delta.is_ok());
    }

    #[test]
    fn test_store_coalesces_rapid_updates() {
        let (tx, mut rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        // Send three rapid updates for the same vessel
        let updates1 = json!([{
            "values": [{
                "path": "navigation.speedOverGround",
                "value": 5.0
            }]
        }]);
        let updates2 = json!([{
            "values": [{
                "path": "navigation.courseOverGroundTrue",
                "value": 1.5
            }]
        }]);
        let updates3 = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates1);
        store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates2);
        store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates3);

        // Only one pending broadcast should be scheduled
        let pending = store.pending_broadcast.read().unwrap();
        assert_eq!(pending.len(), 1);
        drop(pending);

        // Simulate delay elapsed
        {
            let mut pending = store.pending_broadcast.write().unwrap();
            if let Some(time) = pending.get_mut("123456789") {
                *time = Instant::now() - Duration::from_millis(1);
            }
        }

        // Flush should broadcast once with all accumulated data
        let count = store.flush_pending_broadcasts();
        assert_eq!(count, 1);

        // Verify only one message was broadcast
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err()); // No more messages
    }

    #[test]
    fn test_store_no_broadcast_when_unchanged() {
        let (tx, _rx) = broadcast::channel(16);
        let store = AisVesselStore::new(tx);

        let updates = json!([{
            "values": [{
                "path": "navigation.position",
                "value": {"latitude": 52.0, "longitude": 4.0}
            }]
        }]);

        // First update schedules broadcast
        store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);

        // Clear pending to simulate broadcast happened
        {
            let mut pending = store.pending_broadcast.write().unwrap();
            pending.clear();
        }

        // Same update should not schedule another broadcast
        let changed = store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);
        assert!(!changed);

        let pending = store.pending_broadcast.read().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_position_equality() {
        let pos1 = Position {
            latitude: 52.0,
            longitude: 4.0,
        };
        let pos2 = Position {
            latitude: 52.0,
            longitude: 4.0,
        };
        let pos3 = Position {
            latitude: 52.1,
            longitude: 4.0,
        };

        assert_eq!(pos1, pos2);
        assert_ne!(pos1, pos3);
    }

    #[test]
    fn test_dimensions_equality() {
        let dim1 = Dimensions {
            length: Some(25.0),
            beam: Some(6.0),
            from_bow: None,
            from_center: None,
        };
        let dim2 = Dimensions {
            length: Some(25.0),
            beam: Some(6.0),
            from_bow: None,
            from_center: None,
        };
        let dim3 = Dimensions {
            length: Some(30.0),
            beam: Some(6.0),
            from_bow: None,
            from_center: None,
        };

        assert_eq!(dim1, dim2);
        assert_ne!(dim1, dim3);
    }
}
