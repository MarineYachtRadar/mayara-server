//! Anonymous "it works" reports.
//!
//! Mayara sends at most two small reports per run: one when a radar first
//! delivers spoke data, and one when the first control change the user made is
//! accepted. Together they answer the only question the reports exist for --
//! "does this build, on this platform, actually drive this radar?"
//!
//! Nothing is sent until the user says yes. The GUI asks once, and the answer
//! is kept in the settings file -- which means a run that cannot store its
//! settings never asks and never reports, because it could not remember the
//! answer and would ask again on every start. `--no-telemetry` and
//! `MAYARA_TELEMETRY=false` keep the question from being asked at all;
//! `MAYARA_TELEMETRY=true` answers it, so a packager that owns the
//! deployment can decide for its users.
//!
//! No position, no serial number, no network address and no vessel data is
//! ever included: the only identifier is a random UUID created when consent is
//! given, so repeat runs of one install are not counted as many.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use log::{debug, info};
use serde::Serialize;

use crate::config::Consent;
use crate::radar::settings::ControlId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::{Brand, Cli, VERSION};

/// Collector that receives the reports. Set `MAYARA_TELEMETRY_URL` to send a
/// run's reports somewhere else; setting it empty stops them entirely.
const DEFAULT_ENDPOINT: &str = "https://telemetry.keversoft.com/mayara/v1/event";

const ENV_URL: &str = "MAYARA_TELEMETRY_URL";
/// `false` keeps reporting off and the question unasked; `true` answers the
/// question on the user's behalf, so the GUI never puts it to them. Unset
/// leaves it to the user. This is how a packager that owns the deployment --
/// the Signal K plugin, a container image -- decides for its users.
const ENV_TELEMETRY: &str = "MAYARA_TELEMETRY";
/// How mayara was installed, set by whoever packaged it. The values mayara
/// itself ships with are `container`, `signalk-server-plugin` and
/// `mayara_pi`; anything else is passed through, so a packager can label
/// itself without waiting for a mayara release.
const ENV_DEPLOYMENT: &str = "MAYARA_DEPLOYMENT";

/// A report is a courtesy, never a reason to keep a socket open; one attempt,
/// then give up until the next run.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest accepted `MAYARA_DEPLOYMENT` value, so a stray environment
/// variable cannot turn a report into a payload of arbitrary size.
const MAX_DEPLOYMENT_LEN: usize = 32;

/// Control values are held in SI units, so the radar's lifetime transmit
/// counter arrives here as seconds however the radar spells it on the wire.
const SECS_PER_HOUR: f64 = 3600.;

const EVENT_SPOKES: &str = "spokes";
const EVENT_CONTROL: &str = "control";

/// What kind of radar the report is about. Cheap to build, so a caller can
/// snapshot it while it holds a `RadarInfo` and report later.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarIdentity {
    brand: Brand,
    model: Option<String>,
    dual_range: bool,
    /// Lifetime transmit hours, for radars that keep such a counter and have
    /// reported it by the time the snapshot is taken.
    transmit_hours: Option<u64>,
}

impl From<&RadarInfo> for RadarIdentity {
    fn from(info: &RadarInfo) -> Self {
        RadarIdentity {
            brand: info.brand,
            model: info.controls.model_name(),
            dual_range: info.dual_range,
            transmit_hours: transmit_hours(
                info.controls
                    .get(&ControlId::TransmitTime)
                    .and_then(|c| c.value),
            ),
        }
    }
}

/// Emulated and recorded radars say nothing about real hardware working.
fn is_real_radar(brand: Brand) -> bool {
    !matches!(brand, Brand::Emulator | Brand::Playback)
}

/// Whether this caller gets to send the milestone `flag` stands for. Consent
/// is checked before the flag is spent, so a milestone that passes while the
/// user has not answered yet still reports once they say yes.
fn claim_with(consent: Consent, flag: &AtomicBool, brand: Brand) -> bool {
    consent == Consent::Granted && claim_once(flag, brand)
}

