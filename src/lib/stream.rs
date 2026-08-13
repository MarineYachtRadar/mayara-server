use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    cmp::min,
    collections::{HashMap, HashSet},
    str::FromStr,
    time::{Duration, SystemTime},
};
use strum::{EnumString, VariantNames};
use utoipa::ToSchema;
use wildmatch::WildMatch;

use crate::{
    PACKAGE,
    navdata::get_own_ship_context,
    radar::settings::{BareControlValue, Control, ControlDefinition, ControlId, RadarControlValue},
    radar::target::ArpaTargetApi,
    radar::{RadarError, SharedRadars},
};

/// Signal K's self-reference context, used until a concrete own-ship URN
/// (e.g. `vessels.urn:mrn:signalk:uuid:…`) is detected from the upstream.
const SELF_CONTEXT: &str = "vessels.self";

/// The Signal K context for an AIS target, keyed by MMSI the way Signal K
/// itself keys it (`vessels.urn:mrn:imo:mmsi:431004411`) rather than by bare
/// MMSI, so the context is identical whichever server the client asked.
fn ais_context(mmsi: &str) -> String {
    format!("vessels.urn:mrn:imo:mmsi:{}", mmsi)
}

/// Server-to-client delta message containing control value updates
#[derive(Serialize, Clone, Debug, ToSchema)]
#[schema(example = json!({
    "updates": [{
        "$source": "mayara",
        "timestamp": "2024-01-15T10:30:00Z",
        "values": [
            {"path": "radars.nav1034A.controls.gain", "value": 50},
            {"path": "radars.nav1034A.controls.sea", "value": 30, "auto": true}
        ]
    }]
}))]
pub struct SignalKDelta {
    /// Signal K context this delta applies to (e.g. `vessels.self` or a concrete
    /// own-ship URN; for AIS, the observed vessel's context). Serialized as
    /// `context` per the Signal K delta spec; omitted only if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    /// Array of update batches, each containing changed control values
    updates: Vec<DeltaUpdate>,
    /// True for a delta about another vessel (AIS). Not part of the wire
    /// format — it exists because subscription filtering can no longer tell
    /// AIS from own-ship data by path: in Signal K's shape an AIS position is
    /// `navigation.position` under a `vessels.<urn>` context, identical to the
    /// own-ship path. The context is the only discriminator, so the delta
    /// carries the answer rather than making the filter re-derive it.
    #[serde(skip)]
    #[schema(ignore)]
    is_ais: bool,
}

impl Default for SignalKDelta {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalKDelta {
    /// A new delta for own-ship data (radar controls, navigation, targets,
    /// notifications). Its context is the detected own-ship context, falling
    /// back to `vessels.self` until a concrete URN arrives from the upstream.
    /// AIS vessels get their own context via [`SignalKDelta::for_ais_vessel`].
    pub fn new() -> SignalKDelta {
        Self {
            context: Some(get_own_ship_context().unwrap_or_else(|| SELF_CONTEXT.to_string())),
            updates: Vec::new(),
            is_ais: false,
        }
    }

    //
    // Used when starting a websocket, we always check radars for unsent
    //
    pub fn add_meta_updates(&mut self, radars: &SharedRadars, meta_sent: &mut HashSet<String>) {
        if let Some(updates) = get_meta_delta(radars, meta_sent) {
            self.updates.push(updates);
        }
    }

    //
    // Every time we send a SignalKDelta, we check for unsent meta data
    //
    pub fn add_meta_from_updates(
        &mut self,
        radars: &SharedRadars,
        meta_sent: &mut HashSet<String>,
    ) {
        let mut needs_meta = false;
        for update in &self.updates {
            for dv in &update.values {
                // Only check radar control paths (radars.{id}.controls.*)
                // Skip navigation and target paths
                let path = dv.path();
                if !path.starts_with("radars.") || !path.contains(".controls.") {
                    continue;
                }
                if let Some(radar_id) = path.split('.').nth(1)
                    && !meta_sent.contains(radar_id)
                {
                    // Found a radar whose meta hasn't been sent yet
                    needs_meta = true;
                    break;
                }
            }
            if needs_meta {
                break;
            }
        }
        if needs_meta {
            self.add_meta_updates(radars, meta_sent);
        }
    }

    pub fn add_updates(&mut self, rcvs: Vec<RadarControlValue>) {
        let delta_update = DeltaUpdate::from(rcvs);
        self.updates.push(delta_update);
    }

    /// Add a target update to the delta message.
    /// - `Some(target)` sends the target data (acquired or updated)
    /// - `None` indicates the target was lost
    pub fn add_target_update(
        &mut self,
        radar_id: &str,
        target_id: u64,
        target: Option<ArpaTargetApi>,
    ) {
        let path = format!("radars.{}.targets.{}", radar_id, target_id);
        let value: serde_json::Value = match target {
            Some(t) => serde_json::to_value(t).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };

        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(PACKAGE.to_string()),
            meta: Vec::new(),
            values: vec![DeltaValue::Target { path, value }],
        };
        self.updates.push(delta_update);
    }

