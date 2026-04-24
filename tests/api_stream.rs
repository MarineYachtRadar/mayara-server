//! Integration tests for WebSocket stream API endpoints
//!
//! These tests verify that the WebSocket streams work correctly.
//! Run with: cargo test --test api_stream -- --ignored
//!
//! Prerequisites:
//! - mayara-server must be running with --emulator flag
//! - Default port 6502

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::env;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async, connect_async_with_config,
    tungstenite::{
        extensions::ExtensionsConfig,
        protocol::{Message, WebSocketConfig},
    },
};

fn ws_url() -> String {
    env::var("MAYARA_TEST_WS_URL").unwrap_or_else(|_| "ws://localhost:6502".to_string())
}

fn http_url() -> String {
    env::var("MAYARA_TEST_URL").unwrap_or_else(|_| "http://localhost:6502".to_string())
}

async fn first_radar_id() -> String {
    let url = format!("{}/signalk/v2/api/vessels/self/radars", http_url());
    let json: Value = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    json.as_object().unwrap().keys().next().unwrap().clone()
}

fn text_msg(v: &Value) -> Message {
    Message::Text(v.to_string().into())
}

// ============================================================================
// Control Stream (/signalk/v1/stream)
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_connects() {
    let url = format!("{}/signalk/v1/stream", ws_url());
    let result = timeout(Duration::from_secs(5), connect_async(&url)).await;
    assert!(result.is_ok(), "Connection should not timeout");
    result
        .unwrap()
        .expect("WebSocket connection should succeed");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_receives_initial_data() {
    let url = format!("{}/signalk/v1/stream", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (_, mut read) = ws.split();

    let result = timeout(Duration::from_secs(5), read.next()).await;
    assert!(result.is_ok(), "Should receive initial data");

    if let Ok(Some(Ok(Message::Text(text)))) = result {
        let json: Value = serde_json::from_str(&text).expect("Should be valid JSON");
        // Initial message is a hello with name, version, roles, timestamp
        assert!(
            json.get("name").is_some() || json.get("updates").is_some(),
            "Initial message should be a hello or delta: {:?}",
            json
        );
    }
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_subscription() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws.split();

    let msg = json!({
        "subscribe": [{"path": "radars.*.controls.*", "policy": "instant"}]
    });
    write.send(text_msg(&msg)).await.expect("Failed to send");

    // Should receive at least one message (hello or cached controls)
    let mut got_updates = false;
    for _ in 0..5 {
        if let Ok(Some(Ok(Message::Text(text)))) =
            timeout(Duration::from_secs(2), read.next()).await
        {
            let json: Value = serde_json::from_str(&text).unwrap();
            if json.get("updates").is_some() {
                got_updates = true;
                break;
            }
        } else {
            break;
        }
    }
    assert!(got_updates, "Should receive updates after subscribing");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_desubscription() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws.split();

    // Subscribe
    let msg = json!({"subscribe": [{"path": "radars.*.controls.*"}]});
    write.send(text_msg(&msg)).await.unwrap();
    let _ = timeout(Duration::from_secs(2), read.next()).await;

    // Desubscribe
    let msg = json!({"desubscribe": [{"path": "radars.*.controls.*"}]});
    write.send(text_msg(&msg)).await.unwrap();

    // Stream should still be open
    let ping = write.send(Message::Ping(vec![].into())).await;
    assert!(
        ping.is_ok(),
        "Stream should still be open after desubscribe"
    );
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_combined_subscription() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, _) = ws.split();

    let msg = json!({
        "subscribe": [
            {"path": "radars.*.controls.*", "period": 1000},
            {"path": "radars.*.targets.*", "policy": "instant"}
        ]
    });
    write.send(text_msg(&msg)).await.unwrap();

    let ping = write.send(Message::Ping(vec![].into())).await;
    assert!(ping.is_ok(), "Should accept combined subscription");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_control_stream_set_control_via_stream() {
    let id = first_radar_id().await;
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws.split();

    // Subscribe
    let msg = json!({"subscribe": [{"path": format!("radars.{}.controls.*", id)}]});
    write.send(text_msg(&msg)).await.unwrap();
    let _ = timeout(Duration::from_secs(2), read.next()).await;

    // Set a control value via the stream
    let msg = json!({
        "updates": [{"values": [{"path": format!("radars.{}.controls.gain", id), "value": {"value": 60}}]}]
    });
    write.send(text_msg(&msg)).await.unwrap();

    let result = timeout(Duration::from_secs(5), read.next()).await;
    assert!(
        result.is_ok(),
        "Should receive response after setting control"
    );
}

// ============================================================================
// Spoke Data Stream (/signalk/v2/api/vessels/self/radars/{id}/spokes)
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_spoke_stream_connects() {
    let id = first_radar_id().await;
    let url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/spokes",
        ws_url(),
        id
    );

    let result = timeout(Duration::from_secs(5), connect_async(&url)).await;
    assert!(result.is_ok(), "Connection should not timeout");
    result
        .unwrap()
        .expect("WebSocket connection should succeed");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_spoke_stream_receives_binary_data() {
    let id = first_radar_id().await;

    // Ensure radar is transmitting
    let power_url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/controls/power",
        http_url(),
        id
    );
    reqwest::Client::new()
        .put(&power_url)
        .json(&json!({"value": 2}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/spokes",
        ws_url(),
        id
    );
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (_, mut read) = ws.split();

    // Collect messages until we get non-empty binary data
    let mut got_binary = false;
    for _ in 0..20 {
        match timeout(Duration::from_secs(5), read.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) if !data.is_empty() => {
                got_binary = true;
                break;
            }
            Ok(Some(Ok(_))) => continue, // ping/pong/empty binary
            _ => break,
        }
    }

    assert!(got_binary, "Should receive binary spoke data");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_spoke_stream_multiple_connections() {
    let id = first_radar_id().await;

    // Ensure radar is transmitting
    let power_url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/controls/power",
        http_url(),
        id
    );
    reqwest::Client::new()
        .put(&power_url)
        .json(&json!({"value": 2}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let url = format!(
        "{}/signalk/v2/api/vessels/self/radars/{}/spokes",
        ws_url(),
        id
    );
    let (ws1, _) = connect_async(&url).await.expect("Failed to connect ws1");
    let (ws2, _) = connect_async(&url).await.expect("Failed to connect ws2");
    let (_, mut read1) = ws1.split();
    let (_, mut read2) = ws2.split();

    let r1 = timeout(Duration::from_secs(10), read1.next()).await;
    let r2 = timeout(Duration::from_secs(10), read2.next()).await;

    assert!(r1.is_ok(), "Client 1 should receive data");
    assert!(r2.is_ok(), "Client 2 should receive data");
}

// ============================================================================
// Signal K delta format
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_signalk_delta_format() {
    let url = format!("{}/signalk/v1/stream", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (_, mut read) = ws.split();

    let mut messages = Vec::new();
    for _ in 0..5 {
        if let Ok(Some(Ok(Message::Text(text)))) =
            timeout(Duration::from_secs(2), read.next()).await
        {
            messages.push(serde_json::from_str::<Value>(&text).unwrap());
        }
    }

    assert!(!messages.is_empty(), "Should receive at least one message");

    for msg in &messages {
        if let Some(updates) = msg.get("updates") {
            for update in updates.as_array().unwrap() {
                assert!(
                    update.get("$source").is_some(),
                    "Update missing '$source': {:?}",
                    update
                );
                if let Some(values) = update.get("values") {
                    assert!(values.is_array());
                    for value in values.as_array().unwrap() {
                        assert!(value.get("path").is_some(), "Value missing 'path'");
                        assert!(value.get("value").is_some(), "Value missing 'value'");
                    }
                }
            }
        }
    }
}

// ============================================================================
// WebSocket compression (permessage-deflate)
// ============================================================================

#[tokio::test]
#[ignore = "requires running server"]
async fn test_websocket_deflate_negotiation() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());

    let mut config = WebSocketConfig::default();
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(Default::default());
    config.extensions = extensions;

    let (ws, response) =
        connect_async_with_config(url, Some(config), false)
            .await
            .expect("Failed to connect with deflate");

    // Server should echo back the extension in the response
    let ext_header = response
        .headers()
        .get("sec-websocket-extensions")
        .expect("Response missing Sec-WebSocket-Extensions header");
    assert!(
        ext_header
            .to_str()
            .unwrap()
            .contains("permessage-deflate"),
        "Server should negotiate permessage-deflate, got: {:?}",
        ext_header
    );

    // Verify the connection is functional: send a subscribe and read a response
    let (mut write, mut read) = ws.split();
    let msg = json!({"subscribe": [{"path": "radars.*.controls.*", "policy": "instant"}]});
    write.send(text_msg(&msg)).await.expect("Failed to send");

    let result = timeout(Duration::from_secs(5), read.next()).await;
    assert!(result.is_ok(), "Should receive data over compressed connection");
    if let Ok(Some(Ok(Message::Text(text)))) = result {
        let json: Value = serde_json::from_str(&text).expect("Should be valid JSON");
        assert!(
            json.get("updates").is_some() || json.get("name").is_some(),
            "Should receive valid message over compressed connection"
        );
    }
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_websocket_without_deflate() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());

    // Connect without requesting compression
    let (_, response) = connect_async(&url)
        .await
        .expect("Failed to connect");

    // Server should NOT include the extension header
    assert!(
        response.headers().get("sec-websocket-extensions").is_none(),
        "Server should not negotiate deflate when client doesn't offer it"
    );
}
