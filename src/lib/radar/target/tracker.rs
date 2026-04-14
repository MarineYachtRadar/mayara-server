//! Target tracking for radar blob detection.
//!
//! This module tracks detected blobs across radar sweeps, maintaining
//! active (confirmed) and acquiring (potential) target lists.

use std::collections::HashMap;
use std::f64::consts::TAU;

use super::motion::{ImmMotionModel, MotionModel};
use super::{METERS_PER_DEGREE_LATITUDE, meters_per_degree_longitude};
use crate::radar::GeoPosition;

/// Number of revolutions without update before a target is marked as lost
const LOST_REVOLUTION_COUNT: u64 = 3;

/// Number of revolutions without update before a stationary target is marked as lost
/// Stationary targets (buoys, anchored vessels) get extended timeout because
/// they may temporarily merge with passing targets and need more time to reappear
const STATIONARY_LOST_REVOLUTION_COUNT: u64 = 10;

/// Number of revolutions after being marked lost before a target is deleted
const DELETE_REVOLUTION_COUNT: u64 = 4;

/// Number of revolutions after being marked lost before a stationary target is deleted.
/// Extended to handle buoys/anchored vessels that may temporarily disappear behind a passing ship.
const STATIONARY_DELETE_REVOLUTION_COUNT: u64 = 10;

/// Speed threshold (m/s) below which a target is considered stationary
/// 0.5 m/s = ~1 knot - accounts for GPS drift and minor movement
const STATIONARY_SPEED_THRESHOLD: f64 = 0.5;

/// Maximum separation (meters) between two targets that are considered duplicates.
/// A large vessel can produce multiple blobs per rotation from different hull sections,
/// each within this distance of the others but all representing the same physical target.
const DUPLICATE_MERGE_DISTANCE_M: f64 = 100.0;

/// Minimum number of updates before a target can be considered stationary
/// Prevents false positives from slow-starting tracks
const MIN_UPDATES_FOR_STATIONARY: u32 = 5;

/// Minimum match distance (meters) for matching a blob to an active target
/// Even slow targets need some search radius for position uncertainty
const MIN_MATCH_DISTANCE_M: f64 = 50.0;

/// Multiplier for max speed to calculate match distance
/// If a target can move at max_speed for delta_time, it could be anywhere
/// within max_speed * delta_time. We use 1.5x to account for prediction error.
const MATCH_DISTANCE_SPEED_MULTIPLIER: f64 = 1.5;

/// Maximum allowed turn angle (degrees) for high-speed targets
/// Targets appearing to turn more than this at speed are rejected as false matches
const MAX_TURN_ANGLE_DEG: f64 = 130.0;

/// Speed threshold (m/s) above which turn rejection is applied
const TURN_REJECTION_SPEED_MS: f64 = 5.0;

/// Multiplier for per-radar target IDs.
/// In per-radar mode, radar N gets IDs in range [N * RADAR_ID_MULTIPLIER, (N+1) * RADAR_ID_MULTIPLIER - 1].
/// In merged mode, IDs range from 1 to RADAR_ID_MULTIPLIER - 1.
const RADAR_ID_MULTIPLIER: u64 = 100_000_000;

/// Status of a tracked target
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TargetStatus {
    /// Target is being acquired (first sighting, no confirmed motion yet)
    Acquiring,
    /// Target is actively being tracked (confirmed motion)
    Tracking,
    /// Target has not been seen for timeout period
    Lost,
}

impl TargetStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetStatus::Acquiring => "acquiring",
            TargetStatus::Tracking => "tracking",
            TargetStatus::Lost => "lost",
        }
    }
}

/// How a target candidate was detected
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CandidateSource {
    /// Candidate detected in a guard zone (automatic acquisition)
    GuardZone(u8),
    /// Candidate is a Doppler-colored target
    Doppler,
    /// Candidate detected anywhere (only matches existing targets)
    Anywhere,
}

/// A target candidate from blob detection
#[derive(Clone, Debug)]
pub struct TargetCandidate {
    /// Timestamp when blob was detected (millis since epoch)
    pub time: u64,
    /// Geographic position (center of blob)
    pub position: GeoPosition,
    /// Size of the target in meters
    pub size_meters: f64,
    /// Source radar key
    pub radar_key: String,
    /// Radar position (for bearing/distance calculation)
    pub radar_position: Option<GeoPosition>,
    /// Maximum target speed in m/s (from ArpaDetectMaxSpeed)
    pub max_target_speed_ms: f64,
    /// How this candidate was detected
    pub source: CandidateSource,
}

/// A confirmed target being actively tracked
pub struct ActiveTarget {
    /// Unique target ID
    pub id: u64,
    /// Current position
    pub position: GeoPosition,
    /// Previous position (for initial COG calculation)
    prev_position: Option<GeoPosition>,
    /// Current size estimate
    pub size_meters: f64,
    /// Speed over ground (m/s), None until first update
    pub sog: Option<f64>,
    /// Course over ground (radians, 0 = North), None until first update
    pub cog: Option<f64>,
    /// Motion model for estimation (Kalman or IMM)
    motion_model: Box<dyn MotionModel>,
    /// Timestamp when target was first seen (millis since epoch)
    pub first_seen: u64,
    /// Last update timestamp (millis since epoch)
    pub last_update: u64,
    /// Revolution count when target was last updated
    last_update_revolution: u64,
    /// Revolution count when target was marked as lost (u64::MAX if not lost)
    lost_revolution: u64,
    /// Number of updates received
    pub update_count: u32,
    /// Current status (Tracking or Lost)
    pub status: TargetStatus,
    /// Whether target was manually or automatically acquired
    pub is_manual: bool,
    /// Which guard zone acquired this target (1 or 2), or None for manual/doppler
    pub source_zone: Option<u8>,
    /// Key of radar that last updated this target (for Signal K broadcast path)
    pub last_radar_key: String,
    /// Position of radar that last updated this target (for bearing/distance calculation)
    pub last_radar_position: Option<GeoPosition>,
}

impl ActiveTarget {
    fn new(id: u64, candidate: &TargetCandidate) -> Self {
        Self::new_with_uncertainty(id, candidate, 20.0)
    }

    /// Create a new target with custom position uncertainty (for MARPA)
    /// MARPA targets need larger uncertainty since user click position is approximate
    fn new_with_uncertainty(id: u64, candidate: &TargetCandidate, position_variance: f64) -> Self {
        let mut motion_model: Box<dyn MotionModel> = Box::new(ImmMotionModel::new());
        motion_model.init_with_uncertainty(candidate.position, candidate.time, position_variance);

        // GuardZone(0) indicates manual/MARPA acquisition
        let is_manual = matches!(candidate.source, CandidateSource::GuardZone(0));

        // Extract source zone from candidate (1 or 2 for guard zones, None for manual/doppler)
        let source_zone = match candidate.source {
            CandidateSource::GuardZone(zone) if zone > 0 => Some(zone),
            _ => None,
        };

        ActiveTarget {
            id,
            position: candidate.position,
            prev_position: Some(candidate.position), // Store for first COG calculation
            size_meters: candidate.size_meters,
            sog: None, // No speed until first update
            cog: None, // No course until first update
            motion_model,
            first_seen: candidate.time,
            last_update: candidate.time,
            last_update_revolution: 0, // Will be set by tracker on first update
            lost_revolution: u64::MAX,
            update_count: 1,
            status: TargetStatus::Acquiring,
            is_manual,
            source_zone,
            last_radar_key: candidate.radar_key.clone(),
            last_radar_position: candidate.radar_position,
        }
    }