/// Claim a milestone for the one caller that gets to report it.
fn claim_once(flag: &AtomicBool, brand: Brand) -> bool {
    !flag.load(Ordering::Relaxed) && is_real_radar(brand) && !flag.swap(true, Ordering::Relaxed)
}

#[derive(Serialize, Debug, PartialEq)]
struct Report<'a> {
    install: &'a str,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    deployment: &'a str,
    event: &'static str,
    brand: Brand,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    radars: usize,
    dual_range: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    transmit_hours: Option<u64>,
    features: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secs_to_first_spoke: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    control: Option<&'a str>,
}

struct Telemetry {
    endpoint: String,
    /// Cheap to clone (an `Arc` inside), and the only route to the settings
    /// file where the user's answer and the install id live.
    radars: SharedRadars,
    deployment: String,
    /// A packager answered through the environment: report, and never ask.
    forced: bool,
    start: Instant,
    radar_count: AtomicUsize,
    spokes_reported: AtomicBool,
    control_reported: AtomicBool,
}

static TELEMETRY: OnceLock<Telemetry> = OnceLock::new();

fn active() -> Option<&'static Telemetry> {
    TELEMETRY.get()
}

/// Decide whether this run may report at all, and whether the user still has
/// to be asked. Everything else in this module is inert until this has run.
pub fn init(args: &Cli, radars: &SharedRadars) {
    let forced = match env_telemetry() {
        Some(false) => {
            info!("Usage stats are off ({ENV_TELEMETRY}=false); the GUI will not ask");
            return;
        }
        Some(true) => true,
        None => false,
    };
    if args.no_telemetry {
        info!("Usage stats are off (--no-telemetry); the GUI will not ask");
        return;
    }
    if args.is_replay() {
        debug!("Usage stats are off in replay mode");
        return;
    }

    let endpoint = std::env::var(ENV_URL).unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    if endpoint.is_empty() {
        debug!("Usage stats have no collector configured");
        return;
    }

    // Without a settings file there is nowhere to keep the answer, so the
    // question would come back on every start and the install would be
    // counted afresh every time. Say nothing and report nothing.
    if radars.telemetry_consent() == Consent::Unavailable {
        info!("Usage stats are off: this run cannot store an answer to the question");
        return;
    }

    let _ = TELEMETRY.set(Telemetry {
        endpoint,
        radars: radars.clone(),
        deployment: deployment(args),
        forced,
        start: Instant::now(),
        radar_count: AtomicUsize::new(0),
        spokes_reported: AtomicBool::new(false),
        control_reported: AtomicBool::new(false),
    });

    match consent() {
        Consent::Granted if forced => {
            info!("Usage stats are on ({ENV_TELEMETRY}=true); the GUI will not ask")
        }
        Consent::Granted => info!("Usage stats are on; the user agreed"),
        Consent::Denied => info!("Usage stats are off; the user declined"),
        _ => info!("Usage stats: the GUI will ask once whether to report that this works"),
    }
}

/// `MAYARA_TELEMETRY` as a decision, ignoring anything that is not a plain
/// yes or no.
fn env_telemetry() -> Option<bool> {
    parse_telemetry_setting(&std::env::var(ENV_TELEMETRY).ok()?)
}

fn parse_telemetry_setting(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        other => {
            info!("Ignoring {ENV_TELEMETRY}='{other}': expected true or false");
            None
        }
    }
}

/// What the GUI should do about the question. `Unavailable` when this run may
/// not report at all, in which case the question is never put to the user.
pub fn consent() -> Consent {
    match active() {
        None => Consent::Unavailable,
        Some(t) if t.forced => Consent::Granted,
        Some(t) => t.radars.telemetry_consent(),
    }
}

/// Record the user's answer. Refused when this run may not report, or when a
/// packager already answered through the environment.
pub fn set_consent(granted: bool) -> Consent {
    match active() {
        None => Consent::Unavailable,
        Some(t) if t.forced => Consent::Granted,
        Some(t) => t.radars.set_telemetry_consent(granted),
    }
}

