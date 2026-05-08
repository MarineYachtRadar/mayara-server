//! Integration test: replay Garmin xHD pcap fixture.
//!
//! Verifies that replaying the fixture through the full pipeline
//! detects the radar with the correct brand, model, and capabilities.

use mayara::{replay, Cli};
use std::path::Path;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};

fn test_args() -> Cli {
    Cli {
        verbose: <clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>>::default(),
        port: 0,
        tls_cert: None,
        tls_key: None,
        interface: None,
        brand: Some(mayara::Brand::Garmin),
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
        pass_ais: false,
        accept_invalid_certs: false,
        emulator: false,
        merge_targets: false,
    }
}

#[tokio::test]
async fn replay_garmin_xhd() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("garmin-xhd.pcap.gz");
    if !fixture.exists() {
        panic!(
            "Fixture not found: {}. Run: cargo run --features pcap-replay --example generate-fixtures",
            fixture.display()
        );
    }

    replay::init(&fixture).expect("init replay");
    replay::set_instant_timing();
    let args = test_args();

    Toplevel::new(move |s| async move {
        let (radars, _) = mayara::start_session(&s, args).await;

        s.start(SubsystemBuilder::new("test", move |subsys| async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let keys = radars.get_keys();
                if !keys.is_empty() {
                    let key = &keys[0];
                    let info = radars.get_by_key(key).expect("radar info");

                    // Wait until the model has been identified
                    if info.controls.model_name().is_some() && !info.ranges.all.is_empty() {
                        assert!(key.starts_with("gar"), "expected Garmin key, got: {}", key);
                        assert_eq!(info.brand, mayara::Brand::Garmin);
                        let model = info.controls.model_name().unwrap();
                        assert!(model.contains("xHD"), "expected xHD model, got: {}", model);
                        assert!(!info.doppler, "xHD should not support Doppler");
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
        }));
    })
    .handle_shutdown_requests(Duration::from_millis(2000))
    .await
    .expect("toplevel");
}
