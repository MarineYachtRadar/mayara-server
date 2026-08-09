//! Integration tests for REST API endpoints
//!
//! These tests verify that the REST API responses match the documented format.
//! Run with: cargo test --test api_rest -- --ignored
//!
//! Prerequisites:
//! - mayara-server must be running with --emulator flag
//! - Default port 6502

use serde_json::Value;
use std::env;

fn base_url() -> String {
    env::var("MAYARA_TEST_URL").unwrap_or_else(|_| "http://localhost:6502".to_string())
}

async fn get_json(path: &str) -> Value {
    let url = format!("{}{}", base_url(), path);
    reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn get_response(path: &str) -> reqwest::Response {
    let url = format!("{}{}", base_url(), path);
    reqwest::Client::new().get(&url).send().await.unwrap()
}

async fn put_json(path: &str, body: &Value) -> reqwest::Response {
    let url = format!("{}{}", base_url(), path);
    reqwest::Client::new()
        .put(&url)
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn first_radar_id() -> String {
    let json = get_json("/signalk/v2/api/vessels/self/radars").await;
    json["radars"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone()
}

// ============================================================================
// GET /
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_root_redirects_to_gui() {
    let url = format!("{}/", base_url());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client.get(&url).send().await.unwrap();

    assert_eq!(response.status(), 303);
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/gui/");
}

// ============================================================================
// GET /signalk
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_signalk_endpoints() {
    let json = get_json("/signalk").await;

    assert!(json.get("endpoints").is_some(), "Missing 'endpoints'");
    assert!(json.get("server").is_some(), "Missing 'server'");

    let server = &json["server"];
    assert_eq!(server["id"], "mayara");
    assert!(server.get("version").is_some());

    let v2 = &json["endpoints"]["v2"];
    assert!(v2.get("version").is_some());
    assert!(v2.get("signalk-http").is_some());
    assert!(v2.get("signalk-ws").is_some());
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radars() {
    let json = get_json("/signalk/v2/api/vessels/self/radars").await;

    assert!(json["version"].is_string(), "Missing 'version'");
    let radars = json["radars"].as_object().unwrap();
    assert!(!radars.is_empty(), "No radars found");

    for (id, radar) in radars {
        for field in ["name", "brand", "radarIpAddress"] {
            assert!(
                radar.get(field).is_some(),
                "Radar {} missing '{}'",
                id,
                field
            );
        }
    }
}

/// Clients that take "the first radar" must get the same one every time.
/// A dual-range radar otherwise offers range A or range B at random, with
/// nothing the operator can see or control. Issue #497.
///
/// The response is examined as text. Parsing it into a `serde_json::Value`
/// would sort the keys in this test's own parser — `serde_json::Map` is a
/// `BTreeMap` unless the `preserve_order` feature is on — and hide the one
/// thing under test.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radars_key_order_is_stable() {
    /// Enough requests to catch an order that varies per response; the
    /// issue was reported from six.
    const REQUESTS: usize = 6;

    let mut wire_orders: Vec<Vec<String>> = Vec::new();
    for _ in 0..REQUESTS {
        let body = get_response("/signalk/v2/api/vessels/self/radars")
            .await
            .text()
            .await
            .unwrap();

        // The ids themselves come from a parse — only their *order* is
        // untrustworthy there — and the wire order from where each one
        // appears in the raw body.
        let parsed: Value = serde_json::from_str(&body).unwrap();
        let mut ids: Vec<String> = parsed["radars"]
            .as_object()
            .expect("radars object")
            .keys()
            .cloned()
            .collect();
        // Scope the search to the radars member so a radar whose *name*
        // happens to contain another's id cannot skew the order.
        let radars_at = body.find("\"radars\"").expect("radars member");
        let radars = &body[radars_at..];
        ids.sort_by_key(|id| {
            radars
                .find(&format!("\"{}\"", id))
                .expect("id present in the raw body")
        });
        wire_orders.push(ids);
    }

    let first = &wire_orders[0];
    assert!(!first.is_empty(), "No radars found");
    for (n, order) in wire_orders.iter().enumerate() {
        assert_eq!(order, first, "radar order changed on request {}", n + 1);
    }

    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(
        first, &sorted,
        "radar ids should be written in a predictable order"
    );
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radars_returns_emulator() {
    let json = get_json("/signalk/v2/api/vessels/self/radars").await;
    let radars = json["radars"].as_object().unwrap();

    let (_, radar) = radars
        .iter()
        .find(|(id, _)| id.starts_with("emu"))
        .expect("No emulator radar found");
    assert_eq!(radar["brand"], "Emulator");
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars/{radar_id}
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radar_info() {
    let id = first_radar_id().await;
    let json = get_json(&format!("/signalk/v2/api/vessels/self/radars/{}", id)).await;

    for field in ["name", "brand", "radarIpAddress"] {
        assert!(
            json.get(field).is_some(),
            "Radar {} missing '{}'",
            id,
            field
        );
    }
}

/// The single-radar response is the same entry the list returns for that ID.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radar_info_matches_list_entry() {
    let id = first_radar_id().await;
    let list = get_json("/signalk/v2/api/vessels/self/radars").await;
    let single = get_json(&format!("/signalk/v2/api/vessels/self/radars/{}", id)).await;

    assert_eq!(list["radars"][&id], single);
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_radar_info_unknown_id_returns_404() {
    let response = get_response("/signalk/v2/api/vessels/self/radars/no-such-radar").await;
    assert_eq!(response.status(), 404);
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars/{radar_id}/capabilities
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_capabilities() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/capabilities",
        id
    ))
    .await;

    // Response is bare capabilities object
    let caps = &json;

    for field in [
        "maxRange",
        "minRange",
        "supportedRanges",
        "spokesPerRevolution",
        "maxSpokeLength",
        "pixelValues",
        "hasDoppler",
        "hasDualRadar",
        "hasDualRange",
        "hasSparseSpokes",
        "noTransmitSectors",
        "controls",
        "legend",
    ] {
        assert!(
            caps.get(field).is_some(),
            "Missing capability field: {}",
            field
        );
    }

    assert!(caps["maxRange"].is_number());
    assert!(caps["minRange"].is_number());
    assert!(caps["supportedRanges"].is_array());
    assert!(caps["spokesPerRevolution"].is_number());
    assert!(caps["maxSpokeLength"].is_number());
    assert!(caps["pixelValues"].is_number());
    assert!(caps["hasDoppler"].is_boolean());
    assert!(caps["hasDualRadar"].is_boolean());
    assert!(caps["hasDualRange"].is_boolean());
    assert!(caps["hasSparseSpokes"].is_boolean());
    assert!(caps["noTransmitSectors"].is_number());
    assert!(caps["controls"].is_object());
    assert!(caps["legend"].is_object());
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_capabilities_controls_structure() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/capabilities",
        id
    ))
    .await;
    let controls = json["controls"].as_object().unwrap();

    assert!(controls.contains_key("power"), "Missing 'power' control");
    assert!(controls.contains_key("range"), "Missing 'range' control");

    let valid_types = [
        "number", "enum", "string", "button", "sector", "zone", "rect",
    ];
    for (cid, control) in controls {
        assert!(control.get("id").is_some(), "Control {} missing 'id'", cid);
        assert!(
            control.get("name").is_some(),
            "Control {} missing 'name'",
            cid
        );
        assert!(
            control.get("dataType").is_some(),
            "Control {} missing 'dataType'",
            cid
        );
        assert!(
            control.get("category").is_some(),
            "Control {} missing 'category'",
            cid
        );
        let dt = control["dataType"].as_str().unwrap();
        assert!(
            valid_types.contains(&dt),
            "Control {} has invalid dataType: {}",
            cid,
            dt
        );
    }
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_capabilities_legend_structure() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/capabilities",
        id
    ))
    .await;
    let legend = &json["legend"];

    for field in ["pixels", "lowReturn", "mediumReturn", "strongReturn"] {
        assert!(legend.get(field).is_some(), "Legend missing '{}'", field);
    }

    let pixels = legend["pixels"].as_array().unwrap();
    assert!(!pixels.is_empty());

    for (i, pixel) in pixels.iter().enumerate() {
        assert!(pixel.get("type").is_some(), "Pixel {} missing 'type'", i);
        assert!(pixel.get("color").is_some(), "Pixel {} missing 'color'", i);
    }
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_capabilities_units_are_si() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/capabilities",
        id
    ))
    .await;
    let controls = json["controls"].as_object().unwrap();

    let valid_si_units = ["m", "m/s", "rad", "rad/s", "s"];
    for (cid, control) in controls {
        if let Some(units) = control.get("units") {
            let u = units.as_str().unwrap();
            assert!(
                valid_si_units.contains(&u),
                "Control {} has non-SI unit: {}",
                cid,
                u
            );
        }
    }
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars/{radar_id}/controls
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_all_controls() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/controls",
        id
    ))
    .await;

    // Response is bare controls object
    let controls = json.as_object().unwrap();
    assert!(!controls.is_empty());

    for (cid, control) in controls {
        assert!(
            control.is_object(),
            "Control {} value should be an object",
            cid
        );
    }
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_single_control_value() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/controls/power",
        id
    ))
    .await;

    // Single control returns bare value (not wrapped)
    assert!(json.get("value").is_some(), "Power should have 'value'");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_single_control_invalid() {
    let id = first_radar_id().await;
    let response = get_response(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/controls/nonexistent",
        id
    ))
    .await;

    assert_eq!(response.status(), 404);
}

