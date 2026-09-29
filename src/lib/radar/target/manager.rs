//! Target tracker manager.
//!
//! Manages target trackers based on merge mode (single shared tracker vs per-radar trackers).

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, mpsc};

use super::blob::CompletedBlob;
use super::tracker::{
    CandidateSource, ProcessResult, TargetCandidate, TargetStatus, TargetTracker,
};
use super::{
    ArpaTargetApi, PositionCovariance, TargetDangerApi, TargetMotionApi, TargetPositionApi,
};
use crate::radar::{GeoPosition, KN_TO_MS};
use crate::stream::{NotificationMethod, NotificationState, NotificationValue, SignalKDelta};

/// Updates a target needs before it is broadcast to clients. Below this
/// the motion estimate is too green to be worth reporting, so a target
/// that dies young is never seen by a client and never alarms.
const MIN_BROADCAST_UPDATE_COUNT: u32 = 4;

/// How long the radar driving the merged revolution clock may stay silent
/// before another radar is allowed to take it over. Long enough that a
/// slow antenna between sweeps is never mistaken for a departed radar.
const MERGED_CLOCK_TAKEOVER: Duration = Duration::from_secs(30);

/// Position variance (m²) of a manually acquired target. 1250 m² in each
/// axis is a ~100 m uncertainty, which is about how well a user can place a
/// click on a PPI.
const MARPA_CLICK_VARIANCE: f64 = 1250.0;

/// Guard zones a radar carries, numbered 1..=GUARD_ZONE_COUNT on the wire.
/// Zone 0 is reserved for manual MARPA acquisition and never alarms.
const GUARD_ZONE_COUNT: u8 = 2;

/// Index into a per-radar alarm array for a wire zone number, or `None`
/// when the number is outside the guard zones (manual MARPA, or a value
/// a brand backend should never produce).
fn guard_zone_index(zone: u8) -> Option<usize> {
    (1..=GUARD_ZONE_COUNT)
        .contains(&zone)
        .then(|| (zone - 1) as usize)
}

/// Context from the spoke that produced a blob
#[derive(Clone, Debug)]
pub struct SpokeContext {
    /// Timestamp (millis since epoch)
    pub time: u64,
    /// Range in meters
    pub range: u32,
    /// True bearing in spokes (None if no heading available)
    pub bearing: Option<u16>,
    /// Radar latitude
    pub lat: Option<f64>,
    /// Radar longitude
    pub lon: Option<f64>,
    /// Spokes per revolution
    pub spokes_per_revolution: u16,
    /// Spoke data length (pixels)
    pub spoke_len: usize,
    /// Head-relative angle in spokes
    pub angle: u16,
    /// Maximum target speed in m/s (from ArpaDetectMaxSpeed: 0=25kn, 1=40kn, 2=50kn)
    pub max_target_speed_ms: f64,
    /// Whether DopplerAutoTrack is enabled (track approaching Doppler targets everywhere)
    pub doppler_auto_track: bool,
}

impl SpokeContext {
    /// Get max target speed based on ArpaDetectMaxSpeed setting
    pub fn max_speed_from_mode(mode: i32) -> f64 {
        match mode {
            0 => 25.0 * KN_TO_MS, // Normal
            1 => 40.0 * KN_TO_MS, // Medium
            _ => 50.0 * KN_TO_MS, // Fast
        }
    }
}

/// Message sent from radar to tracker
pub struct BlobMessage {
    pub radar_key: String,
    pub blob: CompletedBlob,
    pub context: SpokeContext,
}

/// What a radar feeds the tracker manager.
pub enum TrackerInput {
    /// A blob the detector completed on this radar.
    Blob(BlobMessage),
    /// The radar's antenna finished a revolution. Drives every
    /// revolution-based timeout, so it is sent from the spoke stream and
    /// arrives whether or not the sweep produced any echoes.
    Revolution {
        radar_key: String,
        radar_position: Option<GeoPosition>,
    },
}

/// MARPA (Manual Radar Plotting Aid) request from user click
#[derive(Clone, Debug)]
pub struct MarpaRequest {
    /// Radar key
    pub radar_key: String,
    /// Target position
    pub position: GeoPosition,
    /// Radar position (for computing bearing/distance in API)
    pub radar_position: Option<GeoPosition>,
    /// Timestamp (millis since epoch)
    pub time: u64,
    /// Estimated size in meters (default ~30m for ship)
    pub size_meters: f64,
}

/// Command sent to the tracker manager
#[derive(Debug)]
pub enum TrackerCommand {
    /// MARPA request from user click
    Marpa(MarpaRequest),
    /// Delete a target by ID
    DeleteTarget { radar_key: String, target_id: u64 },
    /// Clear all targets for a radar
    ClearTargets { radar_key: String },
    /// Get all targets for a radar (or all radars if radar_key is None)
    GetTargets {
        radar_key: Option<String>,
        radar_position: Option<GeoPosition>,
        response_tx: tokio::sync::oneshot::Sender<Vec<ArpaTargetApi>>,
    },
}

/// Manages target trackers for all radars
pub struct TrackerManager {
    /// Per-radar trackers (when merge_mode = false)
    per_radar_trackers: HashMap<String, TargetTracker>,
    /// Shared tracker (when merge_mode = true)
    shared_tracker: Option<TargetTracker>,
    /// Whether targets are merged across radars
    merge_mode: bool,
    /// Radar indices for per-radar ID generation
    radar_indices: HashMap<String, usize>,
    /// Next radar index
    next_radar_index: usize,
    /// Broadcast sender for GUI updates
    sk_client_tx: broadcast::Sender<SignalKDelta>,
    /// Command receiver for MARPA requests and control changes
    command_rx: mpsc::Receiver<TrackerCommand>,
    /// Which radar drives the shared tracker's revolution clock in merged
    /// mode, and when it last did. Unused when `merge_mode` is false.
    merged_clock: Option<(String, Instant)>,
    /// Whether `notifications.radar.<key>.guardZone.<n>` currently stands
    /// raised, per (radar_key, [`guard_zone_index`]).
    ///
    /// The alarm means "a target acquired in this zone is still being
    /// tracked", not "a target is inside the zone right now": a target
    /// carries the zone it was acquired in for life, so one that wanders
    /// out keeps the alarm up until it is lost or deleted. That mirrors
    /// the GUI's audible alert, which is likewise driven by acquisition.
    guard_zone_alarm: HashMap<String, [bool; GUARD_ZONE_COUNT as usize]>,
}

impl TrackerManager {
    /// Create a new tracker manager, returns (manager, command_tx)
    pub fn new(
        merge_mode: bool,
        sk_client_tx: broadcast::Sender<SignalKDelta>,
    ) -> (Self, mpsc::Sender<TrackerCommand>) {
        let (command_tx, command_rx) = mpsc::channel(32);

        let manager = TrackerManager {
            per_radar_trackers: HashMap::new(),
            shared_tracker: if merge_mode {
                Some(TargetTracker::new_merged())
            } else {
                None
            },
            merge_mode,
            radar_indices: HashMap::new(),
            next_radar_index: 1,
            sk_client_tx,
            command_rx,
            merged_clock: None,
            guard_zone_alarm: HashMap::new(),
        };

        (manager, command_tx)
    }

    /// Raise `notifications.radar.<key>.guardZone.<zone>` for a target that
    /// has just been confirmed inside the zone. Fired per target rather
    /// than only on the first one, so a second vessel entering an already
    /// alarming zone is still announced.
    fn raise_guard_zone_alarm(&mut self, radar_key: &str, zone: u8, target_id: u64) {
        let Some(idx) = guard_zone_index(zone) else {
            return;
        };
        self.guard_zone_alarm
            .entry(radar_key.to_string())
            .or_insert([false; GUARD_ZONE_COUNT as usize])[idx] = true;
        self.emit_guard_zone_notification(
            radar_key,
            zone,
            NotificationState::Alert,
            format!(
                "Radar {} guard zone {}: target {} acquired",
                radar_key, zone, target_id
            ),
        );
    }