/// A radar was discovered. Only the count is reported.
pub fn note_radar_found() {
    if let Some(t) = active() {
        t.radar_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// A radar delivered spoke data. Called per broadcast frame, so the common
/// path is a single relaxed load.
pub fn note_spokes(info: &RadarInfo) {
    if let Some(t) = active()
        && t.claim(&t.spokes_reported, info.brand)
    {
        let secs = t.start.elapsed().as_secs();
        t.send(EVENT_SPOKES, RadarIdentity::from(info), Some(secs), None);
    }
}

/// A radar accepted a control change made by the user.
pub fn note_control_ok(control_id: &str, identity: RadarIdentity) {
    if let Some(t) = active()
        && t.claim(&t.control_reported, identity.brand)
    {
        t.send(EVENT_CONTROL, identity, None, Some(control_id.to_owned()));
    }
}

impl Telemetry {
    /// Whether this caller is the one that gets to send `flag`'s report.
    /// Consent is checked first, so a milestone that passes while the user
    /// has not answered yet is not spent: it reports on the next one after
    /// they say yes.
    fn claim(&self, flag: &AtomicBool, brand: Brand) -> bool {
        claim_with(consent(), flag, brand)
    }

    fn report<'a>(
        &'a self,
        install: &'a str,
        event: &'static str,
        identity: &'a RadarIdentity,
        secs_to_first_spoke: Option<u64>,
        control: Option<&'a str>,
    ) -> Report<'a> {
        Report {
            install,
            version: VERSION,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            deployment: &self.deployment,
            event,
            brand: identity.brand,
            model: identity.model.as_deref(),
            radars: self.radar_count.load(Ordering::Relaxed).max(1),
            dual_range: identity.dual_range,
            transmit_hours: identity.transmit_hours,
            features: features(),
            secs_to_first_spoke,
            control,
        }
    }

    fn send(
        &'static self,
        event: &'static str,
        identity: RadarIdentity,
        secs_to_first_spoke: Option<u64>,
        control: Option<String>,
    ) {
        let Some(install) = self.radars.telemetry_install_id() else {
            return;
        };
        let body = serde_json::to_string(&self.report(
            &install,
            event,
            &identity,
            secs_to_first_spoke,
            control.as_deref(),
        ))
        .expect("Cannot serialize telemetry report");
        let endpoint = self.endpoint.clone();

        tokio::spawn(async move {
            let result = reqwest::Client::new()
                .post(&endpoint)
                .timeout(SEND_TIMEOUT)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await;
            match result {
                Ok(response) => debug!("Usage stats '{}' sent: {}", event, response.status()),
                Err(e) => debug!("Usage stats '{}' not sent: {}", event, e),
            }
        });
    }
}

/// A radar's lifetime transmit counter in whole hours, which is the unit the
/// radars that keep one count in. A negative reading is a decode that went
/// wrong, not a radar that transmitted backwards, so it is reported as no
/// reading at all.
fn transmit_hours(seconds: Option<f64>) -> Option<u64> {
    seconds
        .filter(|s| s.is_finite() && *s >= 0.)
        .map(|s| (s / SECS_PER_HOUR) as u64)
}

/// Which radar brands this binary was built with -- a report from a build
/// without a brand compiled in says nothing about that brand.
fn features() -> Vec<&'static str> {
    let mut features = Vec::new();
    for (name, enabled) in [
        ("navico", cfg!(feature = "navico")),
        ("furuno", cfg!(feature = "furuno")),
        ("garmin", cfg!(feature = "garmin")),
        ("koden", cfg!(feature = "koden")),
        ("raymarine", cfg!(feature = "raymarine")),
    ] {
        if enabled {
            features.push(name);
        }
    }
    features
}

/// How mayara was installed, for telling a container apart from a Signal K
/// plugin or an OpenCPN one. Only the packager really knows, so it says so
/// through `MAYARA_DEPLOYMENT`; failing that, all mayara can tell is whether
/// some parent process is supervising it.
fn deployment(args: &Cli) -> String {
    deployment_from(std::env::var(ENV_DEPLOYMENT).ok().as_deref(), args)
}

