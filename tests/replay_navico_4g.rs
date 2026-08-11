#![cfg(feature = "pcap-replay")]

//! Integration test: replay Navico 4G pcap fixture.
//!
//! Verifies that replaying the fixture through the full pipeline
//! detects the radar with the correct brand, model, and capabilities.

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
        no_mdns: true,
        pcap_max_time: None,
    }
}

#[tokio::test]
async fn replay_navico_4g() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("navico-4g.pcap.gz");
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

                        // Wait until the model has been identified
                        if info.controls.model_name().is_some() && !info.ranges.all.is_empty() {
                            assert!(key.starts_with("nav"), "expected Navico key, got: {}", key);
                            assert_eq!(info.brand, mayara::Brand::Navico);
                            assert_eq!(info.controls.model_name().unwrap(), "4G");
                            assert!(!info.doppler, "4G should not support Doppler");
                            assert_eq!(info.spokes_per_revolution, 2048);
                            break;
                        }
                    }
                    if tokio::time::Instant::now() > deadline {
                        panic!("Timeout: no radar detected within 5 seconds");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }

                // The 4G beacon advertises two scanners. Replay keeps only
                // range A (see SharedRadars::add), but that instance must
                // carry the dual-range antenna grouping data.
                let a = radars.get_by_key("nav2452A").expect("range A discovered");
                assert!(a.dual_range, "4G should be dual-range capable");
                assert_eq!(a.dual.as_deref(), Some("A"));
                assert_eq!(a.base_key(), "nav2452");

                subsys.request_shutdown();
                Ok::<(), miette::Report>(())
            },
        ));
    })
    .handle_shutdown_requests(Duration::from_millis(2000))
    .await
    .expect("toplevel");
}
