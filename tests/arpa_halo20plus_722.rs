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

use mayara::radar::settings::{BareControlValue, ControlId, ControlValue};
use mayara::replay::{self, NavFix};
use mayara::{Cli, TargetMode};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};

const RECORDING_DIR: &str = "navico/halo";
const PCAP: &str = "halo20plus-722.pcap.gz";
const OWN_SHIP: &str = "halo20plus-722-own-ship.csv";
const AIS: &str = "halo20plus-722-ais.csv";
const BASELINE: &str = "tests/arpa/halo20plus-722-baseline.json";
/// The radar was in standby for the first 15 s of the capture: reports,
/// no spokes.
const DISCOVERY: Duration = Duration::from_secs(10);

/// The radar of the two that ran ARPA, at 7,408 m.
const RADAR_B: &str = "nav6820B";
const RADAR_A: &str = "nav6820A";

/// The sailing yacht RAIJA, closing from 6.6 to 3.7 km.
const SAILBOAT: &[u32] = &[269122560];
/// NEW DREAM and her tender 92 m astern on the line of sight: one echo.
const PAIR: &[u32] = &[538071225, 985380226];

/// How far a track may be from an AIS position and still be that vessel.
/// Mayara reads ranges 90–165 m short here, and a beam is ~200 m wide at
/// 5 km.
const ASSOCIATION_M: f64 = 300.0;
/// A vessel counts as in view out to this range on the 7,408 m radar.
const IN_VIEW_M: f64 = 7000.0;
/// How far an AIS track is extrapolated past its last report.
const AIS_COAST_MS: u64 = 120_000;
/// A track's speed is judged once it has had this long to settle.
const SETTLED_MS: u64 = 60_000;
/// Coverage is counted in bins of this length.
const COVERAGE_BIN_MS: u64 = 10_000;
const EARTH_RADIUS_M: f64 = 6_371_000.0;

fn recordings() -> Option<PathBuf> {
    let dir = std::env::var_os("MAYARA_RADAR_RECORDINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../radar-recordings"))
        .join(RECORDING_DIR);
    if dir.join(PCAP).exists() {
        return Some(dir);
    }
    if std::env::var_os("MAYARA_REQUIRE_RECORDINGS").is_some() {
        panic!("{} not found in {}", PCAP, dir.display());
    }
    eprintln!("skipping: {} not found in {}", PCAP, dir.display());
    None
}

fn test_args(pcap: &Path) -> Cli {
    Cli {
        verbose: <clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>>::default(),
        port: 0,
        tls_cert: None,
        tls_key: None,
        parent: None,
        interface: None,
        brand: Some(mayara::Brand::Navico),
        targets: TargetMode::Arpa,
        navigation_address: None,
        nmea0183: false,
        output: false,
        replay: false,
        pcap: Some(pcap.display().to_string()),
        repeat: false,
        fake_errors: false,
        allow_wifi: false,
        stationary: false,
        static_position: None,
        multiple_radar: false,
        openapi: false,
        transmit: false,
        accept_invalid_certs: false,
        signalk_token: None,
        signalk_token_file: None,
        emulator: false,
        emulator_sparse_spokes: false,
        merge_targets: false,
        no_websocket_compression: false,
        mdns_hostname: None,
        no_telemetry: false,
        no_mdns: true,
        // Ends the session once the capture has played.
        pcap_max_time: Some(u32::MAX),
    }
}

fn csv_rows(path: &Path) -> Vec<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).expect("read csv");
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().expect("csv header").split(',').collect();
    lines
        .map(|l| {
            header
                .iter()
                .map(|h| h.to_string())
                .zip(l.split(',').map(str::to_string))
                .collect()
        })
        .collect()
}

fn num(row: &HashMap<String, String>, key: &str) -> f64 {
    row[key].parse().unwrap_or(f64::NAN)
}

