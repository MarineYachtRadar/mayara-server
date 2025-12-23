//! ARPA Type Definitions
//!
//! Core types for ARPA target tracking that match the SignalK Radar API v6.

use serde::{Deserialize, Serialize};

/// Target acquisition method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcquisitionMethod {
    /// Manually acquired by user
    Manual,
    /// Automatically acquired by detection algorithm
    Auto,
}

impl Default for AcquisitionMethod {
    fn default() -> Self {
        AcquisitionMethod::Auto
    }
}

/// Target tracking status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetStatus {
    /// Target is being acquired (initial tracking)
    Acquiring,
    /// Target is being actively tracked
    Tracking,
    /// Target has been lost (no radar returns)
    Lost,
}

impl Default for TargetStatus {
    fn default() -> Self {
        TargetStatus::Acquiring
    }
}

/// Target position in polar coordinates (relative to radar)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetPosition {
    /// Bearing from own ship in degrees (0-360, true north)
    pub bearing: f64,
    /// Distance from own ship in meters
    pub distance: f64,
    /// Latitude (if own ship position is known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude (if own ship position is known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// Target motion vector
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetMotion {
    /// True course over ground in degrees (0-360)
    pub course: f64,
    /// Speed over ground in knots
    pub speed: f64,
}

/// Danger assessment (CPA/TCPA)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDanger {
    /// Closest point of approach in meters
    pub cpa: f64,
    /// Time to closest point of approach in seconds (negative = past)
    pub tcpa: f64,
}

/// Complete ARPA target information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArpaTarget {
    /// Unique target identifier (1-99 typically)
    pub id: u32,
    /// Current tracking status
    pub status: TargetStatus,
    /// Current position
    pub position: TargetPosition,
    /// Computed motion vector
    pub motion: TargetMotion,
    /// Danger assessment
    pub danger: TargetDanger,
    /// How the target was acquired
    pub acquisition: AcquisitionMethod,
    /// Unix timestamp (ms) when target was first detected
    pub first_seen: u64,
    /// Unix timestamp (ms) of last radar return
    pub last_seen: u64,
}

impl ArpaTarget {
    /// Create a new target with initial position
    pub fn new(
        id: u32,
        bearing: f64,
        distance: f64,
        timestamp: u64,
        method: AcquisitionMethod,
    ) -> Self {
        ArpaTarget {
            id,
            status: TargetStatus::Acquiring,
            position: TargetPosition {
                bearing,
                distance,
                latitude: None,
                longitude: None,
            },
            motion: TargetMotion::default(),
            danger: TargetDanger::default(),
            acquisition: method,
            first_seen: timestamp,
            last_seen: timestamp,
        }
    }

    /// Check if target is dangerous based on CPA/TCPA thresholds
    pub fn is_dangerous(&self, cpa_threshold: f64, tcpa_threshold: f64) -> bool {
        self.danger.cpa < cpa_threshold
            && self.danger.tcpa > 0.0
            && self.danger.tcpa < tcpa_threshold
    }

    /// Get alert state based on CPA/TCPA
    pub fn alert_state(&self, settings: &ArpaSettings) -> AlertState {
        if self.status == TargetStatus::Lost {
            return AlertState::Normal;
        }

        // TCPA must be positive (approaching) and within threshold
        if self.danger.tcpa <= 0.0 || self.danger.tcpa > settings.tcpa_threshold {
            return AlertState::Normal;
        }

        // Determine alert level based on CPA
        let cpa = self.danger.cpa;
        if cpa < settings.cpa_threshold * 0.25 {
            AlertState::Emergency
        } else if cpa < settings.cpa_threshold * 0.5 {
            AlertState::Alarm
        } else if cpa < settings.cpa_threshold * 0.75 {
            AlertState::Warn
        } else if cpa < settings.cpa_threshold {
            AlertState::Alert
        } else {
            AlertState::Normal
        }
    }
}

/// SignalK notification alert states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertState {
    Normal,
    Alert,
    Warn,
    Alarm,
    Emergency,
}

impl AlertState {
    /// Convert to SignalK notification state string
    pub fn as_signalk_state(&self) -> &'static str {
        match self {
            AlertState::Normal => "normal",
            AlertState::Alert => "alert",
            AlertState::Warn => "warn",
            AlertState::Alarm => "alarm",
            AlertState::Emergency => "emergency",
        }
    }
}

/// ARPA processor settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArpaSettings {
    /// Whether ARPA processing is enabled
    pub enabled: bool,
    /// Maximum number of targets to track
    pub max_targets: u32,
    /// CPA threshold in meters for collision warnings
    pub cpa_threshold: f64,
    /// TCPA threshold in seconds for collision warnings
    pub tcpa_threshold: f64,
    /// Time in seconds before a target without returns is marked lost
    pub lost_target_timeout: f64,
    /// Enable automatic target acquisition
    pub auto_acquisition: bool,
    /// Minimum target size (radar pixels) for auto-acquisition
    pub min_target_size: u32,
    /// Detection threshold (0-255 for pixel intensity)
    pub detection_threshold: u8,
    /// Minimum speed (knots) for auto-acquisition
    pub min_speed: f64,
}

