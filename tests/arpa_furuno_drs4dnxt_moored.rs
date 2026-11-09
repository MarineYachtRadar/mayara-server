#![cfg(feature = "pcap-replay")]

//! ARPA regression rig: boats moored around a Furuno DRS4D-NXT at anchor,
//! acquired by a guard zone while Target Analyzer is on.
//!
//! Range A runs with Target Analyzer on Target, ARPA Max Speed Medium and a
//! guard zone all round from 100 to 500 m, with Doppler Auto Track off, so
//! only the guard zone acquires. The radar marks a moored boat's echo as
//! stationary, and those echoes must still reach ARPA (#831). The scores
//! must match the checked-in baseline: worse fails, and so does better,
//! until `UPDATE_ARPA_BASELINE=1` rewrites it.
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
use std::f64::consts::PI;

fn configure(a: &RadarInfo, _b: &RadarInfo) {
    set(a, ControlId::Doppler, json!({ "value": 1 }));
    set(a, ControlId::ArpaDetectMaxSpeed, json!({ "value": 1 }));
    set(
        a,
        ControlId::GuardZone1,
        json!({ "value": -PI, "endValue": PI,
                "startDistance": 100.0, "endDistance": 500.0, "enabled": true }),
    );
}

#[tokio::test]
async fn a_guard_zone_holds_the_moored_boats_with_target_analyzer_on() {
    drs4dnxt_approaching::run_moored(configure, "tests/arpa/furuno-drs4dnxt-moored-baseline.json")
        .await;
}
