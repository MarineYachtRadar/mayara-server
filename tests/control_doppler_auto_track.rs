#![cfg(feature = "pcap-replay")]

//! Regression test for issue #627: a client write of Doppler Auto Track was
//! forwarded to the receiver but never stored, so the control snapped back to
//! its old value on the next broadcast.

use mayara::radar::settings::{BareControlValue, ControlId, ControlValue};
use mayara::{Cli, replay};
use std::path::Path;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};

fn test_args() -> Cli {
    Cli {
        verbose: <clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>>::default(),
        port: 0,
        tls_cert: None,
        tls_key: None,
        parent: None,
        interface: None,
        brand: Some(mayara::Brand::Navico),
        targets: mayara::TargetMode::None,
        navigation_address: None,
        nmea0183: false,
        output: false,
        replay: false,
        pcap: Some("fixture".to_string()),
        repeat: false,
        fake_errors: false,
        allow_wifi: false,
        stationary: false,
        static_position: None,
        multiple_radar: false,
        openapi: false,
        transmit: false,
        accept_invalid_certs: false,
        signalk_token: None,
        signalk_token_file: None,
        emulator: false,
        merge_targets: false,
        no_websocket_compression: false,
        mdns_hostname: None,
        no_telemetry: false,
        no_mdns: true,
        pcap_max_time: None,
    }
}

#[tokio::test]
async fn doppler_auto_track_write_is_stored() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("navico-halo24.pcap.gz");
    if !fixture.exists() {
        panic!(
            "Fixture not found: {}. Run: cargo run --features pcap-replay --example generate-fixtures",
            fixture.display()
        );
    }

    let _ = env_logger::builder().is_test(true).try_init();
    replay::init(&fixture).expect("init replay");
    replay::set_instant_timing();
    let args = test_args();

    Toplevel::new(async move |s: &mut SubsystemHandle| {
        let (radars, _) = mayara::start_session(s, args).await;

        s.start(SubsystemBuilder::new(
            "test",
            async move |subsys: &mut SubsystemHandle| {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                let info = loop {
                    if let Some(key) = radars.get_keys().first()
                        && let Some(info) = radars.get_by_key(key)
                        && info.controls.model_name().is_some()
                        && !info.ranges.all.is_empty()
                    {
                        break info;
                    }
                    if tokio::time::Instant::now() > deadline {
                        panic!("Timeout: no radar detected within 5 seconds");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                };

                assert!(
                    !info.controls.doppler_auto_track(),
                    "HALO24 should start with Doppler Auto Track off"
                );

                let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(10);
                let bare: BareControlValue =
                    serde_json::from_value(serde_json::json!({ "value": 1 })).unwrap();
                info.controls
                    .process_client_request(
                        ControlValue::from_request(ControlId::DopplerAutoTrack, bare),
                        reply_tx,
                    )
                    .expect("client request accepted");

                let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                while !info.controls.doppler_auto_track() {
                    if tokio::time::Instant::now() > deadline {
                        panic!("Doppler Auto Track write was discarded");
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }

                subsys.request_shutdown();
                Ok::<(), miette::Report>(())
            },
        ));
    })
    .handle_shutdown_requests(Duration::from_millis(2000))
    .await
    .expect("toplevel");
}