    /// Update target with new candidate position.
    /// Returns false if update should be rejected (implausible maneuver).
    fn update(&mut self, candidate: &TargetCandidate) -> bool {
        let delta_time = (candidate.time.saturating_sub(self.last_update)) as f64 / 1000.0;

        // Calculate measured COG for turn rejection check
        let measured_cog = if delta_time > 1.0 {
            Some(calculate_bearing(&self.position, &candidate.position))
        } else {
            None
        };

        // Turn rejection: reject implausible maneuvers for fast targets in early tracking
        // Based on radar_pi: turn > 130° at speed > 5 m/s for status < 5
        // Use Kalman-estimated SOG rather than measured SOG: a stationary target's blob
        // position varies by tens of meters per rotation, producing apparent measured
        // speeds of 8-10 m/s with random directions. The Kalman filter correctly
        // converges toward the true low speed and avoids false rejections.
        if self.update_count >= 2 && self.update_count < 5 {
            if let (Some(current_cog), Some(new_cog), Some(kalman_sog)) =
                (self.cog, measured_cog, self.sog)
            {
                if kalman_sog > TURN_REJECTION_SPEED_MS {
                    let mut turn = (new_cog - current_cog).to_degrees();
                    if turn > 180.0 {
                        turn -= 360.0;
                    }
                    if turn < -180.0 {
                        turn += 360.0;
                    }
                    if turn.abs() > MAX_TURN_ANGLE_DEG {
                        log::debug!(
                            "Target {}: rejecting update - turn {:.1}° at {:.1} m/s (Kalman)",
                            self.id,
                            turn,
                            kalman_sog
                        );
                        return false;
                    }
                }
            }
        }

        // Update motion model and get estimated motion
        let estimate = self.motion_model.update(candidate.position, candidate.time);

        self.sog = Some(estimate.sog);
        self.cog = Some(estimate.cog);

        // On first update, clear previous position
        if self.update_count == 1 {
            self.prev_position = None;
        }

        self.position = candidate.position;
        self.size_meters = candidate.size_meters;
        self.last_update = candidate.time;
        self.update_count += 1;
        self.last_radar_key = candidate.radar_key.clone();
        self.last_radar_position = candidate.radar_position;

        // Promote to Tracking only after enough observations for a reliable motion estimate.
        // 4 updates means 3 successive intervals, giving the Kalman filter enough time to
        // converge on a stable COG/SOG before the target is considered confirmed.
        if self.update_count >= 4 && self.cog.is_some() {
            self.status = TargetStatus::Tracking;
        } else if self.status == TargetStatus::Lost {
            self.status = TargetStatus::Acquiring;
        }

        true
    }

    /// Predict position at given time using motion model
    pub fn predict_position(&self, time: u64) -> GeoPosition {
        self.motion_model.predict(time)
    }

    fn get_uncertainty(&self) -> f64 {
        self.motion_model.get_uncertainty()
    }

    /// Check if target is considered stationary (very low speed, enough updates)
    fn is_stationary(&self) -> bool {
        self.update_count >= MIN_UPDATES_FOR_STATIONARY
            && self
                .sog
                .map(|s| s < STATIONARY_SPEED_THRESHOLD)
                .unwrap_or(false)
    }

    /// Update the revolution count when target was last seen
    fn set_last_update_revolution(&mut self, revolution: u64) {
        self.last_update_revolution = revolution;
    }
}

/// Result of processing a target candidate
#[derive(Debug)]
pub enum ProcessResult {
    /// Target was updated (target_id)
    Updated(u64),
    /// New target was promoted from acquiring to tracking (target_id)
    Promoted(u64),
    /// New target was created in acquiring status (target_id)
    NewAcquiring(u64),
    /// No action taken (e.g., candidate outside guard zone didn't match existing target)
    Ignored,
}

/// Statistics for logging
#[derive(Default)]
struct TrackerStats {
    candidates_processed: u32,
    active_matches: u32,
    new_acquiring: u32,
}

/// Target tracker state
pub struct TargetTracker {
    /// Active targets (including those in Acquiring status)
    active_targets: HashMap<u64, ActiveTarget>,
    /// Next target ID number
    next_id: u64,
    /// ID base for this tracker (0 for merged, 1000000 for radar 1, 2000000 for radar 2, etc.)
    id_base: u64,
    /// Maximum ID offset before wrap (RADAR_ID_MULTIPLIER - 1)
    max_id_offset: u64,
    /// Spokes per revolution (for revolution detection)
    spokes_per_revolution: u16,
    /// Last spoke angle seen
    last_angle: u16,
    /// Revolution counter
    revolution_count: u64,
    /// Statistics for current revolution
    stats: TrackerStats,
}

impl TargetTracker {
    /// Create a new tracker for merged mode
    pub fn new_merged(spokes_per_revolution: u16) -> Self {
        TargetTracker {
            active_targets: HashMap::new(),
            next_id: 1,
            id_base: 0,
            max_id_offset: RADAR_ID_MULTIPLIER - 1,
            spokes_per_revolution,
            last_angle: 0,
            revolution_count: 0,
            stats: TrackerStats::default(),
        }
    }

    /// Create a new tracker for per-radar mode
    pub fn new_per_radar(radar_index: usize, spokes_per_revolution: u16) -> Self {
        TargetTracker {
            active_targets: HashMap::new(),
            next_id: 1,
            id_base: (radar_index as u64) * RADAR_ID_MULTIPLIER,
            max_id_offset: RADAR_ID_MULTIPLIER - 1,
            spokes_per_revolution,
            last_angle: 0,
            revolution_count: 0,
            stats: TrackerStats::default(),
        }
    }

    /// Generate next target ID
    fn next_target_id(&mut self) -> u64 {
        let id = self.id_base + self.next_id;

        self.next_id += 1;
        if self.next_id > self.max_id_offset {
            self.next_id = 1;
        }

        id
    }

    /// Check for revolution boundary and perform cleanup.
    /// Returns `true` if a revolution just completed.
    pub fn check_revolution(&mut self, angle: u16, time: u64) -> bool {
        // Detect revolution boundary (angle wraps from high to low)
        let is_boundary = angle < self.last_angle
            && (self.last_angle - angle) > (self.spokes_per_revolution / 2);
        if is_boundary {
            self.on_revolution_complete(time);
        }
        self.last_angle = angle;
        is_boundary
    }

    /// Handle revolution complete event
    fn on_revolution_complete(&mut self, _time: u64) {
        self.revolution_count += 1;

        // Count targets by status
        let acquiring_count = self
            .active_targets
            .values()
            .filter(|t| t.status == TargetStatus::Acquiring)
            .count();
        let tracking_count = self
            .active_targets
            .values()
            .filter(|t| t.status == TargetStatus::Tracking)
            .count();

        // Merge duplicate targets before logging final counts
        self.deduplicate_targets();

        // Log statistics
        log::info!(
            "Revolution {}: {} targets ({} acquiring, {} tracking), {} candidates processed",
            self.revolution_count,
            self.active_targets.len(),
            acquiring_count,
            tracking_count,
            self.stats.candidates_processed,
        );

        // Reset stats
        self.stats = TrackerStats::default();
    }

    /// Merge duplicate targets that are within DUPLICATE_MERGE_DISTANCE_M of each other.
    /// Only young targets (< 4 updates) are candidates for removal. An established target
    /// (>= 4 updates) is never discarded, but a young target is merged into any nearby
    /// target — young or established. Two established targets are never merged.
    /// A large vessel can produce multiple blobs per rotation from different hull sections,
    /// each starting a separate acquiring track near an already-established one.
    /// For young+young pairs the one with fewer updates is discarded; ties go to the
    /// higher (newer) ID. For young+established pairs the young one is always discarded.
    fn deduplicate_targets(&mut self) {
        let ids: Vec<u64> = self.active_targets.keys().copied().collect();
        let mut to_remove: Vec<u64> = Vec::new();

        for i in 0..ids.len() {
            let a = ids[i];
            if to_remove.contains(&a) {
                continue;
            }
            // Only consider young targets as the candidate to be removed
            if self.active_targets[&a].update_count >= 4 {
                continue;
            }
            for j in 0..ids.len() {
                if i == j {
                    continue;
                }
                let b = ids[j];
                if to_remove.contains(&b) {
                    continue;
                }
                let pos_a = self.active_targets[&a].position;
                let pos_b = self.active_targets[&b].position;
                let dist = calculate_distance(&pos_a, &pos_b);
                if dist > DUPLICATE_MERGE_DISTANCE_M {
                    continue;
                }
                // a is young; keep whichever has more updates (b wins ties since a is young)
                let updates_a = self.active_targets[&a].update_count;
                let updates_b = self.active_targets[&b].update_count;
                let (keep, discard) = if updates_b >= updates_a { (b, a) } else { (a, b) };
                // discard must be young — skip if the merge would remove an established target
                if self.active_targets[&discard].update_count >= 4 {
                    continue;
                }
                log::info!(
                    "Merging duplicate target {} ({} updates) into {} ({} updates), distance={:.0}m",
                    discard,
                    self.active_targets[&discard].update_count,
                    keep,
                    self.active_targets[&keep].update_count,
                    dist,
                );
                to_remove.push(discard);
                break; // a is being discarded; no need to check more partners for it
            }
        }

        for id in to_remove {
            self.active_targets.remove(&id);
        }
    }

