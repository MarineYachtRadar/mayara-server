#![cfg(feature = "pcap-replay")]

//! Integration test: replay Raymarine Quantum pcap fixture.
//!
//! Verifies that replaying the fixture through the full pipeline
//! detects the radar with the correct brand, model, and capabilities.

use mayara::radar::settings::ControlId;
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
        brand: Some(mayara::Brand::Raymarine),
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
async fn replay_raymarine_quantum() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("raymarine-quantum.pcap.gz");
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
                    let keys = radars.get_keys();
                    if !keys.is_empty() {
                        let key = &keys[0];
                        let info = radars.get_by_key(key).expect("radar info");

                        // Wait until the model has been identified
                        if info.controls.model_name().is_some() && !info.ranges.all.is_empty() {
                            assert!(
                                key.starts_with("ray"),
                                "expected Raymarine key, got: {}",
                                key
                            );
                            assert_eq!(info.brand, mayara::Brand::Raymarine);
                            let model = info.controls.model_name().unwrap();
                            assert!(
                                model.contains("Quantum"),
                                "expected Quantum model, got: {}",
                                model
                            );
                            assert!(info.doppler, "Quantum should support Doppler");
                            // A Q24D reports Doppler, so the control must be
                            // offered. The absence case is unit-tested: the
                            // part number decides the capability
                            // (part_number_decides_doppler_capability) and the
                            // capability decides the control
                            // (doppler_is_offered_only_to_radars_that_have_it).
                            assert!(
                                info.controls.get(&ControlId::Doppler).is_some(),
                                "a Doppler-capable Quantum must offer the Doppler control"
                            );
                            // In this capture the 0x280001 info report and the
                            // first 0x280002 status report both arrive before
                            // the 0x280007 features report, so that status
                            // report is held back and a later one publishes the
                            // radar (#714). The Q24C fixture covers the order
                            // where the features report arrives first.
                            //
                            // This radar's features value agrees with its part
                            // number, so the replay cannot show which of the two
                            // the capability came from — only that the ordering
                            // works and the control and legend end up right.
                            // Precedence itself is covered by
                            // effective_doppler's tests and by the two
                            // a_features_report_*_publication receiver tests.
                            let legend = info.get_legend();
                            assert!(
                                legend.doppler_approaching.is_some()
                                    && legend.doppler_receding.is_some(),
                                "a Doppler radar needs its Doppler legend entries"
                            );
                            assert_eq!(info.spokes_per_revolution, 250);
                            // Identity and serial come from different places
                            // and must not be confused. The key is the
                            // beacons' link_id (0xd68168b4), which survives a
                            // change of address; the serial is the real one
                            // off the 0x280001 info report, for display.
                            assert_eq!(key, "ray68b4", "key must come from link_id");
                            assert_eq!(
                                info.serial_no.as_deref(),
                                Some("1140360"),
                                "the radar's real serial must be reported"
                            );
                            assert!(
                                info.controls.user_name().contains("1140360"),
                                "the serial should reach the user-visible name, got: {}",
                                info.controls.user_name()
                            );
                            // raymarine-quantum.pcap.gz carries no spoke datagrams. Raymarine
                            // spoke decoding is covered by e120, rd418d and rd418hd.

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