    /// Add a navigation update to the delta message.
    pub fn add_navigation_update(&mut self, path: &str, value: f64, source: &str) {
        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(source.to_string()),
            meta: Vec::new(),
            values: vec![DeltaValue::Navigation {
                path: path.to_string(),
                value,
            }],
        };
        self.updates.push(delta_update);
    }

    /// Add a `navigation.position` update.
    pub fn add_position_update(&mut self, latitude: f64, longitude: f64, source: &str) {
        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(source.to_string()),
            meta: Vec::new(),
            values: vec![DeltaValue::NavigationPosition {
                path: "navigation.position".to_string(),
                value: PositionValue {
                    latitude,
                    longitude,
                },
            }],
        };
        self.updates.push(delta_update);
    }

    /// Add a Signal K `notifications.*` alarm update.
    ///
    /// The notification payload follows the Signal K alarm shape:
    /// `{ state, method, message }`. State drives downstream UI severity
    /// (alert < warn < alarm < emergency); `normal` clears a prior
    /// notification at the same path. Method tells consumers whether to
    /// show a visual badge, play a sound, or both.
    pub(crate) fn add_notification_update(
        &mut self,
        path: &str,
        value: NotificationValue,
        source: &str,
    ) {
        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(source.to_string()),
            meta: Vec::new(),
            values: vec![DeltaValue::Notification {
                path: path.to_string(),
                value,
            }],
        };
        self.updates.push(delta_update);
    }

    /// A delta carrying one AIS vessel, in Signal K's own shape.
    ///
    /// An AIS update belongs to the observed vessel's context, and a Signal K
    /// delta has a single top-level `context` — so each vessel needs its own
    /// delta. This is a constructor rather than an `add_…` method precisely so
    /// several vessels cannot be batched into one and silently take the last
    /// one's context.
    ///
    /// The values are the same paths a Signal K server emits for an AIS target,
    /// so a client cannot tell whether the vessel came from mayara or from
    /// Signal K. Note that a vessel's *top-level* properties (`name`, `mmsi`)
    /// are not leaf paths: Signal K delivers each as its own value on the empty
    /// path with a single-key object, and this mirrors that exactly.
    pub fn for_ais_vessel(vessel: &crate::ais::AisVesselApi) -> SignalKDelta {
        let mut values = Vec::new();
        let mut push = |path: &str, value: serde_json::Value| {
            values.push(DeltaValue::Ais {
                path: path.to_string(),
                value,
            });
        };

        // Top-level vessel properties ride the empty path, one value each.
        push("", json!({ "mmsi": vessel.mmsi }));
        if let Some(name) = &vessel.name {
            push("", json!({ "name": name }));
        }
        if let Some(position) = &vessel.position {
            push(
                "navigation.position",
                json!({ "latitude": position.latitude, "longitude": position.longitude }),
            );
        }
        if let Some(cog) = vessel.cog {
            push("navigation.courseOverGroundTrue", json!(cog));
        }
        if let Some(sog) = vessel.sog {
            push("navigation.speedOverGround", json!(sog));
        }
        if let Some(heading) = vessel.heading {
            push("navigation.headingTrue", json!(heading));
        }
        if let Some(dimensions) = &vessel.dimensions {
            if let Some(length) = dimensions.length {
                push("design.length", json!({ "overall": length }));
            }
            if let Some(beam) = dimensions.beam {
                push("design.beam", json!(beam));
            }
            if let Some(from_bow) = dimensions.from_bow {
                push("sensors.ais.fromBow", json!(from_bow));
            }
            if let Some(from_center) = dimensions.from_center {
                push("sensors.ais.fromCenter", json!(from_center));
            }
        }

        SignalKDelta {
            context: Some(ais_context(&vessel.mmsi)),
            is_ais: true,
            updates: vec![DeltaUpdate {
                timestamp: Some(Utc::now()),
                source: Some("signalk".to_string()),
                meta: Vec::new(),
                values,
            }],
        }
    }

    pub fn add_meta_for_control(&mut self, radar_id: &str, control: &Control) {
        let mut meta = Vec::new();
        let path = format!("radars.{}.controls.{}", radar_id, control.item().control_id);
        let value = control.item().clone();
        meta.push(DeltaMeta { path, value });

        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(PACKAGE.to_string()),
            meta,
            values: Vec::new(),
        };
        self.updates.push(delta_update);
    }

    pub fn apply_subscriptions(&mut self, subscriptions: &mut ActiveSubscriptions) {
        // An AIS delta is subscribed (or not) as a whole, by its context: the
        // client asked for `vessels.*`, not for `navigation.position`. Filtering
        // its values by path would match them against the own-ship rules and
        // leak other vessels to a `subscribe=self` client.
        if self.is_ais {
            let context = self.context.clone().unwrap_or_default();
            if !subscriptions.is_subscribed_ais(&context) {
                self.updates.clear();
            }
            return;
        }
        for update in self.updates.iter_mut() {
            update
                .values
                .retain(|dv| subscriptions.is_subscribed_path(dv.path(), false));
        }
    }

    /// The delta to send, or nothing when there is nothing left to say.
    ///
    /// Filtering a delta against a client's subscriptions empties the values
    /// it did not ask for, which can leave an update carrying only a source
    /// and a timestamp. Sending that tells the client nothing, and a client
    /// subscribed to little enough receives a steady stream of it, so drop
    /// updates that carry neither values nor meta before deciding.
    pub fn build(mut self) -> Option<Self> {
        self.updates
            .retain(|update| !update.values.is_empty() || !update.meta.is_empty());

        if self.updates.is_empty() {
            return None;
        }
        Some(self)
    }
}