fn voyage(dir: &Path) -> Vec<NavFix> {
    csv_rows(&dir.join(OWN_SHIP))
        .iter()
        .map(|r| NavFix {
            time_ms: num(r, "t_ms") as u64,
            lat: num(r, "lat"),
            lon: num(r, "lon"),
            cog: num(r, "cog_deg").to_radians(),
            sog: num(r, "sog_ms"),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Fix {
    t: u64,
    lat: f64,
    lon: f64,
    cog: f64,
    sog: f64,
}

/// Every AIS vessel's reports, in time order.
fn ais(dir: &Path) -> BTreeMap<u32, Vec<Fix>> {
    let mut vessels: BTreeMap<u32, Vec<Fix>> = BTreeMap::new();
    for r in csv_rows(&dir.join(AIS)) {
        vessels
            .entry(num(&r, "mmsi") as u32)
            .or_default()
            .push(Fix {
                t: num(&r, "t_ms") as u64,
                lat: num(&r, "lat"),
                lon: num(&r, "lon"),
                cog: num(&r, "cog_deg").to_radians(),
                sog: num(&r, "sog_ms"),
            });
    }
    for fixes in vessels.values_mut() {
        fixes.sort_by_key(|f| f.t);
    }
    vessels
}

/// North and east metres from `from` to `to`.
fn offset_m(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let north = (to.0 - from.0).to_radians() * EARTH_RADIUS_M;
    let east = (to.1 - from.1).to_radians() * EARTH_RADIUS_M * from.0.to_radians().cos();
    (north, east)
}

fn moved(from: (f64, f64), north: f64, east: f64) -> (f64, f64) {
    (
        from.0 + (north / EARTH_RADIUS_M).to_degrees(),
        from.1 + (east / (EARTH_RADIUS_M * from.0.to_radians().cos())).to_degrees(),
    )
}

/// A track's position at `t`: interpolated between reports, dead-reckoned
/// past either end for a while.
fn position_at(fixes: &[Fix], t: u64) -> Option<(f64, f64)> {
    let after = fixes.partition_point(|f| f.t <= t);
    let reckon = |f: &Fix, dt_ms: f64| {
        let d = f.sog * dt_ms / 1000.0;
        moved((f.lat, f.lon), d * f.cog.cos(), d * f.cog.sin())
    };
    match (after.checked_sub(1).map(|i| &fixes[i]), fixes.get(after)) {
        (Some(a), Some(b)) => {
            let f = (t - a.t) as f64 / (b.t - a.t) as f64;
            Some((a.lat + (b.lat - a.lat) * f, a.lon + (b.lon - a.lon) * f))
        }
        (Some(a), None) if t - a.t <= AIS_COAST_MS => Some(reckon(a, (t - a.t) as f64)),
        (None, Some(b)) if b.t - t <= AIS_COAST_MS => Some(reckon(b, -((b.t - t) as f64))),
        _ => None,
    }
}

fn speed_at(fixes: &[Fix], t: u64) -> Option<f64> {
    let i = fixes.partition_point(|f| f.t <= t);
    fixes.get(i.saturating_sub(1)).map(|f| f.sog)
}

/// One report of one track, as mayara broadcast it.
#[derive(Clone, Debug)]
struct Sample {
    id: u64,
    t: u64,
    tracking: bool,
    lat: f64,
    lon: f64,
    sog: Option<f64>,
}

fn samples_from(delta: &Value, radar: &str, out: &mut Vec<Sample>) {
    let prefix = format!("radars.{radar}.targets.");
    for update in delta["updates"].as_array().into_iter().flatten() {
        for v in update["values"].as_array().into_iter().flatten() {
            let Some(id) = v["path"].as_str().and_then(|p| p.strip_prefix(&prefix)) else {
                continue;
            };
            let t = &v["value"];
            let (Some(lat), Some(lon), Some(seen)) = (
                t["position"]["latitude"].as_f64(),
                t["position"]["longitude"].as_f64(),
                t["lastSeen"].as_str(),
            ) else {
                continue;
            };
            let status = t["status"].as_str().unwrap_or("");
            if status == "lost" {
                continue;
            }
            out.push(Sample {
                id: id.parse().unwrap(),
                t: chrono::DateTime::parse_from_rfc3339(seen)
                    .unwrap()
                    .timestamp_millis() as u64,
                tracking: status == "tracking",
                lat,
                lon,
                sog: t["motion"]["speed"].as_f64(),
            });
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The scores, each with the direction that counts as better.
fn score(
    samples: &[Sample],
    vessels: &BTreeMap<u32, Vec<Fix>>,
    own: &[NavFix],
) -> BTreeMap<String, (f64, bool)> {
    let own_at = |t: u64| {
        let i = own.partition_point(|f| f.time_ms <= t).saturating_sub(1);
        (own[i].lat, own[i].lon)
    };
    // Each report, matched to the nearest vessel in reach.
    let nearest = |s: &Sample| -> Option<(u32, f64)> {
        vessels
            .iter()
            .filter_map(|(mmsi, fixes)| {
                let p = position_at(fixes, s.t)?;
                let (n, e) = offset_m(p, (s.lat, s.lon));
                Some((*mmsi, n.hypot(e)))
            })
            .filter(|(_, d)| *d <= ASSOCIATION_M)
            .min_by(|a, b| a.1.total_cmp(&b.1))
    };
    let mut first_seen: HashMap<u64, u64> = HashMap::new();
    let mut votes: BTreeMap<u64, BTreeMap<Option<u32>, usize>> = BTreeMap::new();
    let matched: Vec<(&Sample, Option<(u32, f64)>)> =
        samples.iter().map(|s| (s, nearest(s))).collect();
    for (s, m) in &matched {
        first_seen.entry(s.id).or_insert(s.t);
        if s.tracking {
            *votes
                .entry(s.id)
                .or_default()
                .entry(m.map(|m| m.0))
                .or_default() += 1;
        }
    }
    // A track is the vessel most of its tracking reports were near; a tie
    // goes to a vessel rather than to none.
    let label: BTreeMap<u64, Option<u32>> = votes
        .iter()
        .map(|(id, v)| {
            (
                *id,
                *v.iter().max_by_key(|(m, n)| (**n, m.is_some())).unwrap().0,
            )
        })
        .collect();

    let mut scores = BTreeMap::new();
    let lower = false;
    let higher = true;
    for (name, group) in [("sailboat", SAILBOAT), ("pair", PAIR)] {
        let ids: BTreeSet<u64> = label
            .iter()
            .filter(|(_, l)| l.is_some_and(|m| group.contains(&m)))
            .map(|(id, _)| *id)
            .collect();
        let ours: Vec<&(&Sample, Option<(u32, f64)>)> = matched
            .iter()
            .filter(|(s, m)| {
                s.tracking && ids.contains(&s.id) && m.is_some_and(|m| group.contains(&m.0))
            })
            .collect();
        // In view: AIS puts the vessel within range of the radar.
        let start = ours.iter().map(|(s, _)| s.t).min().unwrap_or(u64::MAX);
        let end = own.last().unwrap().time_ms;
        let mut bins = 0;
        let mut covered = 0;
        let mut t = start;
        while t < end {
            let in_view = group.iter().any(|m| {
                position_at(&vessels[m], t).is_some_and(|p| {
                    let (n, e) = offset_m(own_at(t), p);
                    n.hypot(e) <= IN_VIEW_M
                })
            });
            if in_view {
                bins += 1;
                if ours
                    .iter()
                    .any(|(s, _)| s.t >= t && s.t < t + COVERAGE_BIN_MS)
                {
                    covered += 1;
                }
            }
            t += COVERAGE_BIN_MS;
        }
        let truth_speed = |s: &Sample| speed_at(&vessels[&group[0]], s.t);
        let settled_sog: Vec<f64> = ours
            .iter()
            .filter(|(s, _)| s.t >= first_seen[&s.id] + SETTLED_MS)
            .filter_map(|(s, _)| Some((s.sog? - truth_speed(s)?).abs()))
            .collect();
        let promoted_sog: Vec<f64> = ids
            .iter()
            .filter_map(|id| {
                let s = ours.iter().find(|(s, _)| s.id == *id)?.0;
                Some((s.sog? - truth_speed(s)?).abs())
            })
            .collect();
        scores.insert(format!("{name}.track_ids"), (ids.len() as f64, lower));
        scores.insert(
            format!("{name}.coverage"),
            (covered as f64 / bins.max(1) as f64, higher),
        );
        scores.insert(
            format!("{name}.position_error_m"),
            (
                median(ours.iter().map(|(_, m)| m.unwrap().1).collect()),
                lower,
            ),
        );
        scores.insert(
            format!("{name}.settled_sog_error_ms"),
            (median(settled_sog), lower),
        );
        scores.insert(
            format!("{name}.sog_error_at_promotion_ms"),
            (median(promoted_sog), lower),
        );
    }
    let false_ids: Vec<u64> = label
        .iter()
        .filter(|(_, l)| l.is_none())
        .map(|(id, _)| *id)
        .collect();
    let false_seconds: f64 = false_ids
        .iter()
        .map(|id| {
            let ts: Vec<u64> = samples
                .iter()
                .filter(|s| s.id == *id && s.tracking)
                .map(|s| s.t)
                .collect();
            (ts.iter().max().unwrap() - ts.iter().min().unwrap()) as f64 / 1000.0
        })
        .sum();
    scores.insert("false.track_ids".into(), (false_ids.len() as f64, lower));
    scores.insert("false.track_seconds".into(), (false_seconds, lower));
    let created: BTreeSet<u64> = samples.iter().map(|s| s.id).collect();
    scores.insert("all.ids_broadcast".into(), (created.len() as f64, lower));
    scores
}

/// How much worse than the baseline a score may get before the test fails.
fn tolerance(name: &str) -> f64 {
    match name.rsplit('.').next().unwrap() {
        "coverage" => 0.02,
        "position_error_m" => 10.0,
        "settled_sog_error_ms" | "sog_error_at_promotion_ms" => 0.2,
        "track_seconds" => 30.0,
        _ => 0.0,
    }
}

/// How much better than the baseline a score may get before the test asks
/// for a new one. The replay is deterministic, so anything more is a change
/// in tracking rather than noise in the arithmetic.
const IMPROVEMENT_NOISE: f64 = 1e-3;

fn compare_to_baseline(scores: &BTreeMap<String, (f64, bool)>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE);
    let current: BTreeMap<&String, f64> = scores.iter().map(|(k, (v, _))| (k, *v)).collect();
    // A NaN would pass every comparison below and be written as null.
    let invalid: Vec<_> = current.iter().filter(|(_, v)| !v.is_finite()).collect();
    assert!(invalid.is_empty(), "scores without a value: {invalid:?}");
    if std::env::var_os("UPDATE_ARPA_BASELINE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = serde_json::to_string_pretty(&current).unwrap();
        std::fs::write(&path, text + "\n").unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let baseline: BTreeMap<String, f64> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("baseline")).unwrap();
    let mut worse = Vec::new();
    let mut better = Vec::new();
    for (name, (value, higher_is_better)) in scores {
        let Some(&base) = baseline.get(name) else {
            eprintln!("{name:32} {value:9.2}  (no baseline)");
            continue;
        };
        let regression = if *higher_is_better {
            base - value
        } else {
            value - base
        };
        let mark = if regression > tolerance(name) {
            worse.push(name.clone());
            "WORSE"
        } else if regression < -IMPROVEMENT_NOISE {
            better.push(name.clone());
            "better"
        } else {
            ""
        };
        eprintln!("{name:32} {value:9.2}  baseline {base:9.2}  {mark}");
    }
    assert!(worse.is_empty(), "ARPA tracking got worse: {worse:?}");
    assert!(
        better.is_empty(),
        "ARPA tracking got better: {better:?}; run with UPDATE_ARPA_BASELINE=1 \
         and commit {BASELINE} to hold it there"
    );
}

fn set(info: &mayara::radar::RadarInfo, id: ControlId, value: Value) {
    let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(10);
    let bare: BareControlValue = serde_json::from_value(value).unwrap();
    info.controls
        .process_client_request(ControlValue::from_request(id, bare), reply_tx)
        .expect("control accepted");
}

/// The reporter's settings for both ranges, unchanged through the capture.
fn configure(a: &mayara::radar::RadarInfo, b: &mayara::radar::RadarInfo) {
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
    let Some(dir) = recordings() else {
        return;
    };
    let _ = env_logger::builder().is_test(true).try_init();
    let own = voyage(&dir);
    let vessels = ais(&dir);
    replay::init(&dir.join(PCAP)).expect("init replay");
    replay::set_instant_timing();
    replay::use_capture_clock(own.clone(), DISCOVERY);
    let args = test_args(&dir.join(PCAP));

    Toplevel::new(async move |s: &mut SubsystemHandle| {
        let (radars, _) = mayara::start_session(s, args).await;
        let mut deltas = radars.new_sk_client_subscription();

        s.start(SubsystemBuilder::new(
            "arpa-722",
            async move |subsys: &mut SubsystemHandle| {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                let (a, b) = loop {
                    if let (Some(a), Some(b)) =
                        (radars.get_by_key(RADAR_A), radars.get_by_key(RADAR_B))
                        && b.controls.model_name().is_some()
                    {
                        break (a, b);
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "no radars found: {:?}",
                        radars.get_keys()
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                configure(&a, &b);
                replay::release_replay();

                let mut samples = Vec::new();
                let mut lagged = 0;
                loop {
                    tokio::select! {
                        _ = subsys.on_shutdown_requested() => break,
                        delta = deltas.recv() => match delta {
                            Ok(d) => samples_from(&serde_json::to_value(&d).unwrap(), RADAR_B, &mut samples),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => lagged += n,
                            Err(_) => break,
                        },
                    }
                }
                assert_eq!(lagged, 0, "missed {lagged} target broadcasts");
                // A coasting track is broadcast every sweep with the same fix.
                samples.sort_by_key(|s| (s.id, s.t));
                samples.dedup_by_key(|s| (s.id, s.t));
                assert!(!samples.is_empty(), "no targets were tracked");
                let scores = score(&samples, &vessels, &own);
                compare_to_baseline(&scores);
                Ok::<(), miette::Report>(())
            },
        ));
    })
    .handle_shutdown_requests(Duration::from_secs(30))
    .await
    .expect("toplevel");
}
