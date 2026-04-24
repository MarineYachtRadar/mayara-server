//! Generate filtered pcap fixtures for replay integration tests.
//!
//! Reads full radar captures from the radar-recordings repository and
//! extracts the relevant packets for each brand into small fixture files
//! under `testdata/pcap/`.
//!
//! Usage:
//!     cargo run --features pcap-replay --example generate-fixtures
//!
//! Set RADAR_RECORDINGS to override the recordings path (default: sibling
//! `radar-recordings/` directory).

use std::path::Path;

use mayara::pcap::{PcapPacket, parse_file, write_file};

fn main() {
    let recordings = std::env::var("RADAR_RECORDINGS").unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("radar-recordings")
            .to_string_lossy()
            .into_owned()
    });
    let base = Path::new(&recordings);

    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("pcap");
    std::fs::create_dir_all(&fixture_dir).expect("create testdata/pcap");

    // Raymarine: beacons (224.0.0.1:5800) + report data (232.1.160.1:2574)
    generate_fixture(
        &base.join("raymarine/Quantum2/pelagia/raymarine1.pcap.gz"),
        &fixture_dir.join("raymarine-quantum.pcap.gz"),
        &|p| p.dst_addr.port() == 5800 || p.dst_addr.port() == 2574,
        500,
    );

    // Navico: discovery beacons (236.6.7.5:6878 or 236.6.7.4:6768) +
    // report/spoke data (varies per beacon, but common are 236.6.7.x ports)
    generate_fixture(
        &base.join("navico/4g/4g-boot-with-opencpn.pcap.gz"),
        &fixture_dir.join("navico-4g.pcap.gz"),
        &navico_filter,
        500,
    );

    // Garmin: CDM heartbeat (239.254.2.2:50050) + reports (239.254.2.0:50100) +
    // spoke data (239.254.2.0:50102)
    generate_fixture(
        &base.join("garmin/garmin_xhd.pcap.gz"),
        &fixture_dir.join("garmin-xhd.pcap.gz"),
        &|p| {
            let port = p.dst_addr.port();
            port == 50050 || port == 50100 || port == 50102
        },
        500,
    );

    // Furuno: beacons (172.31.255.255:10010) + multicast data (239.255.0.2:10024) +
    // status reports (172.31.255.255:10034)
    generate_fixture(
        &base.join("furuno/moin/furuno1.pcap.gz"),
        &fixture_dir.join("furuno-drs4dnxt.pcap.gz"),
        &|p| {
            let port = p.dst_addr.port();
            port == 10010 || port == 10024 || port == 10034
        },
        500,
    );

    // Navico BR24
    generate_fixture(
        &base.join("navico/br24/northstar/br24_davy.pcap.gz"),
        &fixture_dir.join("navico-br24.pcap.gz"),
        &navico_filter,
        500,
    );

    // Navico HALO20+ (halo20+.pcap.gz has C403+C409, halo20plus_willem does not)
    // Needs >1072 packets — the first Gen3+ beacon response arrives late
    generate_fixture(
        &base.join("navico/halo/halo20+.pcap.gz"),
        &fixture_dir.join("navico-halo20plus.pcap.gz"),
        &navico_filter,
        2000,
    );

    // Navico HALO24
    generate_fixture(
        &base.join("navico/halo/halo24-with-mayara.pcap.gz"),
        &fixture_dir.join("navico-halo24.pcap.gz"),
        &navico_filter,
        2000,
    );

    // Navico HALO3006
    generate_fixture(
        &base.join("navico/halo/halo-3006.pcap.gz"),
        &fixture_dir.join("navico-halo3006.pcap.gz"),
        &navico_filter,
        2000,
    );

    println!("Fixtures generated in {}", fixture_dir.display());
}

fn navico_filter(p: &PcapPacket) -> bool {
    let ip = p.dst_addr.ip().octets();
    // Navico multicast 236.6.x.x (discovery + reports + spokes)
    // plus 239.238.55.x (HALO heading info)
    (ip[0] == 236 && ip[1] == 6) || (ip[0] == 239 && ip[1] == 238 && ip[2] == 55)
}

fn generate_fixture(
    src: &Path,
    dst: &Path,
    filter: &dyn Fn(&PcapPacket) -> bool,
    max_packets: usize,
) {
    if !src.exists() {
        println!("SKIP (not found): {}", src.display());
        return;
    }
    let packets = parse_file(src).expect("parse source");
    let filtered: Vec<_> = packets
        .into_iter()
        .filter(|p| filter(p))
        .take(max_packets)
        .collect();
    println!(
        "{}: {} -> {} packets",
        dst.file_name().unwrap().to_string_lossy(),
        src.display(),
        filtered.len()
    );
    write_file(dst, &filtered).expect("write fixture");
}
