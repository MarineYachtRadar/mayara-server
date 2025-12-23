//! CPA/TCPA Calculation
//!
//! Computes Closest Point of Approach (CPA) and Time to CPA (TCPA)
//! for collision avoidance.

use super::types::{OwnShip, TargetDanger, TrackingState};

/// Result of CPA/TCPA calculation
#[derive(Debug, Clone, Copy)]
pub struct CpaResult {
    /// Closest Point of Approach in meters
    pub cpa: f64,
    /// Time to Closest Point of Approach in seconds
    /// Positive = future, Negative = past
    pub tcpa: f64,
}

/// Calculate CPA and TCPA between own ship and target
///
/// Uses relative velocity method:
/// 1. Compute relative position (target - own ship) at origin
/// 2. Compute relative velocity
/// 3. Find time when distance is minimized
///
/// # Arguments
///
/// * `target` - Target tracking state with position and velocity
/// * `own_ship` - Own ship state with course and speed
///
/// # Returns
///
/// CpaResult with CPA in meters and TCPA in seconds
pub(crate) fn calculate_cpa_tcpa(target: &TrackingState, own_ship: &OwnShip) -> CpaResult {
    // Convert own ship velocity to Cartesian (m/s)
    let own_speed_ms = own_ship.speed / 1.94384; // knots to m/s
    let own_course_rad = own_ship.course.to_radians();
    let own_vx = own_speed_ms * own_course_rad.sin();
    let own_vy = own_speed_ms * own_course_rad.cos();

    // Relative position (target relative to own ship at origin)
    let rx = target.x;
    let ry = target.y;

    // Relative velocity (target velocity - own ship velocity)
    let rvx = target.vx - own_vx;
    let rvy = target.vy - own_vy;

    // Calculate TCPA using dot product method
    // TCPA = -(r · v) / |v|²
    let rv_dot = rx * rvx + ry * rvy;
    let v_sq = rvx * rvx + rvy * rvy;

    // Handle case where relative velocity is near zero
    if v_sq < 1e-6 {
        // Target moving same direction and speed as own ship
        // CPA is current distance, TCPA is undefined (set to 0)
        let cpa = (rx * rx + ry * ry).sqrt();
        return CpaResult { cpa, tcpa: 0.0 };
    }

    let tcpa = -rv_dot / v_sq;

    // Calculate CPA
    // Position at TCPA: r + v * tcpa
    let cpa_x = rx + rvx * tcpa;
    let cpa_y = ry + rvy * tcpa;
    let cpa = (cpa_x * cpa_x + cpa_y * cpa_y).sqrt();

    CpaResult { cpa, tcpa }
}

/// Calculate CPA/TCPA and return as TargetDanger struct
pub(crate) fn calculate_danger(target: &TrackingState, own_ship: &OwnShip) -> TargetDanger {
    let result = calculate_cpa_tcpa(target, own_ship);
    TargetDanger {
        cpa: result.cpa,
        tcpa: result.tcpa,
    }
}

/// Calculate CPA/TCPA without own ship motion (stationary reference)
pub(crate) fn calculate_cpa_tcpa_stationary(target: &TrackingState) -> CpaResult {
    let rx = target.x;
    let ry = target.y;
    let rvx = target.vx;
    let rvy = target.vy;

    let rv_dot = rx * rvx + ry * rvy;
    let v_sq = rvx * rvx + rvy * rvy;

    if v_sq < 1e-6 {
        let cpa = (rx * rx + ry * ry).sqrt();
        return CpaResult { cpa, tcpa: 0.0 };
    }

    let tcpa = -rv_dot / v_sq;
    let cpa_x = rx + rvx * tcpa;
    let cpa_y = ry + rvy * tcpa;
    let cpa = (cpa_x * cpa_x + cpa_y * cpa_y).sqrt();

    CpaResult { cpa, tcpa }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arpa::types::AcquisitionMethod;

    #[test]
    fn test_head_on_collision() {
        // Target directly ahead, coming toward us
        let mut target = TrackingState::new(1, 0.0, 1000.0, 0, AcquisitionMethod::Auto);
        target.vx = 0.0;
        target.vy = -5.0; // 5 m/s toward us

        let own_ship = OwnShip {
            latitude: 0.0,
            longitude: 0.0,
            heading: 0.0,
            course: 0.0,
            speed: 10.0 * 1.94384, // 10 m/s = ~19.4 knots
        };

        let result = calculate_cpa_tcpa(&target, &own_ship);

        // Relative velocity: 0 - 0 in x, -5 - 10 = -15 m/s in y
        // Target at (0, 1000), velocity (0, -15)
        // TCPA = -(0*0 + 1000*(-15)) / (0 + 225) = 15000/225 = 66.67s
        assert!((result.tcpa - 66.67).abs() < 1.0);
        // CPA should be very small (head-on)
        assert!(result.cpa < 1.0);
    }

    #[test]
    fn test_parallel_course() {
        // Target to starboard, same course and speed
        let mut target = TrackingState::new(1, 90.0, 500.0, 0, AcquisitionMethod::Auto);
        target.vx = 0.0;
        target.vy = 5.0; // 5 m/s same direction

        let own_ship = OwnShip {
            latitude: 0.0,
            longitude: 0.0,
            heading: 0.0,
            course: 0.0,
            speed: 5.0 * 1.94384, // Same 5 m/s
        };

        let result = calculate_cpa_tcpa(&target, &own_ship);

        // Relative velocity is zero, CPA = current distance
        assert!((result.cpa - 500.0).abs() < 1.0);
    }

    #[test]
    fn test_crossing_situation() {
        // Target crossing from port to starboard
        let mut target = TrackingState::new(1, 315.0, 1000.0, 0, AcquisitionMethod::Auto);
        // Position: (-707, 707) meters (NW of us)
        // Velocity: 5 m/s toward east (crossing our bow)
        target.vx = 5.0;
        target.vy = 0.0;

        let own_ship = OwnShip {
            latitude: 0.0,
            longitude: 0.0,
            heading: 0.0,
            course: 0.0,
            speed: 10.0 * 1.94384, // 10 m/s north
        };

        let result = calculate_cpa_tcpa(&target, &own_ship);

        // Should have positive TCPA and CPA depends on geometry
        assert!(result.tcpa > 0.0);
        // Target will cross ahead of us
        assert!(result.cpa < 1000.0);
    }

    #[test]
    fn test_receding_target() {
        // Target ahead, moving away
        let mut target = TrackingState::new(1, 0.0, 1000.0, 0, AcquisitionMethod::Auto);
        target.vx = 0.0;
        target.vy = 15.0; // 15 m/s away from us

        let own_ship = OwnShip {
            latitude: 0.0,
            longitude: 0.0,
            heading: 0.0,
            course: 0.0,
            speed: 5.0 * 1.94384, // Only 5 m/s, slower than target
        };

        let result = calculate_cpa_tcpa(&target, &own_ship);

        // Target moving away faster, TCPA was in the past (negative)
        // Actually with relative velocity positive away, CPA was at t=0
        // and distance is increasing, so TCPA should be 0 or negative
        assert!(result.tcpa <= 0.0 || result.cpa >= 1000.0);
    }
}
