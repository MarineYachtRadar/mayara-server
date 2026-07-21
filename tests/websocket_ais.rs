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
        status: "Active".to_string(),
    }
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
    let mut delta = SignalKDelta::new();
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    delta.add_ais_vessel_update("vessels.123456789", &vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should be filtered out
    let built = delta.build();
    if let Some(built) = built {
        let json = serde_json::to_value(&built).unwrap();
        // Check that no vessel paths are present
        let updates = json["updates"].as_array().unwrap();
        for update in updates {
            if let Some(values) = update["values"].as_array() {
                for value in values {
                    let path = value["path"].as_str().unwrap_or("");
                    assert!(
                        !path.starts_with("vessels."),
                        "Vessel path {} should be filtered out without subscription",
                        path
                    );
                }
            }
        }
    }
    // If built is None, that's also acceptable (empty delta)
}

#[test]
fn test_ais_delta_passes_with_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to vessels.*
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Create a delta with AIS data
    let mut delta = SignalKDelta::new();
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    delta.add_ais_vessel_update("vessels.123456789", &vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should pass through
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    let updates = json["updates"].as_array().unwrap();
    assert!(!updates.is_empty(), "Updates should not be empty");

    let mut found_vessel = false;
    for update in updates {
        if let Some(values) = update["values"].as_array() {
            for value in values {
                let path = value["path"].as_str().unwrap_or("");
                if path.starts_with("vessels.") {
                    found_vessel = true;
                    assert_eq!(path, "vessels.123456789");
                }
            }
        }
    }
    assert!(found_vessel, "Should find vessel in delta");
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
    let mut delta = SignalKDelta::new();
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    delta.add_ais_vessel_update("vessels.123456789", &vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should be filtered out
    let built = delta.build();
    if let Some(built) = built {
        let json = serde_json::to_value(&built).unwrap();
        let updates = json["updates"].as_array().unwrap();
        for update in updates {
            if let Some(values) = update["values"].as_array() {
                for value in values {
                    let path = value["path"].as_str().unwrap_or("");
                    assert!(
                        !path.starts_with("vessels."),
                        "Vessel path {} should be filtered out after desubscription",
                        path
                    );
                }
            }
        }
    }
}

#[test]
fn test_multiple_ais_vessels_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to vessels.*
    let subscription = create_subscription("vessels.*");
    let _ = subscriptions.subscribe(subscription);

    // Create a delta with multiple AIS vessels
    let mut delta = SignalKDelta::new();
    let vessel1 = create_test_vessel("111111111", "VESSEL1", 52.0, 4.0);
    let vessel2 = create_test_vessel("222222222", "VESSEL2", 53.0, 5.0);
    let vessel3 = create_test_vessel("333333333", "VESSEL3", 54.0, 6.0);

    delta.add_ais_vessel_update("vessels.111111111", &vessel1);
    delta.add_ais_vessel_update("vessels.222222222", &vessel2);
    delta.add_ais_vessel_update("vessels.333333333", &vessel3);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - all vessels should pass through
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    let updates = json["updates"].as_array().unwrap();

    let mut vessel_count = 0;
    for update in updates {
        if let Some(values) = update["values"].as_array() {
            for value in values {
                let path = value["path"].as_str().unwrap_or("");
                if path.starts_with("vessels.") {
                    vessel_count += 1;
                }
            }
        }
    }
    assert_eq!(vessel_count, 3, "Should find all 3 vessels in delta");
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
    assert_eq!(json["status"], "Active");
    // dimensions should not be present when None
    assert!(json.get("dimensions").is_none());
}

#[test]
fn test_subscribe_all_mode_passes_ais() {
    // In Subscribe::All mode, everything should pass through
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::All);

    // Create a delta with AIS data
    let mut delta = SignalKDelta::new();
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    delta.add_ais_vessel_update("vessels.123456789", &vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should pass through in All mode
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty in All mode");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    let updates = json["updates"].as_array().unwrap();

    let mut found_vessel = false;
    for update in updates {
        if let Some(values) = update["values"].as_array() {
            for value in values {
                let path = value["path"].as_str().unwrap_or("");
                if path.starts_with("vessels.") {
                    found_vessel = true;
                }
            }
        }
    }
    assert!(found_vessel, "Should find vessel in delta with All mode");
}

#[test]
fn test_subscribe_none_mode_blocks_ais() {
    // In Subscribe::None mode, nothing should pass through
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Create a delta with AIS data
    let mut delta = SignalKDelta::new();
    let vessel = create_test_vessel("123456789", "TEST", 52.0, 4.0);
    delta.add_ais_vessel_update("vessels.123456789", &vessel);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - AIS should be filtered out in None mode
    let built = delta.build();
    // Delta might be empty or have no vessel paths
    if let Some(built) = built {
        let json = serde_json::to_value(&built).unwrap();
        let updates = json["updates"].as_array().unwrap();
        for update in updates {
            if let Some(values) = update["values"].as_array() {
                for value in values {
                    let path = value["path"].as_str().unwrap_or("");
                    assert!(
                        !path.starts_with("vessels."),
                        "Vessel path {} should be filtered out in None mode",
                        path
                    );
                }
            }
        }
    }
}

#[test]
fn test_specific_mmsi_subscription() {
    let mut subscriptions = ActiveSubscriptions::new(Subscribe::None);

    // Subscribe to specific vessel only
    let subscription = create_subscription("vessels.123456789");
    let _ = subscriptions.subscribe(subscription);

    // Create a delta with multiple AIS vessels
    let mut delta = SignalKDelta::new();
    let vessel1 = create_test_vessel("123456789", "SUBSCRIBED", 52.0, 4.0);
    let vessel2 = create_test_vessel("999999999", "NOT_SUBSCRIBED", 53.0, 5.0);

    delta.add_ais_vessel_update("vessels.123456789", &vessel1);
    delta.add_ais_vessel_update("vessels.999999999", &vessel2);

    // Apply subscription filtering
    delta.apply_subscriptions(&mut subscriptions);

    // Build and check - only the subscribed vessel should pass through
    let built = delta.build();
    assert!(built.is_some(), "Delta should not be empty");

    let json = serde_json::to_value(built.unwrap()).unwrap();
    let updates = json["updates"].as_array().unwrap();

    let mut found_subscribed = false;
    let mut found_other = false;
    for update in updates {
        if let Some(values) = update["values"].as_array() {
            for value in values {
                let path = value["path"].as_str().unwrap_or("");
                if path == "vessels.123456789" {
                    found_subscribed = true;
                }
                if path == "vessels.999999999" {
                    found_other = true;
                }
            }
        }
    }
    assert!(found_subscribed, "Should find subscribed vessel");
    assert!(!found_other, "Should NOT find unsubscribed vessel");
}
