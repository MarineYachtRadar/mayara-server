//! The #722 HALO20+ voyage off Fiji, replayed and scored against AIS; see
//! the scenario files `tests/arpa_halo20plus_722*.rs` that run it.

#[path = "rig.rs"]
mod rig;

use mayara::radar::RadarInfo;
use std::time::Duration;

pub(crate) use rig::set;

const VOYAGE: rig::Voyage = rig::Voyage {
    dir: "navico/halo",
    pcap: "halo20plus-722.pcap.gz",
    own_ship: "halo20plus-722-own-ship.csv",
    ais: "halo20plus-722-ais.csv",
    brand: mayara::Brand::Navico,
    // The radar was in standby for the first 15 s of the capture: reports,
    // no spokes.
    discovery: Duration::from_secs(10),
    radar_a: "nav6820A",
    // The radar of the two that ran ARPA, at 7,408 m.
    radar_b: "nav6820B",
    scored: "nav6820B",
    vessels: &[
        // The sailing yacht RAIJA, closing from 6.6 to 3.7 km.
        ("sailboat", &[269122560]),
        // NEW DREAM and her tender 92 m astern on the line of sight: one echo.
        ("pair", &[538071225, 985380226]),
    ],
    // Mayara reads ranges 90–165 m short here, and a beam is ~200 m wide at
    // 5 km.
    association_m: 300.0,
    // On this voyage, a track that far from every vessel is the coast.
    land_m: 2000.0,
    in_view_m: 7000.0,
};

/// Plays the voyage with `configure` applied to ranges A and B once they are
/// found, and holds the range B scores to `baseline`.
pub(crate) async fn run(configure: fn(&RadarInfo, &RadarInfo), baseline: &'static str) {
    rig::run(&VOYAGE, configure, baseline).await;
}
