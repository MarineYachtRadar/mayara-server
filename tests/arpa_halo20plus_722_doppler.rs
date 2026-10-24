#![cfg(feature = "pcap-replay")]

//! The #722 voyage with no guard zones, acquiring by Doppler alone: every
//! echo that approaches or recedes becomes a target wherever it is, as with
//! the radar's own auto-acquire. Both moving vessels are Doppler echoes, the
//! sailboat approaching and the motor yacht receding; the clutter, land and
//! the fixed point are not.

#[path = "arpa/halo722.rs"]
mod halo722;

use halo722::set;
use mayara::radar::RadarInfo;
use mayara::radar::settings::ControlId;
use serde_json::json;

fn configure(_a: &RadarInfo, b: &RadarInfo) {
    set(b, ControlId::ArpaDetectMaxSpeed, json!({ "value": 2 }));
    set(b, ControlId::DopplerAutoTrack, json!({ "value": 1 }));
}

#[tokio::test]
async fn doppler_alone_tracks_both_moving_vessels_of_the_722_voyage() {
    halo722::run(configure, "tests/arpa/halo20plus-722-doppler-baseline.json").await;
}
