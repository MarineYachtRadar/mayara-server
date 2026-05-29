//! Target tracking API types and utilities.
//!
//! This module provides the API types for representing tracked targets
//! that are sent to the GUI via Signal K, as well as blob detection for
//! identifying potential targets, and target tracking with IMM filtering.

mod blob;
mod kalman;
mod manager;
mod motion;
mod tracker;

pub use blob::{BlobDetector, CompletedBlob, MAX_TARGET_SIZE_M, MIN_TARGET_SIZE_M};
pub use manager::{BlobMessage, MarpaRequest, SpokeContext, TrackerCommand, TrackerManager};
pub use motion::{ImmMotionModel, MotionModel};
pub use tracker::{
    ActiveTarget, CandidateSource, ProcessResult, TargetCandidate, TargetStatus, TargetTracker,
};

use serde::Serialize;
use utoipa::ToSchema;

use super::NAUTICAL_MILE_F64;

// ============================================================================
// Geographic constants and utilities
// ============================================================================

pub const METERS_PER_DEGREE_LATITUDE: f64 = 60. * NAUTICAL_MILE_F64;
pub const KN_TO_MS: f64 = NAUTICAL_MILE_F64 / 3600.;
pub const MS_TO_KN: f64 = 3600. / NAUTICAL_MILE_F64;

/// The length of a degree longitude varies by the latitude,
/// the more north or south you get the shorter it becomes.
/// Since the earth is _nearly_ a sphere, the cosine function
/// is _very_ close.
pub fn meters_per_degree_longitude(lat: &f64) -> f64 {
    METERS_PER_DEGREE_LATITUDE * lat.to_radians().cos()
}

// ============================================================================
// Signal K API Types for Target Streaming
// ============================================================================

/// Signal K compatible target representation for API/WebSocket streaming
#[derive(Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ArpaTarget)]
pub struct ArpaTargetApi {
    /// Target ID (unique within radar)
    pub id: u64,
    /// Current status: "tracking", "acquiring", or "lost"
    pub status: String,
    /// Target position relative to radar
    pub position: TargetPositionApi,
    /// Target motion (course and speed) - omitted if motion not yet known.
    /// Present with zero values for confirmed stationary targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<TargetMotionApi>,
    /// Collision danger assessment - omitted if vessels diverging
    #[serde(skip_serializing_if = "TargetDangerApi::is_empty")]
    pub danger: TargetDangerApi,
    /// How target was acquired: "auto" or "manual"
    pub acquisition: String,
    /// Which guard zone acquired this target (1 or 2), or 0 for manual
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_zone: Option<u8>,
    /// ISO 8601 timestamp when target was first seen
    pub first_seen: String,
    /// ISO 8601 timestamp when target was last updated
    pub last_seen: String,
}

/// Target position in the API format
#[derive(Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetPositionApi {
    /// Bearing from radar in radians true [0, 2π)
    pub bearing: f64,
    /// Distance from radar in meters (rounded to whole meters)
    pub distance: i32,
    /// Latitude if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// Target motion in the API format
#[derive(Serialize, Clone, Debug, ToSchema)]
pub struct TargetMotionApi {
    /// Course over ground in radians true [0, 2π)
    pub course: f64,
    /// Speed in m/s
    pub speed: f64,
}

/// Collision danger assessment in the API format.
///
/// Emitted whenever the two vessels have non-zero relative velocity, so
/// consumers can show the historical CPA of a vessel that has just passed
/// us (negative TCPA) as well as the upcoming CPA of a converging one.
/// Omitted only when relative motion is undefined (both stationary, or
/// motion data not yet available).
#[derive(Serialize, Clone, Debug, ToSchema)]
pub struct TargetDangerApi {
    /// Closest Point of Approach in meters
    pub cpa: f64,
    /// Time to CPA in seconds. Positive = future (closing); negative =
    /// past (already passed). Magnitude is the time interval to/from
    /// the closest point.
    pub tcpa: f64,
    /// True when the target is a close-quarters situation by IMO defaults:
    /// CPA < 0.5 nm (≈926 m) AND TCPA in [0, 6 min]. Historical CPAs
    /// (negative TCPA) never trigger this flag.
    pub is_dangerous: bool,
}

impl TargetDangerApi {
    /// CPA threshold for the danger flag (0.5 nautical miles in metres).
    pub const DANGER_CPA_M: f64 = 0.5 * super::NAUTICAL_MILE_F64;
    /// TCPA threshold for the danger flag (6 minutes in seconds).
    pub const DANGER_TCPA_S: f64 = 6.0 * 60.0;

    pub fn new(cpa: f64, tcpa: f64) -> Self {
        let is_dangerous =
            cpa < Self::DANGER_CPA_M && (0.0..=Self::DANGER_TCPA_S).contains(&tcpa);
        TargetDangerApi {
            cpa,
            tcpa,
            is_dangerous,
        }
    }

    /// Placeholder for targets whose CPA can't be computed yet — either
    /// the radar has no own-ship position, the target's Kalman filter
    /// hasn't produced a motion estimate, or the relative velocity is
    /// zero. Distinct from `new(0.0, 0.0)` which would otherwise
    /// satisfy the danger thresholds (CPA < 926 m AND 0 ≤ TCPA ≤ 360 s)
    /// and incorrectly flag every motion-less target as dangerous.
    pub fn unknown() -> Self {
        TargetDangerApi {
            cpa: 0.0,
            tcpa: 0.0,
            is_dangerous: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.cpa == 0.0 && self.tcpa == 0.0 && !self.is_dangerous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_danger_api_flags_imminent_close_pass() {
        // 200m CPA in 3 minutes is a close-quarters IMO situation.
        let d = TargetDangerApi::new(200.0, 180.0);
        assert!(d.is_dangerous);
    }

    #[test]
    fn target_danger_api_does_not_flag_distant_cpa() {
        // 2000m at 3 min is well outside the 0.5 nm threshold.
        let d = TargetDangerApi::new(2000.0, 180.0);
        assert!(!d.is_dangerous);
    }

    #[test]
    fn target_danger_api_does_not_flag_far_future() {
        // 200m CPA but 10 min away — outside the 6 min IMO window.
        let d = TargetDangerApi::new(200.0, 600.0);
        assert!(!d.is_dangerous);
    }

    #[test]
    fn target_danger_api_does_not_flag_historical_cpa() {
        // Close CPA but TCPA is in the past — already passed, not a
        // future close-quarters situation.
        let d = TargetDangerApi::new(50.0, -30.0);
        assert!(!d.is_dangerous);
    }

    #[test]
    fn target_danger_api_unknown_is_not_dangerous() {
        // Targets without motion data must NOT be flagged as dangerous.
        // new(0.0, 0.0) would otherwise satisfy both threshold checks
        // (cpa < 926m AND tcpa in [0, 360s]) and incorrectly fire.
        let d = TargetDangerApi::unknown();
        assert!(!d.is_dangerous);
        assert!(d.is_empty());
    }
}
