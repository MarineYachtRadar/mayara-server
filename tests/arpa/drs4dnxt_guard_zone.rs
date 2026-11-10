//! A DRS4D-NXT at anchor with a guard zone all round, replayed and scored
//! against AIS; see the scenario file `tests/arpa_furuno_drs4dnxt_guard_zone.rs`
//! that runs it.
//!
//! Three minutes at the same anchorage as the approaching recording, on
//! 2026-10-29, two hours earlier. Range A is at 1.5 NM with Target Analyzer
//! off; range B is in standby. Nothing within 6 km moves faster than 1 kn,
//! so every track should be still. The echoes are moored boats, two of them
//! with AIS, the reef and the shore.

#[path = "rig.rs"]
mod rig;

use mayara::radar::RadarInfo;
use std::time::Duration;

pub(crate) use rig::set;

const VOYAGE: rig::Voyage = rig::Voyage {
    dir: "furuno/moin",
    pcap: "drs4dnxt-anchor-guard-zone.pcap.gz",
    own_ship: "drs4dnxt-anchor-guard-zone-own-ship.csv",
    ais: "drs4dnxt-anchor-guard-zone-ais.csv",
    brand: mayara::Brand::Furuno,
    // The capture starts with a mayara restart; the radar answers the
    // beacon request within a few seconds.
    discovery: Duration::from_secs(10),
    radar_a: "fur6424A",
    radar_b: "fur6424B",
    scored: "fur6424A",
    vessels: &[
        // KIAPA NUI, 141 m off, inside the guard zone's inner edge.
        ("kiapa_nui", &[367589460]),
        // No name in its AIS, 383 m off.
        ("mmsi_368383510", &[368383510]),
    ],
    // As for the approaching recording: the moored boats lie no closer than
    // 140 m to each other.
    association_m: 200.0,
    // The reef and the shore are 1–2.5 km away.
    land_m: 1000.0,
    // Range A shows 2,778 m.
    in_view_m: 2700.0,
};

/// Plays the recording with `configure` applied to ranges A and B once they
/// are found, and holds the range A scores to `baseline`.
pub(crate) async fn run(configure: fn(&RadarInfo, &RadarInfo), baseline: &'static str) {
    rig::run(&VOYAGE, configure, baseline).await;
}
