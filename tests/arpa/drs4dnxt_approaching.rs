//! Two catamarans motoring in to anchor next to a DRS4D-NXT, replayed and
//! scored against AIS; see the scenario files
//! `tests/arpa_furuno_drs4dnxt_approaching*.rs` that run it.
//!
//! Twenty minutes at anchor behind a reef off Fiji, on 2026-10-29. Range A
//! is at 1.5 NM with Target Analyzer on Target, so the radar marks the echoes
//! that approach it; range B is in standby. Both boats come in at 4–5 kn
//! and anchor nearby. The other echoes are moored boats, the reef and the
//! shore, which the Target Analyzer partly paints as approaching (#832).

#[path = "rig.rs"]
mod rig;

use mayara::radar::RadarInfo;
use std::time::Duration;

pub(crate) use rig::set;

const VOYAGE: rig::Voyage = rig::Voyage {
    dir: "furuno/moin",
    pcap: "drs4dnxt-anchor-approaching.pcap.gz",
    own_ship: "drs4dnxt-anchor-approaching-own-ship.csv",
    ais: "drs4dnxt-anchor-approaching-ais.csv",
    brand: mayara::Brand::Furuno,
    // The radar transmits from the first packet, so discovery replays spokes
    // too; it lasts until the radar has answered mayara's beacon request,
    // 5 s in.
    discovery: Duration::from_secs(10),
    radar_a: "fur6424A",
    radar_b: "fur6424B",
    scored: "fur6424A",
    vessels: &[
        // WILD THING 2, closing from 576 m; it passes about 35 m off and
        // anchors at 143 m.
        ("wild_thing_2", &[228071090]),
        // OHANA II, closing from 2.2 km to about 250 m, slowing to under
        // 3 kn on the way.
        ("ohana_ii", &[347019890]),
    ],
    // Fresh AIS fixes and mayara's tracks were 110 m apart at most, and the
    // moored boats lie no closer than 140 m to each other.
    association_m: 200.0,
    // Clear of every vessel at this anchorage are the reef and the shore,
    // 1–2.5 km away.
    land_m: 1000.0,
    // Range A shows 2,778 m.
    in_view_m: 2700.0,
};

/// Plays the recording with `configure` applied to ranges A and B once they
/// are found, and holds the range A scores to `baseline`.
pub(crate) async fn run(configure: fn(&RadarInfo, &RadarInfo), baseline: &'static str) {
    rig::run(&VOYAGE, configure, baseline).await;
}
