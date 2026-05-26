//! CPA (Closest Point of Approach) and TCPA (Time to CPA) calculations.
//!
//! This module provides functions to calculate collision avoidance parameters
//! for both ARPA targets and AIS vessels.

use super::GeoPosition;
use super::target::{METERS_PER_DEGREE_LATITUDE, meters_per_degree_longitude};

/// Result of CPA/TCPA calculation.
///
/// `tcpa` is signed: positive when CPA is in the future (vessels closing),
/// negative when CPA was in the past (vessels diverging after a recent
/// passage). Consumers that only care about closing situations should
/// filter on `tcpa >= 0.0`.
#[derive(Debug, Clone, Copy)]
pub struct CpaResult {
    /// Closest Point of Approach in meters
    pub cpa: f64,
    /// Time to CPA in seconds (signed; see struct doc)
    pub tcpa: f64,
}

/// Own vessel state for CPA calculations
#[derive(Debug, Clone, Copy)]
pub struct OwnVessel {
    /// Position
    pub position: GeoPosition,
    /// Speed over ground in m/s
    pub sog: f64,
    /// Course over ground in radians (0 = North, increasing clockwise)
    pub cog: f64,
}

/// Target vessel state for CPA calculations
#[derive(Debug, Clone, Copy)]
pub struct TargetVessel {
    /// Position
    pub position: GeoPosition,
    /// Speed over ground in m/s
    pub sog: f64,
    /// Course over ground in radians (0 = North, increasing clockwise)
    pub cog: f64,
}

/// Calculate CPA and TCPA between own vessel and target.
///
/// Returns `Some(CpaResult)` with a signed `tcpa` (positive = future,
/// negative = past) whenever the two vessels have any relative motion.
/// Returns `None` only when relative velocity is essentially zero — in
/// that case the inter-vessel distance is constant and CPA is undefined.
pub fn calculate_cpa(own: &OwnVessel, target: &TargetVessel) -> Option<CpaResult> {
    let lat_scale = METERS_PER_DEGREE_LATITUDE;
    let lon_scale = meters_per_degree_longitude(&own.position.lat());

    let dx = (target.position.lon() - own.position.lon()) * lon_scale;
    let dy = (target.position.lat() - own.position.lat()) * lat_scale;

    let own_vx = own.sog * own.cog.sin();
    let own_vy = own.sog * own.cog.cos();
    let target_vx = target.sog * target.cog.sin();
    let target_vy = target.sog * target.cog.cos();

    let dvx = target_vx - own_vx;
    let dvy = target_vy - own_vy;

    let dv_squared = dvx * dvx + dvy * dvy;
    if dv_squared < 1e-10 {
        return None;
    }

    let tcpa = -(dx * dvx + dy * dvy) / dv_squared;

    let cpa_dx = dx + dvx * tcpa;
    let cpa_dy = dy + dvy * tcpa;
    let cpa = (cpa_dx * cpa_dx + cpa_dy * cpa_dy).sqrt();

    Some(CpaResult { cpa, tcpa })
}