    /// Check for timed out targets.
    /// Returns (deleted_ids, newly_lost_ids) - both as target IDs.
    /// Marks targets as Lost if not seen for N revolutions:
    /// - Normal targets: 3 revolutions
    /// - Stationary targets: 10 revolutions (extended to handle temporary merging)
    /// Deletes targets N revolutions after being marked lost:
    /// - Normal targets: 4 revolutions after lost
    /// - Stationary targets: 10 revolutions after lost
    pub fn check_timeouts(&mut self, current_time: u64) -> (Vec<u64>, Vec<u64>) {
        let mut deleted_ids = Vec::new();
        let mut lost_ids = Vec::new();
        let current_revolution = self.revolution_count;

        // Check each active target
        for (id, target) in &mut self.active_targets {
            let revolutions_since_update =
                current_revolution.saturating_sub(target.last_update_revolution);
            let is_stationary = target.is_stationary();

            if target.status == TargetStatus::Lost {
                // Delete after N revolutions since being marked lost
                let delete_revolutions = if is_stationary {
                    STATIONARY_DELETE_REVOLUTION_COUNT
                } else {
                    DELETE_REVOLUTION_COUNT
                };
                let revolutions_since_lost =
                    current_revolution.saturating_sub(target.lost_revolution);
                if revolutions_since_lost >= delete_revolutions {
                    deleted_ids.push(*id);
                    log::info!(
                        "Target {} deleted after {} revolutions lost{}",
                        id,
                        revolutions_since_lost,
                        if is_stationary { " (stationary)" } else { "" }
                    );
                }
            } else {
                // Lost detection is revolution-based
                let lost_revolutions = if is_stationary {
                    STATIONARY_LOST_REVOLUTION_COUNT
                } else {
                    LOST_REVOLUTION_COUNT
                };

                if revolutions_since_update >= lost_revolutions {
                    target.status = TargetStatus::Lost;
                    target.lost_revolution = current_revolution;
                    lost_ids.push(*id);
                    log::info!(
                        "Target {} marked as lost after {} revolutions without update{}",
                        id,
                        revolutions_since_update,
                        if is_stationary { " (stationary)" } else { "" }
                    );
                }
            }
        }

        // Suppress unused warning - current_time still needed for API compatibility
        let _ = current_time;

        // Remove deleted targets
        for id in &deleted_ids {
            self.active_targets.remove(id);
        }

        (deleted_ids, lost_ids)
    }

    /// Process a target candidate, returns what happened
    pub fn process_candidate(&mut self, candidate: TargetCandidate) -> ProcessResult {
        self.stats.candidates_processed += 1;

        // 1. Try to match against active targets (including those in Acquiring status)
        if let Some(target_id) = self.match_active_target(&candidate) {
            if let Some(target) = self.active_targets.get_mut(&target_id) {
                let was_acquiring = target.status == TargetStatus::Acquiring;

                // Update may return false if the maneuver is rejected as implausible
                if !target.update(&candidate) {
                    // Rejected - don't count as match, let it potentially create new target
                    log::debug!(
                        "Update rejected for target {} - maneuver implausible",
                        target_id
                    );
                    // Fall through to create new target if from guard zone
                } else {
                    self.stats.active_matches += 1;
                    // Update revolution count for lost detection
                    target.set_last_update_revolution(self.revolution_count);

                    // If target transitioned from Acquiring to Tracking, report as Promoted
                    if was_acquiring && target.status == TargetStatus::Tracking {
                        log::info!(
                            "Promoted target {} to tracking at ({:.6}, {:.6}), SOG={:.1}m/s, COG={:.1}°",
                            target_id,
                            target.position.lat(),
                            target.position.lon(),
                            target.sog.unwrap_or(0.0),
                            target.cog.map(|c| c.to_degrees()).unwrap_or(0.0)
                        );
                        return ProcessResult::Promoted(target_id);
                    }

                    log::debug!(
                        "Updated active target {} at ({:.6}, {:.6}), SOG={:.1}m/s, COG={:.1}°",
                        target_id,
                        target.position.lat(),
                        target.position.lon(),
                        target.sog.unwrap_or(0.0),
                        target.cog.map(|c| c.to_degrees()).unwrap_or(0.0)
                    );
                    return ProcessResult::Updated(target_id);
                }
            }
        }

        // 2. Only create new targets from GuardZone and Doppler candidates
        // "Anywhere" candidates are only for updating existing targets
        match candidate.source {
            CandidateSource::GuardZone(_) | CandidateSource::Doppler => {
                let target_id = self.create_acquiring_target(&candidate);
                self.stats.new_acquiring += 1;
                ProcessResult::NewAcquiring(target_id)
            }
            CandidateSource::Anywhere => {
                // Don't create target - candidate didn't match any existing target
                ProcessResult::Ignored
            }
        }
    }

    /// Try to match candidate against active targets
    /// Returns the ID of the closest matching target within threshold
    fn match_active_target(&self, candidate: &TargetCandidate) -> Option<u64> {
        let mut best_match: Option<(u64, f64)> = None;

        for (id, target) in &self.active_targets {
            let predicted_pos = target.predict_position(candidate.time);
            let uncertainty = target.get_uncertainty();
            let distance = calculate_distance(&predicted_pos, &candidate.position);

            // Calculate time since last update
            let delta_time_s = (candidate.time.saturating_sub(target.last_update)) as f64 / 1000.0;

            // Physics-based max distance: how far could the target have moved?
            // Use max_target_speed_ms from candidate (user-configured setting)
            // Multiply by 1.5 to account for prediction error when target maneuvers
            let speed_based_dist =
                candidate.max_target_speed_ms * delta_time_s * MATCH_DISTANCE_SPEED_MULTIPLIER;

            // Physics-based max distance: how far could a target at max_target_speed
            // have moved in delta_time? The 1.5 multiplier accounts for:
            // - Prediction error when target is maneuvering
            // - Measurement noise in position estimates
            let max_dist = speed_based_dist.max(MIN_MATCH_DISTANCE_M);

            // Match threshold: physics-based max_dist determines how far a target
            // could have moved at max_target_speed. This provides the primary constraint
            // for matching - if a target is beyond max_dist, it's moving faster than
            // the configured ArpaDetectMaxSpeed and shouldn't be matched.
            //
            // Note: Kalman uncertainty is NOT used to restrict matching because:
            // 1. It can be artificially low in early tracking
            // 2. Even converged, uncertainty reflects model fit, not physical limits
            // Using min(uncertainty, max_dist) would incorrectly reject valid matches
            // when the IMM model hasn't perfectly learned the target's motion.
            let threshold = max_dist;

            log::debug!(
                "Match check: target {} predicted ({:.6}, {:.6}), candidate ({:.6}, {:.6}), distance={:.1}m, threshold={:.1}m (max={:.0}m), uncertainty={:.1}m",
                id,
                predicted_pos.lat(),
                predicted_pos.lon(),
                candidate.position.lat(),
                candidate.position.lon(),
                distance,
                threshold,
                max_dist,
                uncertainty
            );

            if distance < threshold {
                // Track only the closest match
                if best_match.map_or(true, |(_, best_dist)| distance < best_dist) {
                    best_match = Some((*id, distance));
                }
            }
        }

        best_match.map(|(id, _)| id)
    }