/// A batch of control value updates within a SignalKDelta message
#[derive(Serialize, Clone, Debug, ToSchema)]
struct DeltaUpdate {
    /// Source identifier (always "mayara")
    #[serde(
        rename = "$source",
        skip_deserializing,
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(example = "mayara")]
    source: Option<String>,
    /// ISO 8601 timestamp when the update was generated
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, example = "2024-01-15T10:30:00Z")]
    timestamp: Option<DateTime<Utc>>,
    /// Control metadata (schema definitions, sent once per radar)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    meta: Vec<DeltaMeta>,
    /// Control value changes
    #[serde(skip_serializing_if = "Vec::is_empty")]
    values: Vec<DeltaValue>,
}

/// A single value update (control, target, navigation, or AIS)
#[derive(Serialize, Clone, Debug, ToSchema)]
#[serde(untagged)]
enum DeltaValue {
    /// Control value update
    Control {
        /// Full path to the control (e.g., "radars.nav1034A.controls.gain")
        #[schema(example = "radars.nav1034A.controls.gain")]
        path: String,
        /// The control value
        value: BareControlValue,
    },
    /// Target update (acquired, updated, or lost)
    Target {
        /// Full path to the target (e.g., "radars.nav1034A.targets.1")
        path: String,
        /// Target data or null for lost target
        value: serde_json::Value,
    },
    /// Navigation data update
    Navigation {
        /// Full path to the navigation data (e.g., "navigation.headingTrue")
        path: String,
        /// Navigation value (radians for heading, m/s for speed, etc.)
        value: f64,
    },
    /// Navigation position update (separate variant — Signal K position is
    /// a `{latitude, longitude}` object, not a scalar).
    NavigationPosition {
        /// Always "navigation.position".
        path: String,
        /// `{latitude, longitude}` in decimal degrees.
        value: PositionValue,
    },
    /// One value of an AIS vessel update, in Signal K's own shape: either a
    /// leaf path (`navigation.position`, `design.beam`, …) or the empty path
    /// carrying a single top-level vessel property (`{"name": …}`). The
    /// observed vessel is identified by the delta's context, not by this path.
    Ais {
        /// Signal K path, or "" for a top-level vessel property.
        #[schema(example = "navigation.position")]
        path: String,
        /// The value for that path.
        value: serde_json::Value,
    },
    /// Signal K `notifications.*` alarm. Payload shape matches the
    /// notification value defined in the Signal K spec.
    Notification {
        /// Full path under `notifications.` (e.g.
        /// `notifications.radar.fur6424A.guardZone.1`).
        path: String,
        value: NotificationValue,
    },
}

/// Signal K notification alarm payload. State / method / message map
/// directly to the spec's notification value object; `state == "normal"`
/// clears a prior alarm at the same path.
#[derive(Serialize, Clone, Debug, ToSchema)]
pub(crate) struct NotificationValue {
    pub(crate) state: NotificationState,
    pub(crate) method: Vec<NotificationMethod>,
    pub(crate) message: String,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
// Warn / Alarm / Emergency exist to match the Signal K spec ordering
// (Normal < Alert < Warn < Alarm < Emergency); they aren't constructed
// yet but follow-up notification types (e.g. radar connection lost,
// CPA/TCPA breach) will need them.
#[allow(dead_code)]
pub(crate) enum NotificationState {
    Normal,
    Alert,
    Warn,
    Alarm,
    Emergency,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NotificationMethod {
    Visual,
    Sound,
}

/// Geographic position (decimal degrees) serialized to match the Signal K
/// `navigation.position` payload shape.
#[derive(Serialize, Clone, Debug, ToSchema)]
pub(crate) struct PositionValue {
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
}

impl DeltaValue {
    fn path(&self) -> &str {
        match self {
            DeltaValue::Control { path, .. } => path,
            DeltaValue::Target { path, .. } => path,
            DeltaValue::Navigation { path, .. } => path,
            DeltaValue::NavigationPosition { path, .. } => path,
            DeltaValue::Ais { path, .. } => path,
            DeltaValue::Notification { path, .. } => path,
        }
    }
}

impl DeltaUpdate {
    fn from(radar_control_values: Vec<RadarControlValue>) -> Self {
        let mut values = Vec::new();
        for radar_control_value in radar_control_values {
            let path = radar_control_value.path.to_string();

            let value = BareControlValue::from(radar_control_value);
            values.push(DeltaValue::Control { path, value });
        }

        DeltaUpdate {
            timestamp: None,
            source: Some(PACKAGE.to_string()),
            meta: Vec::new(),
            values,
        }
    }
}

/// Control metadata containing schema definitions
#[derive(Serialize, Clone, Debug, ToSchema)]
pub struct DeltaMeta {
    /// Full path to the control
    #[schema(example = "radars.nav1034A.controls.gain")]
    path: String,
    /// Control definition including type, range, and valid values
    value: ControlDefinition,
}

fn get_meta_delta(radars: &SharedRadars, meta_sent: &mut HashSet<String>) -> Option<DeltaUpdate> {
    let mut meta = Vec::new();

    for radar in radars.get_active() {
        let radar_id = radar.key();
        if !meta_sent.insert(radar_id.clone()) {
            continue;
        }
        let controls = radar.controls.get_controls();

        for (k, v) in controls.iter() {
            let path = format!("radars.{}.controls.{}", radar_id, k);
            let value = v.item().clone();
            meta.push(DeltaMeta { path, value });
        }
    }

    if meta.is_empty() {
        return None;
    }
    let delta_update = DeltaUpdate {
        timestamp: Some(Utc::now()),
        source: Some(PACKAGE.to_string()),
        meta,
        values: Vec::new(),
    };

    Some(delta_update)
}

// ====== SELF ======= //

/// Baseline context filter for a stream, from the `?subscribe=` query — matching
/// Signal K's model. Explicit path subscriptions apply additively on top.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Subscribe {
    /// `subscribe=none`: stream nothing until explicit path subscriptions arrive.
    None,
    /// `subscribe=self` (the default): stream all own-ship-context data (radar,
    /// navigation, notifications). Other contexts (AIS `vessels.*`) require an
    /// explicit subscription.
    SelfOnly,
    /// `subscribe=all`: stream every context (own-ship + AIS).
    All,
}
pub struct ActiveSubscriptions {
    pub mode: Subscribe,
    timeout: Duration,
    /// What the client asked for, kept as it asked for it rather than expanded
    /// into the paths it covers. Two requests that cover the same ground stay
    /// two requests, so taking one back leaves the other doing its job.
    requests: Vec<PathSubscribe>,
    /// When each path last went out, keyed by the path itself. Throttling is a
    /// property of the data, not of whichever request asked for it, so it
    /// outlives any one subscription.
    last_sent: HashMap<String, SystemTime>,
}

/// Does `pattern` cover `path`? Signal K subscription paths use `*` for any
/// run of characters, so `radars.*` covers a control and a target alike, and
/// `*` covers everything.
fn pattern_covers(pattern: &str, path: &str) -> bool {
    pattern == path || (pattern.contains('*') && WildMatch::new(pattern).matches(path))
}

/// The shorthand for a published control path, if it is one: `radars.{id}.gain`
/// alongside `radars.{id}.controls.gain`. `None` for anything that is not a
/// control, which is how the caller tells them apart.
fn control_shorthand(path: &str) -> Option<String> {
    let (radar_id, control_id) = path.strip_prefix("radars.")?.split_once(".controls.")?;
    Some(format!("radars.{}.{}", radar_id, control_id))
}

/// The path a control is published under, and the shorthand a client may name
/// it by. `radars.{id}.gain` has always been accepted for
/// `radars.{id}.controls.gain`, so both forms are offered to the patterns.
fn control_paths(radar_id: &str, control_id: &ControlId) -> [String; 2] {
    [
        format!("radars.{}.controls.{}", radar_id, control_id),
        format!("radars.{}.{}", radar_id, control_id),
    ]
}

impl ActiveSubscriptions {
    pub fn new(mode: Subscribe) -> ActiveSubscriptions {
        ActiveSubscriptions {
            mode,
            timeout: Duration::from_secs(99999999),
            requests: Vec::new(),
            last_sent: HashMap::new(),
        }
    }

