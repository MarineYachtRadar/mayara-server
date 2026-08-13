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
    // The document is `{"version": ..., "radars": {...}}`, so the ids are one
    // level in — taking the first key of the document itself yields "radars".
    json["radars"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone()
}

/// How long any one of these tests will wait for the server to say something.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Send one request over the control stream and return the first answer to it,
/// skipping the hello and any deltas that happen to be in flight.
///
/// The wait is one deadline for the whole answer, not one per frame: a stream
/// with traffic on it would otherwise keep resetting the clock and never give
/// up. A closed socket ends the wait rather than being read again forever.
async fn stream_request(request: &Value) -> Value {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws.split();

    write
        .send(text_msg(request))
        .await
        .expect("Failed to send request");

    let answer = timeout(REPLY_TIMEOUT, async {
        while let Some(frame) = read.next().await {
            let frame = frame.expect("Stream failed while waiting for an answer");
            if let Message::Text(text) = frame {
                let json: Value = serde_json::from_str(&text).expect("Should be valid JSON");
                if json.get("state").is_some() {
                    return Some(json);
                }
            }
        }
        None
    })
    .await
    .expect("Timed out waiting for an answer to the request");

    answer.expect("Stream closed before answering the request")
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

    // Set a control value via the stream. The stream takes the control value
    // itself, not a delta wrapped around it — the shape this used to send
    // matched no request at all, and the frame that made the test pass was
    // unrelated traffic that happened to arrive.
    let path = format!("radars.{}.controls.gain", id);
    let msg = json!({ "path": path, "value": 60 });
    write.send(text_msg(&msg)).await.unwrap();

    // The radar reporting the new value back is what says the write landed.
    let mut reported = false;
    for _ in 0..10 {
        let Ok(Some(Ok(Message::Text(text)))) = timeout(Duration::from_secs(2), read.next()).await
        else {
            break;
        };
        if text.contains(&path) {
            reported = true;
            break;
        }
    }
    assert!(
        reported,
        "the control set over the stream was never reported"
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

    let (ws, response) = connect_async_with_config(url, Some(config), false)
        .await
        .expect("Failed to connect with deflate");

    // Server should echo back the extension in the response
    let ext_header = response
        .headers()
        .get("sec-websocket-extensions")
        .expect("Response missing Sec-WebSocket-Extensions header");
    assert!(
        ext_header.to_str().unwrap().contains("permessage-deflate"),
        "Server should negotiate permessage-deflate, got: {:?}",
        ext_header
    );

    // Verify the connection is functional: send a subscribe and read a response
    let (mut write, mut read) = ws.split();
    let msg = json!({"subscribe": [{"path": "radars.*.controls.*", "policy": "instant"}]});
    write.send(text_msg(&msg)).await.expect("Failed to send");

    let result = timeout(Duration::from_secs(5), read.next()).await;
    assert!(
        result.is_ok(),
        "Should receive data over compressed connection"
    );
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
    let (_, response) = connect_async(&url).await.expect("Failed to connect");

    // Server should NOT include the extension header
    assert!(
        response.headers().get("sec-websocket-extensions").is_none(),
        "Server should not negotiate deflate when client doesn't offer it"
    );
}

/// How long to watch a stream that should have nothing to say.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// `subscribe=none` means what it says: after introducing itself the server
/// stops until asked for something. Filtering used to empty each update
/// without dropping it, so a client that had asked for nothing still received
/// a message for every own-ship update on the boat.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_subscribe_none_says_nothing_after_the_hello() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (_, mut read) = ws.split();

    let hello = timeout(Duration::from_secs(5), read.next())
        .await
        .expect("Timed out waiting for the hello");
    match hello {
        Some(Ok(Message::Text(text))) => {
            let json: Value = serde_json::from_str(&text).expect("Should be valid JSON");
            assert!(json.get("self").is_some(), "expected the hello, got {json}");
        }
        other => panic!("Expected a hello, got {other:?}"),
    }

    match timeout(SILENCE_WINDOW, read.next()).await {
        Err(_) => {} // Nothing said, which is the whole point.
        Ok(Some(Ok(Message::Text(text)))) => {
            panic!("a client that subscribed to nothing was sent: {text}")
        }
        Ok(other) => panic!("unexpected frame on a silent stream: {other:?}"),
    }
}

