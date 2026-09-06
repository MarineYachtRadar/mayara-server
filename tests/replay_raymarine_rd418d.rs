#![cfg(feature = "pcap-replay")]

//! Integration test: replay Raymarine RD418D radome pcap fixture.
//!
//! The fixture (MarineYachtRadar/mayara-server#419) contains the radome's
//! discovery beacons plus its report/spoke stream. Replaying it must
//! discover the radar from the "Ethernet Dome" identity beacon, identify
//! the model from the info report's concatenated model+serial field, and
//! initialize the ranges from the status report.

mod common;

use mayara::{Cli, replay};
use std::net::{Ipv4Addr, SocketAddrV4};
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
async fn replay_raymarine_rd418d() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("raymarine-rd418d.pcap.gz");
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
                        if info.controls.model_name() == Some("RD418D".to_string())
                            && !info.ranges.all.is_empty()
                        {
                            assert!(
                                key.starts_with("ray"),
                                "expected Raymarine key, got: {}",
                                key
                            );
                            assert_eq!(info.brand, mayara::Brand::Raymarine);
                            assert_eq!(info.serial_no.as_deref(), Some("9222176"));
                            assert_eq!(
                                info.spokes_per_revolution, 2048,
                                "RD418D azimuths run 0..2047; the info report must not clobber the spoke count"
                            );
                            assert_eq!(info.max_spoke_len, 512);
                            assert_eq!(
                                info.report_addr,
                                SocketAddrV4::new(Ipv4Addr::new(226, 77, 83, 98), 2572)
                            );
                            assert_eq!(
                                info.send_command_addr,
                                SocketAddrV4::new(Ipv4Addr::new(10, 18, 106, 155), 2573)
                            );
                            let spokes = common::collect_spokes(
                                &info,
                                info.spokes_per_revolution as usize,
                                Duration::from_secs(10),
                            )
                            .await;
                            common::assert_spokes(&info, &spokes);

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
