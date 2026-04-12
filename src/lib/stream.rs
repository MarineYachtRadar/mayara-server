use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    radar::settings::{BareControlValue, Control, ControlDefinition, ControlId, RadarControlValue},
    radar::target::ArpaTargetApi,
    radar::{RadarError, SharedRadars},
};

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
    /// Array of update batches, each containing changed control values
    updates: Vec<DeltaUpdate>,
}

impl SignalKDelta {
    pub fn new() -> SignalKDelta {
        Self {
            updates: Vec::new(),
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
                if let Some(radar_id) = path.split('.').nth(1) {
                    if !meta_sent.contains(radar_id) {
                        // Found a radar whose meta hasn't been sent yet
                        needs_meta = true;
                        break;
                    }
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

    /// Add an AIS vessel update to the delta message.
    pub fn add_ais_vessel_update(&mut self, path: &str, vessel: &crate::ais::AisVesselApi) {
        let value = serde_json::to_value(vessel).unwrap_or(serde_json::Value::Null);
        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some("signalk".to_string()),
            meta: Vec::new(),
            values: vec![DeltaValue::Ais {
                path: path.to_string(),
                value,
            }],
        };
        self.updates.push(delta_update);
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
        for update in self.updates.iter_mut() {
            update
                .values
                .retain(|dv| subscriptions.is_subscribed_path(dv.path(), false));
        }
    }

    pub fn build(self) -> Option<Self> {
        if self.updates.len() > 0 {
            return Some(self);
        }
        return None;
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
    /// AIS vessel update (structured data from AIS store)
    Ais {
        /// Vessel path (e.g., "vessels.227334400")
        path: String,
        /// Structured vessel data
        value: serde_json::Value,
    },
}

impl DeltaValue {
    fn path(&self) -> &str {
        match self {
            DeltaValue::Control { path, .. } => path,
            DeltaValue::Target { path, .. } => path,
            DeltaValue::Navigation { path, .. } => path,
            DeltaValue::Ais { path, .. } => path,
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

        let delta_update = DeltaUpdate {
            timestamp: None,
            source: Some(PACKAGE.to_string()),
            meta: Vec::new(),
            values,
        };

        return delta_update;
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

fn get_meta_delta(
    radars: &SharedRadars,
    meta_sent: &mut HashSet<String>,
) -> Option<DeltaUpdate> {
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

    if meta.len() == 0 {
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

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Subscribe {
    None,
    Some,
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

    /// Subscribe to paths. Returns true if a new AIS vessel subscription was added.
    pub fn subscribe(&mut self, subscription: Subscription) -> Result<bool, RadarError> {
        self.mode = Subscribe::Some;
        let mut period = u64::MAX;
        let mut ais_subscribed = false;
        for path_subscription in subscription.subscribe {
            let path = &path_subscription.path;

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
                        log::warn!(
                            "Cannot subscribe radar '{}' path '{}': does not exist",
                            radar_id,
                            control_id,
                        );
                        return Err(RadarError::CannotParseControlId(control_id.to_string()));
                    }
                }
            }
        }
        self.set_timeout(period);

        Ok(ais_subscribed)
    }

    pub fn desubscribe(&mut self, subscription: Desubscription) -> Result<(), RadarError> {
        self.mode = Subscribe::Some;
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
                match ControlId::from_str(&control_id) {
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
            Subscribe::All => {
                return true;
            }
            Subscribe::None => {
                return false;
            }
            Subscribe::Some => {}
        }
        if let (Some(radar_id), Some(control_id)) = (rcv.radar_id.as_deref(), &rcv.control_id) {
            for key in [radar_id, "*"] {
                if let Some(paths) = self.paths.get_mut(key) {
                    if let Some(path) = paths.get_mut(control_id) {
                        let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);

                        if *policy == Policy::Fixed {
                            if !full {
                                return false;
                            }
                            if let Some(period) = path.period {
                                let now = SystemTime::now();

                                if path.last_sent.is_none()
                                    || path.last_sent.unwrap() + Duration::from_micros(period) > now
                                {
                                    path.last_sent = Some(now);
                                    return false;
                                }
                            }
                        }

                        if let Some(min_period) = path.min_period {
                            let now = SystemTime::now();

                            if path.last_sent.is_none()
                                || path.last_sent.unwrap() + Duration::from_micros(min_period) > now
                            {
                                path.last_sent = Some(now);
                                return false;
                            }
                        }
                        return true;
                    }
                }
            }
        } else {
            panic!("Invalid use of is_subscribed(), can only be done on internal RCV");
        }

        return false;
    }

    pub fn is_subscribed_path(&mut self, path: &str, full: bool) -> bool {
        match self.mode {
            Subscribe::All => {
                return true;
            }
            Subscribe::None => {
                return false;
            }
            Subscribe::Some => {}
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

        // Handle control paths (existing logic)
        let (radar_id, control_id) = extract_path(path);
        let control_id = match ControlId::from_str(control_id) {
            Ok(c) => c,
            Err(_) => {
                return false;
            }
        };

        for key in [radar_id, "*"] {
            if let Some(paths) = self.paths.get_mut(key) {
                if let Some(path) = paths.get_mut(&control_id) {
                    let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);

                    if *policy == Policy::Fixed {
                        if !full {
                            return false;
                        }
                        if let Some(period) = path.period {
                            let now = SystemTime::now();

                            if path.last_sent.is_none()
                                || path.last_sent.unwrap() + Duration::from_micros(period) > now
                            {
                                path.last_sent = Some(now);
                                return false;
                            }
                        }
                    }

                    if let Some(min_period) = path.min_period {
                        let now = SystemTime::now();

                        if path.last_sent.is_none()
                            || path.last_sent.unwrap() + Duration::from_micros(min_period) > now
                        {
                            path.last_sent = Some(now);
                            return false;
                        }
                    }
                    return true;
                }
            }
        }

        return false;
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
            .map_err(|_| serde::de::Error::unknown_variant(&s, &Policy::VARIANTS)),
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
        let mut subs = ActiveSubscriptions::new(Subscribe::Some);
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
        let mut subs = ActiveSubscriptions::new(Subscribe::Some);
        subs.subscribe(Subscription {
            subscribe: vec![
                path("radars.nav1.targets.*"),
                path("radars.nav1.targets.5"),
            ],
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
        let mut subs = ActiveSubscriptions::new(Subscribe::Some);
        subs.subscribe(Subscription {
            subscribe: vec![
                path("navigation.headingTrue"),
                path("navigation.position"),
            ],
        })
        .unwrap();
        assert_eq!(subs.navigation_subscriptions.len(), 2);

        subs.desubscribe(Desubscription {
            desubscribe: vec![path("navigation.headingTrue")],
        })
        .unwrap();
        assert_eq!(subs.navigation_subscriptions, vec!["navigation.position"]);
    }
}
