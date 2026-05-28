/**
 * Debug Panel - Protocol Debugger UI
 *
 * Main component that provides real-time protocol analysis for reverse engineering.
 * Only available when mayara-server is built with --features dev.
 *
 * NOTE: The debug API endpoints were removed in v3. This feature is kept for
 * compatibility with v2 but will not work with the v3 backend.
 */

import { createEventTimeline, updateTimeline, selectEvent, clearEvents } from './event-timeline.js';
import { createPacketView, showPacketDetails, clearPacketView } from './packet-view.js';
import { createStateDiffView, showStateDiff } from './state-diff.js';
import { apiBase, wsBase } from './api.js';

// ============================================================================
// State
// ============================================================================

let panelElement = null;
let toggleButton = null;
let webSocket = null;
let isPaused = false;
let isRecording = false;
let recordingStartTime = null;
let recordingInterval = null;
let events = [];
let selectedEventId = null;
let radars = new Map();
let eventCount = 0;
let filterText = '';
let filterRadar = 'all';
let filterType = 'all';
let currentTab = 'timeline';

// Callbacks
let onEventSelected = null;

// ============================================================================
// Debug API
// ============================================================================

const DEBUG_EVENTS_URL = apiBase('/v2/api/debug/events');
const DEBUG_RECORDING_START_URL = apiBase('/v2/api/debug/recording/start');
const DEBUG_RECORDING_STOP_URL = apiBase('/v2/api/debug/recording/stop');

/**
 * Check if debug mode is available (server built with --features dev)
 */
export async function isDebugAvailable() {
  try {
    const response = await fetch(DEBUG_EVENTS_URL, { method: 'HEAD' });
    return response.ok;
  } catch (e) {
    return false;
  }
}

/**
 * Connect to debug WebSocket
 */
function connectWebSocket() {
  if (webSocket && webSocket.readyState === WebSocket.OPEN) {
    return;
  }

  const wsUrl = wsBase('/v2/api/debug');

  console.log('Debug: Connecting to', wsUrl);
  webSocket = new WebSocket(wsUrl);

  webSocket.onopen = () => {
    console.log('Debug: WebSocket connected');
    updateConnectionStatus(true);
    // Request history
    webSocket.send(JSON.stringify({ type: 'getHistory', limit: 100 }));
  };

  webSocket.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data);
      handleWebSocketMessage(message);
    } catch (e) {
      console.error('Debug: Failed to parse message', e);
    }
  };

  webSocket.onclose = () => {
    console.log('Debug: WebSocket closed');
    updateConnectionStatus(false);
    // Reconnect after delay
    setTimeout(connectWebSocket, 3000);
  };

  webSocket.onerror = (error) => {
    console.error('Debug: WebSocket error', error);
  };
}

/**
 * Handle incoming WebSocket messages
 */
function handleWebSocketMessage(message) {
  console.log('Debug: Received message', message.type, message);
  switch (message.type) {
    case 'connected':
      eventCount = message.eventCount || 0;
      console.log('Debug: Connected, event count:', eventCount);
      updateStats();
      break;

    case 'event':
      if (!isPaused) {
        addEvent(message);
      }
      break;

    case 'history':
      console.log('Debug: Got history with', message.events?.length || 0, 'events');
      if (message.events) {
        events = message.events;
        // Extract radar info from history events
        for (const event of events) {
          if (event.radarId && !radars.has(event.radarId)) {
            radars.set(event.radarId, {
              radar_id: event.radarId,
              brand: event.brand || 'unknown',
              connection_state: 'connected'
            });
          }
        }
        updateRadarCards();
        updateTimeline(events, filterText, filterRadar, filterType);
        updateStats();
      }
      break;

    case 'snapshot':
      if (message.radars) {
        for (const radar of message.radars) {
          radars.set(radar.radar_id, radar);
        }
        updateRadarCards();
      }
      break;
  }
}

/**
 * Add a new event
 */