    fn set_timeout(&mut self, timeout: u64) {
        if timeout < u64::MAX {
            let timeout = Duration::from_millis(timeout);
            if self.timeout < timeout {
                self.timeout = timeout;
            };
        }
    }

    pub fn get_timeout(&mut self) -> Duration {
        self.timeout
    }

    /// Every request that covers `path`, which is how overlapping
    /// subscriptions compose: they are all still here to be asked.
    fn covering(&self, path: &str) -> impl Iterator<Item = &PathSubscribe> {
        self.requests
            .iter()
            .filter(move |r| pattern_covers(&r.path, path))
    }

    pub fn subscribe(&mut self, subscription: Subscription) -> Result<bool, RadarError> {
        let mut period = u64::MAX;

        // Answering `true` makes the caller send every vessel it knows of, so
        // it means "newly asked for", not "asked for". A client repeating a
        // subscription it already holds is not asking again.
        let asked_before = self.asks_for_vessels();

        for path_subscription in subscription.subscribe {
            let path = &path_subscription.path;
            log::debug!("Subscribing to path: {}", path);

            if let Some(p) = path_subscription.min_period {
                period = min(p, period);
            }
            if let Some(p) = path_subscription.period {
                period = min(p, period);
            }

            // Asking twice for the same thing is one subscription, but asking
            // for it differently is not — the terms may differ.
            if !self.requests.iter().any(|r| {
                r.path == *path
                    && r.policy == path_subscription.policy
                    && r.period == path_subscription.period
                    && r.min_period == path_subscription.min_period
            }) {
                self.requests.push(path_subscription);
            }
        }
        self.set_timeout(period);

        Ok(!asked_before && self.asks_for_vessels())
    }

    /// Whether any request covers another vessel's data. `*` covers it, as it
    /// covers everything.
    fn asks_for_vessels(&self) -> bool {
        self.requests
            .iter()
            .any(|r| r.path == "*" || r.path.starts_with("vessels."))
    }

    pub fn desubscribe(&mut self, subscription: Desubscription) -> Result<(), RadarError> {
        for path_desubscription in subscription.desubscribe {
            let path = &path_desubscription.path;
            log::debug!("Desubscribing from path: {}", path);

            // Take back what was asked for, leaving anything else that happens
            // to cover the same ground still asked for.
            self.requests.retain(|r| r.path != *path);
        }

        Ok(())
    }

