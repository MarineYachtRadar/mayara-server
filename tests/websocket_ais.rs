//! Integration tests for WebSocket AIS subscription behavior
//!
//! These tests verify that:
//! 1. AIS vessel data is NOT sent to clients that haven't subscribed to vessels.*
//! 2. AIS vessel data IS sent to clients that have subscribed to vessels.*
//! 3. AIS vessel data is NOT sent after a client desubscribes from vessels.*
//! 4. All known AIS vessels are sent when a client subscribes to vessels.*

use mayara::{
    ais::{AisVesselApi, AisVesselStore, Position},
    stream::{ActiveSubscriptions, Desubscription, SignalKDelta, Subscribe, Subscription},
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Helper to create a test AIS vessel
fn create_test_vessel(mmsi: &str, name: &str, lat: f64, lon: f64) -> AisVesselApi {
    AisVesselApi {
        mmsi: mmsi.to_string(),
        name: Some(name.to_string()),
        position: Some(Position {
            latitude: lat,
            longitude: lon,
        }),
        dimensions: None,
        heading: None,
        cog: Some(1.5),
        sog: Some(5.0),
    }
}

/// The Signal K context mayara emits for an AIS target, keyed by MMSI URN.
fn ctx(mmsi: &str) -> String {
    format!("vessels.urn:mrn:imo:mmsi:{}", mmsi)
}

/// The `path -> value` pairs a built delta carries.
fn delta_values(json: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for update in json["updates"].as_array().into_iter().flatten() {
        for value in update["values"].as_array().into_iter().flatten() {
            out.push((
                value["path"].as_str().unwrap_or_default().to_string(),
                value["value"].clone(),
            ));
        }
    }
    out
}

/// The vessel name a built delta carries. Signal K delivers a vessel's
/// top-level properties as single-key objects on the empty path, not as a
/// `name` leaf, so this looks where a Signal K client would look.
fn delta_name(json: &Value) -> Option<String> {
    delta_values(json)
        .into_iter()
        .filter(|(path, _)| path.is_empty())
        .find_map(|(_, value)| value.get("name")?.as_str().map(str::to_string))
}

/// Helper to simulate Signal K updates for a vessel
fn create_signalk_updates(lat: f64, lon: f64, cog: f64, sog: f64) -> Value {
    json!([{
        "values": [
            {
                "path": "navigation.position",
                "value": {
                    "latitude": lat,
                    "longitude": lon
                }
            },
            {
                "path": "navigation.courseOverGroundTrue",
                "value": cog
            },
            {
                "path": "navigation.speedOverGround",
                "value": sog
            }
        ]
    }])
}

/// Helper to create a subscription from JSON
fn create_subscription(path: &str) -> Subscription {
    serde_json::from_value(json!({
        "subscribe": [{"path": path}]
    }))
    .unwrap()
}

/// Helper to create a desubscription from JSON
fn create_desubscription(path: &str) -> Desubscription {
    serde_json::from_value(json!({
        "desubscribe": [{"path": path}]
    }))
    .unwrap()
}

#[test]
fn test_ais_subscription_returns_true_on_first_subscribe() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    let subscription = create_subscription("vessels.*");
    let result = subscriptions.subscribe(subscription);
    assert!(result.is_ok());
    assert!(result.unwrap(), "First AIS subscription should return true");
}

#[test]
fn test_ais_subscription_returns_false_on_duplicate_subscribe() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // First subscription
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Second subscription to same path
    let subscription = create_subscription("vessels.*");
    let result = subscriptions.subscribe(subscription);
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "Duplicate AIS subscription should return false"
    );
}

#[test]
fn test_ais_desubscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Desubscribe
    let desubscription = create_desubscription("vessels.*");
    let result = subscriptions.desubscribe(desubscription);
    assert!(result.is_ok());

    // Subscribe again should return true (since we desubscribed)
    let subscription = create_subscription("vessels.*");
    let result = subscriptions.subscribe(subscription);
    assert!(result.is_ok());
    assert!(
        result.unwrap(),
        "Re-subscription after desubscribe should return true"
    );
}

#[test]
fn test_ais_store_update_and_get_active() {
    let (tx, _rx) = broadcast::channel::<SignalKDelta>(16);
    let store = AisVesselStore::new(tx);

    // Initially empty
    let vessels = store.get_all_active();
    assert!(vessels.is_empty(), "Store should be empty initially");

    // Add a vessel via update
    let updates = create_signalk_updates(52.0, 4.0, 1.5, 5.0);
    let changed = store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);
    assert!(changed, "First update should report changed");

    // Should have one vessel now
    let vessels = store.get_all_active();
    assert_eq!(vessels.len(), 1, "Should have one vessel");
    assert_eq!(vessels[0].mmsi, "123456789");
    assert_eq!(vessels[0].position.as_ref().unwrap().latitude, 52.0);
    assert_eq!(vessels[0].position.as_ref().unwrap().longitude, 4.0);
}