function addEvent(event) {
  events.push(event);
  eventCount++;

  // Keep buffer reasonable
  if (events.length > 1000) {
    events = events.slice(-500);
  }

  updateTimeline(events, filterText, filterRadar, filterType);
  updateStats();

  // Update radar info if needed
  if (!radars.has(event.radarId)) {
    radars.set(event.radarId, {
      radar_id: event.radarId,
      brand: event.brand,
      connection_state: 'connected'
    });
    updateRadarCards();
  }
}

// ============================================================================
// Recording
// ============================================================================

async function startRecording() {
  try {
    const radarList = Array.from(radars.values()).map(r => ({
      radarId: r.radar_id,
      brand: r.brand
    }));

    const response = await fetch(DEBUG_RECORDING_START_URL, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ radars: radarList })
    });

    if (response.ok) {
      isRecording = true;
      recordingStartTime = Date.now();
      updateRecordingUI();

      recordingInterval = setInterval(updateRecordingTime, 1000);
    }
  } catch (e) {
    console.error('Failed to start recording:', e);
  }
}

async function stopRecording() {
  try {
    const response = await fetch(DEBUG_RECORDING_STOP_URL, {
      method: 'POST'
    });

    if (response.ok) {
      const result = await response.json();
      console.log('Recording saved:', result.file);
    }
  } catch (e) {
    console.error('Failed to stop recording:', e);
  } finally {
    isRecording = false;
    recordingStartTime = null;
    if (recordingInterval) {
      clearInterval(recordingInterval);
      recordingInterval = null;
    }
    updateRecordingUI();
  }
}