    /// Whether this control goes out now, which is both whether the client
    /// asked for it and whether the terms it asked on allow it yet.
    pub fn is_subscribed(&mut self, rcv: &RadarControlValue, full: bool) -> bool {
        match self.mode {
            Subscribe::All => {
                return true;
            }
            Subscribe::SelfOnly => {
                return true;
            }
            Subscribe::None => {}
        }

        let (Some(radar_id), Some(control_id)) = (rcv.radar_id.as_deref(), &rcv.control_id) else {
            panic!("Invalid use of is_subscribed(), can only be done on internal RCV");
        };
        let [canonical, shorthand] = control_paths(radar_id, control_id);

        let Some(terms) = self.terms_for(&[&canonical, &shorthand]) else {
            return false;
        };

        self.allow_now(&canonical, &terms, full)
    }

    /// The terms every covering request agrees to answer on, taking the most
    /// permissive of each: a client that asked for something instantly gets it
    /// instantly, and a broad slow subscription can never quieten a narrow
    /// fast one it happens to overlap.
    fn terms_for(&self, paths: &[&str]) -> Option<Terms> {
        let mut terms: Option<Terms> = None;
        for path in paths {
            for request in self.covering(path) {
                let found = terms.get_or_insert(Terms {
                    policy: Policy::Fixed,
                    period: None,
                    min_period: None,
                });
                let policy = request.policy.clone().unwrap_or(Policy::Instant);
                if policy.permits_more_than(&found.policy) {
                    found.policy = policy;
                }
                found.period = least(found.period, request.period);
                found.min_period = least(found.min_period, request.min_period);
            }
        }
        terms
    }

    /// Whether the terms allow sending now, and if so record that we did.
    fn allow_now(&mut self, path: &str, terms: &Terms, full: bool) -> bool {
        let now = SystemTime::now();
        let last = self.last_sent.get(path).copied();

        if terms.policy == Policy::Fixed {
            if !full {
                return false;
            }
            if let (Some(period), Some(last)) = (terms.period, last)
                && last + Duration::from_micros(period) > now
            {
                return false;
            }
        }

        if let (Some(min_period), Some(last)) = (terms.min_period, last)
            && last + Duration::from_micros(min_period) > now
        {
            return false;
        }

        self.last_sent.insert(path.to_string(), now);
        true
    }

    pub fn is_subscribed_path(&mut self, path: &str, full: bool) -> bool {
        match self.mode {
            Subscribe::All => {
                return true;
            }
            Subscribe::SelfOnly => {
                // Own-ship data is in the `self` baseline; only AIS (`vessels.*`,
                // another vessel's context) needs an explicit subscription. The
                // path prefix is the context discriminator, so this stays correct
                // even when navigation comes from NMEA 0183 (no upstream own-ship
                // URN) — `navigation.*` is own-ship by path regardless.
                if !path.starts_with("vessels.") {
                    return true;
                }
            }
            Subscribe::None => {}
        }

        // A control is throttled by the terms it was asked on; everything else
        // goes out whenever it changes. Both the path a control is published
        // under and the shorthand a client may have named it by are offered,
        // the same pair `is_subscribed` uses — a client that subscribed the
        // short way must not be answered on connect and then go quiet.
        if let Some(shorthand) = control_shorthand(path) {
            let Some(terms) = self.terms_for(&[path, &shorthand]) else {
                return false;
            };
            return self.allow_now(path, &terms, full);
        }

        self.covering(path).next().is_some()
    }

    /// Whether the client wants AIS data for `context` (e.g.
    /// `vessels.urn:mrn:imo:mmsi:431004411`). Another vessel's data is never in
    /// the `self` baseline, so outside `subscribe=all` it takes an explicit
    /// `vessels.*` subscription — matching Signal K, where `self` means own
    /// ship only.
    fn is_subscribed_ais(&self, context: &str) -> bool {
        if self.mode == Subscribe::All {
            return true;
        }
        self.covering(context).next().is_some()
    }
}

/// The terms a path goes out on, resolved from every request covering it.
struct Terms {
    policy: Policy,
    period: Option<u64>,
    min_period: Option<u64>,
}

/// The smaller of two optional periods, treating absence as no limit.
fn least(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(min(a, b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// Client-to-server message to subscribe to control value updates
#[derive(Deserialize, Debug, Serialize, ToSchema)]
#[schema(example = json!({
    "subscribe": [
        {"path": "radars.*.controls.*", "period": 1000},
        {"path": "radars.nav1034A.controls.gain", "policy": "instant"}
    ]
}))]
pub struct Subscription {
    /// List of path subscriptions
    subscribe: Vec<PathSubscribe>,
}

/// Client-to-server message to unsubscribe from control value updates
#[derive(Deserialize, Debug, ToSchema)]
#[schema(example = json!({
    "desubscribe": [{"path": "radars.*.controls.gain"}]
}))]
pub struct Desubscription {
    /// List of paths to unsubscribe from
    desubscribe: Vec<PathSubscribe>,
}

/// A single path subscription specification
#[derive(Deserialize, Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathSubscribe {
    /// Path pattern to subscribe to. Supports wildcards:
    /// - `radars.*.controls.*` - all controls on all radars
    /// - `radars.nav1034A.controls.gain` - specific control
    /// - `*.gain` - gain control on all radars
    #[schema(example = "radars.*.controls.*")]
    path: String,
    /// Update period in milliseconds (for fixed policy)
    #[schema(example = 1000)]
    period: Option<u64>,
    /// Delivery policy: instant (immediate), ideal (rate-limited), fixed (periodic)
    #[serde(default, deserialize_with = "deserialize_policy")]
    policy: Option<Policy>,
    /// Minimum period between updates in milliseconds
    #[schema(example = 200)]
    min_period: Option<u64>,
}