/// Calculate CPA/TCPA given positions and motion data directly.
///
/// This is a convenience function for cases where the data isn't already
/// in OwnVessel/TargetVessel structs.
///
/// # Arguments
/// * `own_pos` - Own vessel position
/// * `own_sog` - Own vessel speed in m/s
/// * `own_cog` - Own vessel course in radians
/// * `target_pos` - Target position
/// * `target_sog` - Target speed in m/s
/// * `target_cog` - Target course in radians
pub fn calculate_cpa_from_motion(
    own_pos: GeoPosition,
    own_sog: f64,
    own_cog: f64,
    target_pos: GeoPosition,
    target_sog: f64,
    target_cog: f64,
) -> Option<CpaResult> {
    let own = OwnVessel {
        position: own_pos,
        sog: own_sog,
        cog: own_cog,
    };
    let target = TargetVessel {
        position: target_pos,
        sog: target_sog,
        cog: target_cog,
    };
    calculate_cpa(&own, &target)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn approx_eq(a: f64, b: f64, epsilon: f64) -> bool {
        (a - b).abs() < epsilon
    }

    #[test]
    fn test_head_on_collision() {
        // Own vessel at origin, heading North at 10 m/s
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 10.0,
            cog: 0.0, // North
        };

        // Target 1000m North, heading South at 10 m/s
        let target_pos = own.position.position_from_bearing(0.0, 1000.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 10.0,
            cog: PI, // South
        };

        let result = calculate_cpa(&own, &target).expect("Should have CPA");

        // Closing at 20 m/s over 1000m -> TCPA = 50s
        assert!(
            approx_eq(result.tcpa, 50.0, 1.0),
            "TCPA should be ~50s, got {}",
            result.tcpa
        );
        // Head-on collision -> CPA should be ~0
        assert!(
            result.cpa < 10.0,
            "CPA should be near 0, got {}",
            result.cpa
        );
    }

    #[test]
    fn test_crossing_situation() {
        // Own vessel at origin, heading North at 10 m/s
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 10.0,
            cog: 0.0, // North
        };

        // Target 1000m East, heading West at 10 m/s
        let target_pos = own.position.position_from_bearing(PI / 2.0, 1000.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 10.0,
            cog: 3.0 * PI / 2.0, // West (270 degrees)
        };

        let result = calculate_cpa(&own, &target).expect("Should have CPA");

        // Both moving at 10 m/s perpendicular
        // Target will reach our track at t = 100s (1000m / 10 m/s)
        // We will have moved 1000m North by then
        // So they pass at different points - need to calculate actual CPA
        assert!(result.tcpa > 0.0, "TCPA should be positive");
        assert!(
            result.cpa < 1000.0,
            "CPA should be less than initial distance"
        );
    }

    #[test]
    fn test_vessels_diverging_returns_negative_tcpa() {
        // Own vessel heading North
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 10.0,
            cog: 0.0, // North
        };

        // Target 1000m South, also heading South — already past their
        // closest approach. CPA is now historical.
        let target_pos = own.position.position_from_bearing(PI, 1000.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 10.0,
            cog: PI, // South
        };

        let result = calculate_cpa(&own, &target).expect("relative velocity is non-zero");
        // TCPA must be negative — the closest point is in the past.
        assert!(result.tcpa < 0.0, "TCPA should be negative, got {}", result.tcpa);
    }

    #[test]
    fn test_stationary_target_perpendicular_passed() {
        // Own vessel heading North, stationary target due East. We're
        // moving perpendicular to the target, so we're already at our
        // closest point and any further travel only opens the range.
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 10.0,
            cog: 0.0,
        };
        let target_pos = own.position.position_from_bearing(PI / 2.0, 500.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 0.0,
            cog: 0.0,
        };

        let result = calculate_cpa(&own, &target).expect("relative velocity is non-zero");
        // We're moving away from the closest-approach point right now,
        // so TCPA must be non-positive. CPA is the current ~500m distance.
        assert!(result.tcpa <= 0.0, "TCPA should be ≤ 0, got {}", result.tcpa);
        assert!(
            approx_eq(result.cpa, 500.0, 5.0),
            "CPA should be ~500m, got {}",
            result.cpa
        );
    }

    #[test]
    fn test_stationary_target_ahead() {
        // Own vessel heading North
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 10.0,
            cog: 0.0, // North
        };

        // Stationary target 500m ahead (North)
        let target_pos = own.position.position_from_bearing(0.0, 500.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 0.0,
            cog: 0.0,
        };

        let result = calculate_cpa(&own, &target).expect("Should have CPA");

        // We're closing at 10 m/s on a target 500m away
        // TCPA = 500/10 = 50s, CPA = 0
        assert!(
            approx_eq(result.tcpa, 50.0, 1.0),
            "TCPA should be ~50s, got {}",
            result.tcpa
        );
        assert!(
            result.cpa < 10.0,
            "CPA should be near 0, got {}",
            result.cpa
        );
    }

    #[test]
    fn test_both_stationary() {
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 0.0,
            cog: 0.0,
        };

        let target = TargetVessel {
            position: GeoPosition::new(52.001, 4.001),
            sog: 0.0,
            cog: 0.0,
        };

        let result = calculate_cpa(&own, &target);

        // Both stationary, should return None
        assert!(result.is_none(), "Both stationary should return None");
    }

    #[test]
    fn test_overtaking() {
        // Own vessel heading North at 15 m/s
        let own = OwnVessel {
            position: GeoPosition::new(52.0, 4.0),
            sog: 15.0,
            cog: 0.0, // North
        };

        // Target 500m ahead, also heading North but slower (10 m/s)
        let target_pos = own.position.position_from_bearing(0.0, 500.0);
        let target = TargetVessel {
            position: target_pos,
            sog: 10.0,
            cog: 0.0, // North
        };

        let result = calculate_cpa(&own, &target).expect("Should have CPA");

        // Closing at 5 m/s (15 - 10) over 500m -> TCPA = 100s
        assert!(
            approx_eq(result.tcpa, 100.0, 1.0),
            "TCPA should be ~100s, got {}",
            result.tcpa
        );
        // Same course, will collide -> CPA ~0
        assert!(
            result.cpa < 10.0,
            "CPA should be near 0, got {}",
            result.cpa
        );
    }
}
