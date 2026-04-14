//! Integration test: replay Furuno DRS4D-NXT pcap fixture.
//!
//! Verifies that replaying the fixture through the full pipeline
//! detects the radar with the correct brand.

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
        pass_ais: false,
        emulator: false,
        merge_targets: false,
    }
}

#[tokio::test]
async fn replay_furuno_drs4dnxt() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("furuno-drs4dnxt.pcap.gz");
    if !fixture.exists() {
        panic!(
            "Fixture not found: {}. Run: cargo test --lib generate_fixtures -- --ignored",
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
                    assert!(
                        key.starts_with("fur"),
                        "expected Furuno key, got: {}",
                        key
                    );
                    let info = radars.get_by_key(key).expect("radar info");
                    assert_eq!(info.brand, mayara::Brand::Furuno);
                    break;
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
