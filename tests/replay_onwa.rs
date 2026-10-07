#![cfg(feature = "pcap-replay")]

//! Integration test: replay the ONWA KRA-5001 pcap fixture.
//!
//! Verifies that replaying the fixture through the full pipeline detects the
//! radar by its MAC, names the model from its firmware, and decodes spokes.

mod common;

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
        brand: Some(mayara::Brand::Onwa),
        targets: mayara::TargetMode::None,
        navigation_address: None,
        nmea0183: false,
        output: false,
        replay: false,
        pcap: Some("fixture".to_string()),
        // Loop the fixture: the radar only exists once discovery has run, so a
        // test that subscribes then would otherwise find the dispatcher
        // already finished and never see a spoke.
        repeat: true,
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
async fn replay_onwa_kra5001() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("onwa-kra5001.pcap.gz");
    if !fixture.exists() {
        panic!(
            "Fixture not found: {}. Run: cargo run --features pcap-replay --example generate-fixtures",
            fixture.display()
        );
    }

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
                    if let Some(info) = radars.get_keys().first().and_then(|k| radars.get_by_key(k))
                        && info.controls.model_name().is_some()
                    {
                        assert_eq!(info.brand, mayara::Brand::Onwa);
                        // Keyed on the MAC in the radar's state report.
                        assert_eq!(info.hardware_id.as_deref(), Some("00306c000029"));
                        assert_eq!(info.controls.model_name().as_deref(), Some("KRA-5001"));
                        let spokes = common::collect_spokes(
                            &info,
                            info.spokes_per_revolution as usize,
                            Duration::from_secs(10),
                        )
                        .await;
                        common::assert_spokes(&info, &spokes);
                        break;
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
