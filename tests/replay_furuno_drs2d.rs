#![cfg(feature = "pcap-replay")]

//! Integration test: replay Furuno DRS2D pcap fixture.
//!
//! Regression for the spoke-alignment overshoot panic on the last spoke of a
//! frame (issue #195). The fixture contains beacons, reports, and a spoke
//! frame whose final sweep consumes its buffer exactly — the 4-byte alignment
//! rounding then overshoots and used to panic with `range start index N out
//! of range for slice of length M`.

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
        interface: None,
        brand: Some(mayara::Brand::Furuno),
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
        pcap_max_time: None,
    }
}

#[tokio::test]
async fn replay_furuno_drs2d() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("furuno-drs2d.pcap.gz");
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
                loop {
                    let keys = radars.get_keys();
                    if !keys.is_empty() {
                        let key = &keys[0];
                        let info = radars.get_by_key(key).expect("radar info");

                        if info.controls.model_name().is_some() && !info.ranges.all.is_empty() {
                            assert!(key.starts_with("fur"), "expected Furuno key, got: {}", key);
                            assert_eq!(info.brand, mayara::Brand::Furuno);
                            assert_eq!(info.controls.model_name().unwrap(), "DRS");
                            // Give the spoke worker time to ingest the trailing
                            // spoke frames in the fixture so the alignment bug
                            // (issue #195) would have a chance to panic.
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            break;
                        }
                    }
                    if tokio::time::Instant::now() > deadline {
                        panic!("Timeout: no radar detected within 5 seconds");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
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
