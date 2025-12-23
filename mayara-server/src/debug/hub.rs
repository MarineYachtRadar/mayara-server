//! DebugHub - Central aggregator for debug events.
//!
//! The DebugHub collects events from all DebugIoProviders and broadcasts them
//! to connected WebSocket clients. It maintains a ring buffer of recent events
//! for historical queries.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::broadcast;

use super::recording::DebugRecorder;
use super::{DebugEvent, DebugEventPayload, EventSource, RadarStateSnapshot};

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the debug hub.
#[derive(Debug, Clone)]
pub struct DebugHubConfig {
    /// Maximum events to keep in ring buffer.
    pub max_events: usize,

    /// Enable change detection.
    pub change_detection: bool,

    /// Broadcast channel capacity.
    pub broadcast_capacity: usize,

    /// Directory for debug recordings.
    pub recordings_dir: PathBuf,
}

impl Default for DebugHubConfig {
    fn default() -> Self {
        // Default recordings directory is in the user's data directory
        let recordings_dir = directories::ProjectDirs::from("com", "marineyachtradar", "mayara")
            .map(|dirs| dirs.data_dir().join("debug-recordings"))
            .unwrap_or_else(|| PathBuf::from("./debug-recordings"));

        Self {
            max_events: 10_000,
            change_detection: true,
            broadcast_capacity: 1024,
            recordings_dir,
        }
    }
}

// =============================================================================
// DebugHub
// =============================================================================

/// Central hub that collects debug events from all radars.
///
/// The hub is thread-safe and can be shared across async tasks.
pub struct DebugHub {
    config: DebugHubConfig,

    /// Ring buffer of events.
    events: RwLock<VecDeque<DebugEvent>>,

    /// Broadcast channel for real-time streaming.
    event_tx: broadcast::Sender<DebugEvent>,

    /// Last state snapshot per radar for change detection.
    last_snapshots: RwLock<HashMap<String, RadarStateSnapshot>>,

    /// Next event ID (monotonically increasing).
    next_event_id: AtomicU64,

    /// Session start time for relative timestamps.
    start_time: Instant,

    /// Debug recorder for session recording.
    recorder: Arc<DebugRecorder>,
}

impl DebugHub {
    /// Create a new DebugHub with default configuration.
    pub fn new() -> Self {
        Self::with_config(DebugHubConfig::default())
    }

    /// Create a new DebugHub with custom configuration.
    pub fn with_config(config: DebugHubConfig) -> Self {
        let (event_tx, _) = broadcast::channel(config.broadcast_capacity);
        let recorder = Arc::new(DebugRecorder::new(config.recordings_dir.clone()));
        Self {
            events: RwLock::new(VecDeque::with_capacity(config.max_events)),
            event_tx,
            last_snapshots: RwLock::new(HashMap::new()),
            next_event_id: AtomicU64::new(0),
            start_time: Instant::now(),
            recorder,
            config,
        }
    }

    /// Get the debug recorder.
    pub fn recorder(&self) -> &Arc<DebugRecorder> {
        &self.recorder
    }

