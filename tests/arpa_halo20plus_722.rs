#![cfg(feature = "pcap-replay")]

//! ARPA regression rig: the #722 HALO20+ voyage, tracked against AIS.
//!
//! Thirteen minutes under way off Fiji, recorded by the reporter of #722 on
//! the radar network and the NMEA 2000 bus at once. The radar is replayed on
//! the capture's own clock, own ship sails the recorded voyage, and every
//! track mayara reports on range B is scored against the AIS positions of the
//! vessels around it. The scores must match the checked-in baseline: worse
//! fails, and so does better, until `UPDATE_ARPA_BASELINE=1` rewrites it and
//! the improvement becomes the score to hold.
//!
//! The recording lives in the radar-recordings repository, next to this one
//! or wherever `MAYARA_RADAR_RECORDINGS` points; without it the test is
//! skipped, unless `MAYARA_REQUIRE_RECORDINGS` asks for it.

#[path = "arpa/halo722.rs"]
mod halo722;

use halo722::set;
use mayara::radar::RadarInfo;
use mayara::radar::settings::ControlId;
use serde_json::json;

/// The reporter's settings for both ranges, unchanged through the capture.
fn configure(a: &RadarInfo, b: &RadarInfo) {
    let zone = |start: f64, end: f64| {
        json!({ "value": 1f64.to_radians(), "endValue": 358f64.to_radians(),
                "startDistance": start, "endDistance": end, "enabled": true })
    };
    set(a, ControlId::GuardZone1, zone(1000.0, 1500.0));
    set(b, ControlId::GuardZone1, zone(5000.0, 6000.0));
    for info in [a, b] {
        set(info, ControlId::ArpaDetectMaxSpeed, json!({ "value": 2 }));
    }
    set(b, ControlId::DopplerAutoTrack, json!({ "value": 1 }));
}

#[tokio::test]
async fn arpa_tracks_the_722_voyage_as_well_as_before() {
    halo722::run(configure, "tests/arpa/halo20plus-722-baseline.json").await;
}
