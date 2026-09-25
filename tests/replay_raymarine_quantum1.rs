#![cfg(feature = "pcap-replay")]

//! Integration test: replay the Raymarine Quantum 1 (Q24C) pcap fixture.
//!
//! This radar announces its identity with 56-byte beacon subtype 0x4c rather
//! than the 0x66 a Quantum 2 sends, which is why it went undiscovered before
//! MarineYachtRadar/mayara-server#701. Replaying the capture proves the whole
//! pipeline — beacon pair, link_id registration, model identification from the
//! E70210 part number — works for this generation.

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
async fn replay_raymarine_quantum1() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("raymarine-quantum1.pcap.gz");
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
                            assert_eq!(info.brand, mayara::Brand::Raymarine);
                            // The key is the identity beacon's link_id. Getting
                            // here at all means the 0x4c identity registered it
                            // and the 0x28 address beacon was not discarded.
                            assert_eq!(key, "ray3937", "key must come from link_id 0xCB823937");
                            assert_eq!(
                                info.controls.model_name().unwrap(),
                                "Quantum Q24C",
                                "E70210 must resolve to the Q24C"
                            );
                            assert_eq!(info.serial_no.as_deref(), Some("0370569"));
                            assert!(
                                !info.doppler,
                                "a Q24C has no Doppler; features were 0x00001900"
                            );
                            // ...so it must not be offered the control either.
                            // The unit tests cover each half of this chain; the
                            // radar proves the whole of it.
                            assert!(
                                info.controls.get(&ControlId::Doppler).is_none(),
                                "a radar without Doppler must not be offered the control"
                            );
                            // In this capture the 0x280007 features report
                            // arrives BEFORE the 0x280001 info report, so this
                            // is the order in which the radar's own word has to
                            // survive the part-number table (#709). The Q24D
                            // fixture in replay_raymarine.rs covers the reverse
                            // order, where the table lands first.
                            let legend = info.get_legend();
                            assert!(
                                legend.doppler_approaching.is_none()
                                    && legend.doppler_receding.is_none(),
                                "a radar without Doppler needs no Doppler legend entries"
                            );
                            assert_eq!(info.spokes_per_revolution, 250);
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