    /// Clear the alarm for every zone that no longer holds a target.
    /// Occupancy is recomputed from tracker state rather than tallied
    /// from enter/leave events, so the Lost timeout, an explicit target
    /// delete and a radar-wide clear all release the alarm identically.
    fn refresh_guard_zone_alarms(&mut self) {
        let mut occupied: HashMap<&str, [bool; GUARD_ZONE_COUNT as usize]> = HashMap::new();
        let trackers: Vec<&TargetTracker> = if self.merge_mode {
            self.shared_tracker.iter().collect()
        } else {
            self.per_radar_trackers.values().collect()
        };
        for target in trackers.iter().flat_map(|t| t.get_active_targets()) {
            if target.status == TargetStatus::Lost
                || target.update_count < MIN_BROADCAST_UPDATE_COUNT
            {
                continue;
            }
            if let Some(idx) = target.source_zone.and_then(guard_zone_index) {
                occupied
                    .entry(target.last_radar_key.as_str())
                    .or_insert([false; GUARD_ZONE_COUNT as usize])[idx] = true;
            }
        }

        let mut cleared = Vec::new();
        for (radar_key, alarms) in self.guard_zone_alarm.iter_mut() {
            let occupancy = occupied
                .get(radar_key.as_str())
                .copied()
                .unwrap_or([false; GUARD_ZONE_COUNT as usize]);
            for (idx, alarm) in alarms.iter_mut().enumerate() {
                if *alarm && !occupancy[idx] {
                    *alarm = false;
                    cleared.push((radar_key.clone(), idx as u8 + 1));
                }
            }
        }

        for (radar_key, zone) in cleared {
            self.emit_guard_zone_notification(
                &radar_key,
                zone,
                NotificationState::Normal,
                format!("Radar {} guard zone {}: clear", radar_key, zone),
            );
        }
    }

    fn emit_guard_zone_notification(
        &self,
        radar_key: &str,
        zone: u8,
        state: NotificationState,
        message: String,
    ) {
        let path = format!("notifications.radar.{}.guardZone.{}", radar_key, zone);
        let method = match state {
            NotificationState::Normal => Vec::new(),
            _ => vec![NotificationMethod::Visual, NotificationMethod::Sound],
        };
        let value = NotificationValue {
            state,
            method,
            message,
        };
        let mut delta = SignalKDelta::new();
        delta.add_notification_update(&path, value, "mayara");
        if let Err(e) = self.sk_client_tx.send(delta) {
            log::trace!("Failed to broadcast guard-zone notification: {}", e);
        }
    }

    /// Get or create tracker for a radar
    fn get_or_create_tracker(&mut self, radar_key: &str) -> &mut TargetTracker {
        if self.merge_mode {
            if let Some(ref mut tracker) = self.shared_tracker {
                return tracker;
            }
            // Should not happen, but create if missing
            self.shared_tracker = Some(TargetTracker::new_merged());
            self.shared_tracker.as_mut().unwrap()
        } else {
            // Per-radar mode
            if !self.per_radar_trackers.contains_key(radar_key) {
                let index = self.get_radar_index(radar_key);
                let tracker = TargetTracker::new_per_radar(index);
                self.per_radar_trackers
                    .insert(radar_key.to_string(), tracker);
            }
            self.per_radar_trackers.get_mut(radar_key).unwrap()
        }
    }

    /// Get radar index (for per-radar ID generation)
    fn get_radar_index(&mut self, radar_key: &str) -> usize {
        if let Some(&index) = self.radar_indices.get(radar_key) {
            index
        } else {
            let index = self.next_radar_index;
            self.next_radar_index += 1;
            self.radar_indices.insert(radar_key.to_string(), index);
            log::info!("Assigned radar {} index {}", radar_key, index);
            index
        }
    }

    /// Process a blob message
    pub fn process_blob(&mut self, msg: BlobMessage) {
        let ctx = &msg.context;

        // Convert blob to geo position
        let Some(position) = blob_to_position(&msg.blob, ctx) else {
            log::trace!("Cannot convert blob to position (missing lat/lon/bearing)");
            return;
        };

        // Radar position for API conversion
        let radar_position = match (ctx.lat, ctx.lon) {
            (Some(lat), Some(lon)) => Some(GeoPosition::new(lat, lon)),
            _ => None,
        };

        // Determine candidate source: guard zone takes priority, then Doppler auto-track,
        // then Anywhere (matches existing targets only, no new acquisition).
        let source = if let Some(&zone_id) = msg.blob.in_guard_zones.first() {
            CandidateSource::GuardZone(zone_id)
        } else if msg.blob.has_doppler_approaching && msg.context.doppler_auto_track {
            CandidateSource::Doppler
        } else {
            CandidateSource::Anywhere
        };

        // Create target candidate
        let candidate = TargetCandidate {
            time: ctx.time,
            position,
            size_meters: msg.blob.size_meters,
            radar_key: msg.radar_key.clone(),
            radar_position,
            max_target_speed_ms: ctx.max_target_speed_ms,
            position_covariance: blob_to_covariance(&msg.blob, ctx),
            source,
        };

        // Get tracker and process
        let tracker = self.get_or_create_tracker(&msg.radar_key);

        let result = tracker.process_candidate(candidate);

        log::debug!(
            "Processing blob: pos=({:.6}, {:.6}), angle={}, source={:?}, size={:.1}m -> {:?}",
            position.lat(),
            position.lon(),
            ctx.angle,
            source,
            msg.blob.size_meters,
            result
        );

        // Only broadcast immediately when a target is first promoted to tracking.
        // All other updates are batched and sent once per revolution to avoid flooding.
        let mut promoted_in_zone = None;
        if let ProcessResult::Promoted(target_id) = result
            && let Some(target) = tracker.get_target(target_id)
        {
            // A target acquired in a guard zone only alarms if the echo that
            // confirmed it is still in that zone. A track that drifted out
            // while it was collecting its updates is not an intrusion, and
            // one that promotes elsewhere entirely was never the same
            // object. Dropping `source_zone` rather than only skipping the
            // alarm also keeps the zone from counting as occupied in
            // `refresh_guard_zone_alarms`, which would pin another target's
            // alarm up for the rest of this track's life.
            promoted_in_zone = target
                .source_zone
                .filter(|zone| msg.blob.in_guard_zones.contains(zone))
                .map(|zone| (target_id, zone));
            let target_api = active_target_to_api(target, radar_position.as_ref());
            let mut delta = SignalKDelta::new();
            delta.add_target_update(&msg.radar_key, target_id, Some(target_api));
            if let Err(e) = self.sk_client_tx.send(delta) {
                log::trace!("Failed to broadcast promoted target: {}", e);
            }
        }

        // Alarm on promotion rather than on first acquisition: a target
        // that never survives long enough to be reported to clients is
        // clutter, not an intrusion.
        if let Some((target_id, zone)) = promoted_in_zone {
            self.raise_guard_zone_alarm(&msg.radar_key, zone, target_id);
        } else if let ProcessResult::Promoted(target_id) = result {
            self.clear_source_zone(&msg.radar_key, target_id);
        }
    }

    /// Advance a radar's revolution clock and ship the batched target update.
    ///
    /// Driven by the antenna rather than by blob arrivals, so the lost and
    /// delete timeouts, deduplication and the per-revolution broadcast all
    /// keep running through a sweep that produced no echoes at all.
    pub fn process_revolution(&mut self, radar_key: &str, radar_position: Option<GeoPosition>) {
        if self.merge_mode && !self.drives_merged_clock(radar_key) {
            return;
        }
        self.get_or_create_tracker(radar_key).complete_revolution();
        self.broadcast_all_targets(radar_key, radar_position.as_ref());
    }