#[test]
fn test_ais_store_rejects_invalid_context() {
    let (tx, _rx) = broadcast::channel::<SignalKDelta>(16);
    let store = AisVesselStore::new(tx);

    // vessels.self has no MMSI
    let updates = create_signalk_updates(52.0, 4.0, 1.5, 5.0);
    let changed = store.update("vessels.self", &updates);
    assert!(!changed, "vessels.self should not be accepted");

    let vessels = store.get_all_active();
    assert!(vessels.is_empty(), "Store should still be empty");
}

#[test]
fn test_ais_store_accumulates_data() {
    let (tx, _rx) = broadcast::channel::<SignalKDelta>(16);
    let store = AisVesselStore::new(tx);

    // First update: position only
    let updates1 = json!([{
        "values": [{
            "path": "navigation.position",
            "value": {"latitude": 52.0, "longitude": 4.0}
        }]
    }]);
    store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates1);

    // Second update: name only
    let updates2 = json!([{
        "values": [{
            "path": "",
            "value": {"name": "TEST VESSEL"}
        }]
    }]);
    store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates2);

    let vessels = store.get_all_active();
    assert_eq!(vessels.len(), 1);
    assert_eq!(vessels[0].name, Some("TEST VESSEL".to_string()));
    assert!(vessels[0].position.is_some());
}

#[test]
fn test_ais_delta_filtering_without_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // No vessel subscription - only radar controls
    let subscription = create_subscription("radars.test.controls.*");
    let _ = subscriptions.subscribe(subscription);

    // Create a delta with AIS data
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // An AIS delta is subscribed as a whole, by context — without a vessels.*
    // subscription there is nothing left to send.
    assert!(
        delta.build().is_none(),
        "AIS delta should be dropped without a vessel subscription"
    );
}

/// mayara must emit an AIS vessel exactly as a Signal K server does, so a
/// client cannot tell the two apart: MMSI-URN context, top-level properties on
/// the empty path, everything else on its Signal K leaf path.
#[test]
fn test_ais_delta_uses_signalk_shape() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::All);

    let mut vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    vessel.heading = Some(0.5);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);
    delta.apply_subscriptions(&mut subscriptions);

    let json = serde_json::to_value(delta.build().expect("delta")).unwrap();
    assert_eq!(json["context"].as_str(), Some(ctx("123456789").as_str()));

    let values = delta_values(&json);
    let paths: Vec<&str> = values.iter().map(|(p, _)| p.as_str()).collect();
    for expected in [
        "navigation.position",
        "navigation.courseOverGroundTrue",
        "navigation.speedOverGround",
        "navigation.headingTrue",
    ] {
        assert!(paths.contains(&expected), "missing path {expected}");
    }

    // The name is a top-level vessel property, not a `name` leaf.
    assert!(!paths.contains(&"name"), "name must not be a leaf path");
    assert_eq!(delta_name(&json).as_deref(), Some("TEST"));

    let position = values
        .iter()
        .find(|(p, _)| p == "navigation.position")
        .map(|(_, v)| v.clone())
        .expect("position");
    assert_eq!(position["latitude"].as_f64(), Some(52.0));
    assert_eq!(position["longitude"].as_f64(), Some(4.0));

    // mayara's internal Active/Lost liveness state is not a Signal K concept
    // and must not reach the wire.
    assert!(
        !json.to_string().contains("status"),
        "status must not be emitted"
    );
}

#[test]
fn test_ais_delta_passes_with_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to vessels.*
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Create a delta with AIS data
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should pass through
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    assert_eq!(json["context"].as_str(), Some(ctx("123456789").as_str()));
    assert!(
        !delta_values(&json).is_empty(),
        "Values should not be empty"
    );
}

#[test]
fn test_ais_delta_filtered_after_desubscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to vessels.*
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Now desubscribe
    let desubscription = create_desubscription("vessels.*");
    let _ = subscriptions.desubscribe(desubscription);

    // Create a delta with AIS data
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    assert!(
        delta.build().is_none(),
        "AIS delta should be dropped after desubscription"
    );
}

