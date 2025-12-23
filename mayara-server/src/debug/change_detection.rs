//! Change detection for correlating commands with state changes.
//!
//! This module tracks pending commands and correlates them with observed
//! state changes to help developers understand cause and effect.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use super::hub::DebugHub;
use super::EventSource;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for change detection.
#[derive(Debug, Clone)]
pub struct ChangeDetectionConfig {
    /// Maximum age for pending commands in milliseconds.
    pub max_pending_age_ms: u64,

    /// Maximum number of pending commands to track per radar.
    pub max_pending_commands: usize,
}

impl Default for ChangeDetectionConfig {
    fn default() -> Self {
        Self {
            max_pending_age_ms: 5000,
            max_pending_commands: 100,
        }
    }
}

// =============================================================================
// PendingCommand
// =============================================================================

/// A command that was sent and might trigger a state change.
#[derive(Debug, Clone)]
pub struct PendingCommand {
    /// Event ID of the command.
    pub event_id: u64,

    /// Timestamp when command was sent.
    pub timestamp: u64,

    /// Control ID that the command might affect.
    pub control_id: String,

    /// Expected value (if known).
    pub expected_value: Option<serde_json::Value>,
}

// =============================================================================
// ChangeDetector
// =============================================================================

/// Tracks state changes by comparing control values.
pub struct ChangeDetector {
    config: ChangeDetectionConfig,

    /// Hub for submitting state change events.
    hub: Arc<DebugHub>,

    /// Pending commands per radar.
    pending: RwLock<HashMap<String, VecDeque<PendingCommand>>>,
}

impl ChangeDetector {
    /// Create a new change detector.
    pub fn new(hub: Arc<DebugHub>) -> Self {
        Self::with_config(hub, ChangeDetectionConfig::default())
    }

    /// Create a new change detector with custom configuration.
    pub fn with_config(hub: Arc<DebugHub>, config: ChangeDetectionConfig) -> Self {
        Self {
            config,
            hub,
            pending: RwLock::new(HashMap::new()),
        }
    }

    /// Record a command that was sent.
    ///
    /// Call this when a control command is sent so we can correlate
    /// it with subsequent state changes.
    pub fn on_command_sent(
        &self,
        radar_id: &str,
        event_id: u64,
        timestamp: u64,
        control_id: &str,
        expected_value: Option<serde_json::Value>,
    ) {
        let mut pending = self.pending.write().unwrap();
        let commands = pending.entry(radar_id.to_string()).or_default();

        // Evict old commands
        self.cleanup_old_commands(commands, timestamp);

        // Add new command
        commands.push_back(PendingCommand {
            event_id,
            timestamp,
            control_id: control_id.to_string(),
            expected_value,
        });

        // Limit queue size
        while commands.len() > self.config.max_pending_commands {
            commands.pop_front();
        }
    }

    /// Called when a state change is observed.
    ///
    /// Returns the event ID of the command that likely triggered this change.
    pub fn on_state_change(
        &self,
        radar_id: &str,
        brand: &str,
        control_id: &str,
        before: serde_json::Value,
        after: serde_json::Value,
    ) -> Option<u64> {
        let trigger_id = {
            let mut pending = self.pending.write().unwrap();
            let commands = pending.get_mut(radar_id)?;

            // Find the most recent command for this control
            let trigger = commands
                .iter()
                .rev()
                .find(|c| c.control_id == control_id)
                .map(|c| c.event_id);

            // Remove matched command
            commands.retain(|c| c.control_id != control_id);

            trigger
        };

        // Submit state change event
        let event = self
            .hub
            .event_builder(radar_id, brand)
            .source(EventSource::IoProvider)
            .state_change(control_id, before, after, trigger_id);
        self.hub.submit(event);

        trigger_id
    }

    /// Clean up old pending commands.
    fn cleanup_old_commands(&self, commands: &mut VecDeque<PendingCommand>, current_time: u64) {
        commands
            .retain(|c| current_time.saturating_sub(c.timestamp) < self.config.max_pending_age_ms);
    }

    /// Get pending commands for a radar (for debugging).
    pub fn get_pending_commands(&self, radar_id: &str) -> Vec<PendingCommand> {
        let pending = self.pending.read().unwrap();
        pending
            .get(radar_id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Clear all pending commands.
    pub fn clear(&self) {
        let mut pending = self.pending.write().unwrap();
        pending.clear();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_correlation() {
        let hub = Arc::new(DebugHub::new());
        let detector = ChangeDetector::new(hub);

        // Send a command
        detector.on_command_sent("radar-1", 1, 1000, "gain", Some(serde_json::json!(50)));

        // State changes
        let trigger = detector.on_state_change(
            "radar-1",
            "furuno",
            "gain",
            serde_json::json!(25),
            serde_json::json!(50),
        );

        assert_eq!(trigger, Some(1));
    }

    #[test]
    fn test_no_correlation_for_different_control() {
        let hub = Arc::new(DebugHub::new());
        let detector = ChangeDetector::new(hub);

        // Send a gain command
        detector.on_command_sent("radar-1", 1, 1000, "gain", Some(serde_json::json!(50)));

        // Sea clutter changes (not correlated)
        let trigger = detector.on_state_change(
            "radar-1",
            "furuno",
            "sea",
            serde_json::json!(0),
            serde_json::json!(25),
        );

        assert_eq!(trigger, None);
    }

    #[test]
    fn test_pending_cleanup() {
        let config = ChangeDetectionConfig {
            max_pending_age_ms: 100,
            max_pending_commands: 10,
        };
        let hub = Arc::new(DebugHub::new());
        let detector = ChangeDetector::with_config(hub, config);

        // Send old command
        detector.on_command_sent("radar-1", 1, 0, "gain", None);

        // Send new command (should evict old one due to time)
        detector.on_command_sent("radar-1", 2, 200, "sea", None);

        let pending = detector.get_pending_commands("radar-1");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].control_id, "sea");
    }
}
