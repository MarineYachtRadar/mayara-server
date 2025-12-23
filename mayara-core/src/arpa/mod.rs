//! ARPA (Automatic Radar Plotting Aid) Target Tracking
//!
//! This module provides automatic target detection, tracking, and collision
//! avoidance calculations. It is designed to be platform-independent and
//! can be used in both native and WASM environments.
//!
//! # Architecture
//!
//! The ARPA module is split into several submodules:
//!
//! - **polar**: Polar coordinate types and conversions
//! - **doppler**: Doppler state machine for approaching/receding targets
//! - **contour**: Contour detection and representation
//! - **history**: History buffer for storing radar spoke data
//! - **kalman**: Extended Kalman filter for target tracking
//! - **target**: Target state and refresh algorithm
//! - **cpa**: CPA/TCPA calculations
//! - **detector**: Simple target detection for auto-acquisition
//! - **tracker**: High-level processor (simple API)
//! - **types**: Legacy API types (ArpaTarget, ArpaSettings, etc.)
//!
//! # Usage
//!
//! For full-featured ARPA with contour detection and Doppler:
//!
//! ```rust,ignore
//! use mayara_core::arpa::{
//!     HistoryBuffer, TargetState, RefreshConfig, refresh_target, Pass,
//!     Legend, ExtendedPosition, TargetStatus,
//! };
//!
//! // Create history buffer
//! let mut history = HistoryBuffer::new(2048);
//!
//! // Update spoke data
//! history.update_spoke(angle, &data, timestamp, lat, lon, &Legend::default());
//!
//! // Create target
//! let pos = ExtendedPosition::new(lat, lon, 0.0, 0.0, timestamp, 0.0, 0.0);
//! let mut target = TargetState::new(1, pos, own_lat, own_lon, 2048, TargetStatus::Acquire0, false);
//!
//! // Refresh target
//! let config = RefreshConfig { ... };
//! refresh_target(&mut target, &mut history, own_lat, own_lon, &config, search_radius, Pass::First);
//! ```
//!
//! For simple detection-based ARPA (SignalK API style):
//!
//! ```rust,ignore
//! use mayara_core::arpa::{ArpaProcessor, ArpaSettings, OwnShip};
//!
//! let settings = ArpaSettings::default();
//! let mut processor = ArpaProcessor::new(settings);
//! processor.update_own_ship(OwnShip { ... });
//! let events = processor.process_spoke(&spoke_data, bearing, timestamp);
//! ```

// New modular ARPA implementation
mod contour;
mod doppler;
mod history;
mod kalman;
mod polar;
mod target;

// Legacy/simple implementation
mod cpa;
mod detector;
mod tracker;
mod types;

// Re-export new modular types
pub use contour::{Contour, ContourError, MAX_CONTOUR_LENGTH, MIN_CONTOUR_LENGTH};
pub use doppler::DopplerState;
pub use history::{HistoryBuffer, HistoryPixel, HistorySpoke, Legend};
pub use kalman::KalmanFilter;
pub use polar::{
    meters_per_degree_longitude, LocalPosition, Polar, PolarConverter, FOUR_DIRECTIONS, KN_TO_MS,
    METERS_PER_DEGREE_LATITUDE, MS_TO_KN, NAUTICAL_MILE,
};
pub use target::{
    refresh_target, ExtendedPosition, Pass, RefreshConfig, RefreshState, TargetState, TargetStatus,
    MAX_DETECTION_SPEED_KN, MAX_LOST_COUNT,
};

// Re-export legacy types (for backward compatibility)
pub use cpa::CpaResult;
pub use detector::TargetDetector;
pub use tracker::ArpaProcessor;
pub use types::*;