    /// Create a new active target in Acquiring status
    fn create_acquiring_target(&mut self, candidate: &TargetCandidate) -> u64 {
        let id = self.next_target_id();
        let mut target = ActiveTarget::new(id, candidate);
        target.set_last_update_revolution(self.revolution_count);

        log::info!(
            "Created acquiring target {} at ({:.6}, {:.6}), size={:.1}m",
            id,
            candidate.position.lat(),
            candidate.position.lon(),
            candidate.size_meters,
        );

        self.active_targets.insert(id, target);
        id
    }

    /// Directly add a target as active (for MARPA - manual acquisition)
    /// Returns the new target ID
    pub fn add_active_target(&mut self, candidate: &TargetCandidate) -> u64 {
        let id = self.next_target_id();
        // MARPA targets need larger initial uncertainty since user clicks are approximate
        // Position variance of 1250 gives ~100m uncertainty (2 * sqrt(1250 + 1250))
        let mut target = ActiveTarget::new_with_uncertainty(id, candidate, 1250.0);
        target.set_last_update_revolution(self.revolution_count);

        log::info!(
            "MARPA: Created active target {} at ({:.6}, {:.6}), size={:.1}m",
            id,
            candidate.position.lat(),
            candidate.position.lon(),
            candidate.size_meters,
        );

        self.active_targets.insert(id, target);
        id
    }

    /// Get all active targets
    pub fn get_active_targets(&self) -> impl Iterator<Item = &ActiveTarget> {
        self.active_targets.values()
    }

    /// Get a specific active target by ID
    pub fn get_target(&self, id: u64) -> Option<&ActiveTarget> {
        self.active_targets.get(&id)
    }

    /// Remove a target by ID (cancel tracking)
    /// Returns true if target was found and removed
    pub fn remove_target(&mut self, id: u64) -> bool {
        if self.active_targets.remove(&id).is_some() {
            log::info!("Target {} removed (tracking cancelled)", id);
            true
        } else {
            log::warn!("Target {} not found for removal", id);
            false
        }
    }

    /// Get number of active targets (including those in Acquiring status)
    pub fn active_count(&self) -> usize {
        self.active_targets.len()
    }
}

/// Calculate distance between two positions in meters
fn calculate_distance(p1: &GeoPosition, p2: &GeoPosition) -> f64 {
    let dlat = (p2.lat() - p1.lat()) * METERS_PER_DEGREE_LATITUDE;
    let dlon = (p2.lon() - p1.lon()) * meters_per_degree_longitude(&p1.lat());
    (dlat * dlat + dlon * dlon).sqrt()
}