    /// Whether `radar_key` may advance the shared tracker's clock.
    ///
    /// In merged mode every radar feeds one tracker, so only one of them
    /// can drive its revolutions — a dual-range antenna reporting twice per
    /// turn would otherwise halve every timeout. The first radar to report
    /// takes the clock and keeps it until it falls silent.
    fn drives_merged_clock(&mut self, radar_key: &str) -> bool {
        let now = Instant::now();
        match self.merged_clock {
            Some((ref owner, ref mut last)) if owner == radar_key => {
                *last = now;
                true
            }
            Some((_, last)) if now.duration_since(last) < MERGED_CLOCK_TAKEOVER => false,
            _ => {
                log::debug!("Radar {} now drives the merged revolution clock", radar_key);
                self.merged_clock = Some((radar_key.to_string(), now));
                true
            }
        }
    }

    /// Forget which guard zone acquired a target, so neither it nor the
    /// occupancy sweep can raise or hold that zone's alarm.
    fn clear_source_zone(&mut self, radar_key: &str, target_id: u64) {
        let tracker = if self.merge_mode {
            self.shared_tracker.as_mut()
        } else {
            self.per_radar_trackers.get_mut(radar_key)
        };
        if let Some(target) = tracker.and_then(|t| t.get_target_mut(target_id))
            && let Some(zone) = target.source_zone.take()
        {
            log::info!(
                "Target {} promoted outside guard zone {}; not alarming",
                target_id,
                zone
            );
        }
    }

    /// Get all active targets as API objects
    pub fn get_targets_api(&self, radar_position: Option<GeoPosition>) -> Vec<ArpaTargetApi> {
        let mut targets = Vec::new();

        if self.merge_mode {
            if let Some(ref tracker) = self.shared_tracker {
                for target in tracker.get_active_targets() {
                    if target.update_count >= MIN_BROADCAST_UPDATE_COUNT {
                        targets.push(active_target_to_api(target, radar_position.as_ref()));
                    }
                }
            }
        } else {
            for tracker in self.per_radar_trackers.values() {
                for target in tracker.get_active_targets() {
                    if target.update_count >= MIN_BROADCAST_UPDATE_COUNT {
                        targets.push(active_target_to_api(target, radar_position.as_ref()));
                    }
                }
            }
        }

        targets
    }

    /// Process a MARPA request (manual target acquisition from user click)
    /// MARPA targets are immediately added as active (no acquisition phase needed)
    pub fn process_marpa(&mut self, request: MarpaRequest) -> u64 {
        log::info!(
            "MARPA acquisition at ({:.6}, {:.6}) for radar {}",
            request.position.lat(),
            request.position.lon(),
            request.radar_key
        );

        // Create a candidate with default max speed (fast mode - 50 knots)
        // MARPA uses GuardZone(0) to indicate manual acquisition
        let candidate = TargetCandidate {
            time: request.time,
            position: request.position,
            size_meters: request.size_meters,
            radar_key: request.radar_key.clone(),
            radar_position: request.radar_position,
            max_target_speed_ms: SpokeContext::max_speed_from_mode(2), // Fast mode for MARPA
            // A click is only as accurate as the user's aim, and carries no
            // bearing of its own, so it starts as a circle.
            position_covariance: PositionCovariance::isotropic(MARPA_CLICK_VARIANCE),
            source: CandidateSource::GuardZone(0), // 0 = manual/MARPA
        };

        let tracker = self.get_or_create_tracker(&request.radar_key);

        // MARPA targets go directly to active - user explicitly clicked on them
        let target_id = tracker.add_active_target(&candidate);

        // Broadcast the new target
        if let Some(target) = tracker.get_target(target_id) {
            let target_api = active_target_to_api(target, request.radar_position.as_ref());

            let mut delta = SignalKDelta::new();
            delta.add_target_update(&request.radar_key, target_id, Some(target_api));

            if let Err(e) = self.sk_client_tx.send(delta) {
                log::trace!("Failed to broadcast MARPA target update: {}", e);
            }
        }

        target_id
    }

    /// Clear all active targets for a radar, broadcasting deletions to clients
    fn clear_all_targets(&mut self, radar_key: &str) {
        log::info!("Clearing all targets for radar {}", radar_key);

        let ids: Vec<u64> = if self.merge_mode {
            self.shared_tracker
                .as_ref()
                .map(|t| t.get_active_targets().map(|t| t.id).collect())
                .unwrap_or_default()
        } else {
            self.per_radar_trackers
                .get(radar_key)
                .map(|t| t.get_active_targets().map(|t| t.id).collect())
                .unwrap_or_default()
        };

        for id in ids {
            if self.merge_mode {
                if let Some(ref mut tracker) = self.shared_tracker {
                    tracker.remove_target(id);
                }
            } else if let Some(tracker) = self.per_radar_trackers.get_mut(radar_key) {
                tracker.remove_target(id);
            }
            self.broadcast_deletion(id, radar_key);
        }
    }

    /// Delete a target by ID (cancel tracking)
    pub fn delete_target(&mut self, radar_key: &str, target_id: u64) -> bool {
        log::info!("Delete target {} for radar {}", target_id, radar_key);

        let deleted = if self.merge_mode {
            if let Some(ref mut tracker) = self.shared_tracker {
                tracker.remove_target(target_id)
            } else {
                false
            }
        } else if let Some(tracker) = self.per_radar_trackers.get_mut(radar_key) {
            tracker.remove_target(target_id)
        } else {
            false
        };

        if deleted {
            self.broadcast_deletion(target_id, radar_key);
        }

        deleted
    }

