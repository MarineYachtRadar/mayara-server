#![cfg(feature = "pcap-replay")]

//! ARPA regression rig: a Furuno DRS4D-NXT at anchor with a guard zone all
//! round, where every echo stands still.
//!
//! Range A runs with the settings it was recorded with: Target Analyzer off,
//! ARPA Max Speed Medium, Doppler Auto Track on and guard zone 1 all round
//! from 300 to 2,500 m. Tracks on moored boats showed 20–50 kn (#849), and
//! some jumped onto an echo beside own ship and raised CPA 0 alarms (#848).
//! The scores must match the checked-in baseline: worse fails, and so does
//! better, until `UPDATE_ARPA_BASELINE=1` rewrites it.
//!
//! The recording lives in the radar-recordings repository, next to this one
//! or wherever `MAYARA_RADAR_RECORDINGS` points; without it the test is
//! skipped, unless `MAYARA_REQUIRE_RECORDINGS` asks for it.

#[path = "arpa/drs4dnxt_guard_zone.rs"]
mod drs4dnxt_guard_zone;

use drs4dnxt_guard_zone::set;
use mayara::radar::RadarInfo;
use mayara::radar::settings::ControlId;
use serde_json::json;
use std::f64::consts::PI;

fn configure(a: &RadarInfo, _b: &RadarInfo) {
    set(a, ControlId::ArpaDetectMaxSpeed, json!({ "value": 1 }));
    set(a, ControlId::DopplerAutoTrack, json!({ "value": 1 }));
    set(
        a,
        ControlId::GuardZone1,
        json!({ "value": -PI, "endValue": PI,
                "startDistance": 300.0, "endDistance": 2500.0, "enabled": true }),
    );
}

#[tokio::test]
async fn arpa_holds_still_echoes_at_anchor_as_well_as_before() {
    drs4dnxt_guard_zone::run(
        configure,
        "tests/arpa/furuno-drs4dnxt-guard-zone-baseline.json",
    )
    .await;
}