function updateRecordingTime() {
  if (!isRecording || !recordingStartTime) return;

  const elapsed = Date.now() - recordingStartTime;
  const seconds = Math.floor(elapsed / 1000) % 60;
  const minutes = Math.floor(elapsed / 60000);

  const timeEl = panelElement?.querySelector('.recording-time');
  if (timeEl) {
    timeEl.textContent = `${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
  }
}

function updateRecordingUI() {
  const recordBtn = panelElement?.querySelector('.debug-btn-record');
  const recordingSection = panelElement?.querySelector('.debug-recording');

  if (recordBtn) {
    recordBtn.textContent = isRecording ? 'Stop' : 'Record';
    recordBtn.classList.toggle('recording', isRecording);
  }

  if (recordingSection) {
    recordingSection.style.display = isRecording ? 'flex' : 'none';
  }
}

// ============================================================================
// UI Updates
// ============================================================================

function updateConnectionStatus(connected) {
  const statusEl = panelElement?.querySelector('.debug-connection-status');
  if (statusEl) {
    statusEl.classList.toggle('connected', connected);
    statusEl.textContent = connected ? 'Connected' : 'Disconnected';
  }
}

function updateRadarCards() {
  const container = panelElement?.querySelector('.debug-radars');
  if (!container) return;

  container.innerHTML = '';

  if (radars.size === 0) {
    container.innerHTML = '<div class="radar-card"><span class="radar-info">No radars connected</span></div>';
    return;
  }

  for (const [id, radar] of radars) {
    const card = document.createElement('div');
    card.className = 'radar-card';

    const state = radar.connection_state || 'connected';
    const stateClass = typeof state === 'object' ? 'error' : state.toLowerCase();

    card.innerHTML = `
      <div class="radar-status-dot ${stateClass}"></div>
      <div class="radar-info">
        <div class="radar-name">${radar.brand} - ${id}</div>
        <div class="radar-details">${state}</div>
      </div>
    `;

    container.appendChild(card);
  }

  // Update radar filter dropdown
  updateRadarFilterDropdown();
}

function updateRadarFilterDropdown() {
  const select = panelElement?.querySelector('.debug-filter-radar');
  if (!select) return;

  // Remember current selection
  const currentValue = select.value;

  // Rebuild options
  select.innerHTML = '<option value="all">All Radars</option>';

  for (const [id, radar] of radars) {
    const option = document.createElement('option');
    option.value = id;
    option.textContent = `${radar.brand} - ${id}`;
    select.appendChild(option);
  }

  // Restore selection if still valid
  if (currentValue !== 'all' && radars.has(currentValue)) {
    select.value = currentValue;
  }
}

function updateStats() {
  const statsEl = panelElement?.querySelector('.debug-stats');
  if (!statsEl) return;

  // Use eventType field (renamed from type to avoid conflict with WebSocket message type)
  const dataEvents = events.filter(e => e.eventType === 'data').length;
  const stateEvents = events.filter(e => e.eventType === 'stateChange').length;
  const unknownEvents = events.filter(e => e.decoded?.brand === 'unknown').length;

  statsEl.innerHTML = `
    <div class="debug-stat">
      <span>Total:</span>
      <span class="debug-stat-value">${eventCount}</span>
    </div>
    <div class="debug-stat">
      <span>Buffer:</span>
      <span class="debug-stat-value">${events.length}</span>
    </div>
    <div class="debug-stat">
      <span>Data:</span>
      <span class="debug-stat-value">${dataEvents}</span>
    </div>
    <div class="debug-stat">
      <span>State:</span>
      <span class="debug-stat-value">${stateEvents}</span>
    </div>
    ${unknownEvents > 0 ? `
    <div class="debug-stat">
      <span>Unknown:</span>
      <span class="debug-stat-value" style="color: #ff8866">${unknownEvents}</span>
    </div>
    ` : ''}
  `;
}

function updateFilters() {
  updateTimeline(events, filterText, filterRadar, filterType);
}

// ============================================================================
// Event Selection
// ============================================================================

function handleEventClick(eventId) {
  selectedEventId = eventId;
  selectEvent(eventId);

  const event = events.find(e => e.id === eventId);
  if (!event) return;

  if (event.eventType === 'stateChange') {
    showTab('state');
    showStateDiff(event);
  } else {
    showTab('packet');
    showPacketDetails(event);
  }
}

function showTab(tabName) {
  currentTab = tabName;

  const tabs = panelElement?.querySelectorAll('.debug-tab');
  tabs?.forEach(tab => {
    tab.classList.toggle('active', tab.dataset.tab === tabName);
  });

  const packetView = panelElement?.querySelector('.packet-view-container');
  const stateView = panelElement?.querySelector('.state-diff-container');

  if (packetView) packetView.style.display = tabName === 'packet' ? 'flex' : 'none';
  if (stateView) stateView.style.display = tabName === 'state' ? 'flex' : 'none';
}

// ============================================================================
// Panel Creation
// ============================================================================

/**
 * Create the debug panel UI
 */
export function createDebugPanel() {
  // Create toggle button
  toggleButton = document.createElement('button');
  toggleButton.className = 'debug-toggle';
  toggleButton.title = 'Protocol Debugger';
  toggleButton.innerHTML = `
    <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
      <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/>
    </svg>
  `;
  toggleButton.addEventListener('click', togglePanel);
  document.body.appendChild(toggleButton);

  // Create panel
  panelElement = document.createElement('div');
  panelElement.className = 'debug-panel';
  panelElement.innerHTML = `
    <div class="debug-header">
      <h2>🔬 Protocol Debugger</h2>
      <div class="debug-header-controls">
        <span class="debug-connection-status">Connecting...</span>
        <button class="debug-btn debug-btn-pause">Pause</button>
        <button class="debug-btn debug-btn-record">Record</button>
        <button class="debug-btn debug-btn-clear">Clear</button>
      </div>
    </div>

    <div class="debug-recording" style="display: none;">
      <div class="recording-indicator">
        <div class="recording-dot"></div>
        <span class="recording-time">00:00</span>
      </div>
      <span>Recording session...</span>
    </div>

    <div class="debug-radars">
      <div class="radar-card"><span class="radar-info">Waiting for radars...</span></div>
    </div>

    <div class="debug-filter">
      <input type="text" class="debug-filter-input" placeholder="Filter events..." />
      <select class="debug-filter-radar">
        <option value="all">All Radars</option>
      </select>
      <select class="debug-filter-type">
        <option value="all">All Types</option>
        <option value="data">Data</option>
        <option value="socketOp">Socket Ops</option>
        <option value="stateChange">State Changes</option>
        <option value="unknown">Unknown Only</option>
      </select>
    </div>

    <div class="debug-timeline"></div>

    <div class="debug-tabs">
      <div class="debug-tab active" data-tab="packet">Packet</div>
      <div class="debug-tab" data-tab="state">State Diff</div>
    </div>

    <div class="debug-detail">
      <div class="packet-view-container" style="display: flex; flex-direction: column; flex: 1;"></div>
      <div class="state-diff-container" style="display: none; flex-direction: column; flex: 1;"></div>
    </div>

    <div class="debug-stats"></div>
  `;

  document.body.appendChild(panelElement);

  // Initialize sub-components
  const timelineContainer = panelElement.querySelector('.debug-timeline');
  createEventTimeline(timelineContainer, handleEventClick);

  const packetContainer = panelElement.querySelector('.packet-view-container');
  createPacketView(packetContainer);

  const stateContainer = panelElement.querySelector('.state-diff-container');
  createStateDiffView(stateContainer);

  // Set up event handlers
  setupEventHandlers();

  // Connect WebSocket
  connectWebSocket();

  return panelElement;
}

/**
 * Set up event handlers for panel controls
 */
function setupEventHandlers() {
  // Pause button
  const pauseBtn = panelElement.querySelector('.debug-btn-pause');
  pauseBtn?.addEventListener('click', () => {
    isPaused = !isPaused;
    pauseBtn.textContent = isPaused ? 'Resume' : 'Pause';
    pauseBtn.classList.toggle('active', isPaused);

    if (webSocket?.readyState === WebSocket.OPEN) {
      webSocket.send(JSON.stringify({ type: isPaused ? 'pause' : 'resume' }));
    }
  });

  // Record button
  const recordBtn = panelElement.querySelector('.debug-btn-record');
  recordBtn?.addEventListener('click', () => {
    if (isRecording) {
      stopRecording();
    } else {
      startRecording();
    }
  });

  // Clear button
  const clearBtn = panelElement.querySelector('.debug-btn-clear');
  clearBtn?.addEventListener('click', () => {
    events = [];
    selectedEventId = null;
    clearEvents();
    clearPacketView();
    updateStats();
  });

  // Filter input
  const filterInput = panelElement.querySelector('.debug-filter-input');
  filterInput?.addEventListener('input', (e) => {
    filterText = e.target.value;
    updateFilters();
  });

  // Radar filter
  const radarSelect = panelElement.querySelector('.debug-filter-radar');
  radarSelect?.addEventListener('change', (e) => {
    filterRadar = e.target.value;
    updateFilters();
  });

  // Type filter
  const typeSelect = panelElement.querySelector('.debug-filter-type');
  typeSelect?.addEventListener('change', (e) => {
    filterType = e.target.value;
    updateFilters();
  });

  // Tab switching
  const tabs = panelElement.querySelectorAll('.debug-tab');
  tabs.forEach(tab => {
    tab.addEventListener('click', () => {
      showTab(tab.dataset.tab);
    });
  });
}

/**
 * Toggle panel open/closed
 */
export function togglePanel() {
  const isOpen = panelElement?.classList.toggle('open');
  toggleButton?.classList.toggle('panel-open', isOpen);
}

/**
 * Open the panel
 */
export function openPanel() {
  panelElement?.classList.add('open');
  toggleButton?.classList.add('panel-open');
}

/**
 * Close the panel
 */
export function closePanel() {
  panelElement?.classList.remove('open');
  toggleButton?.classList.remove('panel-open');
}

/**
 * Clean up resources
 */
export function destroyDebugPanel() {
  if (webSocket) {
    webSocket.close();
    webSocket = null;
  }

  if (recordingInterval) {
    clearInterval(recordingInterval);
    recordingInterval = null;
  }

  toggleButton?.remove();
  panelElement?.remove();

  toggleButton = null;
  panelElement = null;
}

// ============================================================================
// Auto-initialization
// ============================================================================

/**
 * Initialize debug panel if available
 */
export async function initDebugPanel() {
  const available = await isDebugAvailable();
  if (available) {
    console.log('Debug mode available - creating debug panel');
    createDebugPanel();
  } else {
    console.log('Debug mode not available (server not built with --features dev)');
  }
}