fn deployment_from(label: Option<&str>, args: &Cli) -> String {
    // A label that sanitizes away to nothing -- `!!!` -- is no label, and
    // must not be reported as an empty deployment.
    match label.map(sanitized_deployment).filter(|l| !l.is_empty()) {
        Some(label) => label,
        None if args.parent.is_some() => "embedded".to_string(),
        None => "standalone".to_string(),
    }
}

/// Keep the reported value to something that groups cleanly in the
/// collector: one short word, no whitespace or punctuation to normalise
/// away later.
fn sanitized_deployment(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(MAX_DEPLOYMENT_LEN)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        use clap::Parser;
        Cli::parse_from(std::iter::once("mayara").chain(args.iter().copied()))
    }

    fn identity() -> RadarIdentity {
        RadarIdentity {
            brand: Brand::Navico,
            model: Some("HALO".to_string()),
            dual_range: true,
            transmit_hours: Some(1234),
        }
    }

    /// The report builder, without the `SharedRadars` a live `Telemetry`
    /// carries -- these are the fields that go on the wire.
    fn report<'a>(
        install: &'a str,
        identity: &'a RadarIdentity,
        event: &'static str,
        secs_to_first_spoke: Option<u64>,
        control: Option<&'a str>,
    ) -> Report<'a> {
        Report {
            install,
            version: VERSION,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            deployment: "standalone",
            event,
            brand: identity.brand,
            model: identity.model.as_deref(),
            radars: 2,
            dual_range: identity.dual_range,
            transmit_hours: identity.transmit_hours,
            features: features(),
            secs_to_first_spoke,
            control,
        }
    }

    const INSTALL: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn report_holds_only_the_announced_fields() {
        let identity = identity();
        let json =
            serde_json::to_value(report(INSTALL, &identity, EVENT_SPOKES, Some(12), None)).unwrap();

        assert_eq!(json["install"], INSTALL);
        assert_eq!(json["version"], VERSION);
        assert_eq!(json["brand"], "Navico");
        assert_eq!(json["model"], "HALO");
        assert_eq!(json["dual_range"], true);
        assert_eq!(json["transmit_hours"], 1234);
        assert_eq!(json["event"], "spokes");
        assert_eq!(json["secs_to_first_spoke"], 12);

        // Nothing beyond the fields the question promises.
        let fields: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            vec![
                "arch",
                "brand",
                "deployment",
                "dual_range",
                "event",
                "features",
                "install",
                "model",
                "os",
                "radars",
                "secs_to_first_spoke",
                "transmit_hours",
                "version"
            ]
        );
    }

    #[test]
    fn control_report_names_the_control_and_omits_spoke_timing() {
        let identity = identity();
        let json = serde_json::to_value(report(
            INSTALL,
            &identity,
            EVENT_CONTROL,
            None,
            Some("range"),
        ))
        .unwrap();

        assert_eq!(json["event"], "control");
        assert_eq!(json["control"], "range");
        assert!(json.get("secs_to_first_spoke").is_none());
    }

    #[test]
    fn model_is_omitted_while_the_radar_has_not_reported_one() {
        let identity = RadarIdentity {
            brand: Brand::Furuno,
            model: None,
            dual_range: false,
            transmit_hours: None,
        };
        let json =
            serde_json::to_value(report(INSTALL, &identity, EVENT_SPOKES, Some(3), None)).unwrap();

        assert!(json.get("model").is_none());
    }

    /// Radars count transmit time in hours or in seconds; a report says hours
    /// either way, and says nothing at all when the radar keeps no such
    /// counter or has not reported it yet.
    #[test]
    fn transmit_time_is_reported_in_whole_hours_or_not_at_all() {
        assert_eq!(transmit_hours(Some(0.)), Some(0));
        assert_eq!(transmit_hours(Some(3599.)), Some(0));
        assert_eq!(transmit_hours(Some(3600.)), Some(1));
        assert_eq!(transmit_hours(Some(4_444_200.)), Some(1234));
        assert_eq!(transmit_hours(None), None);
        assert_eq!(transmit_hours(Some(-3600.)), None);
        assert_eq!(transmit_hours(Some(f64::NAN)), None);
    }

    #[test]
    fn a_radar_without_a_transmit_counter_omits_it_from_the_report() {
        let identity = RadarIdentity {
            brand: Brand::Furuno,
            model: Some("DRS4W".to_string()),
            dual_range: false,
            transmit_hours: None,
        };
        let json =
            serde_json::to_value(report(INSTALL, &identity, EVENT_SPOKES, Some(3), None)).unwrap();

        assert!(json.get("transmit_hours").is_none());
    }

    /// Regression: a milestone reached before the user answers must not be
    /// spent, or a yes would be followed by silence for the rest of the run --
    /// and nothing may be sent while the question is still open.
    #[test]
    fn a_milestone_reached_before_the_answer_is_not_spent() {
        let spokes = AtomicBool::new(false);

        assert!(!claim_with(Consent::Unasked, &spokes, Brand::Navico));
        assert!(!claim_with(Consent::Denied, &spokes, Brand::Navico));
        assert!(!claim_with(Consent::Unavailable, &spokes, Brand::Navico));

        assert!(
            claim_with(Consent::Granted, &spokes, Brand::Navico),
            "saying yes must still report the milestone that already passed"
        );
        assert!(!claim_with(Consent::Granted, &spokes, Brand::Navico));
    }

    #[test]
    fn each_milestone_is_claimed_once() {
        let spokes = AtomicBool::new(false);

        assert!(claim_once(&spokes, Brand::Navico));
        assert!(!claim_once(&spokes, Brand::Navico));
    }

    #[test]
    fn emulated_and_recorded_radars_are_never_reported() {
        let spokes = AtomicBool::new(false);

        assert!(!claim_once(&spokes, Brand::Emulator));
        assert!(!claim_once(&spokes, Brand::Playback));
        // A real radar in the same run can still report.
        assert!(claim_once(&spokes, Brand::Navico));
    }

    /// A packager answers for its users through the environment; anything
    /// that is not a plain yes or no leaves the question to the user.
    #[test]
    fn the_environment_answers_only_when_it_says_so_plainly() {
        assert_eq!(parse_telemetry_setting("true"), Some(true));
        assert_eq!(parse_telemetry_setting(" TRUE "), Some(true));
        assert_eq!(parse_telemetry_setting("false"), Some(false));
        assert_eq!(parse_telemetry_setting("0"), Some(false));
        assert_eq!(parse_telemetry_setting("maybe"), None);
        assert_eq!(parse_telemetry_setting(""), None);
    }

    /// Without a packager saying so, all mayara knows is whether something
    /// is supervising it.
    #[test]
    fn deployment_falls_back_to_what_mayara_can_see_for_itself() {
        assert_eq!(deployment_from(None, &cli(&[])), "standalone");
        assert_eq!(deployment_from(None, &cli(&["--parent", "42"])), "embedded");
    }

    /// A label that survives nothing of itself must not be reported as an
    /// empty deployment.
    #[test]
    fn a_label_that_sanitizes_away_is_no_label() {
        assert_eq!(deployment_from(Some("!!!"), &cli(&[])), "standalone");
        assert_eq!(
            deployment_from(Some("  "), &cli(&["--parent", "42"])),
            "embedded"
        );
        assert_eq!(deployment_from(Some("container"), &cli(&[])), "container");
    }

    #[test]
    fn a_deployment_label_is_reduced_to_one_groupable_word() {
        assert_eq!(sanitized_deployment("container"), "container");
        assert_eq!(
            sanitized_deployment(" signalk-server-plugin\n"),
            "signalk-server-plugin"
        );
        assert_eq!(sanitized_deployment("mayara_pi"), "mayara_pi");
        assert_eq!(sanitized_deployment("home brew!"), "homebrew");
        assert_eq!(sanitized_deployment("!!!"), "");
        assert_eq!(sanitized_deployment(&"x".repeat(64)).len(), 32);
    }
}
