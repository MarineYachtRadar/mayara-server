#![cfg(feature = "pcap-replay")]

//! Integration test: replay a Garmin Fantom Pro running in dual-range mode.
//!
//! The one fixture whose radar transmits on both ranges, so it is the only
//! automated cover for Range B: that it registers alongside Range A, that
//! spokes actually reach it, and that it reports transmitting and stays
//! alive on the report stream it shares with Range A.

mod common;

use mayara::radar::Power;
use mayara::radar::settings::ControlId;
use mayara::{Cli, replay};
use std::path::Path;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};

/// How long the radar watchdog lets a radar stay silent before setting it
/// Off, and how often it checks (`SharedRadars::RADAR_SILENCE_TIMEOUT` and the
/// watchdog interval, both private to the library).
const RADAR_SILENCE_TIMEOUT: Duration = Duration::from_secs(30);
const WATCHDOG_PERIOD: Duration = Duration::from_secs(5);

fn test_args() -> Cli {
    Cli {
        verbose: <clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>>::default(),
        port: 0,
        tls_cert: None,
        tls_key: None,
        parent: None,
        interface: None,
        brand: Some(mayara::Brand::Garmin),
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
        emulator_sparse_spokes: false,
        emulator_land: None,
        merge_targets: false,
        no_websocket_compression: false,
        mdns_hostname: None,
        no_telemetry: false,
        no_mdns: true,
        pcap_max_time: None,
    }
}

#[tokio::test]
async fn replay_garmin_fantom_pro_dual_range() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap")
        .join("garmin-fantom-pro.pcap.gz");
    if !fixture.exists() {
        panic!(
            "Fixture not found: {}. Run: cargo run --features pcap-replay --example generate-fixtures",
            fixture.display()
        );
    }

    let _ = env_logger::builder().is_test(true).try_init();
    replay::init(&fixture).expect("init replay");
    // Deliberately not instant: the fixture's own 6.5s of timing is what keeps
    // spokes arriving after the radars have registered and this test has
    // subscribed. Dispatched instantly they could all be gone before there is
    // a radar to subscribe to, and a broadcast receiver never sees what was
    // sent before it.
    let args = test_args();

    Toplevel::new(async move |s: &mut SubsystemHandle| {
        let (radars, _) = mayara::start_session(s, args).await;

        s.start(SubsystemBuilder::new(
            "test",
            async move |subsys: &mut SubsystemHandle| {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

                // Both ranges of a dual-range radar register.
                let keys = loop {
                    let keys = radars.get_keys();
                    if keys.len() == 2 {
                        break keys;
                    }
                    if tokio::time::Instant::now() > deadline {
                        panic!("Timeout: expected two ranges, got {:?}", radars.get_keys());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                };
                assert!(keys[0].ends_with('A') && keys[1].ends_with('B'), "{keys:?}");
                assert_eq!(
                    keys[0][..keys[0].len() - 1],
                    keys[1][..keys[1].len() - 1],
                    "the two ranges are one radar"
                );
                let registered = tokio::time::Instant::now();

                // And each of them decodes a picture of its own. Range A and
                // Range B run at different ranges, which is what makes this the
                // fixture that would catch a decoder mixing the two up.
                for key in &keys {
                    let info = radars.get_by_key(key).expect("radar info");
                    let spokes = common::collect_spokes(
                        &info,
                        info.spokes_per_revolution as usize,
                        Duration::from_secs(10),
                    )
                    .await;
                    common::assert_spokes(&info, &spokes);
                }

                // The scanner reports transmit once for both ranges; Range B
                // must show it too, or clients treat a painting range as off.
                // The capture switches to transmit 2.5 s into each loop.
                let transmit = Some(Power::Transmit as i32 as f64);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                for key in &keys {
                    loop {
                        let power = radars
                            .get_by_key(key)
                            .expect("radar info")
                            .controls
                            .get(&ControlId::Power)
                            .and_then(|c| c.value())
                            .and_then(|v| v.as_f64());
                        if power == transmit {
                            break;
                        }
                        if tokio::time::Instant::now() > deadline {
                            panic!("{key}: power {power:?}, expected Transmit");
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }

                // Both ranges share one report stream. Once the watchdog's
                // silence timeout has passed, a range the stream does not
                // keep alive is set to Off on every watchdog tick, so watch
                // both for longer than a tick.
                tokio::time::sleep_until(registered + RADAR_SILENCE_TIMEOUT).await;
                let watch_until = tokio::time::Instant::now() + WATCHDOG_PERIOD * 2;
                let off = Some(Power::Off as i32 as f64);
                while tokio::time::Instant::now() < watch_until {
                    for key in &keys {
                        let power = radars
                            .get_by_key(key)
                            .expect("radar info")
                            .controls
                            .get(&ControlId::Power)
                            .and_then(|c| c.value())
                            .and_then(|v| v.as_f64());
                        assert_ne!(power, off, "{key}: set Off while reports keep coming");
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
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
