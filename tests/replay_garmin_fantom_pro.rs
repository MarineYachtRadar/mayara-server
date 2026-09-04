#![cfg(feature = "pcap-replay")]

//! Integration test: replay a Garmin Fantom Pro running in dual-range mode.
//!
//! The one fixture whose radar transmits on both ranges, so it is the only
//! automated cover for Range B: that it registers alongside Range A, and that
//! spokes actually reach it.

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

                // And spokes reach both of them.
                let mut spokes: Vec<_> = keys
                    .iter()
                    .map(|k| {
                        radars
                            .get_by_key(k)
                            .expect("radar info")
                            .message_tx
                            .subscribe()
                    })
                    .collect();

                for (key, rx) in keys.iter().zip(spokes.iter_mut()) {
                    let waited = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
                    assert!(
                        waited.is_ok_and(|r| r.is_ok()),
                        "no spokes arrived for {key}"
                    );
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