impl Policy {
    /// How freely this policy lets data through, so the most permissive of
    /// several can be picked: instant sends on every change, ideal holds to a
    /// floor between sends, fixed only speaks on its own schedule.
    fn permissiveness(&self) -> u8 {
        match self {
            Policy::Instant => 2,
            Policy::Ideal => 1,
            Policy::Fixed => 0,
        }
    }

    fn permits_more_than(&self, other: &Policy) -> bool {
        self.permissiveness() > other.permissiveness()
    }
}

/// Subscription delivery policy
#[derive(Clone, Serialize, PartialEq, Debug, EnumString, VariantNames, ToSchema)]
#[strum(serialize_all = "camelCase")]
pub enum Policy {
    /// Send updates immediately when values change
    Instant,
    /// Rate-limit updates to minPeriod
    Ideal,
    /// Send updates at fixed intervals (period)
    Fixed,
}

use serde::Deserializer;

fn deserialize_policy<'de, D>(deserializer: D) -> Result<Option<Policy>, D::Error>
where
    D: Deserializer<'de>,
{
    // Try to read an Option<String>.  If the key is absent we get None.
    let opt = Option::<String>::deserialize(deserializer)?;

    match opt {
        Some(s) => Policy::from_str(&s.to_ascii_lowercase())
            .map(Some)
            .map_err(|_| serde::de::Error::unknown_variant(&s, Policy::VARIANTS)),
        None => Ok(None), // field missing → None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn deserialize_subscription() {
        let s = Subscription {
            subscribe: vec![
                PathSubscribe {
                    path: "radars.1.controls.gain".to_string(),
                    period: None,
                    policy: Some(Policy::Ideal),
                    min_period: Some(50),
                },
                PathSubscribe {
                    path: "radars.2.controls.gain".to_string(),
                    period: Some(1000),
                    policy: Some(Policy::Instant),
                    min_period: None,
                },
            ],
        };
        let r = serde_json::to_string(&s);
        assert!(r.is_ok());
        let r = r.unwrap();
        println!("r = {}", r);

        match serde_json::from_str::<Subscription>(&r) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.1.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Ideal));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{"subscribe":[{"path":"radars.1.controls.gain","period":null,"policy":"ideal","min_period":null}]}"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "radars.1.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Ideal));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "*.gain" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "*.gain");
                assert_eq!(r.subscribe[0].policy, None);
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "*" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "*");
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "radars.*.controls.gain" }, { "path": "radars.*.controls.power" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.controls.gain");
                assert_eq!(r.subscribe[1].path, "radars.*.controls.power");
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "radars.*.controls.gain", "policy": "instant", "period": 1000 }, { "path": "radars.*.controls.power", "period": 1000 } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Instant));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }
    }

    fn path(p: &str) -> PathSubscribe {
        PathSubscribe {
            path: p.to_string(),
            period: None,
            policy: None,
            min_period: None,
        }
    }

    #[test]
    fn target_subscribe_dedupes_repeated_patterns() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        for _ in 0..100 {
            subs.subscribe(Subscription {
                subscribe: vec![path("radars.nav1.targets.*")],
            })
            .unwrap();
        }
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
    }

    /// `radars.*` asks for everything a radar has, which is more than its
    /// controls: a client that subscribes that way — as the Signal K plugin
    /// does — must receive tracked targets without naming `.targets.` itself.
    #[test]
    fn a_radar_wildcard_covers_targets_as_well_as_controls() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.*")],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", false));
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
    }

    /// The same asked of one radar rather than all of them.
    #[test]
    fn a_single_radar_wildcard_covers_its_targets() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.nav1.*")],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(
            !subs.is_subscribed_path("radars.nav2.targets.5", false),
            "a wildcard under one radar must not reach another"
        );
    }

    /// Subscribing to everything has to mean everything, targets included.
    #[test]
    fn subscribing_to_everything_includes_targets() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("*")],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", false));
    }

    /// A wildcard subscription has to be as easy to take back as it was to
    /// make: whatever asking for it added, unasking has to remove, or a client
    /// keeps receiving what it just said it no longer wants.
    /// A control may be named without its `controls.` segment, and a client
    /// that names it that way has to keep receiving it — not just be answered
    /// once on connect and then go quiet as its changes are filtered out.
    #[test]
    fn the_shorthand_form_receives_changes_too() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.nav1.gain")],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", false));
        assert!(!subs.is_subscribed_path("radars.nav1.controls.rain", false));
    }

    /// The same when the shorthand carries a wildcard.
    #[test]
    fn a_shorthand_wildcard_receives_changes_too() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("*.gain")],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", false));
        assert!(subs.is_subscribed_path("radars.nav2.controls.gain", false));
        assert!(!subs.is_subscribed_path("radars.nav1.controls.rain", false));
    }

    /// A broad, slow subscription must never quieten a narrow, fast one it
    /// happens to cover: the client asked for gain instantly, and adding a
    /// throttled subscription over every radar does not take that back.
    #[test]
    fn the_most_permissive_terms_win() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.*.controls.*",
                Policy::Fixed,
                Some(1_000_000_000),
                None,
            )],
        })
        .unwrap();
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.nav1.controls.gain",
                Policy::Instant,
                None,
                None,
            )],
        })
        .unwrap();

        // Twice in a row: instant means every change, so neither is held back.
        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", true));
        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", true));

        // A control only the slow subscription covers still obeys it.
        assert!(subs.is_subscribed_path("radars.nav1.controls.rain", true));
        assert!(!subs.is_subscribed_path("radars.nav1.controls.rain", true));
    }

    /// Throttling follows the data, not the subscription that asked for it, so
    /// two radars covered by one wildcard do not share a deadline.
    #[test]
    fn radars_are_throttled_apart() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.*.controls.gain",
                Policy::Fixed,
                Some(1_000_000_000),
                None,
            )],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", true));
        assert!(
            subs.is_subscribed_path("radars.nav2.controls.gain", true),
            "another radar's first update must not be swallowed by the first radar's"
        );
        assert!(!subs.is_subscribed_path("radars.nav1.controls.gain", true));
    }

    /// Two wildcards covering the same ground are two subscriptions, not one
    /// shared entry, so taking the narrower one back leaves the wider one
    /// doing its job.
    #[test]
    fn overlapping_wildcards_compose() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("*")],
        })
        .unwrap();
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.*")],
        })
        .unwrap();

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.*")],
        })
        .unwrap();

        assert!(
            subs.is_subscribed_path("radars.nav1.targets.5", false),
            "the everything wildcard still wants targets"
        );
        assert!(
            subs.is_subscribed_path("radars.nav1.controls.gain", false),
            "and controls, which is the half that was wrong first"
        );
        assert!(subs.is_subscribed_path("navigation.headingTrue", false));
    }

    #[test]
    fn a_radar_wildcard_can_be_taken_back() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.*")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.*")],
        })
        .unwrap();

        assert!(!subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(!subs.is_subscribed_path("radars.nav1.controls.gain", false));
    }

    /// The same for the everything wildcard, which reaches further: it seeds
    /// navigation, notifications and vessels alongside the radar categories.
    #[test]
    fn everything_can_be_taken_back() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("*")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(subs.is_subscribed_path("navigation.headingTrue", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("*")],
        })
        .unwrap();

        assert!(!subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(!subs.is_subscribed_path("navigation.headingTrue", false));
        assert!(!subs.is_subscribed_path("radars.nav1.controls.gain", false));
    }

    /// Taking back one radar must leave the others alone.
    #[test]
    fn taking_back_one_radar_leaves_another_subscribed() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.nav1.*"), path("radars.nav2.*")],
        })
        .unwrap();

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.nav1.*")],
        })
        .unwrap();

        assert!(!subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(subs.is_subscribed_path("radars.nav2.targets.5", false));
    }

    #[test]
    fn taking_back_one_target_pattern_leaves_the_other() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.nav1.targets.*"), path("radars.nav1.targets.5")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.nav1.targets.*")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("radars.nav1.targets.5", false));
        assert!(!subs.is_subscribed_path("radars.nav1.targets.9", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.nav1.targets.5")],
        })
        .unwrap();
        assert!(!subs.is_subscribed_path("radars.nav1.targets.5", false));
    }

    #[test]
    fn navigation_desubscribe_removes_path() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("navigation.headingTrue"), path("navigation.position")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("navigation.position", false));
        assert!(subs.is_subscribed_path("navigation.headingTrue", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("navigation.headingTrue")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("navigation.position", false));
        assert!(!subs.is_subscribed_path("navigation.headingTrue", false));
    }

    #[test]
    fn notification_exact_path_matches() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("notifications.radar.fur6424A.guardZone.1")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("notifications.radar.fur6424A.guardZone.1", false));
        // Unrelated radar/zone must not match an exact subscription.
        assert!(!subs.is_subscribed_path("notifications.radar.fur6424B.guardZone.1", false));
    }

    #[test]
    fn notification_wildcard_matches_per_zone_paths() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("notifications.radar.*")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("notifications.radar.fur6424A.guardZone.1", false));
        assert!(subs.is_subscribed_path("notifications.radar.fur6424B.guardZone.2", false));
        // The wildcard scope is `notifications.radar.*`, so other
        // notification subtrees stay outside.
        assert!(!subs.is_subscribed_path("notifications.security.accessRequest.x", false));
    }

    #[test]
    fn notification_desubscribe_removes_path() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("notifications.radar.*"), path("notifications.foo")],
        })
        .unwrap();
        assert!(subs.is_subscribed_path("notifications.radar.nav1.guardZone.1", false));

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("notifications.foo")],
        })
        .unwrap();
        assert!(!subs.is_subscribed_path("notifications.foo", false));
        assert!(subs.is_subscribed_path("notifications.radar.fur6424A.guardZone.1", false));
    }

    #[test]
    fn notification_apply_subscriptions_retains_subscribed_paths() {
        use crate::stream::{NotificationMethod, NotificationState, NotificationValue};

        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("notifications.radar.*")],
        })
        .unwrap();

        let value = NotificationValue {
            state: NotificationState::Alert,
            method: vec![NotificationMethod::Visual, NotificationMethod::Sound],
            message: "test".to_string(),
        };
        let mut delta = SignalKDelta::new();
        delta.add_notification_update("notifications.radar.fur6424A.guardZone.1", value, "mayara");

        delta.apply_subscriptions(&mut subs);
        // The notification update must survive the subscription filter
        // — this is the bug the fix addresses; without the new branch in
        // is_subscribed_path the update list would be empty here.
        assert_eq!(delta.updates.len(), 1);
        assert_eq!(delta.updates[0].values.len(), 1);
    }

    fn throttled_path(
        p: &str,
        policy: Policy,
        period: Option<u64>,
        min_period: Option<u64>,
    ) -> PathSubscribe {
        PathSubscribe {
            path: p.to_string(),
            period,
            policy: Some(policy),
            min_period,
        }
    }

    fn last_sent_of(
        subs: &ActiveSubscriptions,
        radar: &str,
        control: ControlId,
    ) -> Option<SystemTime> {
        subs.last_sent
            .get(&format!("radars.{}.controls.{}", radar, control))
            .copied()
    }

    fn set_last_sent(
        subs: &mut ActiveSubscriptions,
        radar: &str,
        control: ControlId,
        ts: SystemTime,
    ) {
        subs.last_sent
            .insert(format!("radars.{}.controls.{}", radar, control), ts);
    }

    #[test]
    fn throttle_first_update_is_delivered() {
        // The first update on a freshly-subscribed path must be delivered,
        // and must mark `last_sent` so subsequent updates can be throttled.
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.nav1.controls.gain",
                Policy::Fixed,
                Some(1_000_000),
                None,
            )],
        })
        .unwrap();

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", true));
        assert!(last_sent_of(&subs, "nav1", ControlId::Gain).is_some());
    }

    #[test]
    fn throttle_drop_does_not_advance_last_sent() {
        // Regression test for the starvation bug: when an update arrives
        // too soon, it must be dropped *without* moving the deadline
        // forward. Otherwise every subsequent update is also dropped
        // and the subscriber is starved indefinitely.
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.nav1.controls.gain",
                Policy::Fixed,
                Some(1_000_000_000), // 1000 s, effectively "never elapses during the test"
                None,
            )],
        })
        .unwrap();

        let pinned = SystemTime::now();
        set_last_sent(&mut subs, "nav1", ControlId::Gain, pinned);

        assert!(!subs.is_subscribed_path("radars.nav1.controls.gain", true));
        assert_eq!(last_sent_of(&subs, "nav1", ControlId::Gain), Some(pinned));
    }

    #[test]
    fn throttle_send_after_period_advances_last_sent() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.nav1.controls.gain",
                Policy::Fixed,
                Some(1_000_000), // 1 s
                None,
            )],
        })
        .unwrap();

        // Last send was a day ago — well past the period.
        let long_ago = SystemTime::now() - Duration::from_secs(86_400);
        set_last_sent(&mut subs, "nav1", ControlId::Gain, long_ago);

        assert!(subs.is_subscribed_path("radars.nav1.controls.gain", true));
        let after = last_sent_of(&subs, "nav1", ControlId::Gain).unwrap();
        assert!(after > long_ago, "delivered update must advance last_sent");
    }

    #[test]
    fn throttle_min_period_drop_does_not_advance_last_sent() {
        // Same regression as above, but on the `min_period` branch with a
        // non-Fixed policy — the bug existed in both code paths.
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![throttled_path(
                "radars.nav1.controls.gain",
                Policy::Instant,
                None,
                Some(1_000_000_000),
            )],
        })
        .unwrap();

        let pinned = SystemTime::now();
        set_last_sent(&mut subs, "nav1", ControlId::Gain, pinned);

        assert!(!subs.is_subscribed_path("radars.nav1.controls.gain", false));
        assert_eq!(last_sent_of(&subs, "nav1", ControlId::Gain), Some(pinned));
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use crate::radar::settings::new_numeric;

    fn update(values: Vec<DeltaValue>) -> DeltaUpdate {
        DeltaUpdate {
            source: Some(PACKAGE.to_string()),
            timestamp: Some(Utc::now()),
            meta: Vec::new(),
            values,
        }
    }

    fn navigation_value() -> DeltaValue {
        DeltaValue::Navigation {
            path: "navigation.headingTrue".to_string(),
            value: 1.5,
        }
    }

    fn delta_of(updates: Vec<DeltaUpdate>) -> SignalKDelta {
        let mut delta = SignalKDelta::new();
        delta.updates = updates;
        delta
    }

    /// Filtering a delta against a subscription empties the values the client
    /// did not ask for, leaving an update carrying only a source and a
    /// timestamp. Sending that says nothing, and a client subscribed to little
    /// enough gets a steady stream of it — `subscribe=none` turned every
    /// own-ship update into one.
    #[test]
    fn an_update_with_nothing_left_in_it_is_not_sent() {
        let delta = delta_of(vec![update(vec![])]);

        assert!(delta.build().is_none());
    }

    #[test]
    fn a_delta_with_values_is_sent() {
        let delta = delta_of(vec![update(vec![navigation_value()])]);

        assert_eq!(delta.build().map(|d| d.updates.len()), Some(1));
    }

    /// A control is described before its value ever changes, so an update
    /// carrying only meta still has something to say.
    #[test]
    fn a_delta_carrying_only_meta_is_sent() {
        let (_, control) = new_numeric(ControlId::Gain, 0., 100.).take();
        let mut delta = SignalKDelta::new();
        delta.add_meta_for_control("nav1034A", &control);

        assert_eq!(delta.build().map(|d| d.updates.len()), Some(1));
    }

    /// An emptied update must not take a full one down with it, nor survive
    /// alongside it.
    #[test]
    fn only_the_emptied_updates_are_dropped() {
        let delta = delta_of(vec![
            update(vec![]),
            update(vec![navigation_value()]),
            update(vec![]),
        ]);

        let built = delta.build().expect("a delta with values is sent");
        assert_eq!(built.updates.len(), 1);
        assert_eq!(built.updates[0].values.len(), 1);
    }
}