// ============================================================================
// PUT /signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_set_control_value() {
    let id = first_radar_id().await;
    let path = format!("/signalk/v2/api/vessels/self/radars/{}/controls/gain", id);

    let response = put_json(&path, &serde_json::json!({"value": 75})).await;
    assert!(response.status().is_success(), "PUT should succeed");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_set_power_standby_transmit() {
    let id = first_radar_id().await;
    let path = format!("/signalk/v2/api/vessels/self/radars/{}/controls/power", id);

    let response = put_json(&path, &serde_json::json!({"value": 1})).await;
    assert!(response.status().is_success(), "PUT standby should succeed");

    let response = put_json(&path, &serde_json::json!({"value": 2})).await;
    assert!(
        response.status().is_success(),
        "PUT transmit should succeed"
    );
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_set_invalid_control() {
    let id = first_radar_id().await;
    let response = put_json(
        &format!(
            "/signalk/v2/api/vessels/self/radars/{}/controls/nonexistent",
            id
        ),
        &serde_json::json!({"value": 50}),
    )
    .await;

    assert_eq!(response.status(), 404);
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars/{radar_id}/targets
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_get_targets() {
    let id = first_radar_id().await;
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/targets",
        id
    ))
    .await;

    // Response is a bare array of targets
    assert!(json.is_array(), "Targets response should be an array");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_acquire_target() {
    let id = first_radar_id().await;
    let url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/targets",
        base_url(),
        id
    );
    let body = serde_json::json!({"bearing": 0.785, "distance": 2000});

    let response = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let json: Value = response.json().await.unwrap();
    assert!(
        json.get("targetId").is_some(),
        "Response should include 'targetId'"
    );
    assert!(
        json.get("radarId").is_some(),
        "Response should include 'radarId'"
    );
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_delete_target() {
    let id = first_radar_id().await;

    // Get existing targets to find one to delete
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/targets",
        id
    ))
    .await;
    let targets = json.as_array().unwrap();

    if targets.is_empty() {
        // Acquire one first
        let url = format!(
            "{}/signalk/v2/api/vessels/self/radars/{}/targets",
            base_url(),
            id
        );
        reqwest::Client::new()
            .post(&url)
            .json(&serde_json::json!({"bearing": 1.57, "distance": 1000}))
            .send()
            .await
            .unwrap();
    }

    // Get target list again
    let json = get_json(&format!(
        "/signalk/v2/api/vessels/self/radars/{}/targets",
        id
    ))
    .await;
    let targets = json.as_array().unwrap();

    if let Some(target) = targets.first() {
        let target_id = target["id"].as_i64().unwrap();
        let url = format!(
            "{}/signalk/v2/api/vessels/self/radars/{}/targets/{}",
            base_url(),
            id,
            target_id
        );
        let response = reqwest::Client::new().delete(&url).send().await.unwrap();
        assert!(response.status().is_success());
    }
}

// ============================================================================
// GET /signalk/v2/api/vessels/self/radars/resources/openapi.json
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_openapi_spec() {
    let json = get_json("/signalk/v2/api/vessels/self/radars/resources/openapi.json").await;

    assert!(json.get("openapi").is_some());
    assert!(json.get("info").is_some());
    assert!(json.get("paths").is_some());

    let paths = json["paths"].as_object().unwrap();
    assert!(paths.contains_key("/signalk/v2/api/vessels/self/radars"));
}