impl Default for ArpaSettings {
    fn default() -> Self {
        ArpaSettings {
            enabled: true,
            max_targets: 40,
            cpa_threshold: 500.0,      // 500 meters
            tcpa_threshold: 600.0,     // 10 minutes
            lost_target_timeout: 30.0, // 30 seconds
            auto_acquisition: false,
            min_target_size: 3,
            detection_threshold: 128,
            min_speed: 2.0, // 2 knots minimum
        }
    }
}

/// Own ship state (required for CPA/TCPA calculations)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnShip {
    /// Latitude in degrees
    pub latitude: f64,
    /// Longitude in degrees
    pub longitude: f64,
    /// True heading in degrees (0-360)
    pub heading: f64,
    /// Course over ground in degrees (0-360)
    pub course: f64,
    /// Speed over ground in knots
    pub speed: f64,
}

/// Events emitted by the ARPA processor
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArpaEvent {
    /// New target acquired
    TargetAcquired { target: ArpaTarget },
    /// Target state updated
    TargetUpdate { target: ArpaTarget },
    /// Target lost
    TargetLost {
        target_id: u32,
        last_position: TargetPosition,
    },
    /// Collision warning state changed
    CollisionWarning {
        target_id: u32,
        state: AlertState,
        cpa: f64,
        tcpa: f64,
    },
}

/// Internal tracking state for Kalman filter
#[derive(Debug, Clone)]
pub(crate) struct TrackingState {
    /// Target ID
    pub id: u32,
    /// Position in Cartesian coordinates (meters from own ship)
    pub x: f64,
    pub y: f64,
    /// Velocity in Cartesian coordinates (m/s)
    pub vx: f64,
    pub vy: f64,
    /// State covariance matrix (4x4, flattened)
    pub covariance: [f64; 16],
    /// How the target was acquired
    pub acquisition: AcquisitionMethod,
    /// Unix timestamp (ms) when first seen
    pub first_seen: u64,
    /// Unix timestamp (ms) of last update
    pub last_seen: u64,
    /// Number of updates (for status transition)
    pub update_count: u32,
    /// Previous alert state (for change detection)
    pub prev_alert_state: AlertState,
}

impl TrackingState {
    /// Create new tracking state from polar position
    pub fn new(
        id: u32,
        bearing_deg: f64,
        distance_m: f64,
        timestamp: u64,
        method: AcquisitionMethod,
    ) -> Self {
        let bearing_rad = bearing_deg.to_radians();
        let x = distance_m * bearing_rad.sin();
        let y = distance_m * bearing_rad.cos();

        // Initial covariance - high uncertainty in position, very high in velocity
        let pos_var = 100.0; // 10m std dev
        let vel_var = 25.0; // 5 m/s std dev (~10 knots)

        TrackingState {
            id,
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            covariance: [
                pos_var, 0.0, 0.0, 0.0, 0.0, pos_var, 0.0, 0.0, 0.0, 0.0, vel_var, 0.0, 0.0, 0.0,
                0.0, vel_var,
            ],
            acquisition: method,
            first_seen: timestamp,
            last_seen: timestamp,
            update_count: 0,
            prev_alert_state: AlertState::Normal,
        }
    }

    /// Get distance from own ship in meters
    pub fn distance(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// Get bearing from own ship in degrees (0-360)
    pub fn bearing(&self) -> f64 {
        let mut bearing = self.x.atan2(self.y).to_degrees();
        if bearing < 0.0 {
            bearing += 360.0;
        }
        bearing
    }

    /// Get speed in knots
    pub fn speed_knots(&self) -> f64 {
        let speed_ms = (self.vx * self.vx + self.vy * self.vy).sqrt();
        speed_ms * 1.94384 // m/s to knots
    }

    /// Get course in degrees (0-360)
    pub fn course(&self) -> f64 {
        let mut course = self.vx.atan2(self.vy).to_degrees();
        if course < 0.0 {
            course += 360.0;
        }
        course
    }

    /// Convert to ArpaTarget for API output
    pub fn to_arpa_target(
        &self,
        status: TargetStatus,
        danger: TargetDanger,
        own_ship: Option<&OwnShip>,
    ) -> ArpaTarget {
        let (lat, lon) = own_ship
            .map(|os| {
                // Convert offset to lat/lon using simple approximation
                // This is good enough for short ranges (< 50km)
                let lat_offset = self.y / 111_320.0; // meters to degrees latitude
                let lon_offset = self.x / (111_320.0 * os.latitude.to_radians().cos());
                (os.latitude + lat_offset, os.longitude + lon_offset)
            })
            .unzip();

        ArpaTarget {
            id: self.id,
            status,
            position: TargetPosition {
                bearing: self.bearing(),
                distance: self.distance(),
                latitude: lat,
                longitude: lon,
            },
            motion: TargetMotion {
                course: self.course(),
                speed: self.speed_knots(),
            },
            danger,
            acquisition: self.acquisition,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
        }
    }
}
