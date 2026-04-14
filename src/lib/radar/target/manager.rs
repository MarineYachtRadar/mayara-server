//! Target tracker manager.
//!
//! Manages target trackers based on merge mode (single shared tracker vs per-radar trackers).

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, mpsc};

use super::blob::CompletedBlob;
use super::tracker::{CandidateSource, ProcessResult, TargetCandidate, TargetTracker};
use super::{ArpaTargetApi, TargetDangerApi, TargetMotionApi, TargetPositionApi};
use crate::radar::GeoPosition;
use crate::stream::SignalKDelta;

/// Knots to m/s conversion
const KN_TO_MS: f64 = 1852.0 / 3600.0;

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
                Some(TargetTracker::new_merged(2048))
            } else {
                None
            },
            merge_mode,
            radar_indices: HashMap::new(),
            next_radar_index: 1,
            sk_client_tx,
            command_rx,
        };

        (manager, command_tx)
    }

    /// Get or create tracker for a radar
    fn get_or_create_tracker(
        &mut self,
        radar_key: &str,
        spokes_per_revolution: u16,
    ) -> &mut TargetTracker {
        if self.merge_mode {
            // Update spokes if needed
            if let Some(ref mut tracker) = self.shared_tracker {
                return tracker;
            }
            // Should not happen, but create if missing
            self.shared_tracker = Some(TargetTracker::new_merged(spokes_per_revolution));
            self.shared_tracker.as_mut().unwrap()
        } else {
            // Per-radar mode
            if !self.per_radar_trackers.contains_key(radar_key) {
                let index = self.get_radar_index(radar_key);
                let tracker = TargetTracker::new_per_radar(index, spokes_per_revolution);
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
            source,
        };

        // Get tracker and process
        let tracker = self.get_or_create_tracker(&msg.radar_key, ctx.spokes_per_revolution);

        // Check for revolution boundary — batch-broadcast all targets once per revolution
        let revolution_completed = tracker.check_revolution(ctx.angle, ctx.time);

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
        if let ProcessResult::Promoted(target_id) = result {
            if let Some(target) = tracker.get_target(target_id) {
                let target_api = active_target_to_api(target, radar_position.as_ref());
                let mut delta = SignalKDelta::new();
                delta.add_target_update(&msg.radar_key, target_id, Some(target_api));
                if let Err(e) = self.sk_client_tx.send(delta) {
                    log::trace!("Failed to broadcast promoted target: {}", e);
                }
            }
        }

        // On revolution boundary, send a single batched delta with all tracking targets
        if revolution_completed {
            self.broadcast_all_targets(&msg.radar_key, radar_position.as_ref());
        }
    }

    /// Get all active targets as API objects
    pub fn get_targets_api(&self, radar_position: Option<GeoPosition>) -> Vec<ArpaTargetApi> {
        let mut targets = Vec::new();

        if self.merge_mode {
            if let Some(ref tracker) = self.shared_tracker {
                for target in tracker.get_active_targets() {
                    if target.update_count >= 4 {
                        targets.push(active_target_to_api(target, radar_position.as_ref()));
                    }
                }
            }
        } else {
            for tracker in self.per_radar_trackers.values() {
                for target in tracker.get_active_targets() {
                    if target.update_count >= 4 {
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
            source: CandidateSource::GuardZone(0),                     // 0 = manual/MARPA
        };

        // Get tracker (use default 2048 spokes if not yet created)
        let tracker = self.get_or_create_tracker(&request.radar_key, 2048);

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

    /// Run the tracker manager, receiving blobs and MARPA requests
    pub async fn run(mut self, mut blob_rx: mpsc::Receiver<BlobMessage>) {
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
                Some(msg) = blob_rx.recv() => {
                    self.process_blob(msg);
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
    }

    /// Broadcast all tracking targets in a single batched delta (once per revolution)
    fn broadcast_all_targets(&self, radar_key: &str, radar_position: Option<&GeoPosition>) {
        let targets: Vec<(u64, ArpaTargetApi)> = if self.merge_mode {
            self.shared_tracker
                .as_ref()
                .map(|t| {
                    t.get_active_targets()
                        .filter(|t| t.update_count >= 4)
                        .map(|t| (t.id, active_target_to_api(t, radar_position)))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.per_radar_trackers
                .get(radar_key)
                .map(|t| {
                    t.get_active_targets()
                        .filter(|t| t.update_count >= 4)
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

/// Convert blob center to geographic position
fn blob_to_position(blob: &CompletedBlob, ctx: &SpokeContext) -> Option<GeoPosition> {
    let radar_lat = ctx.lat?;
    let radar_lon = ctx.lon?;
    // Get true bearing of the current spoke (requires heading info)
    let spoke_true_bearing = ctx.bearing?;

    let radar_pos = GeoPosition::new(radar_lat, radar_lon);

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

    Some(radar_pos.position_from_bearing(bearing_rad, distance_m))
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

/// Calculate CPA/TCPA danger assessment for a target
fn calculate_danger(
    target: &super::tracker::ActiveTarget,
    radar_position: Option<&GeoPosition>,
) -> TargetDangerApi {
    use crate::radar::cpa::calculate_cpa_from_motion;

    // Need own-ship position and motion
    let Some(own_pos) = radar_position else {
        return TargetDangerApi {
            cpa: 0.0,
            tcpa: 0.0,
        };
    };

    // Get own-ship SOG/COG from navdata
    let own_sog = crate::navdata::get_sog().unwrap_or(0.0);
    let own_cog = crate::navdata::get_cog().unwrap_or(0.0);

    // Need target motion data
    let Some(target_sog) = target.sog else {
        return TargetDangerApi {
            cpa: 0.0,
            tcpa: 0.0,
        };
    };
    let Some(target_cog) = target.cog else {
        return TargetDangerApi {
            cpa: 0.0,
            tcpa: 0.0,
        };
    };

    // Calculate CPA/TCPA
    match calculate_cpa_from_motion(
        *own_pos,
        own_sog,
        own_cog,
        target.position,
        target_sog,
        target_cog,
    ) {
        Some(result) => TargetDangerApi {
            cpa: result.cpa,
            tcpa: result.tcpa,
        },
        None => TargetDangerApi {
            cpa: 0.0,
            tcpa: 0.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            source: CandidateSource::GuardZone(1),
        };

        let mut tracker = super::super::tracker::TargetTracker::new_merged(2048);

        // Process 4 times to promote (requires 4 updates)
        tracker.process_candidate(candidate.clone());
        for i in 1..4u64 {
            let c = TargetCandidate {
                time: 1000 + i * 3000,
                position: GeoPosition::new(52.001 + i as f64 * 0.0001, 4.001),
                size_meters: 30.0,
                radar_key: "test".to_string(),
                radar_position: Some(radar_pos),
                max_target_speed_ms: max_speed,
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
}