    /// Run the tracker manager, receiving radar input and MARPA requests
    pub async fn run(mut self, mut tracker_rx: mpsc::Receiver<TrackerInput>) {
        use std::time::{Duration, Instant};

        log::info!(
            "TrackerManager started in {} mode",
            if self.merge_mode {
                "merged"
            } else {
                "per-radar"
            }
        );

        // Track last timeout check to ensure we check at least every second
        let mut last_timeout_check = Instant::now();
        let timeout_interval = Duration::from_secs(1);

        loop {
            // Check timeouts if enough time has passed
            if last_timeout_check.elapsed() >= timeout_interval {
                self.check_all_timeouts();
                last_timeout_check = Instant::now();
            }

            tokio::select! {
                Some(input) = tracker_rx.recv() => {
                    match input {
                        TrackerInput::Blob(msg) => self.process_blob(msg),
                        TrackerInput::Revolution { radar_key, radar_position } => {
                            self.process_revolution(&radar_key, radar_position);
                        }
                    }
                }
                Some(command) = self.command_rx.recv() => {
                    match command {
                        TrackerCommand::Marpa(request) => {
                            self.process_marpa(request);
                        }
                        TrackerCommand::DeleteTarget { radar_key, target_id } => {
                            self.delete_target(&radar_key, target_id);
                        }
                        TrackerCommand::ClearTargets { radar_key } => {
                            self.clear_all_targets(&radar_key);
                        }
                        TrackerCommand::GetTargets { radar_key, radar_position, response_tx } => {
                            let targets = self.get_targets_api(radar_position);
                            // Filter by radar_key if specified (only relevant in non-merged mode)
                            let targets = if let Some(_key) = radar_key {
                                // In merged mode, all targets are shared so we return all
                                // In per-radar mode, get_targets_api already returns all,
                                // but we could filter here if needed in the future
                                targets
                            } else {
                                targets
                            };
                            let _ = response_tx.send(targets);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(1000)) => {
                    // Periodic wake-up to check timeouts when idle
                }
                else => break,
            }
        }

        log::info!("TrackerManager shutting down");
    }

    /// Check timeouts on all trackers and broadcast deletions and lost status updates
    fn check_all_timeouts(&mut self) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Collect updates to broadcast (to avoid borrow issues)
        // Format: (target_id, radar_key, api)
        let mut lost_updates: Vec<(u64, String, ArpaTargetApi)> = Vec::new();
        let mut deletions: Vec<(u64, String)> = Vec::new(); // (target_id, radar_key)

        if self.merge_mode {
            if let Some(ref mut tracker) = self.shared_tracker {
                let (deleted_ids, lost_ids) = tracker.check_timeouts(current_time);

                // Collect lost status updates
                for id in &lost_ids {
                    if let Some(target) = tracker.get_target(*id) {
                        let radar_key = target.last_radar_key.clone();
                        let api = active_target_to_api(target, target.last_radar_position.as_ref());
                        lost_updates.push((*id, radar_key, api));
                    }
                }

                // Collect deletions - use last_radar_key for path
                for id in &deleted_ids {
                    if let Some(target) = tracker.get_target(*id) {
                        deletions.push((*id, target.last_radar_key.clone()));
                    } else {
                        // Target already removed, use empty key (shouldn't happen)
                        deletions.push((*id, String::new()));
                    }
                }
            }
        } else {
            let radar_keys: Vec<String> = self.per_radar_trackers.keys().cloned().collect();
            for radar_key in radar_keys {
                if let Some(tracker) = self.per_radar_trackers.get_mut(&radar_key) {
                    let (deleted_ids, lost_ids) = tracker.check_timeouts(current_time);

                    // Collect lost status updates
                    for id in &lost_ids {
                        if let Some(target) = tracker.get_target(*id) {
                            let api =
                                active_target_to_api(target, target.last_radar_position.as_ref());
                            lost_updates.push((*id, radar_key.clone(), api));
                        }
                    }

                    // Collect deletions
                    for id in deleted_ids {
                        deletions.push((id, radar_key.clone()));
                    }
                }
            }
        }

        // Now broadcast outside the tracker borrow
        for (target_id, radar_key, api) in lost_updates {
            self.broadcast_lost_update(target_id, &radar_key, api);
        }

        for (target_id, radar_key) in deletions {
            self.broadcast_deletion(target_id, &radar_key);
        }

        self.refresh_guard_zone_alarms();
    }

    /// Broadcast all tracking targets in a single batched delta (once per revolution)
    fn broadcast_all_targets(&self, radar_key: &str, radar_position: Option<&GeoPosition>) {
        let targets: Vec<(u64, ArpaTargetApi)> = if self.merge_mode {
            self.shared_tracker
                .as_ref()
                .map(|t| {
                    t.get_active_targets()
                        .filter(|t| t.update_count >= MIN_BROADCAST_UPDATE_COUNT)
                        .map(|t| (t.id, active_target_to_api(t, radar_position)))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.per_radar_trackers
                .get(radar_key)
                .map(|t| {
                    t.get_active_targets()
                        .filter(|t| t.update_count >= MIN_BROADCAST_UPDATE_COUNT)
                        .map(|t| (t.id, active_target_to_api(t, radar_position)))
                        .collect()
                })
                .unwrap_or_default()
        };

        if targets.is_empty() {
            return;
        }

        log::debug!(
            "Broadcasting {} targets for radar {radar_key} to {} receivers",
            targets.len(),
            self.sk_client_tx.receiver_count()
        );

        let mut delta = SignalKDelta::new();
        for (id, api) in targets {
            delta.add_target_update(radar_key, id, Some(api));
        }

        if let Err(e) = self.sk_client_tx.send(delta) {
            log::trace!("Failed to broadcast batched target update: {}", e);
        }
    }

    /// Broadcast a lost status update to SignalK
    fn broadcast_lost_update(&self, target_id: u64, radar_key: &str, target_api: ArpaTargetApi) {
        let mut delta = SignalKDelta::new();
        delta.add_target_update(radar_key, target_id, Some(target_api));

        if let Err(e) = self.sk_client_tx.send(delta) {
            log::trace!("Failed to broadcast lost status update: {}", e);
        }
    }

    /// Broadcast a deletion (null target) to SignalK
    fn broadcast_deletion(&self, target_id: u64, radar_key: &str) {
        let mut delta = SignalKDelta::new();
        delta.add_target_update(radar_key, target_id, None);

        if let Err(e) = self.sk_client_tx.send(delta) {
            log::trace!("Failed to broadcast target deletion: {}", e);
        }

        log::info!("Broadcast deletion for target {}", target_id);
    }
}

/// True bearing (radians, 0 = north) and range (meters) of a blob's centre
/// as seen from the radar. `None` when the spoke carried no heading.
fn blob_to_polar(blob: &CompletedBlob, ctx: &SpokeContext) -> Option<(f64, f64)> {
    // Get true bearing of the current spoke (requires heading info)
    let spoke_true_bearing = ctx.bearing?;

    // blob.center_spoke is head-relative (like ctx.angle)
    // ctx.bearing is true bearing, ctx.angle is head-relative
    // Heading offset = ctx.bearing - ctx.angle (in spokes)
    // True bearing of blob = blob.center_spoke + heading_offset
    let heading_offset = spoke_true_bearing as i32 - ctx.angle as i32;
    let blob_true_bearing = (blob.center_spoke as i32 + heading_offset)
        .rem_euclid(ctx.spokes_per_revolution as i32) as u16;

    // Convert true bearing from spokes to radians
    let bearing_rad = (blob_true_bearing as f64 / ctx.spokes_per_revolution as f64) * TAU;

    // Calculate distance from pixel position
    let distance_m = if ctx.spoke_len > 0 {
        (blob.center_pixel as f64 / ctx.spoke_len as f64) * ctx.range as f64
    } else {
        0.0
    };

    Some((bearing_rad, distance_m))
}

/// Convert blob center to geographic position
fn blob_to_position(blob: &CompletedBlob, ctx: &SpokeContext) -> Option<GeoPosition> {
    let radar_pos = GeoPosition::new(ctx.lat?, ctx.lon?);
    let (bearing_rad, distance_m) = blob_to_polar(blob, ctx)?;

    Some(radar_pos.position_from_bearing(bearing_rad, distance_m))
}

/// How much of a blob's own extent to take as the 1-sigma error of its
/// centre. A beam-smeared point target wanders by a fair fraction of the
/// beam width from sweep to sweep — the reported capture showed 1.5-3
/// degrees against a beam of about 5 — and a physically large target has a
/// correspondingly less certain centre. Half the extent covers both.
const CENTROID_SIGMA_FRACTION: f64 = 0.5;

/// Floor on either axis of the measurement error (meters). Keeps a
/// single-pixel blob close in from claiming an implausibly exact position.
const MIN_MEASUREMENT_SIGMA_M: f64 = 5.0;

/// Floor on the bearing error (radians) regardless of how narrow the blob
/// is. The detector thresholds the echo, so a weak target can be reported
/// over fewer spokes than the beam actually illuminates, and its extent
/// would then understate how well its bearing is really known. One degree
/// is below the horizontal beam width of any marine radar — open arrays
/// are around 1-2 degrees and radomes 4-6 — so it errs towards trusting
/// the measurement rather than away from it.
const MIN_BEARING_SIGMA_RAD: f64 = std::f64::consts::PI / 180.0;

/// Measurement covariance of a blob's centre, in the local north/east frame.
///
/// The radial error comes from the blob's depth in pixels; the cross-range
/// error is its angular width times the range, which is what dominates
/// beyond a few hundred meters.
fn blob_to_covariance(blob: &CompletedBlob, ctx: &SpokeContext) -> PositionCovariance {
    let Some((bearing_rad, distance_m)) = blob_to_polar(blob, ctx) else {
        return PositionCovariance::isotropic(MIN_MEASUREMENT_SIGMA_M.powi(2));
    };

    let meters_per_pixel = if ctx.spoke_len > 0 {
        ctx.range as f64 / ctx.spoke_len as f64
    } else {
        0.0
    };
    let radial_sigma = (blob.pixel_extent as f64 * meters_per_pixel * CENTROID_SIGMA_FRACTION)
        .max(MIN_MEASUREMENT_SIGMA_M);

    let angular_extent = blob.spoke_extent as f64 / ctx.spokes_per_revolution as f64 * TAU;
    let bearing_sigma = (angular_extent * CENTROID_SIGMA_FRACTION).max(MIN_BEARING_SIGMA_RAD);
    let cross_sigma = (distance_m * bearing_sigma).max(MIN_MEASUREMENT_SIGMA_M);

    PositionCovariance::from_polar(bearing_rad, radial_sigma.powi(2), cross_sigma.powi(2))
}

/// Convert active target to API format
fn active_target_to_api(
    target: &super::tracker::ActiveTarget,
    radar_position: Option<&GeoPosition>,
) -> ArpaTargetApi {
    let (bearing, distance) = if let Some(radar_pos) = radar_position {
        let dlat = (target.position.lat() - radar_pos.lat()) * super::METERS_PER_DEGREE_LATITUDE;
        let dlon = (target.position.lon() - radar_pos.lon())
            * super::meters_per_degree_longitude(&radar_pos.lat());

        let dist = (dlat * dlat + dlon * dlon).sqrt();
        let bearing = dlon.atan2(dlat);
        let bearing = if bearing < 0.0 {
            bearing + TAU
        } else {
            bearing
        };

        (bearing, dist as i32)
    } else {
        (0.0, 0)
    };

    // Use the target's actual status
    let status_str = target.status.as_str();

    // Calculate CPA/TCPA if we have own-ship motion data
    let danger = calculate_danger(target, radar_position);

    // Motion is only included when we have computed SOG/COG.
    // This distinguishes "unknown motion" (acquiring) from "stationary" (speed=0).
    let motion = match (target.sog, target.cog) {
        (Some(speed), Some(course)) => Some(TargetMotionApi { course, speed }),
        _ => None,
    };

    ArpaTargetApi {
        id: target.id,
        status: status_str.to_string(),
        position: TargetPositionApi {
            bearing,
            distance,
            latitude: Some(target.position.lat()),
            longitude: Some(target.position.lon()),
        },
        motion,
        danger,
        acquisition: if target.is_manual { "manual" } else { "auto" }.to_string(),
        source_zone: target.source_zone,
        first_seen: millis_to_iso8601(target.first_seen),
        last_seen: millis_to_iso8601(target.last_update),
    }
}

/// Convert milliseconds since epoch to ISO 8601 timestamp string
fn millis_to_iso8601(millis: u64) -> String {
    let datetime: DateTime<Utc> = (UNIX_EPOCH + Duration::from_millis(millis)).into();
    datetime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Calculate CPA/TCPA danger assessment for a target.
/// Reads own-ship SOG/COG from navdata; delegates the math to
/// `compute_danger` so tests can exercise the pure helper.
fn calculate_danger(
    target: &super::tracker::ActiveTarget,
    radar_position: Option<&GeoPosition>,
) -> TargetDangerApi {
    let Some(own_pos) = radar_position else {
        return TargetDangerApi::unknown();
    };

    // Missing own-ship SOG/COG must NOT be coerced to 0.0 — that would
    // compute CPA/TCPA against a fictitious stationary own-ship and
    // could silently flip is_dangerous. When navdata isn't reporting
    // motion (boot, GPS loss), the right answer is "unknown."
    let Some(own_sog) = crate::navdata::get_sog() else {
        return TargetDangerApi::unknown();
    };
    let Some(own_cog) = crate::navdata::get_cog() else {
        return TargetDangerApi::unknown();
    };
    compute_danger(target, own_pos, own_sog, own_cog)
}

/// Pure CPA/TCPA helper. Uses the Kalman-filtered position from the motion
/// model rather than the raw last measurement — that raw value swings tens
/// of metres revolution-to-revolution, and using it produces equally noisy
/// CPA output that's not what consumers expect.
fn compute_danger(
    target: &super::tracker::ActiveTarget,
    own_pos: &GeoPosition,
    own_sog: f64,
    own_cog: f64,
) -> TargetDangerApi {
    use crate::radar::cpa::calculate_cpa_from_motion;

    let Some(target_sog) = target.sog else {
        return TargetDangerApi::unknown();
    };
    let Some(target_cog) = target.cog else {
        return TargetDangerApi::unknown();
    };

    let target_pos = target.predict_position(target.last_update);

    match calculate_cpa_from_motion(
        *own_pos, own_sog, own_cog, target_pos, target_sog, target_cog,
    ) {
        Some(result) => TargetDangerApi::new(result.cpa, result.tcpa),
        None => TargetDangerApi::unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isotropic 5 m measurement noise, so these tests exercise tracking
    /// logic rather than the range-dependent noise model.
    const TEST_COV: PositionCovariance = PositionCovariance::isotropic(25.0);

    fn make_test_manager(merge_mode: bool) -> TrackerManager {
        let (sk_tx, _rx) = broadcast::channel(16);
        let (manager, _command_tx) = TrackerManager::new(merge_mode, sk_tx);
        manager
    }

    fn make_blob(center_spoke: u16, center_pixel: usize, size_meters: f64) -> CompletedBlob {
        CompletedBlob {
            contour: vec![(center_spoke, center_pixel)],
            all_pixels: vec![(center_spoke, center_pixel)],
            center_spoke,
            center_pixel,
            spoke_extent: 1,
            pixel_extent: 1,
            size_meters,
            in_guard_zones: vec![1], // Default to guard zone 1 for tests
            has_doppler_approaching: false,
        }
    }

    fn make_context(time: u64, bearing: u16) -> SpokeContext {
        SpokeContext {
            time,
            range: 1000,
            bearing: Some(bearing),
            lat: Some(52.0),
            lon: Some(4.0),
            spokes_per_revolution: 2048,
            spoke_len: 512,
            angle: bearing,
            max_target_speed_ms: SpokeContext::max_speed_from_mode(0), // Normal mode
            doppler_auto_track: false,
        }
    }

    #[test]
    fn test_manager_per_radar_mode() {
        let manager = make_test_manager(false);
        assert!(!manager.merge_mode);
        assert!(manager.shared_tracker.is_none());
    }

    #[test]
    fn test_manager_merged_mode() {
        let manager = make_test_manager(true);
        assert!(manager.merge_mode);
        assert!(manager.shared_tracker.is_some());
    }

    #[test]
    fn test_radar_index_assignment() {
        let mut manager = make_test_manager(false);

        let idx1 = manager.get_radar_index("radar1");
        let idx2 = manager.get_radar_index("radar2");
        let idx1_again = manager.get_radar_index("radar1");

        assert_eq!(idx1, 1);
        assert_eq!(idx2, 2);
        assert_eq!(idx1_again, 1);
    }

    #[test]
    fn test_blob_to_position_north() {
        // Blob at center_spoke=0 (North), center_pixel=256
        let blob = make_blob(0, 256, 30.0);
        let ctx = make_context(1000, 0); // bearing must be Some for position calc

        let pos = blob_to_position(&blob, &ctx).unwrap();

        // Distance: 256/512 * 1000 = 500m
        // Bearing: 0 = North (from blob.center_spoke)
        // Should be ~500m north of 52.0, 4.0
        assert!(pos.lat() > 52.0, "Position should be north: {}", pos.lat());
        assert!(
            (pos.lon() - 4.0).abs() < 0.0001,
            "Longitude should be unchanged"
        );
    }

    #[test]
    fn test_blob_to_position_east() {
        // Blob at center_spoke=512 (East = 512/2048 = 0.25 revolution = 90 degrees)
        let blob = make_blob(512, 256, 30.0);
        let ctx = make_context(1000, 512); // bearing must be Some for position calc

        let pos = blob_to_position(&blob, &ctx).unwrap();

        // Should be ~500m east
        assert!(pos.lon() > 4.0, "Position should be east: {}", pos.lon());
        assert!(
            (pos.lat() - 52.0).abs() < 0.001,
            "Latitude should be nearly unchanged"
        );
    }

    #[test]
    fn test_blob_to_position_with_heading_offset() {
        // Test that head-relative blob angle is converted to true bearing correctly
        // Scenario: boat heading is 90 degrees (East), blob is dead ahead (head-relative 0)
        // True bearing should be 90 degrees (East)
        let blob = make_blob(0, 256, 30.0); // head-relative spoke 0 = dead ahead
        let mut ctx = make_context(1000, 0);
        ctx.angle = 0; // head-relative angle of spoke
        ctx.bearing = Some(512); // true bearing = 512/2048 = 90 degrees (East)

        let pos = blob_to_position(&blob, &ctx).unwrap();

        // Should be east of radar (true bearing 90 degrees)
        assert!(
            pos.lon() > 4.0,
            "Position should be east: lon={}",
            pos.lon()
        );
        assert!(
            (pos.lat() - 52.0).abs() < 0.001,
            "Latitude should be nearly unchanged"
        );
    }

    #[test]
    fn blob_covariance_is_elongated_across_the_beam_at_range() {
        // One spoke wide, one pixel deep, at 4 km over 512 pixels.
        let blob = make_blob(0, 500, 30.0);
        let ctx = SpokeContext {
            range: 4000,
            ..make_context(1000, 0)
        };

        let cov = blob_to_covariance(&blob, &ctx);

        // Dead ahead (north), so the cross-range error lands on the east
        // axis and the radial error on the north axis.
        assert!(
            cov.ee > cov.nn * 10.0,
            "cross-range error must dominate at range: nn={:.0} ee={:.0}",
            cov.nn,
            cov.ee
        );

        // A blob as wide as a real beam is less certain still.
        let mut wide = make_blob(0, 500, 30.0);
        wide.spoke_extent = 28; // ~4.9 degrees at 2048 spokes
        assert!(blob_to_covariance(&wide, &ctx).ee > cov.ee);
    }

    #[test]
    fn blob_covariance_is_near_circular_close_in() {
        // The same blob at 100 m: one spoke subtends well under a metre, so
        // the floor applies on both axes and the ellipse collapses to a circle.
        let blob = make_blob(0, 13, 30.0);
        let ctx = SpokeContext {
            range: 100,
            ..make_context(1000, 0)
        };

        let cov = blob_to_covariance(&blob, &ctx);

        assert!((cov.nn - cov.ee).abs() < 1.0, "nn={} ee={}", cov.nn, cov.ee);
    }

    #[test]
    fn test_blob_to_position_missing_bearing() {
        let blob = make_blob(1024, 256, 30.0);
        let mut ctx = make_context(1000, 0);
        ctx.bearing = None;

        let pos = blob_to_position(&blob, &ctx);
        assert!(pos.is_none());
    }

    #[test]
    fn test_blob_to_position_missing_lat() {
        let blob = make_blob(1024, 256, 30.0);
        let mut ctx = make_context(1000, 0);
        ctx.lat = None;

        let pos = blob_to_position(&blob, &ctx);
        assert!(pos.is_none());
    }

    #[test]
    fn test_process_blob_creates_tracker() {
        let mut manager = make_test_manager(false);

        let blob = make_blob(1024, 256, 30.0);
        let ctx = make_context(1000, 512);
        let msg = BlobMessage {
            radar_key: "test_radar".to_string(),
            blob,
            context: ctx,
        };

        manager.process_blob(msg);

        // Should have created a tracker for this radar
        assert!(manager.per_radar_trackers.contains_key("test_radar"));
    }

    #[test]
    fn test_process_blob_merged_mode() {
        let mut manager = make_test_manager(true);

        let blob = make_blob(1024, 256, 30.0);
        let ctx = make_context(1000, 512);
        let msg = BlobMessage {
            radar_key: "test_radar".to_string(),
            blob,
            context: ctx,
        };

        manager.process_blob(msg);

        // In merged mode, no per-radar trackers
        assert!(manager.per_radar_trackers.is_empty());
        assert!(manager.shared_tracker.is_some());
    }

    #[test]
    fn test_active_target_to_api() {
        use super::super::tracker::TargetCandidate;

        let max_speed = SpokeContext::max_speed_from_mode(0);

        // Create an active target directly
        let radar_pos = GeoPosition::new(52.0, 4.0);
        let candidate = TargetCandidate {
            time: 1000,
            position: GeoPosition::new(52.001, 4.001),
            size_meters: 30.0,
            radar_key: "test".to_string(),
            radar_position: Some(radar_pos),
            max_target_speed_ms: max_speed,
            position_covariance: TEST_COV,
            source: CandidateSource::GuardZone(1),
        };

        let mut tracker = super::super::tracker::TargetTracker::new_merged();

        // Process 4 times to promote (requires 4 updates)
        tracker.process_candidate(candidate);
        for i in 1..4u64 {
            let c = TargetCandidate {
                time: 1000 + i * 3000,
                position: GeoPosition::new(52.001 + i as f64 * 0.0001, 4.001),
                size_meters: 30.0,
                radar_key: "test".to_string(),
                radar_position: Some(radar_pos),
                max_target_speed_ms: max_speed,
                position_covariance: TEST_COV,
                source: CandidateSource::GuardZone(1),
            };
            tracker.process_candidate(c);
        }

        // Get the target
        let target = tracker.get_active_targets().next().unwrap();

        let api = active_target_to_api(target, Some(&radar_pos));

        assert_eq!(api.status, "tracking");
        assert!(api.position.distance > 0);
        assert!(api.position.latitude.is_some());
        assert!(api.position.longitude.is_some());
        assert_eq!(api.acquisition, "auto");
    }

    #[test]
    fn test_get_targets_api_empty() {
        let manager = make_test_manager(false);
        let targets = manager.get_targets_api(None);
        assert!(targets.is_empty());
    }

    #[test]
    fn test_get_targets_api_merged() {
        let manager = make_test_manager(true);
        let radar_pos = GeoPosition::new(52.0, 4.0);
        let targets = manager.get_targets_api(Some(radar_pos));
        assert!(targets.is_empty()); // No blobs processed yet
    }

    /// Sink that drains the broadcast channel into a Vec so tests can
    /// assert on the deltas the manager emits.
    fn drain_sk_deltas(
        mut rx: tokio::sync::broadcast::Receiver<SignalKDelta>,
    ) -> Vec<SignalKDelta> {
        let mut out = Vec::new();
        while let Ok(delta) = rx.try_recv() {
            out.push(delta);
        }
        out
    }

    fn make_test_manager_with_rx() -> (
        TrackerManager,
        tokio::sync::broadcast::Receiver<SignalKDelta>,
    ) {
        make_test_manager_with_rx_mode(false)
    }

    fn make_test_manager_with_rx_mode(
        merge_mode: bool,
    ) -> (
        TrackerManager,
        tokio::sync::broadcast::Receiver<SignalKDelta>,
    ) {
        let (sk_tx, sk_rx) = broadcast::channel(64);
        let (manager, _command_tx) = TrackerManager::new(merge_mode, sk_tx);
        (manager, sk_rx)
    }

    /// The id of the first target the tracker holds for `radar_key`,
    /// whichever tracker mode the manager is in.
    fn first_target_id(manager: &TrackerManager, radar_key: &str) -> u64 {
        let tracker = if manager.merge_mode {
            manager.shared_tracker.as_ref().unwrap()
        } else {
            &manager.per_radar_trackers[radar_key]
        };
        tracker.get_active_targets().next().unwrap().id
    }

    /// The id of the most recently created target for `radar_key`.
    fn last_target_id(manager: &TrackerManager, radar_key: &str) -> u64 {
        let tracker = if manager.merge_mode {
            manager.shared_tracker.as_ref().unwrap()
        } else {
            &manager.per_radar_trackers[radar_key]
        };
        tracker.get_active_targets().map(|t| t.id).max().unwrap()
    }

    /// Reduce the emitted deltas to the `notifications.*` (path, state)
    /// pairs a Signal K client would see.
    fn notifications(deltas: &[SignalKDelta]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for delta in deltas {
            let json = serde_json::to_value(delta).unwrap();
            for update in json["updates"].as_array().into_iter().flatten() {
                for value in update["values"].as_array().into_iter().flatten() {
                    let path = value["path"].as_str().unwrap_or_default();
                    if path.starts_with("notifications.")
                        && let Some(state) = value["value"]["state"].as_str()
                    {
                        out.push((path.to_string(), state.to_string()));
                    }
                }
            }
        }
        out
    }

    /// Feed `count` revolutions' worth of blobs for one slowly closing
    /// target inside guard zone `zone`, at a bearing of `spoke`.
    fn feed_guard_zone_target(
        manager: &mut TrackerManager,
        radar_key: &str,
        spoke: u16,
        zone: u8,
        count: u64,
    ) {
        for i in 0..count {
            let mut blob = make_blob(spoke, 256 - 2 * i as usize, 30.0);
            blob.in_guard_zones = vec![zone];
            manager.process_blob(BlobMessage {
                radar_key: radar_key.to_string(),
                blob,
                context: make_context(1000 + i * 3000, spoke),
            });
        }
    }

    #[test]
    fn guard_zone_alert_fires_when_target_is_promoted() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        assert_eq!(
            notifications(&drain_sk_deltas(rx)),
            vec![(
                "notifications.radar.nav1.guardZone.1".to_string(),
                "alert".to_string()
            )]
        );
    }

    #[test]
    fn guard_zone_stays_silent_for_unconfirmed_blobs() {
        let (mut manager, rx) = make_test_manager_with_rx();
        // Three hits is one short of promotion, so this is still clutter
        // as far as clients are concerned.
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 3);
        assert!(notifications(&drain_sk_deltas(rx)).is_empty());
    }

    #[test]
    fn guard_zone_alerts_for_every_new_target() {
        let (mut manager, rx) = make_test_manager_with_rx();
        // Two targets on opposite bearings, both in zone 1: the second one
        // must be announced even though the zone is already alarming.
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        feed_guard_zone_target(&mut manager, "nav1", 1024, 1, 4);
        let alerts = notifications(&drain_sk_deltas(rx));
        assert_eq!(alerts.len(), 2);
        assert!(alerts.iter().all(|(_, state)| state == "alert"));
    }

    #[test]
    fn guard_zone_alerts_are_per_radar_and_per_zone() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        feed_guard_zone_target(&mut manager, "nav1", 1024, 2, 4);
        feed_guard_zone_target(&mut manager, "nav2", 0, 1, 4);
        let paths: Vec<String> = notifications(&drain_sk_deltas(rx))
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            paths,
            vec![
                "notifications.radar.nav1.guardZone.1",
                "notifications.radar.nav1.guardZone.2",
                "notifications.radar.nav2.guardZone.1",
            ]
        );
    }

    /// Feed one blob that matches the track `feed_guard_zone_target` built
    /// but lies outside every guard zone, taking it to its 4th update and
    /// so to promotion.
    fn promote_outside_guard_zone(manager: &mut TrackerManager, radar_key: &str, spoke: u16) {
        let mut blob = make_blob(spoke, 250, 30.0);
        blob.in_guard_zones = Vec::new();
        manager.process_blob(BlobMessage {
            radar_key: radar_key.to_string(),
            blob,
            context: make_context(10_000, spoke),
        });
    }

    #[test]
    fn guard_zone_stays_silent_when_promotion_happens_outside_the_zone() {
        let (mut manager, rx) = make_test_manager_with_rx();
        // Three hits inside zone 1, then the confirming hit outside it: the
        // track was never established as an intrusion.
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 3);
        promote_outside_guard_zone(&mut manager, "nav1", 0);

        // The track really was promoted — it is the alarm that is withheld,
        // not the confirmation.
        let target_id = first_target_id(&manager, "nav1");
        assert_eq!(
            manager.per_radar_trackers["nav1"]
                .get_target(target_id)
                .unwrap()
                .status,
            TargetStatus::Tracking
        );
        assert!(notifications(&drain_sk_deltas(rx)).is_empty());
    }

