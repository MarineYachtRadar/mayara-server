extern crate tokio;

use clap::Parser;
use env_logger::Env;
use log::{info, warn};
use miette::{IntoDiagnostic, Result};
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

mod web;

use mayara;
use mayara::{Cli, network, replay};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    // Handle --openapi flag: output OpenAPI spec and exit
    if args.openapi {
        println!("{}", web::generate_openapi_json());
        return Ok(());
    }

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        // Only log mayara and mayara_server modules at the selected level
        .filter_module("mayara_server", log_level)
        .filter_module("mayara", log_level)
        .filter_module("tower_http", log_level)
        .init();

    network::set_replay(args.is_replay());

    if let Some(pcap_path) = args.pcap_file() {
        replay::init(std::path::Path::new(pcap_path)).into_diagnostic()?;
    }

    info!("Mayara {} loglevel {}", mayara::VERSION, log_level);
    if args.is_replay() {
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
        let web = Web::new(&s, args).await;
        s.start(SubsystemBuilder::new("Webserver", move |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