/// A control cannot be read without knowing what it is, so the definitions
/// have to arrive no later than the values they describe — they are held back
/// on connect only until the client asks for the data.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_subscribing_brings_the_definitions_with_the_values() {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws.split();

    let subscribe = json!({
        "subscribe": [{"path": "radars.*.controls.*", "policy": "instant"}]
    });
    write
        .send(text_msg(&subscribe))
        .await
        .expect("Failed to subscribe");

    let mut seen_meta = false;
    for _ in 0..10 {
        let frame = match timeout(Duration::from_secs(2), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => text,
            _ => break,
        };
        let json: Value = serde_json::from_str(&frame).expect("Should be valid JSON");
        let Some(updates) = json["updates"].as_array() else {
            continue;
        };

        if updates.iter().any(|u| u.get("meta").is_some()) {
            seen_meta = true;
        }
        if updates.iter().any(|u| u.get("values").is_some()) {
            assert!(
                seen_meta,
                "control values arrived before anything said what they are: {frame}"
            );
            return;
        }
    }

    panic!("subscribing produced no control values");
}
/// A Signal K client writes a value by sending a `put` over the stream and
/// waiting for the answer that carries its `requestId` back. mayara used to
/// drop the message on the floor: it matched none of the shapes the stream
/// understood, and nothing was sent in reply, so the client waited forever.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_put_over_stream_is_answered() {
    let id = first_radar_id().await;
    let put = serde_json::json!({
        "context": "vessels.self",
        "requestId": "test-put-1",
        "put": {
            "path": format!("radars.{}.controls.rain", id),
            "value": { "value": 30 }
        }
    });

    let response = stream_request(&put).await;

    assert_eq!(response["requestId"], "test-put-1");
    assert_eq!(response["state"], "COMPLETED");
    assert_eq!(response["statusCode"], 200);
}

/// A control that is only a number is written as one, the way a Signal K
/// client writes any scalar path.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_put_over_stream_takes_a_bare_number() {
    let id = first_radar_id().await;
    let put = serde_json::json!({
        "requestId": "test-put-2",
        "put": { "path": format!("radars.{}.controls.rain", id), "value": 40 }
    });

    let response = stream_request(&put).await;

    assert_eq!(response["state"], "COMPLETED");
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_put_over_stream_reports_an_unknown_radar() {
    let put = serde_json::json!({
        "requestId": "test-put-3",
        "put": { "path": "radars.nosuchradar.controls.rain", "value": 30 }
    });

    let response = stream_request(&put).await;

    assert_eq!(response["requestId"], "test-put-3");
    assert_eq!(response["state"], "FAILED");
    assert_eq!(response["statusCode"], 404);
    assert!(response["message"].is_string());
}

/// A path that names no control it recognises is the other way a put can be
/// answered 404, and it reaches that answer through a different error than an
/// unknown radar does.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_put_over_stream_reports_an_unknown_control() {
    let id = first_radar_id().await;
    let put = serde_json::json!({
        "requestId": "test-put-5",
        "put": { "path": format!("radars.{}.controls.nosuchcontrol", id), "value": 30 }
    });

    let response = stream_request(&put).await;

    assert_eq!(response["requestId"], "test-put-5");
    assert_eq!(response["state"], "FAILED");
    assert_eq!(response["statusCode"], 404);
}

/// Anything the stream cannot read at all is still answered, so a client is
/// never left waiting on a request that was thrown away.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_unreadable_stream_request_is_answered() {
    let response = stream_request(&serde_json::json!({"totally": "unrelated"})).await;

    assert_eq!(response["state"], "FAILED");
    assert_eq!(response["statusCode"], 400);
}

/// How long a test will wait for the server to introduce itself.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

async fn hello() -> Value {
    let url = format!("{}/signalk/v1/stream?subscribe=none", ws_url());
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    let (_, mut read) = ws.split();

    let first = timeout(HELLO_TIMEOUT, read.next())
        .await
        .expect("Timed out waiting for the hello");

    match first {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text).expect("Should be valid JSON"),
        other => panic!("Expected a hello, got {other:?}"),
    }
}

/// A client that reconnects and finds a different id knows the values it
/// cached came from an instance that is gone.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_hello_carries_a_server_start_id() {
    let first = hello().await;
    let id = first["serverStartId"]
        .as_str()
        .expect("the hello must carry a serverStartId");
    assert!(!id.is_empty());

    let second = hello().await;
    assert_eq!(
        second["serverStartId"], first["serverStartId"],
        "the id identifies the run, so it must not change between connections"
    );
}

/// Every other Signal K server stamps the hello with milliseconds and a `Z`,
/// so that is the form clients are known to read. mayara used to send an
/// offset and nanoseconds, which is valid RFC 3339 and unlike everyone else.
#[tokio::test]
#[ignore = "requires running server"]
async fn test_hello_timestamp_is_the_shape_clients_read() {
    let hello = hello().await;
    let timestamp = hello["timestamp"].as_str().expect("hello has a timestamp");

    assert!(
        !timestamp.contains("+00:00"),
        "timestamp should not carry an offset, got {timestamp}"
    );

    // `Z` on its own would also be satisfied by a whole-second stamp, and the
    // precision is the half of this that clients actually parse.
    let (_, fraction) = timestamp
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("timestamp should carry a fraction, got {timestamp}"));
    assert_eq!(
        fraction.len(),
        4,
        "expected three millisecond digits and a Z, got {timestamp}"
    );
    assert!(
        fraction.ends_with('Z') && fraction[..3].chars().all(|c| c.is_ascii_digit()),
        "expected three millisecond digits and a Z, got {timestamp}"
    );
}
