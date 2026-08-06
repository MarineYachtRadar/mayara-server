use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    cmp::min,
    collections::{HashMap, HashSet},
    str::FromStr,
    time::{Duration, SystemTime},
};
use strum::{EnumString, IntoEnumIterator, VariantNames};
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

    pub fn build(self) -> Option<Self> {
        if !self.updates.is_empty() {
            return Some(self);
        }
        None
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
    paths: HashMap<String, HashMap<ControlId, PathSubscribe>>,
    /// Target subscriptions: radar_id -> wildcard pattern (e.g., "targets.*")
    target_subscriptions: HashMap<String, Vec<String>>,
    /// Navigation path subscriptions (e.g., "navigation.headingTrue")
    navigation_subscriptions: Vec<String>,
    /// Vessel (AIS) path subscriptions (e.g., "vessels.*")
    vessel_subscriptions: Vec<String>,
    /// Signal K `notifications.*` path subscriptions (e.g.,
    /// `notifications.radar.*`, `notifications.radar.nav1.guardZone.1`).
    /// Without this list `apply_subscriptions` would drop every emitted
    /// notification delta because the generic control-path matcher
    /// rejects the `notifications.` prefix as an unknown control id.
    notification_subscriptions: Vec<String>,
}

impl ActiveSubscriptions {
    pub fn new(mode: Subscribe) -> ActiveSubscriptions {
        ActiveSubscriptions {
            mode,
            paths: HashMap::new(),
            timeout: Duration::from_secs(99999999),
            target_subscriptions: HashMap::new(),
            navigation_subscriptions: Vec::new(),
            vessel_subscriptions: Vec::new(),
            notification_subscriptions: Vec::new(),
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

    /// Subscribe to paths. Returns true if a new AIS vessel subscription was
    /// added. Additive: it adds explicit path subscriptions without changing the
    /// query baseline (`none`/`self`/`all`), matching Signal K semantics.
    pub fn subscribe(&mut self, subscription: Subscription) -> Result<bool, RadarError> {
        let mut period = u64::MAX;
        let mut ais_subscribed = false;
        for path_subscription in subscription.subscribe {
            let path = &path_subscription.path;

            // Signal K's subscribe-everything wildcard — what mayara itself
            // sends to its upstream Signal K server, and so what a client can
            // send to either server. It is not a control path: fan it out to
            // every category, then fall through so the control handler below
            // sees it too.
            if path == "*" {
                log::debug!("Subscribing to all paths");
                for list in [
                    &mut self.navigation_subscriptions,
                    &mut self.notification_subscriptions,
                ] {
                    if !list.iter().any(|p| p == "*") {
                        list.push("*".to_string());
                    }
                }
                if !self.vessel_subscriptions.iter().any(|p| p == "*") {
                    self.vessel_subscriptions.push("*".to_string());
                    ais_subscribed = true;
                }
            }

            // Handle navigation subscriptions (e.g., "navigation.headingTrue")
            if path.starts_with("navigation.") {
                log::debug!("Subscribing to navigation path: {}", path);
                if !self.navigation_subscriptions.contains(path) {
                    self.navigation_subscriptions.push(path.clone());
                }
                continue;
            }

            // Handle target subscriptions (e.g., "radars.nav1.targets.*")
            if path.contains(".targets.") {
                let (radar_id, target_pattern) = extract_path(path);
                log::debug!(
                    "Subscribing to targets for radar '{}' pattern '{}'",
                    radar_id,
                    target_pattern
                );
                let patterns = self
                    .target_subscriptions
                    .entry(radar_id.to_string())
                    .or_default();
                if !patterns.iter().any(|p| p == target_pattern) {
                    patterns.push(target_pattern.to_string());
                }
                continue;
            }

            // Handle vessel (AIS) subscriptions (e.g., "vessels.*")
            if path.starts_with("vessels.") {
                log::debug!("Subscribing to vessel path: {}", path);
                if !self.vessel_subscriptions.contains(path) {
                    self.vessel_subscriptions.push(path.clone());
                    ais_subscribed = true;
                }
                continue;
            }

            // Handle Signal K notification subscriptions
            // (e.g., "notifications.radar.*", "notifications.radar.nav1.guardZone.1").
            if path.starts_with("notifications.") {
                log::debug!("Subscribing to notification path: {}", path);
                if !self.notification_subscriptions.contains(path) {
                    self.notification_subscriptions.push(path.clone());
                }
                continue;
            }

            // Handle control subscriptions (existing logic)
            let (radar_id, control_id) = extract_path(path);
            let mut paths = self.paths.get_mut(radar_id);
            if paths.is_none() {
                log::debug!("Creating radar '{}' self", radar_id);
                self.paths.insert(radar_id.to_string(), HashMap::new());
                paths = self.paths.get_mut(radar_id);
            }
            let paths = paths.unwrap();

            if control_id.contains("*") {
                for id in ControlId::iter() {
                    let matcher = WildMatch::new(control_id);
                    if matcher.matches(&id.to_string()) {
                        log::trace!("{} matches {}", id, control_id);
                        paths.insert(id, path_subscription.clone());
                    }
                }
                if let Some(p) = path_subscription.min_period {
                    period = min(p, period);
                }
                if let Some(p) = path_subscription.period {
                    period = min(p, period);
                }
            } else {
                match ControlId::from_str(control_id) {
                    Ok(control_id) => {
                        if let Some(p) = path_subscription.min_period {
                            period = min(p, period);
                        }
                        if let Some(p) = path_subscription.period {
                            period = min(p, period);
                        }
                        paths.insert(control_id, path_subscription);
                    }
                    Err(_e) => {
                        // Not a control mayara knows. Signal K answers a
                        // subscription for a path it has no data for by simply
                        // never sending it, so do the same rather than failing
                        // the whole subscribe — one unrecognised leaf must not
                        // cost the client the paths it asked for alongside it.
                        log::debug!(
                            "Ignoring subscription to radar '{}' path '{}': not a known control",
                            radar_id,
                            control_id,
                        );
                        continue;
                    }
                }
            }
        }
        self.set_timeout(period);

        Ok(ais_subscribed)
    }

    pub fn desubscribe(&mut self, subscription: Desubscription) -> Result<(), RadarError> {
        for path_desubscription in subscription.desubscribe {
            let path = &path_desubscription.path;

            // Handle navigation desubscriptions (e.g., "navigation.headingTrue")
            if path.starts_with("navigation.") {
                log::debug!("Desubscribing from navigation path: {}", path);
                self.navigation_subscriptions.retain(|p| p != path);
                continue;
            }

            // Handle target desubscriptions (e.g., "radars.nav1.targets.*")
            if path.contains(".targets.") {
                let (radar_id, target_pattern) = extract_path(path);
                log::debug!(
                    "Desubscribing from targets for radar '{}' pattern '{}'",
                    radar_id,
                    target_pattern
                );
                if let Some(patterns) = self.target_subscriptions.get_mut(radar_id) {
                    patterns.retain(|p| p != target_pattern);
                    if patterns.is_empty() {
                        self.target_subscriptions.remove(radar_id);
                    }
                }
                continue;
            }

            // Handle vessel (AIS) desubscriptions (e.g., "vessels.*")
            if path.starts_with("vessels.") {
                log::debug!("Desubscribing from vessel path: {}", path);
                self.vessel_subscriptions.retain(|p| p != path);
                continue;
            }

            // Handle Signal K notification desubscriptions.
            if path.starts_with("notifications.") {
                log::debug!("Desubscribing from notification path: {}", path);
                self.notification_subscriptions.retain(|p| p != path);
                continue;
            }

            // Handle control desubscriptions (existing logic)
            let (radar_id, control_id) = extract_path(path);
            let paths = self.paths.get_mut(radar_id);
            if paths.is_none() {
                continue;
            }
            let paths = paths.unwrap();

            if control_id.contains("*") {
                for id in ControlId::iter() {
                    let matcher = WildMatch::new(control_id);
                    if matcher.matches(&id.to_string()) {
                        paths.remove(&id);
                    }
                }
            } else {
                match ControlId::from_str(control_id) {
                    Ok(id) => {
                        paths.remove(&id);
                    }
                    Err(_e) => {
                        log::warn!(
                            "Cannot desubscribe context '{}' path '{}': does not exist",
                            radar_id,
                            path_desubscription.path
                        );
                        return Err(RadarError::CannotParseControlId(control_id.to_string()));
                    }
                }
            }
        }

        Ok(())
    }

    //
    // This is called with a RadarControlValue generated internally, with a fixed path and no wildcards
    // and a control_id filled in.
    //
    pub fn is_subscribed(&mut self, rcv: &RadarControlValue, full: bool) -> bool {
        match self.mode {
            // Radar control values are own-ship data, so they are in the baseline
            // for both `self` and `all`.
            Subscribe::All | Subscribe::SelfOnly => {
                return true;
            }
            Subscribe::None => {}
        }
        if let (Some(radar_id), Some(control_id)) = (rcv.radar_id.as_deref(), &rcv.control_id) {
            for key in [radar_id, "*"] {
                if let Some(paths) = self.paths.get_mut(key)
                    && let Some(path) = paths.get_mut(control_id)
                {
                    let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);
                    let now = SystemTime::now();

                    if *policy == Policy::Fixed {
                        if !full {
                            return false;
                        }
                        if let Some(period) = path.period
                            && let Some(last) = path.last_sent
                            && last + Duration::from_micros(period) > now
                        {
                            return false;
                        }
                    }

                    if let Some(min_period) = path.min_period
                        && let Some(last) = path.last_sent
                        && last + Duration::from_micros(min_period) > now
                    {
                        return false;
                    }

                    path.last_sent = Some(now);
                    return true;
                }
            }
        } else {
            panic!("Invalid use of is_subscribed(), can only be done on internal RCV");
        }

        false
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

        // Handle navigation paths (e.g., "navigation.headingTrue")
        if path.starts_with("navigation.") {
            return self.is_subscribed_navigation_path(path);
        }

        // Handle target paths (e.g., "radars.nav1.targets.5")
        if path.contains(".targets.") {
            return self.is_subscribed_target_path(path);
        }

        // Handle vessel (AIS) paths (e.g., "vessels.227334400")
        if path.starts_with("vessels.") {
            return self.is_subscribed_vessel_path(path);
        }

        // Handle Signal K notifications paths (e.g.,
        // "notifications.radar.nav1.guardZone.1").
        if path.starts_with("notifications.") {
            return self.is_subscribed_notification_path(path);
        }

        // Handle control paths (existing logic)
        let (radar_id, control_id) = extract_path(path);
        let control_id = match ControlId::from_str(control_id) {
            Ok(c) => c,
            Err(_) => {
                return false;
            }
        };

        for key in [radar_id, "*"] {
            if let Some(paths) = self.paths.get_mut(key)
                && let Some(path) = paths.get_mut(&control_id)
            {
                let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);
                let now = SystemTime::now();

                if *policy == Policy::Fixed {
                    if !full {
                        return false;
                    }
                    if let Some(period) = path.period
                        && let Some(last) = path.last_sent
                        && last + Duration::from_micros(period) > now
                    {
                        return false;
                    }
                }

                if let Some(min_period) = path.min_period
                    && let Some(last) = path.last_sent
                    && last + Duration::from_micros(min_period) > now
                {
                    return false;
                }

                path.last_sent = Some(now);
                return true;
            }
        }

        false
    }

