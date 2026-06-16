//! Network diagnostics endpoint.
//!
//! Produces a gzipped JSON dump describing how the mayara process sees the
//! network: host interfaces, per-interface locator/listener status, and any
//! radars that did get detected. The dump is meant to be attached to a GitHub
//! issue so support can reproduce a "radar not detected" report without
//! needing remote access to the user's network.

use axum::{
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use flate2::Compression;
use flate2::write::GzEncoder;
use mayara::network::devices::{self, Device};
use mayara::{InterfaceApi, PACKAGE, VERSION};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::net::Ipv4Addr;
use std::str::FromStr;
use tokio::sync::mpsc;

use super::super::Web;

pub(crate) const DIAGNOSTICS_URI: &str =
    "/signalk/v2/api/vessels/self/radars/diagnostics";

#[derive(Serialize)]
struct HostAddress {
    family: &'static str,
    ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    netmask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    broadcast: Option<String>,
}

#[derive(Serialize)]
struct HostInterface {
    name: String,
    index: u32,
    internal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mac: Option<String>,
    addresses: Vec<HostAddress>,
}

fn enumerate_host_interfaces() -> Vec<HostInterface> {
    match NetworkInterface::show() {
        Ok(interfaces) => interfaces
            .into_iter()
            .map(|itf| HostInterface {
                name: itf.name,
                index: itf.index,
                internal: itf.internal,
                mac: itf.mac_addr,
                addresses: itf
                    .addr
                    .into_iter()
                    .map(|a| match a {
                        Addr::V4(v4) => HostAddress {
                            family: "v4",
                            ip: v4.ip.to_string(),
                            netmask: v4.netmask.map(|n| n.to_string()),
                            broadcast: v4.broadcast.map(|b| b.to_string()),
                        },
                        Addr::V6(v6) => HostAddress {
                            family: "v6",
                            ip: v6.ip.to_string(),
                            netmask: v6.netmask.map(|n| n.to_string()),
                            broadcast: v6.broadcast.map(|b| b.to_string()),
                        },
                    })
                    .collect(),
            })
            .collect(),
        Err(e) => {
            log::warn!("diagnostics: NetworkInterface::show() failed: {}", e);
            Vec::new()
        }
    }
}

fn compiled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    #[cfg(feature = "navico")]
    features.push("navico");
    #[cfg(feature = "furuno")]
    features.push("furuno");
    #[cfg(feature = "garmin")]
    features.push("garmin");
    #[cfg(feature = "koden")]
    features.push("koden");
    #[cfg(feature = "raymarine")]
    features.push("raymarine");
    #[cfg(feature = "emulator")]
    features.push("emulator");
    #[cfg(feature = "pcap-replay")]
    features.push("pcap-replay");
    features
}

/// Serialize the parts of the CLI configuration that influence radar
/// discovery, excluding any secrets (Signal K bearer token paths and values).
fn config_summary(args: &mayara::Cli) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("port".into(), json!(args.port));
    obj.insert("tls_enabled".into(), json!(args.tls_cert.is_some()));
    obj.insert("interface".into(), json!(args.interface));
    obj.insert("brand".into(), json!(args.brand.map(|b| b.to_string())));
    obj.insert("allow_wifi".into(), json!(args.allow_wifi));
    obj.insert("multiple_radar".into(), json!(args.multiple_radar));
    obj.insert("transmit".into(), json!(args.transmit));
    obj.insert("stationary".into(), json!(args.stationary));
    obj.insert("emulator".into(), json!(args.emulator));
    obj.insert("replay".into(), json!(args.replay));
    obj.insert("nmea0183".into(), json!(args.nmea0183));
    obj.insert(
        "navigation_address".into(),
        json!(args.navigation_address),
    );
    obj.insert(
        "targets".into(),
        json!(format!("{:?}", args.targets).to_lowercase()),
    );
    #[cfg(feature = "pcap-replay")]
    obj.insert("pcap".into(), json!(args.pcap));
    Value::Object(obj)
}

/// Upper bound on how long we wait for the locator subsystem to reply
/// with its `InterfaceApi` snapshot. The reply is normally back in a few
/// milliseconds; if the locator is wedged (e.g. mid-shutdown or stuck
/// in `reply_with_interface_state`) we'd rather return a partial dump
/// than hang the HTTP request indefinitely.
const INTERFACE_API_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