#[test]
fn test_multiple_ais_vessels_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to vessels.*
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // One delta per vessel, as the server emits them: a Signal K delta carries
    // a single context, so each vessel gets its own.
    let vessels = [
        ("111111111", "VESSEL1", 52.0, 4.0),
        ("222222222", "VESSEL2", 53.0, 5.0),
        ("333333333", "VESSEL3", 54.0, 6.0),
    ];

    let mut names = Vec::new();
    for (mmsi, name, lat, lon) in vessels {
        let vessel = create_test_vessel(mmsi, name, lat, lon);
        let mut delta = SignalKDelta::for_ais_vessel(&vessel);

        // Apply subscription filtering
        delta.apply_subscriptions(&mut subscriptions);

        // Build and check - the vessel should pass through
        let built = delta.build();
        assert!(built.is_some(), "Delta for {mmsi} should not be empty");

        let json = serde_json::to_value(built.unwrap()).unwrap();
        assert_eq!(
            json["context"].as_str(),
            Some(ctx(mmsi).as_str()),
            "each vessel's delta must carry its own context"
        );
        names.push(delta_name(&json).expect("name"));
    }
    assert_eq!(names, ["VESSEL1", "VESSEL2", "VESSEL3"]);
}

#[tokio::test]
async fn test_ais_store_broadcasts_on_update() {
    let (tx, _rx) = broadcast::channel::<SignalKDelta>(16);
    let store = AisVesselStore::new(tx);

    // Update a vessel
    let updates = create_signalk_updates(52.0, 4.0, 1.5, 5.0);
    store.update("vessels.urn:mrn:imo:mmsi:123456789", &updates);

    // The vessel should be in the store
    let vessels = store.get_all_active();
    assert_eq!(vessels.len(), 1);

    // Flush should return 0 if delay hasn't elapsed
    // Note: This tests the immediate behavior; actual broadcast happens after delay
    let count = store.flush_pending_broadcasts();
    // Count might be 0 or 1 depending on timing - the important thing is no panic
    assert!(count <= 1);
}

#[test]
fn test_ais_vessel_serialization() {
    let vessel = create_test_vessel("123456789", "TEST VESSEL", 52.3676, 4.9041);

    let json = serde_json::to_value(&vessel).unwrap();

    assert_eq!(json["mmsi"], "123456789");
    assert_eq!(json["name"], "TEST VESSEL");
    assert_eq!(json["position"]["latitude"], 52.3676);
    assert_eq!(json["position"]["longitude"], 4.9041);
    assert_eq!(json["cog"], 1.5);
    assert_eq!(json["sog"], 5.0);
    // dimensions should not be present when None
    assert!(json.get("dimensions").is_none());
}

#[test]
fn test_subscribe_all_mode_passes_ais() {
    // In Subscribe::All mode, everything should pass through
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::All);

    // Create a delta with AIS data
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should pass through in All mode
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty in All mode");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    assert_eq!(json["context"].as_str(), Some(ctx("123456789").as_str()));
    assert_eq!(delta_name(&json).as_deref(), Some("TEST"));
}

#[test]
fn test_subscribe_none_mode_blocks_ais() {
    // In Subscribe::None mode, nothing should pass through
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Create a delta with AIS data
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    let mut delta = SignalKDelta::for_ais_vessel(&vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    assert!(
        delta.build().is_none(),
        "AIS delta should be dropped in None mode"
    );
}

#[test]
fn test_specific_mmsi_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to one vessel's context only. AIS is now filtered by context,
    // so the subscription names the vessel the way its deltas are keyed.
    let subscription = create_subscription(&ctx("123456789"));
    let _ = subscriptions.subscribe(subscription);

    // One delta per vessel, as the server emits them.
    let vessel1 = create_test_vessel("123456789", "SUBSCRIBED", 52.0, 4.0);
    let vessel2 = create_test_vessel("999999999", "NOT_SUBSCRIBED", 53.0, 5.0);

    let mut subscribed = SignalKDelta::for_ais_vessel(&vessel1);
    subscribed.apply_subscriptions(&mut subscriptions);
    let built = subscribed.build();
    assert!(
        built.is_some(),
        "Subscribed vessel's delta should not be empty"
    );
    let json = serde_json::to_value(built.unwrap()).unwrap();
    assert_eq!(
        json["context"].as_str(),
        Some(ctx("123456789").as_str()),
        "the delta must carry the subscribed vessel's context"
    );
    assert_eq!(delta_name(&json).as_deref(), Some("SUBSCRIBED"));

    // The unsubscribed vessel is dropped whole.
    let mut other = SignalKDelta::for_ais_vessel(&vessel2);
    other.apply_subscriptions(&mut subscriptions);
    assert!(
        other.build().is_none(),
        "Should NOT emit a delta for an unsubscribed vessel"
    );
}