    #[test]
    fn guard_zone_promoted_outside_does_not_hold_the_alarm_up() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 3);
        promote_outside_guard_zone(&mut manager, "nav1", 0);

        // A real intrusion on another bearing raises zone 1; deleting it has
        // to clear the zone again. It cannot while the track promoted
        // outside still counts as occupying it.
        feed_guard_zone_target(&mut manager, "nav1", 1024, 1, 4);
        let intruder = last_target_id(&manager, "nav1");
        assert!(manager.delete_target("nav1", intruder));
        manager.check_all_timeouts();

        assert_eq!(
            notifications(&drain_sk_deltas(rx)),
            vec![
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "alert".to_string()
                ),
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "normal".to_string()
                ),
            ]
        );
    }

    /// Reproduces the stalled clock of #723. Every echo in that capture sat
    /// on one bearing, so the blob-driven wrap detector never fired: one
    /// tracker counted a single revolution across 470 s, and a target was
    /// still being reported lost "after 3 revolutions" 13 minutes on.
    /// `feed_guard_zone_target` feeds one bearing, which is that case.
    #[test]
    fn revolution_clock_ages_targets_when_no_echo_ever_wraps() {
        let mut manager = make_test_manager(false);
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        let target_id = first_target_id(&manager, "nav1");

        // Time alone must not age the target: the timeouts are counted in
        // antenna revolutions, and no revolution has been reported yet.
        manager.check_all_timeouts();
        assert_eq!(
            manager.per_radar_trackers["nav1"]
                .get_target(target_id)
                .unwrap()
                .status,
            TargetStatus::Tracking
        );

        // Revolutions that produced no echo at all still have to be counted,
        // or a lost target coasts for minutes (LOST_REVOLUTION_COUNT = 3).
        for _ in 0..3 {
            manager.process_revolution("nav1", None);
        }
        manager.check_all_timeouts();

        assert_eq!(
            manager.per_radar_trackers["nav1"]
                .get_target(target_id)
                .unwrap()
                .status,
            TargetStatus::Lost
        );
    }

    #[test]
    fn merged_clock_is_driven_by_one_radar_only() {
        let mut manager = make_test_manager(true);
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        let target_id = first_target_id(&manager, "nav1");
        let status = |m: &TrackerManager| {
            m.shared_tracker
                .as_ref()
                .unwrap()
                .get_target(target_id)
                .unwrap()
                .status
        };

        // A dual-range antenna reports a revolution per range. Counting both
        // would reach LOST_REVOLUTION_COUNT in half the turns it should.
        for _ in 0..2 {
            manager.process_revolution("nav1", None);
            manager.process_revolution("nav2", None);
        }
        manager.check_all_timeouts();
        assert_eq!(status(&manager), TargetStatus::Tracking);

        manager.process_revolution("nav1", None);
        manager.check_all_timeouts();
        assert_eq!(status(&manager), TargetStatus::Lost);
    }

    /// Reproduces ghost target 100000041 of #723: acquired at 1,933 m on
    /// relative bearing 196 degrees, inside a 1,500-2,000 m guard zone, then
    /// "promoted" five minutes later at 75 m on bearing 20 degrees — own-ship
    /// clutter, 1.9 km from where the track was acquired — and alarming the
    /// zone it had long left. Capping the coasting gate keeps the two apart,
    /// so the clutter echo never confirms the track at all.
    #[test]
    fn a_clutter_echo_kilometres_away_cannot_confirm_a_guard_zone_track() {
        let (mut manager, rx) = make_test_manager_with_rx();

        // Range 4000 m over 512 pixels: 1 pixel is 7.8 m.
        let ctx_at = |time: u64, spoke: u16| SpokeContext {
            range: 4000,
            ..make_context(time, spoke)
        };
        for i in 0..3 {
            let mut blob = make_blob(1115, 247, 71.3); // 1,933 m at 196 degrees
            blob.in_guard_zones = vec![1];
            manager.process_blob(BlobMessage {
                radar_key: "nav1".to_string(),
                blob,
                context: ctx_at(1_000 + i * 3_000, 1115),
            });
        }

        let mut blob = make_blob(114, 10, 30.0); // 75 m at 20 degrees
        blob.in_guard_zones = Vec::new();
        manager.process_blob(BlobMessage {
            radar_key: "nav1".to_string(),
            blob,
            context: ctx_at(301_000, 114),
        });

        let target_id = first_target_id(&manager, "nav1");
        assert_eq!(
            manager.per_radar_trackers["nav1"]
                .get_target(target_id)
                .unwrap()
                .status,
            TargetStatus::Acquiring,
            "clutter 1.9 km away must not confirm the track"
        );
        assert!(notifications(&drain_sk_deltas(rx)).is_empty());
    }

    #[test]
    fn guard_zone_clears_once_the_zone_is_empty() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);

        let target_id = first_target_id(&manager, "nav1");
        assert!(manager.delete_target("nav1", target_id));
        manager.check_all_timeouts();

        assert_eq!(
            notifications(&drain_sk_deltas(rx)),
            vec![
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "alert".to_string()
                ),
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "normal".to_string()
                ),
            ]
        );
    }

    #[test]
    fn guard_zone_stays_raised_while_a_target_remains() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);
        feed_guard_zone_target(&mut manager, "nav1", 1024, 1, 4);

        let target_id = first_target_id(&manager, "nav1");
        manager.delete_target("nav1", target_id);
        manager.check_all_timeouts();

        // One of two targets gone: both alerts stand and no clear follows.
        let states = notifications(&drain_sk_deltas(rx));
        assert_eq!(
            states.len(),
            2,
            "expected one alert per target: {:?}",
            states
        );
        assert!(
            states.iter().all(|(_, state)| state == "alert"),
            "unexpected clear: {:?}",
            states
        );
    }

    #[test]
    fn guard_zone_clears_after_clear_all_targets() {
        let (mut manager, rx) = make_test_manager_with_rx();
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);

        // Clearing a radar's targets removes them without any Lost
        // transition; the next sweep must still release the alarm.
        manager.clear_all_targets("nav1");
        manager.check_all_timeouts();

        let states = notifications(&drain_sk_deltas(rx));
        assert_eq!(
            states.last(),
            Some(&(
                "notifications.radar.nav1.guardZone.1".to_string(),
                "normal".to_string()
            )),
            "clear_all_targets should end with a clear: {:?}",
            states
        );
    }

    #[test]
    fn guard_zone_alarms_work_in_merged_mode() {
        // Merged mode routes every radar through one shared tracker, so
        // occupancy is keyed by each target's last_radar_key rather than
        // by which tracker it came from.
        let (mut manager, rx) = make_test_manager_with_rx_mode(true);
        feed_guard_zone_target(&mut manager, "nav1", 0, 1, 4);

        let target_id = first_target_id(&manager, "nav1");
        assert!(manager.delete_target("nav1", target_id));
        manager.check_all_timeouts();

        assert_eq!(
            notifications(&drain_sk_deltas(rx)),
            vec![
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "alert".to_string()
                ),
                (
                    "notifications.radar.nav1.guardZone.1".to_string(),
                    "normal".to_string()
                ),
            ]
        );
    }

    #[test]
    fn guard_zone_ignores_manual_marpa_targets() {
        let (mut manager, rx) = make_test_manager_with_rx();
        manager.process_marpa(MarpaRequest {
            radar_key: "nav1".to_string(),
            position: GeoPosition::new(52.001, 4.001),
            radar_position: Some(GeoPosition::new(52.0, 4.0)),
            time: 1000,
            size_meters: 30.0,
        });
        manager.check_all_timeouts();
        // MARPA acquisitions carry no source zone and must not alarm.
        assert!(notifications(&drain_sk_deltas(rx)).is_empty());
    }

    /// After a noisy measurement the Kalman filter's smoothed position
    /// is significantly closer to the true track than the raw last
    /// measurement. `compute_danger` uses the smoothed position; this
    /// test confirms that feeding the raw `target.position` into the
    /// same CPA math produces a materially different result, so the
    /// noise suppression at the API boundary is real.
    #[test]
    fn compute_danger_uses_filtered_position_not_raw_measurement() {
        use super::super::tracker::{CandidateSource, TargetCandidate, TargetTracker};
        use crate::radar::cpa::calculate_cpa_from_motion;

        let radar_pos = GeoPosition::new(52.0, 4.0);
        let max_speed = SpokeContext::max_speed_from_mode(2);

        // Target starts ~400m north and closes due south at ~10 m/s for
        // 6 clean revolutions (the Kalman filter has fully converged).
        let mut tracker = TargetTracker::new_merged();
        let lat_step = 30.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        let start_lat = 52.0 + 400.0 / super::super::METERS_PER_DEGREE_LATITUDE;
        for i in 0..6u64 {
            tracker.process_candidate(TargetCandidate {
                time: i * 3000,
                position: GeoPosition::new(start_lat - lat_step * i as f64, 4.0),
                size_meters: 30.0,
                radar_key: "test".to_string(),
                radar_position: Some(radar_pos),
                max_target_speed_ms: max_speed,
                position_covariance: TEST_COV,
                source: CandidateSource::GuardZone(1),
            });
        }

        // One badly noisy measurement: ~80m east of the true track.
        let lon_noise = 80.0 / super::super::meters_per_degree_longitude(&52.0);
        let noisy_true_lat = start_lat - lat_step * 6.0;
        let noisy_true_lon = 4.0 + lon_noise;
        tracker.process_candidate(TargetCandidate {
            time: 6 * 3000,
            position: GeoPosition::new(noisy_true_lat, noisy_true_lon),
            size_meters: 30.0,
            radar_key: "test".to_string(),
            radar_position: Some(radar_pos),
            max_target_speed_ms: max_speed,
            position_covariance: TEST_COV,
            source: CandidateSource::GuardZone(1),
        });

        let target = tracker.get_active_targets().next().unwrap();

        // Smoothed CPA: compute_danger uses target.predict_position
        // (the Kalman state).
        let smoothed = compute_danger(target, &radar_pos, 0.0, 0.0);
        assert!(smoothed.tcpa > 0.0, "expected closing geometry");

        // Raw CPA: same math but fed with target.position (the raw
        // 80m-east last measurement) instead of the filtered estimate.
        let raw = calculate_cpa_from_motion(
            radar_pos,
            0.0,
            0.0,
            target.position,
            target.sog.unwrap(),
            target.cog.unwrap(),
        )
        .expect("raw CPA should also be a closing situation");

        // The smoothed Kalman position barely moved (it weights the noisy
        // measurement against many clean priors), while the raw position is
        // exactly the 80m-east noise. So the raw CPA should be materially
        // larger than the smoothed CPA.
        assert!(
            raw.cpa > smoothed.cpa + 30.0,
            "raw CPA ({:.1}m) should be ≥ 30m larger than smoothed CPA ({:.1}m); \
             the Kalman state must filter the noise spike",
            raw.cpa,
            smoothed.cpa
        );
    }
}
