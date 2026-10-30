#![cfg(feature = "pcap-replay")]

//! ARPA regression rig: two boats approaching a Furuno DRS4D-NXT at anchor,
//! tracked against AIS.
//!
//! Range A runs with the settings it was recorded with: Target Analyzer on
//! Target, ARPA Max Speed Medium, and Doppler Auto Track on with no guard
//! zones, so every echo the radar marks as approaching may
//! become a target. The scores must match the checked-in baseline: worse
//! fails, and so does better, until `UPDATE_ARPA_BASELINE=1` rewrites it.
//!
//! The recording lives in the radar-recordings repository, next to this one
//! or wherever `MAYARA_RADAR_RECORDINGS` points; without it the test is
//! skipped, unless `MAYARA_REQUIRE_RECORDINGS` asks for it.

#[path = "arpa/drs4dnxt_approaching.rs"]
mod drs4dnxt_approaching;

use drs4dnxt_approaching::set;
use mayara::radar::RadarInfo;
use mayara::radar::settings::ControlId;
use serde_json::json;

fn configure(a: &RadarInfo, _b: &RadarInfo) {
    // Nothing in the capture says Target Analyzer was on; setting it tells
    // the replay to decode the spokes as Doppler.
    set(a, ControlId::Doppler, json!({ "value": 1 }));
    set(a, ControlId::ArpaDetectMaxSpeed, json!({ "value": 1 }));
    set(a, ControlId::DopplerAutoTrack, json!({ "value": 1 }));
}

#[tokio::test]
async fn doppler_tracks_both_boats_approaching_a_drs4d_nxt() {
    drs4dnxt_approaching::run(
        configure,
        "tests/arpa/furuno-drs4dnxt-approaching-baseline.json",
    )
    .await;
}