    /// Check if subscribed to a navigation path
    fn is_subscribed_navigation_path(&self, path: &str) -> bool {
        for subscribed_path in &self.navigation_subscriptions {
            if subscribed_path == path {
                return true;
            }
            // Support wildcard matching (e.g., "navigation.*")
            if subscribed_path.contains('*') {
                let matcher = WildMatch::new(subscribed_path);
                if matcher.matches(path) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if subscribed to a target path
    fn is_subscribed_target_path(&self, path: &str) -> bool {
        // Extract radar_id and target part from path like "radars.nav1.targets.5"
        let (radar_id, target_part) = extract_path(path);

        // Check both specific radar and wildcard subscriptions
        for key in [radar_id, "*"] {
            if let Some(patterns) = self.target_subscriptions.get(key) {
                for pattern in patterns {
                    if pattern == target_part {
                        return true;
                    }
                    // Support wildcard matching (e.g., "targets.*" matches "targets.5")
                    if pattern.contains('*') {
                        let matcher = WildMatch::new(pattern);
                        if matcher.matches(target_part) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Check if subscribed to a vessel (AIS) path
    /// Whether the client wants AIS data for `context` (e.g.
    /// `vessels.urn:mrn:imo:mmsi:431004411`). Another vessel's data is never in
    /// the `self` baseline, so outside `subscribe=all` it takes an explicit
    /// `vessels.*` subscription — matching Signal K, where `self` means own
    /// ship only.
    fn is_subscribed_ais(&self, context: &str) -> bool {
        match self.mode {
            Subscribe::All => true,
            Subscribe::SelfOnly | Subscribe::None => self.is_subscribed_vessel_path(context),
        }
    }

    fn is_subscribed_vessel_path(&self, path: &str) -> bool {
        for subscribed_path in &self.vessel_subscriptions {
            if subscribed_path == path {
                return true;
            }
            // Support wildcard matching (e.g., "vessels.*" matches "vessels.227334400")
            if subscribed_path.contains('*') {
                let matcher = WildMatch::new(subscribed_path);
                if matcher.matches(path) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if subscribed to a Signal K notification path. Same exact /
    /// wildcard semantics as the navigation and vessel helpers so a
    /// subscription to `notifications.radar.*` covers every per-radar /
    /// per-zone leaf path mayara emits.
    fn is_subscribed_notification_path(&self, path: &str) -> bool {
        for subscribed_path in &self.notification_subscriptions {
            if subscribed_path == path {
                return true;
            }
            if subscribed_path.contains('*') {
                let matcher = WildMatch::new(subscribed_path);
                if matcher.matches(path) {
                    return true;
                }
            }
        }
        false
    }
}

fn extract_path(mut path: &str) -> (&str, &str) {
    if path.starts_with("radars.") {
        path = &path["radars.".len()..];
    }
    if path == "*" {
        return ("*", "*");
    }
    if let Some((radar, mut control)) = path.split_once('.') {
        if control.starts_with("controls.") {
            control = &control["controls.".len()..];
        }
        return (radar, control);
    }

    ("*", path)
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
    #[serde(skip)]
    #[schema(ignore)]
    last_sent: Option<SystemTime>,
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
                    last_sent: None,
                },
                PathSubscribe {
                    path: "radars.2.controls.gain".to_string(),
                    period: Some(1000),
                    policy: Some(Policy::Instant),
                    min_period: None,
                    last_sent: None,
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
            last_sent: None,
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
        let patterns = subs.target_subscriptions.get("nav1").unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0], "targets.*");
    }

    #[test]
    fn target_desubscribe_removes_pattern_and_empty_bucket() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("radars.nav1.targets.*"), path("radars.nav1.targets.5")],
        })
        .unwrap();
        assert_eq!(subs.target_subscriptions.get("nav1").unwrap().len(), 2);

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.nav1.targets.*")],
        })
        .unwrap();
        let remaining = subs.target_subscriptions.get("nav1").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0], "targets.5");

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("radars.nav1.targets.5")],
        })
        .unwrap();
        assert!(!subs.target_subscriptions.contains_key("nav1"));
    }

    #[test]
    fn navigation_desubscribe_removes_path() {
        let mut subs = ActiveSubscriptions::new(Subscribe::None);
        subs.subscribe(Subscription {
            subscribe: vec![path("navigation.headingTrue"), path("navigation.position")],
        })
        .unwrap();
        assert_eq!(subs.navigation_subscriptions.len(), 2);

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("navigation.headingTrue")],
        })
        .unwrap();
        assert_eq!(subs.navigation_subscriptions, vec!["navigation.position"]);
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
        assert_eq!(subs.notification_subscriptions.len(), 2);

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("notifications.foo")],
        })
        .unwrap();
        assert_eq!(
            subs.notification_subscriptions,
            vec!["notifications.radar.*"]
        );
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
            last_sent: None,
        }
    }

    fn last_sent_of(
        subs: &ActiveSubscriptions,
        radar: &str,
        control: ControlId,
    ) -> Option<SystemTime> {
        subs.paths
            .get(radar)
            .and_then(|m| m.get(&control))
            .and_then(|p| p.last_sent)
    }

    fn set_last_sent(
        subs: &mut ActiveSubscriptions,
        radar: &str,
        control: ControlId,
        ts: SystemTime,
    ) {
        subs.paths
            .get_mut(radar)
            .and_then(|m| m.get_mut(&control))
            .unwrap()
            .last_sent = Some(ts);
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
