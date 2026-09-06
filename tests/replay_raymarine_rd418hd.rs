#![cfg(feature = "pcap-replay")]

//! Integration test: replay Raymarine RD418HD radome pcap fixture.
//!
//! The fixture contains an E92142 (4kW 18" HD Color Radome) announcing with
//! the 56-byte "Digital Radar" identity beacon (subtype 0x0a) and its
//! report/spoke stream. This radar sends no 0x010006 info or 0x010002 fixed
//! reports; replaying it must discover the radar from the identity beacon,
//! identify the model from the 0x018701 HD info report, and initialize the
//! ranges from the 0x018801 status report.

use mayara::radar::Power;
use mayara::radar::settings::ControlId;
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
async fn replay_raymarine_rd418hd() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("raymarine-rd418hd.pcap.gz");
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

                        // The capture switches the radar to transmit at
                        // t=3.0; the HD status report carries the power
                        // state at byte 124, so Power must reach Transmit.
                        let power = info
                            .controls
                            .get(&ControlId::Power)
                            .and_then(|c| c.value())
                            .and_then(|v| v.as_f64());

                        // Wait until the model has been identified
                        if info.controls.model_name() == Some("RD418HD".to_string())
                            && !info.ranges.all.is_empty()
                            && power == Some(Power::Transmit as i32 as f64)
                        {
                            assert!(
                                key.starts_with("ray"),
                                "expected Raymarine key, got: {}",
                                key
                            );
                            assert_eq!(info.brand, mayara::Brand::Raymarine);
                            assert_eq!(info.serial_no.as_deref(), Some("9137606"));
                            assert_eq!(
                                info.spokes_per_revolution, 2048,
                                "RD HD radars sweep 2048 azimuths per revolution"
                            );
                            assert_eq!(
                                info.max_spoke_len, 1024,
                                "RD418HD spokes carry 1024 samples"
                            );
                            assert_eq!(
                                info.report_addr,
                                SocketAddrV4::new(Ipv4Addr::new(224, 106, 90, 66), 2572)
                            );
                            assert_eq!(
                                info.send_command_addr,
                                SocketAddrV4::new(Ipv4Addr::new(10, 3, 82, 210), 2573)
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