async fn fetch_interface_api(state: &Web) -> InterfaceApi {
    let (tx, mut rx) = mpsc::channel(1);
    if state.tx_interface_request.send(Some(tx)).is_err() {
        return InterfaceApi::default();
    }
    match tokio::time::timeout(INTERFACE_API_FETCH_TIMEOUT, rx.recv()).await {
        Ok(Some(api)) => api,
        Ok(None) => InterfaceApi::default(),
        Err(_) => {
            log::warn!(
                "diagnostics: locator did not reply with InterfaceApi within {:?}",
                INTERFACE_API_FETCH_TIMEOUT
            );
            InterfaceApi::default()
        }
    }
}

fn radars_summary(state: &Web) -> Vec<Value> {
    state
        .radars
        .get_active()
        .into_iter()
        .map(|info| {
            json!({
                "id": info.key(),
                "brand": info.brand.to_string(),
                "model": info.controls.model_name(),
                "name": info.controls.user_name(),
                "serial_no": info.serial_no,
                "address": info.addr.to_string(),
                "via_nic": info.nic_addr.to_string(),
                "spoke_data_address": info.spoke_data_addr.to_string(),
                "report_address": info.report_addr.to_string(),
                "send_command_address": info.send_command_addr.to_string(),
                "replay": info.replay(),
            })
        })
        .collect()
}

fn build_diagnostics(
    state: &Web,
    locator: InterfaceApi,
    devices_by_nic: HashMap<Ipv4Addr, Vec<Device>>,
) -> Value {
    let mut locator_value =
        serde_json::to_value(&locator).expect("InterfaceApi is always serializable");
    inject_devices(&mut locator_value, &devices_by_nic);

    json!({
        "schema": "mayara-network-diagnostics-v1",
        "generated_at": Utc::now().to_rfc3339(),
        "server": {
            "package": PACKAGE,
            "version": VERSION,
            "api_version": mayara::SIGNALK_RADAR_API_VERSION,
            "compiled_features": compiled_features(),
            "os": std::env::consts::OS,
            "family": std::env::consts::FAMILY,
            "arch": std::env::consts::ARCH,
        },
        "config": config_summary(&state.args),
        "host": {
            "interfaces": enumerate_host_interfaces(),
        },
        "locator": locator_value,
        "radars": radars_summary(state),
    })
}

/// Walk `locator.interfaces` (serialized as a map keyed by `"name (ip)"`)
/// and, for any entry whose `ip` field matches a key in `devices_by_nic`,
/// inject a `devices` array next to the existing `listeners` block.
fn inject_devices(locator: &mut Value, devices_by_nic: &HashMap<Ipv4Addr, Vec<Device>>) {
    let Some(interfaces) = locator.get_mut("interfaces").and_then(Value::as_object_mut) else {
        return;
    };
    for entry in interfaces.values_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let ip = obj
            .get("ip")
            .and_then(Value::as_str)
            .and_then(|s| Ipv4Addr::from_str(s).ok());
        let Some(ip) = ip else { continue };
        let Some(devices) = devices_by_nic.get(&ip) else {
            continue;
        };
        obj.insert(
            "devices".into(),
            serde_json::to_value(devices).expect("Device is always serializable"),
        );
    }
}

fn gzip_json(body: &Value) -> std::io::Result<Vec<u8>> {
    let json_bytes = serde_json::to_vec_pretty(body).expect("Value is always serializable");
    let mut encoder = GzEncoder::new(Vec::with_capacity(json_bytes.len() / 4), Compression::default());
    encoder.write_all(&json_bytes)?;
    encoder.finish()
}

pub(crate) async fn get_diagnostics(State(state): State<Web>) -> Response {
    let locator = fetch_interface_api(&state).await;
    let devices_by_nic = devices::collect_per_interface(&locator).await;
    let body = build_diagnostics(&state, locator, devices_by_nic);

    let gz = match gzip_json(&body) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::error!("Failed to gzip diagnostics: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to compress diagnostics: {}", e),
            )
                .into_response();
        }
    };

    let filename = format!(
        "mayara-network-diagnostics-{}.json.gz",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(gz))
        .expect("static headers cannot fail")
}