/// Calculate bearing from p1 to p2 in radians (0 = North)
fn calculate_bearing(p1: &GeoPosition, p2: &GeoPosition) -> f64 {
    let dlat = (p2.lat() - p1.lat()) * METERS_PER_DEGREE_LATITUDE;
    let dlon = (p2.lon() - p1.lon()) * meters_per_degree_longitude(&p1.lat());

    let bearing = dlon.atan2(dlat);
    if bearing < 0.0 {
        bearing + TAU
    } else {
        bearing
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    /// Default max speed for tests (50 knots)
    const TEST_MAX_SPEED_MS: f64 = 50.0 * 0.5144;

    fn make_candidate(lat: f64, lon: f64, time: u64) -> TargetCandidate {
        make_candidate_with_source(lat, lon, time, CandidateSource::GuardZone(1))
    }

    fn make_candidate_with_source(
        lat: f64,
        lon: f64,
        time: u64,
        source: CandidateSource,
    ) -> TargetCandidate {
        TargetCandidate {
            time,
            position: GeoPosition::new(lat, lon),
            size_meters: 30.0,
            radar_key: "test".to_string(),
            radar_position: Some(GeoPosition::new(52.0, 4.0)),
            max_target_speed_ms: TEST_MAX_SPEED_MS,
            source,
        }
    }

    #[test]
    fn test_target_id_generation_merged() {
        let mut tracker = TargetTracker::new_merged(2048);
        // Merged mode: id_base=0, so IDs are 1, 2, ...
        assert_eq!(tracker.next_target_id(), 1);
        assert_eq!(tracker.next_target_id(), 2);
    }

    #[test]
    fn test_target_id_generation_per_radar() {
        let mut tracker = TargetTracker::new_per_radar(1, 2048);
        // Per-radar mode: radar index 1 has id_base=RADAR_ID_MULTIPLIER
        assert_eq!(tracker.next_target_id(), RADAR_ID_MULTIPLIER + 1);
        assert_eq!(tracker.next_target_id(), RADAR_ID_MULTIPLIER + 2);
    }

    #[test]
    fn test_target_id_wrap() {
        let mut tracker = TargetTracker::new_merged(2048);
        tracker.next_id = RADAR_ID_MULTIPLIER - 1;
        // Merged mode: max_id_offset=RADAR_ID_MULTIPLIER-1, wraps to 1
        assert_eq!(tracker.next_target_id(), RADAR_ID_MULTIPLIER - 1);
        assert_eq!(tracker.next_target_id(), 1); // Wraps to 1, not 0
        assert_eq!(tracker.next_target_id(), 2);
    }

    #[test]
    fn test_calculate_distance() {
        let p1 = GeoPosition::new(52.0, 4.0);
        let p2 = GeoPosition::new(52.001, 4.0); // ~111m north

        let dist = calculate_distance(&p1, &p2);
        assert!(dist > 100.0 && dist < 120.0, "Distance was {}", dist);
    }

    #[test]
    fn test_calculate_bearing_north() {
        let p1 = GeoPosition::new(52.0, 4.0);
        let p2 = GeoPosition::new(52.001, 4.0); // North

        let bearing = calculate_bearing(&p1, &p2);
        assert!(bearing.abs() < 0.01, "Bearing north was {}", bearing);
    }

    #[test]
    fn test_calculate_bearing_east() {
        let p1 = GeoPosition::new(52.0, 4.0);
        let p2 = GeoPosition::new(52.0, 4.001); // East

        let bearing = calculate_bearing(&p1, &p2);
        let expected = PI / 2.0; // 90 degrees
        assert!(
            (bearing - expected).abs() < 0.01,
            "Bearing east was {} (expected {})",
            bearing.to_degrees(),
            expected.to_degrees()
        );
    }

    #[test]
    fn test_new_acquiring_target() {
        let mut tracker = TargetTracker::new_merged(2048);

        let candidate = make_candidate(52.0, 4.0, 1000);
        let result = tracker.process_candidate(candidate);

        // First candidate creates an active target in Acquiring status
        assert!(matches!(result, ProcessResult::NewAcquiring(_)));
        assert_eq!(tracker.active_count(), 1);

        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Acquiring);
    }

    #[test]
    fn test_promote_to_tracking() {
        let mut tracker = TargetTracker::new_merged(2048);

        // First 3 candidates build up the track but remain Acquiring
        for i in 0..3 {
            let lat = 52.0 + (i as f64 * 0.0001);
            tracker.process_candidate(make_candidate(lat, 4.0, 1000 + i * 3000));
        }
        assert_eq!(tracker.active_count(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Acquiring);

        // Fourth candidate promotes to tracking (4 updates with COG established)
        let result = tracker.process_candidate(make_candidate(52.0003, 4.0, 10000));

        assert!(matches!(result, ProcessResult::Promoted(_)));
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);
        assert!(target.cog.is_some());
    }

    #[test]
    fn test_active_target_matching() {
        let mut tracker = TargetTracker::new_merged(2048);

        // Create an active target via promotion
        let candidate1 = make_candidate(52.0, 4.0, 1000);
        tracker.process_candidate(candidate1);

        let candidate2 = make_candidate(52.0001, 4.0, 4000);
        tracker.process_candidate(candidate2);

        assert_eq!(tracker.active_count(), 1);

        // Third candidate should match the active target
        let candidate3 = make_candidate(52.0002, 4.0, 7000);
        tracker.process_candidate(candidate3);

        // Still just one active target (it was updated, not duplicated)
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_acquiring_timeout() {
        let mut tracker = TargetTracker::new_merged(2048);

        // Add a candidate - now creates an active target in Acquiring status
        let candidate = make_candidate(52.0, 4.0, 1000);
        tracker.process_candidate(candidate);
        assert_eq!(tracker.active_count(), 1);

        // Simulate 3 revolutions passing without updates (LOST_REVOLUTION_COUNT = 3)
        for i in 0..3 {
            tracker.check_revolution(2000, 2000 + i * 3000);
            tracker.check_revolution(100, 3000 + i * 3000);
        }

        // Check - acquiring targets should become lost after 3 revolutions
        let (deleted, lost) = tracker.check_timeouts(11_000);
        assert!(deleted.is_empty());
        assert_eq!(lost.len(), 1);

        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Lost);
    }

    #[test]
    fn test_revolution_detection() {
        let mut tracker = TargetTracker::new_merged(2048);

        // Simulate spokes without wrap
        tracker.check_revolution(100, 1000);
        tracker.check_revolution(200, 1000);
        tracker.check_revolution(300, 1000);
        assert_eq!(tracker.revolution_count, 0);

        // Wrap around (high to low = revolution complete)
        tracker.check_revolution(2000, 1000);
        tracker.check_revolution(100, 2000);
        assert_eq!(tracker.revolution_count, 1);
    }

    #[test]
    fn test_no_match_too_far_apart() {
        let mut tracker = TargetTracker::new_merged(2048);

        // First candidate - creates active target
        let candidate1 = make_candidate(52.0, 4.0, 1000);
        tracker.process_candidate(candidate1);

        // Second candidate very far away - should not match, creates new target
        let candidate2 = make_candidate(53.0, 5.0, 4000); // ~100km away
        tracker.process_candidate(candidate2);

        // Should have two separate active targets (both in Acquiring status)
        assert_eq!(tracker.active_count(), 2);
    }

    #[test]
    fn test_active_target_update() {
        let mut tracker = TargetTracker::new_merged(2048);

        // Create active target via promotion
        tracker.process_candidate(make_candidate(52.0, 4.0, 0));
        tracker.process_candidate(make_candidate(52.0001, 4.0, 3000));

        assert_eq!(tracker.active_count(), 1);

        // Update with moving target
        tracker.process_candidate(make_candidate(52.0002, 4.0, 6000));
        tracker.process_candidate(make_candidate(52.0003, 4.0, 9000));

        // Get the active target and check SOG
        let target = tracker.get_active_targets().next().unwrap();
        assert!(target.sog.is_some(), "SOG should be set");
        assert!(
            target.sog.unwrap() > 0.0,
            "SOG should be positive: {:?}",
            target.sog
        );
        assert!(target.cog.is_some(), "COG should be set");
        assert!(
            target.update_count >= 3,
            "Update count: {}",
            target.update_count
        );
    }

    #[test]
    fn test_target_lost_timeout() {
        let mut tracker = TargetTracker::new_merged(2048);

        // 4 updates to reach Tracking (last update at 9000ms)
        for i in 0..4u64 {
            let lat = 52.0 + (i as f64 * 0.0001);
            tracker.process_candidate(make_candidate(lat, 4.0, i * 3000));
        }
        assert_eq!(tracker.active_count(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        // Simulate 2 revolutions without update - should still be Tracking
        for i in 0..2 {
            tracker.check_revolution(2000, 10_000 + i * 3000);
            tracker.check_revolution(100, 11_000 + i * 3000);
        }
        let (deleted, lost) = tracker.check_timeouts(16_000);
        assert!(deleted.is_empty());
        assert!(lost.is_empty());

        // One more revolution (total 3) - should become Lost
        tracker.check_revolution(2000, 16_000);
        tracker.check_revolution(100, 17_000);
        let (deleted, lost) = tracker.check_timeouts(18_000);
        assert!(deleted.is_empty());
        assert_eq!(lost.len(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Lost);

        // Check again - should not re-report as lost
        let (deleted, lost) = tracker.check_timeouts(21_000);
        assert!(deleted.is_empty());
        assert!(lost.is_empty());
    }

    #[test]
    fn test_target_deleted_timeout() {
        let mut tracker = TargetTracker::new_merged(2048);

        // 4 updates to reach Tracking (last update at 9000ms)
        for i in 0..4u64 {
            let lat = 52.0 + (i as f64 * 0.0001);
            tracker.process_candidate(make_candidate(lat, 4.0, i * 3000));
        }
        assert_eq!(tracker.active_count(), 1);

        // 3 revolutions → lost
        for i in 0..3 {
            tracker.check_revolution(2000, 10_000 + i * 3000);
            tracker.check_revolution(100, 11_000 + i * 3000);
        }
        let (deleted, lost) = tracker.check_timeouts(19_000);
        assert!(deleted.is_empty());
        assert_eq!(lost.len(), 1);

        // 4 more revolutions after lost → deleted (DELETE_REVOLUTION_COUNT = 4)
        for i in 0..4 {
            tracker.check_revolution(2000, 19_000 + i * 3000);
            tracker.check_revolution(100, 20_000 + i * 3000);
        }
        let (deleted, _) = tracker.check_timeouts(31_000);
        assert_eq!(deleted.len(), 1);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_target_recovers_from_lost() {
        let mut tracker = TargetTracker::new_merged(2048);

        // MARPA target (counts as update 1) + 3 more updates to reach Tracking
        let candidate = make_candidate(52.0, 4.0, 0);
        tracker.add_active_target(&candidate);
        for i in 1..4u64 {
            let lat = 52.0 + (i as f64 * 0.0001);
            tracker.process_candidate(make_candidate(lat, 4.0, i * 3000));
        }
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        // Get predicted position for recovery
        let predicted = target.predict_position(21_000);

        // 3 revolutions without update → lost
        for i in 0..3 {
            tracker.check_revolution(2000, 10_000 + i * 3000);
            tracker.check_revolution(100, 11_000 + i * 3000);
        }
        let (_, lost) = tracker.check_timeouts(19_000);
        assert_eq!(lost.len(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Lost);

        // Target is seen again at predicted position - should recover to Tracking
        // (update_count is now 5 which is >= 4, and COG is set)
        let candidate3 = make_candidate(predicted.lat(), predicted.lon(), 21_000);
        tracker.process_candidate(candidate3);

        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);
    }

    #[test]
    fn test_stationary_target_extended_timeout() {
        let mut tracker = TargetTracker::new_merged(2048);

        // Create a stationary target (buoy) - same position for multiple updates
        // Need MIN_UPDATES_FOR_STATIONARY (5) updates at same position
        let pos = (52.0, 4.0);
        for i in 0..6 {
            let candidate = make_candidate(pos.0, pos.1, i * 3000);
            tracker.process_candidate(candidate);
        }

        assert_eq!(tracker.active_count(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);
        assert!(
            target.sog.unwrap() < 0.5,
            "SOG should be near zero: {:?}",
            target.sog
        );
        assert!(target.update_count >= 5);

        // Simulate 5 revolutions - normal target would be lost after 3
        // But stationary target has 10 revolution timeout, so should still be tracking
        for i in 0..5 {
            tracker.check_revolution(2000, 16_000 + i * 3000);
            tracker.check_revolution(100, 17_000 + i * 3000);
        }
        let (deleted, lost) = tracker.check_timeouts(35_000);
        assert!(deleted.is_empty());
        assert!(lost.is_empty());
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        // Simulate 5 more revolutions (total 10) - should be lost
        for i in 0..5 {
            tracker.check_revolution(2000, 32_000 + i * 3000);
            tracker.check_revolution(100, 33_000 + i * 3000);
        }
        let (deleted, lost) = tracker.check_timeouts(50_000);
        assert!(deleted.is_empty());
        assert_eq!(lost.len(), 1);
        let target = tracker.get_active_targets().next().unwrap();
        assert_eq!(target.status, TargetStatus::Lost);

        // 10 more revolutions after lost → deleted (STATIONARY_DELETE_REVOLUTION_COUNT = 10)
        for i in 0..10 {
            tracker.check_revolution(2000, 48_000 + i * 3000);
            tracker.check_revolution(100, 49_000 + i * 3000);
        }
        let (deleted, _lost) = tracker.check_timeouts(78_000);
        assert_eq!(deleted.len(), 1);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_circling_target_tracks_continuously() {
        // Simulates a boat circling at 15 knots in a 250m radius circle
        // for 2 full revolutions of the circle (not radar revolutions).
        // With forced position override, the tracker should maintain track
        // through continuous turns by blending measured velocities.

        let mut tracker = TargetTracker::new_merged(2048);

        // Circle parameters (matching emulator world.rs)
        let radius_m = 250.0;
        let speed_knots = 15.0;
        let speed_ms = speed_knots * 1852.0 / 3600.0; // ~7.72 m/s
        let angular_velocity = speed_ms / radius_m; // ~0.031 rad/s

        // Time for one full circle = 2π / angular_velocity ≈ 203 seconds
        // Time for 2 full circles ≈ 406 seconds
        let circle_time_s = TAU / angular_velocity;

        // Center of circle is 350m north of radar (at 52.0, 4.0)
        let radar_lat = 52.0;
        let radar_lon = 4.0;
        let center_lat = radar_lat + 350.0 / METERS_PER_DEGREE_LATITUDE;
        let center_lon = radar_lon;

        // Helper to calculate position on circle at given angle
        // angle=0 is south of center (closest to radar), increasing clockwise
        let position_at_angle = |angle: f64| -> (f64, f64) {
            // bearing from center: south + angle
            let bearing = PI + angle;
            let lat = center_lat + radius_m * bearing.cos() / METERS_PER_DEGREE_LATITUDE;
            let lon =
                center_lon + radius_m * bearing.sin() / meters_per_degree_longitude(&center_lat);
            (lat, lon)
        };

        // Radar revolution time ~3 seconds (typical radar)
        let revolution_ms = 3000u64;

        // Number of radar revolutions needed for 2 full circles
        let num_revolutions = (2.0 * circle_time_s / 3.0).ceil() as u64 + 2;

        // Start tracking: first detection at angle=0 (south of center)
        let (lat0, lon0) = position_at_angle(0.0);
        let result0 = tracker.process_candidate(make_candidate(lat0, lon0, 0));
        let target_id = match result0 {
            ProcessResult::NewAcquiring(id) => id,
            _ => panic!("Expected NewAcquiring"),
        };

        // Feed 3 more candidates to reach promotion (4 updates total)
        for i in 1..4u64 {
            let angle = angular_velocity * (i as f64 * 3.0);
            let (lat, lon) = position_at_angle(angle);
            tracker.process_candidate(make_candidate(lat, lon, i * revolution_ms));
        }
        let target = tracker.get_target(target_id).unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        // Continue tracking through 2 full circles
        let mut successful_updates = 0;
        let mut lost_count = 0;

        for rev in 4..num_revolutions {
            let time = rev * revolution_ms;
            let angle = angular_velocity * (time as f64 / 1000.0);
            let (lat, lon) = position_at_angle(angle);

            let candidate = make_candidate(lat, lon, time);
            let result = tracker.process_candidate(candidate);

            match result {
                ProcessResult::Updated(id) if id == target_id => {
                    successful_updates += 1;
                }
                ProcessResult::NewAcquiring(_) => {
                    lost_count += 1;
                }
                _ => {}
            }
        }

        // With forced position override for fast targets, we should maintain tracking
        // through the entire test
        let total_circles = (angular_velocity * num_revolutions as f64 * 3.0) / TAU;
        println!(
            "Circling target: {} successful updates, {} lost, {:.1} full circles completed ({}s circle time, {} radar revs)",
            successful_updates, lost_count, total_circles, circle_time_s as i32, num_revolutions
        );

        // Should have maintained tracking for at least 90% of updates
        let expected_updates = (num_revolutions - 4) as usize;
        let min_successful = (expected_updates as f64 * 0.9) as usize;
        assert!(
            successful_updates >= min_successful,
            "Expected at least {} successful updates (90% of {}), got {}",
            min_successful,
            expected_updates,
            successful_updates
        );

        // Should have only one active target (no fragmentation)
        assert_eq!(
            tracker.active_count(),
            1,
            "Expected exactly 1 target for circling boat, got {}",
            tracker.active_count()
        );

        // Verify target has reasonable speed estimate (should be close to 15 knots = 7.72 m/s)
        let target = tracker.get_target(target_id).unwrap();
        let tracked_speed = target.sog.unwrap_or(0.0);
        assert!(
            (tracked_speed - speed_ms).abs() < 2.0,
            "Target speed {:.1} m/s should be close to actual {:.1} m/s",
            tracked_speed,
            speed_ms
        );
    }

    #[test]
    fn test_circling_target_imm_strategy() {
        // Same test as test_circling_target_tracks_continuously but using IMM strategy
        // for 2 full revolutions of the circle (not radar revolutions).
        // IMM should handle the constant turning better due to multiple motion models

        let mut tracker = TargetTracker::new_merged(2048);

        // Circle parameters (matching emulator world.rs)
        let radius_m = 250.0;
        let speed_knots = 15.0;
        let speed_ms = speed_knots * 1852.0 / 3600.0; // ~7.72 m/s
        let angular_velocity = speed_ms / radius_m; // ~0.031 rad/s

        // Time for one full circle = 2π / angular_velocity ≈ 203 seconds
        // Time for 2 full circles ≈ 406 seconds
        let circle_time_s = TAU / angular_velocity;

        // Center of circle is 350m north of radar (at 52.0, 4.0)
        let radar_lat = 52.0;
        let radar_lon = 4.0;
        let center_lat = radar_lat + 350.0 / METERS_PER_DEGREE_LATITUDE;
        let center_lon = radar_lon;

        // Helper to calculate position on circle at given angle
        let position_at_angle = |angle: f64| -> (f64, f64) {
            let bearing = PI + angle;
            let lat = center_lat + radius_m * bearing.cos() / METERS_PER_DEGREE_LATITUDE;
            let lon =
                center_lon + radius_m * bearing.sin() / meters_per_degree_longitude(&center_lat);
            (lat, lon)
        };

        let revolution_ms = 3000u64;

        // Number of radar revolutions needed for 2 full circles
        let num_revolutions = (2.0 * circle_time_s / 3.0).ceil() as u64 + 2;

        // Start tracking
        let (lat0, lon0) = position_at_angle(0.0);
        let result0 = tracker.process_candidate(make_candidate(lat0, lon0, 0));
        let target_id = match result0 {
            ProcessResult::NewAcquiring(id) => id,
            _ => panic!("Expected NewAcquiring"),
        };

        // Feed 3 more candidates to reach promotion (4 updates total)
        for i in 1..4u64 {
            let angle = angular_velocity * (i as f64 * 3.0);
            let (lat, lon) = position_at_angle(angle);
            tracker.process_candidate(make_candidate(lat, lon, i * revolution_ms));
        }
        let target = tracker.get_target(target_id).unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        let mut successful_updates = 0;
        let mut lost_count = 0;

        for rev in 4..num_revolutions {
            let time = rev * revolution_ms;
            let angle = angular_velocity * (time as f64 / 1000.0);
            let (lat, lon) = position_at_angle(angle);

            let candidate = make_candidate(lat, lon, time);
            let result = tracker.process_candidate(candidate);

            match result {
                ProcessResult::Updated(id) if id == target_id => {
                    successful_updates += 1;
                }
                ProcessResult::NewAcquiring(_) => {
                    lost_count += 1;
                }
                _ => {}
            }
        }

        let total_circles = (angular_velocity * num_revolutions as f64 * 3.0) / TAU;
        println!(
            "IMM circling target: {} successful updates, {} lost, {:.1} full circles completed ({}s circle time, {} radar revs)",
            successful_updates, lost_count, total_circles, circle_time_s as i32, num_revolutions
        );

        // IMM should maintain tracking through continuous turns
        let expected_updates = (num_revolutions - 4) as usize;
        let min_successful = (expected_updates as f64 * 0.9) as usize;
        assert!(
            successful_updates >= min_successful,
            "IMM: Expected at least {} successful updates (90% of {}), got {}",
            min_successful,
            expected_updates,
            successful_updates
        );

        // Should have only one active target (no fragmentation)
        assert_eq!(
            tracker.active_count(),
            1,
            "IMM: Expected exactly 1 target for circling boat, got {}",
            tracker.active_count()
        );

        // Verify target has reasonable speed estimate
        let target = tracker.get_target(target_id).unwrap();
        let tracked_speed = target.sog.unwrap_or(0.0);
        assert!(
            (tracked_speed - speed_ms).abs() < 3.0, // IMM may have slightly different estimates
            "IMM: Target speed {:.1} m/s should be close to actual {:.1} m/s",
            tracked_speed,
            speed_ms
        );
    }

    #[test]
    fn test_circling_target_leaves_guard_zone() {
        // Simulates a circling target that is acquired in a guard zone, then
        // leaves the zone but continues to be tracked via CandidateSource::Anywhere.
        // This mimics real emulator behavior where guard zones may only cover
        // part of the circle.

        let mut tracker = TargetTracker::new_merged(2048);

        // Circle parameters (matching emulator world.rs)
        let radius_m = 250.0;
        let speed_knots = 15.0;
        let speed_ms = speed_knots * 1852.0 / 3600.0; // ~7.72 m/s
        let angular_velocity = speed_ms / radius_m; // ~0.031 rad/s

        let circle_time_s = TAU / angular_velocity;

        let radar_lat = 52.0;
        let radar_lon = 4.0;
        let center_lat = radar_lat + 350.0 / METERS_PER_DEGREE_LATITUDE;
        let center_lon = radar_lon;

        let position_at_angle = |angle: f64| -> (f64, f64) {
            let bearing = PI + angle;
            let lat = center_lat + radius_m * bearing.cos() / METERS_PER_DEGREE_LATITUDE;
            let lon =
                center_lon + radius_m * bearing.sin() / meters_per_degree_longitude(&center_lat);
            (lat, lon)
        };

        let revolution_ms = 3000u64;
        let num_revolutions = (2.0 * circle_time_s / 3.0).ceil() as u64 + 2;

        // First 4 detections are in guard zone (target gets acquired and promoted)
        let (lat0, lon0) = position_at_angle(0.0);
        let result0 =
            tracker.process_candidate(make_candidate_with_source(lat0, lon0, 0, CandidateSource::GuardZone(1)));
        let target_id = match result0 {
            ProcessResult::NewAcquiring(id) => id,
            _ => panic!("Expected NewAcquiring"),
        };

        for i in 1..4u64 {
            let angle = angular_velocity * (i as f64 * 3.0);
            let (lat, lon) = position_at_angle(angle);
            tracker.process_candidate(make_candidate_with_source(
                lat, lon, i * revolution_ms, CandidateSource::GuardZone(1),
            ));
        }
        let target = tracker.get_target(target_id).unwrap();
        assert_eq!(target.status, TargetStatus::Tracking);

        // Remaining detections are OUTSIDE guard zone (CandidateSource::Anywhere)
        // These should still match the existing active target
        let mut successful_updates = 0;
        let mut lost_count = 0;

        for rev in 4..num_revolutions {
            let time = rev * revolution_ms;
            let angle = angular_velocity * (time as f64 / 1000.0);
            let (lat, lon) = position_at_angle(angle);

            // After initial acquisition, target is outside guard zone
            let candidate = make_candidate_with_source(lat, lon, time, CandidateSource::Anywhere);
            let result = tracker.process_candidate(candidate);

            match result {
                ProcessResult::Updated(id) if id == target_id => {
                    successful_updates += 1;
                }
                ProcessResult::NewAcquiring(_) => {
                    lost_count += 1;
                }
                ProcessResult::Ignored => {
                    // This is the problem case - candidate didn't match existing target
                    lost_count += 1;
                }
                _ => {}
            }
        }

        let total_circles = (angular_velocity * num_revolutions as f64 * 3.0) / TAU;
        println!(
            "Circling outside guard zone: {} successful, {} lost, {:.1} circles ({}s circle, {} radar revs)",
            successful_updates, lost_count, total_circles, circle_time_s as i32, num_revolutions
        );

        // Should maintain tracking even when leaving guard zone
        let expected_updates = (num_revolutions - 4) as usize;
        let min_successful = (expected_updates as f64 * 0.9) as usize;
        assert!(
            successful_updates >= min_successful,
            "Expected at least {} successful updates (90% of {}), got {} (lost {})",
            min_successful,
            expected_updates,
            successful_updates,
            lost_count
        );

        // Should have only one active target
        assert_eq!(
            tracker.active_count(),
            1,
            "Expected 1 target, got {}",
            tracker.active_count()
        );
    }

    #[test]
    fn test_circling_target_marpa_acquisition() {
        // Simulates a circling target that is manually acquired via MARPA (user click).
        // MARPA targets go directly to active status and should be tracked
        // through continuous turns for 2 full circles.

        let mut tracker = TargetTracker::new_merged(2048);

        // Circle parameters (matching emulator world.rs)
        let radius_m = 250.0;
        let speed_knots = 15.0;
        let speed_ms = speed_knots * 1852.0 / 3600.0; // ~7.72 m/s
        let angular_velocity = speed_ms / radius_m; // ~0.031 rad/s

        let circle_time_s = TAU / angular_velocity;

        let radar_lat = 52.0;
        let radar_lon = 4.0;
        let center_lat = radar_lat + 350.0 / METERS_PER_DEGREE_LATITUDE;
        let center_lon = radar_lon;

        let position_at_angle = |angle: f64| -> (f64, f64) {
            let bearing = PI + angle;
            let lat = center_lat + radius_m * bearing.cos() / METERS_PER_DEGREE_LATITUDE;
            let lon =
                center_lon + radius_m * bearing.sin() / meters_per_degree_longitude(&center_lat);
            (lat, lon)
        };

        let revolution_ms = 3000u64;
        let num_revolutions = (2.0 * circle_time_s / 3.0).ceil() as u64 + 2;

        // MARPA acquisition - user clicks on the target (GuardZone(0) = manual)
        let (lat0, lon0) = position_at_angle(0.0);
        let candidate0 = make_candidate_with_source(lat0, lon0, 0, CandidateSource::GuardZone(0));

        // Use add_active_target for MARPA (bypasses acquiring phase)
        let target_id = tracker.add_active_target(&candidate0);

        let target = tracker.get_target(target_id).unwrap();
        assert!(target.is_manual, "MARPA target should be marked as manual");
        assert_eq!(
            target.status,
            TargetStatus::Acquiring,
            "MARPA target starts in Acquiring"
        );

        // Second detection updates the target
        let angle1 = angular_velocity * 3.0;
        let (lat1, lon1) = position_at_angle(angle1);
        let candidate1 =
            make_candidate_with_source(lat1, lon1, revolution_ms, CandidateSource::Anywhere);
        let result1 = tracker.process_candidate(candidate1);
        assert!(
            matches!(
                result1,
                ProcessResult::Promoted(_) | ProcessResult::Updated(_)
            ),
            "Should update/promote MARPA target: {:?}",
            result1
        );

        // Continue tracking through 2 full circles (all detections outside guard zone)
        let mut successful_updates = 0;
        let mut lost_count = 0;

        for rev in 2..num_revolutions {
            let time = rev * revolution_ms;
            let angle = angular_velocity * (time as f64 / 1000.0);
            let (lat, lon) = position_at_angle(angle);

            let candidate = make_candidate_with_source(lat, lon, time, CandidateSource::Anywhere);
            let result = tracker.process_candidate(candidate);

            match result {
                ProcessResult::Updated(id) if id == target_id => {
                    successful_updates += 1;
                }
                ProcessResult::NewAcquiring(_) => {
                    lost_count += 1;
                }
                ProcessResult::Ignored => {
                    lost_count += 1;
                }
                _ => {}
            }
        }

        let total_circles = (angular_velocity * num_revolutions as f64 * 3.0) / TAU;
        println!(
            "MARPA circling target: {} successful, {} lost, {:.1} circles ({}s circle, {} radar revs)",
            successful_updates, lost_count, total_circles, circle_time_s as i32, num_revolutions
        );

        // Should maintain tracking through continuous turns
        let expected_updates = (num_revolutions - 2) as usize;
        let min_successful = (expected_updates as f64 * 0.9) as usize;
        assert!(
            successful_updates >= min_successful,
            "MARPA: Expected at least {} successful updates (90% of {}), got {} (lost {})",
            min_successful,
            expected_updates,
            successful_updates,
            lost_count
        );

        // Should have only one active target
        assert_eq!(
            tracker.active_count(),
            1,
            "MARPA: Expected 1 target, got {}",
            tracker.active_count()
        );

        // Verify it's still marked as manual
        let target = tracker.get_target(target_id).unwrap();
        assert!(target.is_manual, "Target should still be marked as manual");
    }

    /// Helper to make a candidate with specific max_target_speed_ms
    fn make_candidate_with_speed(
        lat: f64,
        lon: f64,
        time: u64,
        max_speed_ms: f64,
    ) -> TargetCandidate {
        TargetCandidate {
            time,
            position: GeoPosition::new(lat, lon),
            size_meters: 30.0,
            radar_key: "test".to_string(),
            radar_position: Some(GeoPosition::new(52.0, 4.0)),
            max_target_speed_ms: max_speed_ms,
            source: CandidateSource::GuardZone(1),
        }
    }

    #[test]
    fn test_fast_target_missed_with_normal_speed() {
        // Test that 40-knot targets are eventually lost when using normal speed setting (25 knots)
        // but successfully tracked when using medium (40 knots) or fast (50 knots) settings.
        //
        // The emulator has fast targets moving east at 40 knots (FAST_TARGET_SPEED_KNOTS).
        // At 3-second radar revolution:
        // - 40 kn target moves: 40 * 0.5144 * 3 = ~62m per revolution
        // - Normal (25 kn) max_dist (established): 25 * 0.5144 * 3 * 1.5 = ~58m (misses 62m)
        // - Medium (40 kn) max_dist: 40 * 0.5144 * 3 * 1.5 = ~93m (catches 62m)
        // - Fast (50 kn) max_dist: 50 * 0.5144 * 3 * 1.5 = ~116m (catches 62m)
        //
        // During early tracking (update_count <= 2), physics-based matching is used with 2x
        // multiplier, so all speed settings can initially acquire the target. After the target
        // reaches established tracking (update_count > 2), the normal speed max_dist becomes
        // limiting and the target is lost.

        const KN_TO_MS: f64 = 0.5144;
        const NORMAL_SPEED_MS: f64 = 25.0 * KN_TO_MS; // ~12.9 m/s
        const MEDIUM_SPEED_MS: f64 = 40.0 * KN_TO_MS; // ~20.6 m/s
        const FAST_SPEED_MS: f64 = 50.0 * KN_TO_MS; // ~25.7 m/s
        const TARGET_SPEED_MS: f64 = 40.0 * KN_TO_MS; // Fast boat speed

        let revolution_ms = 3000u64;
        let num_revolutions = 15; // Need more revolutions to see misses after established tracking

        // Starting position (300m north of radar)
        let radar_lat = 52.0;
        let start_lat = radar_lat + 300.0 / METERS_PER_DEGREE_LATITUDE;
        let start_lon = 4.0;

        // Distance traveled per revolution (eastward)
        let distance_per_rev = TARGET_SPEED_MS * (revolution_ms as f64 / 1000.0);
        let lon_per_rev = distance_per_rev / meters_per_degree_longitude(&start_lat);

        // Test 1: Normal speed setting - should eventually MISS fast targets
        // After initial acquisition (revs 0-2), the target becomes established and then
        // the normal speed max_dist (58m) can't catch the 62m movements.
        {
            let mut tracker = TargetTracker::new_merged(2048);

            let mut new_target_count = 0;
            let mut miss_after_established = 0;

            for rev in 0..num_revolutions {
                let time = rev * revolution_ms;
                let lon = start_lon + lon_per_rev * rev as f64;

                let candidate = make_candidate_with_speed(start_lat, lon, time, NORMAL_SPEED_MS);
                let result = tracker.process_candidate(candidate);

                if matches!(result, ProcessResult::NewAcquiring(_)) {
                    new_target_count += 1;
                    // After revolution 5, the first target should be established
                    // (4 updates for promotion + 1 for established tracking)
                    // Any new targets after that indicate misses
                    if rev >= 5 {
                        miss_after_established += 1;
                    }
                }
            }

            // With normal speed setting (25 kn), fast 40-knot targets should be missed
            // after they become established (update_count > 2). We expect misses to start
            // around revolution 3-4.
            assert!(
                miss_after_established >= 3,
                "Normal speed: Expected at least 3 misses after established tracking, got {} (total new targets: {}, active: {})",
                miss_after_established,
                new_target_count,
                tracker.active_count()
            );
            println!(
                "Normal speed: {} new targets, {} misses after established, {} active total",
                new_target_count,
                miss_after_established,
                tracker.active_count()
            );
        }

        // Test 2: Medium speed setting - should TRACK fast targets continuously
        // Medium speed (40 kn) matches target speed (40 kn), so max_dist = 93m catches 62m moves
        {
            let mut tracker = TargetTracker::new_merged(2048);

            let mut update_count = 0;
            let mut promoted_id: Option<u64> = None;

            for rev in 0..num_revolutions {
                let time = rev * revolution_ms;
                let lon = start_lon + lon_per_rev * rev as f64;

                let candidate = make_candidate_with_speed(start_lat, lon, time, MEDIUM_SPEED_MS);
                let result = tracker.process_candidate(candidate);

                match result {
                    ProcessResult::Promoted(id) => {
                        promoted_id = Some(id);
                    }
                    ProcessResult::Updated(id) if promoted_id == Some(id) => {
                        update_count += 1;
                    }
                    _ => {}
                }
            }

            // With medium speed setting (40 kn), 40-knot targets should be tracked.
            assert!(
                promoted_id.is_some(),
                "Medium speed: Expected target to be promoted to tracking"
            );

            // After promotion at rev 3, we should have updates for revs 4-14 (11 updates)
            let expected_updates = num_revolutions - 4;
            assert!(
                update_count >= expected_updates - 1,
                "Medium speed: Expected at least {} updates after promotion, got {}",
                expected_updates - 1,
                update_count
            );
            assert_eq!(
                tracker.active_count(),
                1,
                "Medium speed: Should have exactly 1 target, got {}",
                tracker.active_count()
            );
            println!(
                "Medium speed: {} updates after promotion, {} active total",
                update_count,
                tracker.active_count()
            );
        }

        // Test 3: Fast speed setting - should definitely TRACK fast targets continuously
        // Fast speed (50 kn) exceeds target speed (40 kn), so max_dist = 116m easily catches 62m moves
        {
            let mut tracker = TargetTracker::new_merged(2048);

            let mut update_count = 0;
            let mut promoted_id: Option<u64> = None;

            for rev in 0..num_revolutions {
                let time = rev * revolution_ms;
                let lon = start_lon + lon_per_rev * rev as f64;

                let candidate = make_candidate_with_speed(start_lat, lon, time, FAST_SPEED_MS);
                let result = tracker.process_candidate(candidate);

                match result {
                    ProcessResult::Promoted(id) => {
                        promoted_id = Some(id);
                    }
                    ProcessResult::Updated(id) if promoted_id == Some(id) => {
                        update_count += 1;
                    }
                    _ => {}
                }
            }

            // With fast speed setting (50 kn), 40-knot targets should definitely be tracked
            assert!(
                promoted_id.is_some(),
                "Fast speed: Expected target to be promoted to tracking"
            );

            let expected_updates = num_revolutions - 4;
            assert_eq!(
                update_count, expected_updates,
                "Fast speed: Expected all {} updates after promotion, got {}",
                expected_updates, update_count
            );
            assert_eq!(
                tracker.active_count(),
                1,
                "Fast speed: Should have exactly 1 target"
            );
            println!(
                "Fast speed: {} updates after promotion, {} active total",
                update_count,
                tracker.active_count()
            );
        }
    }
}