    /// Get the current number of events in the buffer.
    pub fn event_count(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Get the current timestamp in milliseconds since session start.
    pub fn timestamp(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Get the next event ID.
    pub fn next_id(&self) -> u64 {
        self.next_event_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Submit an event to the hub.
    ///
    /// The event's `id` and `timestamp` will be set automatically.
    pub fn submit(&self, mut event: DebugEvent) {
        // Set ID and timestamp
        event.id = self.next_id();
        event.timestamp = self.timestamp();

        // Broadcast to real-time subscribers (ignore if no subscribers)
        let subscribers = self.event_tx.receiver_count();
        let result = self.event_tx.send(event.clone());
        log::debug!(
            "[DebugHub] Event #{} (type: {}) broadcast to {} subscribers: {:?}",
            event.id,
            match &event.payload {
                DebugEventPayload::Data { direction, .. } => format!("data {:?}", direction),
                DebugEventPayload::SocketOp { operation, .. } => format!("socket {:?}", operation),
                DebugEventPayload::StateChange { control_id, .. } =>
                    format!("state {}", control_id),
            },
            subscribers,
            result.is_ok()
        );

        // Store in ring buffer
        let mut events = self.events.write().unwrap();
        if events.len() >= self.config.max_events {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Create a new event builder for convenience.
    pub fn event_builder(&self, radar_id: &str, brand: &str) -> DebugEventBuilder {
        DebugEventBuilder {
            radar_id: radar_id.to_string(),
            brand: brand.to_string(),
            source: EventSource::IoProvider,
        }
    }

    /// Subscribe to real-time events.
    ///
    /// Returns a broadcast receiver that will receive all new events.
    pub fn subscribe(&self) -> broadcast::Receiver<DebugEvent> {
        self.event_tx.subscribe()
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }

    /// Get events from history with optional filtering.
    ///
    /// # Arguments
    /// * `radar_id` - Optional filter by radar ID
    /// * `limit` - Maximum number of events to return
    /// * `after` - Optional, only return events with ID > this value
    pub fn get_events(
        &self,
        radar_id: Option<&str>,
        limit: usize,
        after: Option<u64>,
    ) -> Vec<DebugEvent> {
        let events = self.events.read().unwrap();
        events
            .iter()
            .filter(|e| {
                // Filter by radar_id if specified
                if let Some(rid) = radar_id {
                    if e.radar_id != rid {
                        return false;
                    }
                }
                // Filter by ID if 'after' is specified
                if let Some(after_id) = after {
                    if e.id <= after_id {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get events from history by ID.
    ///
    /// Returns events with ID >= `from_id`, up to `limit` events.
    pub fn get_events_from_id(&self, from_id: u64, limit: usize) -> Vec<DebugEvent> {
        let events = self.events.read().unwrap();
        events
            .iter()
            .filter(|e| e.id >= from_id)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get recent events.
    ///
    /// Returns the last `count` events.
    pub fn get_recent_events(&self, count: usize) -> Vec<DebugEvent> {
        let events = self.events.read().unwrap();
        events
            .iter()
            .rev()
            .take(count)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Get all events (up to buffer limit).
    pub fn get_all_events(&self) -> Vec<DebugEvent> {
        let events = self.events.read().unwrap();
        events.iter().cloned().collect()
    }

    /// Get total event count (including those that have been evicted).
    pub fn total_event_count(&self) -> u64 {
        self.next_event_id.load(Ordering::SeqCst)
    }

    /// Get current buffer size.
    pub fn buffer_size(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Clear all events from the buffer.
    pub fn clear(&self) {
        let mut events = self.events.write().unwrap();
        events.clear();
    }

    /// Update radar state snapshot for change detection.
    ///
    /// Returns a list of state changes detected.
    pub fn update_snapshot(&self, snapshot: RadarStateSnapshot) -> Vec<StateChange> {
        if !self.config.change_detection {
            let mut snapshots = self.last_snapshots.write().unwrap();
            snapshots.insert(snapshot.radar_id.clone(), snapshot);
            return Vec::new();
        }

        let mut changes = Vec::new();
        let mut snapshots = self.last_snapshots.write().unwrap();

        if let Some(old) = snapshots.get(&snapshot.radar_id) {
            // Compare controls
            for (key, new_val) in &snapshot.controls {
                if let Some(old_val) = old.controls.get(key) {
                    if old_val != new_val {
                        changes.push(StateChange {
                            radar_id: snapshot.radar_id.clone(),
                            control_id: key.clone(),
                            before: old_val.clone(),
                            after: new_val.clone(),
                        });
                    }
                } else {
                    // New control appeared
                    changes.push(StateChange {
                        radar_id: snapshot.radar_id.clone(),
                        control_id: key.clone(),
                        before: serde_json::Value::Null,
                        after: new_val.clone(),
                    });
                }
            }
        }

        snapshots.insert(snapshot.radar_id.clone(), snapshot);
        changes
    }

    /// Get the last snapshot for a radar.
    pub fn get_snapshot(&self, radar_id: &str) -> Option<RadarStateSnapshot> {
        let snapshots = self.last_snapshots.read().unwrap();
        snapshots.get(radar_id).cloned()
    }

    /// Get all radar snapshots.
    pub fn get_all_snapshots(&self) -> Vec<RadarStateSnapshot> {
        let snapshots = self.last_snapshots.read().unwrap();
        snapshots.values().cloned().collect()
    }
}

impl Default for DebugHub {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// State Change
// =============================================================================

/// A detected state change.
#[derive(Debug, Clone)]
pub struct StateChange {
    pub radar_id: String,
    pub control_id: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

// =============================================================================
// Event Builder
// =============================================================================

/// Builder for creating DebugEvents.
pub struct DebugEventBuilder {
    pub radar_id: String,
    pub brand: String,
    pub source: EventSource,
}

impl DebugEventBuilder {
    /// Set the event source.
    pub fn source(mut self, source: EventSource) -> Self {
        self.source = source;
        self
    }

    /// Build a data event (will have id/timestamp set when submitted).
    pub fn data(
        self,
        direction: super::IoDirection,
        protocol: super::ProtocolType,
        remote_addr: &str,
        remote_port: u16,
        data: &[u8],
        decoded: Option<super::DecodedMessage>,
    ) -> DebugEvent {
        DebugEvent {
            id: 0, // Will be set by hub.submit()
            timestamp: 0,
            radar_id: self.radar_id,
            brand: self.brand,
            source: self.source,
            payload: DebugEventPayload::Data {
                direction,
                protocol,
                local_addr: None,
                remote_addr: remote_addr.to_string(),
                remote_port,
                raw_hex: super::hex_encode(data),
                raw_ascii: super::ascii_encode(data),
                decoded,
                length: data.len(),
            },
        }
    }

    /// Build a socket operation event.
    pub fn socket_op(
        self,
        operation: super::SocketOperation,
        success: bool,
        error: Option<String>,
    ) -> DebugEvent {
        DebugEvent {
            id: 0,
            timestamp: 0,
            radar_id: self.radar_id,
            brand: self.brand,
            source: self.source,
            payload: DebugEventPayload::SocketOp {
                operation,
                success,
                error,
            },
        }
    }

    /// Build a state change event.
    pub fn state_change(
        self,
        control_id: &str,
        before: serde_json::Value,
        after: serde_json::Value,
        trigger_event_id: Option<u64>,
    ) -> DebugEvent {
        DebugEvent {
            id: 0,
            timestamp: 0,
            radar_id: self.radar_id,
            brand: self.brand,
            source: self.source,
            payload: DebugEventPayload::StateChange {
                control_id: control_id.to_string(),
                before,
                after,
                trigger_event_id,
            },
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::{IoDirection, ProtocolType};

    #[test]
    fn test_hub_creation() {
        let hub = DebugHub::new();
        assert_eq!(hub.buffer_size(), 0);
        assert_eq!(hub.total_event_count(), 0);
    }

    #[test]
    fn test_event_submission() {
        let hub = DebugHub::new();

        let event = hub.event_builder("radar-1", "furuno").data(
            IoDirection::Send,
            ProtocolType::Tcp,
            "172.31.1.4",
            10050,
            b"$S69,50\r\n",
            None,
        );

        hub.submit(event);

        assert_eq!(hub.buffer_size(), 1);
        assert_eq!(hub.total_event_count(), 1);

        let events = hub.get_all_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, 0);
        assert_eq!(events[0].radar_id, "radar-1");
    }

    #[test]
    fn test_ring_buffer_eviction() {
        let config = DebugHubConfig {
            max_events: 3,
            ..Default::default()
        };
        let hub = DebugHub::with_config(config);

        for i in 0..5 {
            let event = hub.event_builder("radar-1", "furuno").data(
                IoDirection::Send,
                ProtocolType::Tcp,
                "172.31.1.4",
                10050,
                format!("event{}", i).as_bytes(),
                None,
            );
            hub.submit(event);
        }

        assert_eq!(hub.buffer_size(), 3);
        assert_eq!(hub.total_event_count(), 5);

        let events = hub.get_all_events();
        assert_eq!(events[0].id, 2); // First two were evicted
        assert_eq!(events[1].id, 3);
        assert_eq!(events[2].id, 4);
    }

    #[test]
    fn test_get_events_from_id() {
        let hub = DebugHub::new();

        for _ in 0..5 {
            let event = hub.event_builder("radar-1", "furuno").data(
                IoDirection::Send,
                ProtocolType::Tcp,
                "172.31.1.4",
                10050,
                b"test",
                None,
            );
            hub.submit(event);
        }

        let events = hub.get_events_from_id(2, 10);
        assert_eq!(events.len(), 3); // IDs 2, 3, 4
        assert_eq!(events[0].id, 2);
    }

    #[test]
    fn test_get_events_with_filter() {
        let hub = DebugHub::new();

        for i in 0..5 {
            let radar_id = if i % 2 == 0 { "radar-1" } else { "radar-2" };
            let event = hub.event_builder(radar_id, "furuno").data(
                IoDirection::Send,
                ProtocolType::Tcp,
                "172.31.1.4",
                10050,
                b"test",
                None,
            );
            hub.submit(event);
        }

        // Filter by radar-1
        let events = hub.get_events(Some("radar-1"), 10, None);
        assert_eq!(events.len(), 3); // IDs 0, 2, 4

        // Filter by radar-2
        let events = hub.get_events(Some("radar-2"), 10, None);
        assert_eq!(events.len(), 2); // IDs 1, 3

        // No filter, but with 'after' ID
        let events = hub.get_events(None, 10, Some(2));
        assert_eq!(events.len(), 2); // IDs 3, 4
    }

    #[test]
    fn test_snapshot_change_detection() {
        let hub = DebugHub::new();

        // First snapshot
        let snapshot1 = RadarStateSnapshot {
            radar_id: "radar-1".to_string(),
            brand: "furuno".to_string(),
            timestamp: 0,
            controls: [("gain".to_string(), serde_json::json!(50))]
                .into_iter()
                .collect(),
            connection_state: super::super::ConnectionState::Connected,
        };
        let changes = hub.update_snapshot(snapshot1);
        assert!(changes.is_empty()); // First snapshot, no changes

        // Second snapshot with change
        let snapshot2 = RadarStateSnapshot {
            radar_id: "radar-1".to_string(),
            brand: "furuno".to_string(),
            timestamp: 100,
            controls: [("gain".to_string(), serde_json::json!(75))]
                .into_iter()
                .collect(),
            connection_state: super::super::ConnectionState::Connected,
        };
        let changes = hub.update_snapshot(snapshot2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].control_id, "gain");
        assert_eq!(changes[0].before, serde_json::json!(50));
        assert_eq!(changes[0].after, serde_json::json!(75));
    }
}
