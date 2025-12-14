extern crate tokio;

use clap::Parser;
use env_logger::Env;
use log::{info, warn};
use miette::Result;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

mod web;

use mayara_server::{network, Cli, Session, VERSION};

fn main() -> Result<()> {
    // Build tokio runtime with larger stack size for worker threads
    // to avoid stack overflow when building complex capability manifests
    // 16MB is needed for debug builds with many control definitions
    tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(16 * 1024 * 1024) // 16MB stack
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .filter_module("tungstenite", log::LevelFilter::Info)
        .filter_module("mdns_sd", log::LevelFilter::Info)
        .filter_module("polling", log::LevelFilter::Info)
        .init();

    network::set_replay(args.replay);

    info!("Mayara {} loglevel {}", VERSION, log_level);
    if args.replay {
        warn!("Replay mode activated, this does the following:");
        warn!(" * A circle is drawn at the last two pixels in each spoke");
        warn!(" * Timestamp on each spoke is as if received now");
        warn!(" * Any dual range of the radar is ignored and not reported");
    }
    if args.fake_errors {
        warn!("Fake error mode activated, this does the following:");
        warn!(" * Any control operation on Rain Clutter beyond values 0..10 will fail");
        warn!(" * Failure for value 11..13 are all different");
    }
    if args.allow_wifi {
        warn!("Allow WiFi mode activated, this does the following:");
        warn!(" * Radars will be detected even on WiFi interfaces");
    }
    if args.output {
        warn!("Output mode activated; 'protobuf' formatted RadarMessage sent to stdout");
    }
    if args.nmea0183 {
        warn!(
            "NMEA0183 mode activated; will load GPS position, heading and date/time from {}",
            args.navigation_address
                .as_ref()
                .unwrap_or(&"MDNS".to_string())
        );
    }

    Toplevel::new(|s| async move {
        let session = Session::new(&s, args).await;
        let web = Web::new(session.clone());
        s.start(SubsystemBuilder::new("Webserver", move |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
