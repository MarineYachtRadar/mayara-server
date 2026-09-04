#![cfg(feature = "pcap-replay")]

//! Integration test: replay a Raymarine E120 Classic pcap fixture.
//!
//! The fixture (MarineYachtRadar/mayara-server#579) is an E-Series Classic
//! MFD publishing a cabled analogue Pathfinder scanner over SeaTalkHS. Its
//! `0x010006` info report is zero-filled apart from the message id, so it
//! names neither a model nor a serial. Replaying it must still discover the
//! radar, identify it as a SeaTalkHS radar, and initialize the ranges from
//! the status report — the whole receiver state machine is gated behind the
//! info report, so a rejected one leaves the radar found but invisible.

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
async fn replay_raymarine_e120_classic() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("raymarine-e120-classic.pcap.gz");
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
                        if info.controls.model_name() == Some("SeaTalkHS".to_string())
                            && !info.ranges.all.is_empty()
                        {
                            assert_eq!(info.brand, mayara::Brand::Raymarine);
                            assert_eq!(
                                info.serial_no, None,
                                "the info report carries no serial; an empty field must not become one"
                            );
                            assert_eq!(
                                key, "rayda27",
                                "with no serial the key must fall back to the beacon link_id, not the IP"
                            );
                            assert_eq!(
                                info.spokes_per_revolution, 2048,
                                "azimuths run 0..2047 even though spokes carry 512 samples"
                            );
                            assert_eq!(info.max_spoke_len, 512);
                            assert_eq!(
                                info.report_addr,
                                SocketAddrV4::new(Ipv4Addr::new(224, 28, 237, 35), 2562)
                            );
                            assert_eq!(
                                info.send_command_addr,
                                SocketAddrV4::new(Ipv4Addr::new(10, 0, 231, 105), 2052)
                            );
                            let spokes = common::collect_spokes(
                                &info,
                                info.spokes_per_revolution as usize,
                                Duration::from_secs(10),
                            )
                            .await;
                            // this capture holds most of a revolution.
                            common::assert_spokes(&info, &spokes, 0.70);

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
