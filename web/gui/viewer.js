"use strict";

export {
  setZoneEditMode,
  setZoneCreateMode,
  setRectCreateMode,
  setSectorEditMode,
  updateZoneForEditing,
  updateRectForEditing,
};

import {
  loadRadar,
  getControl,
  registerRadarCallback,
  registerControlCallback,
  registerStreamMessageCallback,
  registerAcquireTargetModeCallback,
  getOperatingTime,
  getUserName,
  togglePower,
  zoomIn,
  zoomOut,
  getCurrentRangeDisplay,
  isAcquireTargetMode,
  acquireTargetAtPosition,
  subscribeToAis,
  unsubscribeFromAis,
} from "./control.js";
import { isStandaloneMode, detectMode } from "./api.js";
import "./vendor/protobuf.min.js";

import { WebGPURenderer } from "./render_webgpu.js";
import { WebGLRenderer } from "./render_webgl.js";
import { PPI } from "./ppi.js";

var webSocket;
var headingSocket;
var RadarMessage;
var ppi; // The PPI display instance
var renderer; // The backend renderer (WebGPU or WebGL)
var capabilities;
var renderMethod = "webgpu"; // "webgpu" or "webgl"

// Heading mode: "headingUp" or "northUp"
var headingMode = "headingUp";
var trueHeading = 0; // in radians
var lastLoggedHeading = null; // last heading value logged to console
var lastHeadingTime = 0; // Timestamp of last heading update
var headingTimeoutId = null; // Timer for heading timeout
const HEADING_TIMEOUT_MS = 5000; // Revert to HU after 5 seconds without heading

// Position display
var lastRadarLat = null;
var lastRadarLon = null;
var lastRadarHeading = null;
var isStationary = false; // Set from server config

// AIS display
var showAis = null; // null = not yet received from server, then true/false

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);
registerStreamMessageCallback(handleStreamMessage);

window.onload = async function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");
  const requestedRenderer = urlParams.get("renderer");

  // Determine which renderer to use
  renderMethod = await selectRenderer(requestedRenderer);
  if (!renderMethod) {
    return; // Error message already shown
  }

  console.log(`Using ${renderMethod} renderer`);

  // Load protobuf definition - must complete before websocket can process messages
  const protobufPromise = new Promise((resolve, reject) => {
    protobuf.load("./proto/RadarMessage.proto", function (err, root) {
      if (err) {
        reject(err);
        return;
      }
      RadarMessage = root.lookupType(".RadarMessage");
      console.log("RadarMessage protobuf loaded successfully");
      resolve();
    });
  });

  // Create renderer based on selected method
  const canvas = document.getElementById("myr_canvas_webgl");
  if (renderMethod === "webgpu") {
    renderer = new WebGPURenderer(canvas);
  } else {
    renderer = new WebGLRenderer(canvas);
  }

  // Create PPI display (handles overlay, zones, spoke processing)
  ppi = new PPI(
    renderer,
    document.getElementById("myr_canvas_overlay"),
    document.getElementById("myr_canvas_background"),
  );

  // Wait for renderer initialization AND protobuf loading before proceeding
  await Promise.all([renderer.initPromise, protobufPromise]);
  console.log(`Both ${renderMethod} and protobuf ready`);

  // Debug: expose ppi globally for console debugging
  window.ppi = ppi;
  window.renderer = ppi; // Backwards compatibility

  // Register acquire target mode callback
  registerAcquireTargetModeCallback((enabled) => {
    console.log(
      `Viewer: acquire target mode callback called with enabled=${enabled}`,
    );
    ppi.setAcquireTargetMode(enabled, async (bearing, distance) => {
      console.log(
        `Viewer: Acquiring target at bearing ${bearing.toFixed(1)}°, distance ${distance.toFixed(0)}m`,
      );
      const result = await acquireTargetAtPosition(bearing, distance);
      if (result) {
        console.log(`Target ${result.targetId} acquired successfully`);
      }
    });
  });

  // Process any pending radar data that arrived before renderer was ready
  if (pendingRadarData) {
    console.log("Processing deferred radar data");
    radarLoaded(pendingRadarData);
    pendingRadarData = null;
  } else {
    // No pending data - load radar now
    loadRadar(id);
  }

  // Subscribe to SignalK heading delta (only in SignalK mode)
  subscribeToHeading();

  // Create hamburger menu button and setup controls toggle
  createHamburgerMenu();

  // Create position display box
  createPositionBox();

  // Create heading mode toggle button
  createHeadingModeToggle();

  // Create power lozenge
  createPowerLozenge();

  // Create speaker lozenge (audio alerts toggle)
  createSpeakerLozenge();

  // Create range lozenge
  createRangeLozenge();

  window.onresize = function () {
    ppi.redrawCanvas();
  };

  // Keyboard shortcuts for display zoom
  document.addEventListener("keydown", (event) => {
    if (event.key === "i" || event.key === "I") {
      ppi.zoomIn();
    } else if (event.key === "o" || event.key === "O") {
      ppi.zoomOut();
    }
  });
};

// Subscribe to navigation.headingTrue via SignalK WebSocket
function subscribeToHeading() {
  if (isStandaloneMode()) {
    console.log("Standalone mode: heading subscription disabled (no SignalK)");
    return;
  }

  const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const streamUrl = `${wsProtocol}//${window.location.host}/signalk/v1/stream?subscribe=none`;

  headingSocket = new WebSocket(streamUrl);

  headingSocket.onopen = () => {
    console.log("Heading WebSocket connected");
    const subscription = {
      context: "vessels.self",
      subscribe: [
        {
          path: "navigation.headingTrue",
          period: 200,
        },
      ],
    };
    headingSocket.send(JSON.stringify(subscription));
  };

  headingSocket.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      if (data.updates) {
        for (const update of data.updates) {
          if (update.values) {
            for (const value of update.values) {
              if (value.path === "navigation.headingTrue") {
                trueHeading = value.value; // Already in radians
                if (
                  lastLoggedHeading === null ||
                  Math.abs(trueHeading - lastLoggedHeading) >
                    (5 * Math.PI) / 180
                ) {
                  console.log(
                    `Heading: ${((trueHeading * 180) / Math.PI).toFixed(1)}\u00B0`,
                  );
                  lastLoggedHeading = trueHeading;
                }
                onHeadingReceived();
                updateHeadingDisplay();
              }
            }
          }
        }
      }
    } catch (e) {
      // Ignore parse errors (e.g., hello message)
    }
  };

  headingSocket.onerror = (e) => {
    console.log("Heading WebSocket error:", e);
  };

  headingSocket.onclose = () => {
    console.log("Heading WebSocket closed, reconnecting in 5s...");
    setTimeout(subscribeToHeading, 5000);
  };
}

// Update PPI with current heading
function updateHeadingDisplay(mode) {
  if (ppi) {
    ppi.setTrueHeading(trueHeading);
    if (mode) {
      return ppi.setHeadingMode(mode);
    }
  }
  return mode || headingMode;
}

// Coordinate format: 0 = DMS (degrees/minutes/seconds), 1 = DDM (degrees/decimal minutes), 2 = DD (decimal degrees)
var coordFormat = 0;

// Create position display box above heading toggle
function createPositionBox() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const box = document.createElement("div");
  box.id = "myr_position_box";
  box.className = "myr_position_box";
  box.style.display = "none"; // Hidden until we have position data
  box.style.cursor = "pointer";
  box.innerHTML = `
    <div class="myr_pos_label">Ship Pos</div>
    <div class="myr_pos_coords">--°--'--" N</div>
    <div class="myr_pos_coords">--°--'--" E</div>
    <div class="myr_pos_heading">Hdg: ---°</div>
  `;
  box.addEventListener("click", cycleCoordFormat);
  container.appendChild(box);
}

// Cycle through coordinate formats on click
function cycleCoordFormat() {
  coordFormat = (coordFormat + 1) % 3;
  // Re-render with current position
  if (lastRadarLat !== null && lastRadarLon !== null) {
    updatePositionBox(lastRadarLat, lastRadarLon, lastRadarHeading);
  }
}

// Format degrees to degrees, minutes, seconds (DMS)
function formatDMS(deg, isLat) {
  const abs = Math.abs(deg);
  const d = Math.floor(abs);
  const minFloat = (abs - d) * 60;
  const m = Math.floor(minFloat);
  const s = ((minFloat - m) * 60).toFixed(1);
  const dir = isLat ? (deg >= 0 ? "N" : "S") : deg >= 0 ? "E" : "W";
  return `${d}°${m.toString().padStart(2, "0")}'${s.padStart(4, "0")}" ${dir}`;
}

// Format degrees to degrees, decimal minutes (DDM)
function formatDDM(deg, isLat) {
  const abs = Math.abs(deg);
  const d = Math.floor(abs);
  const minFloat = (abs - d) * 60;
  const dir = isLat ? (deg >= 0 ? "N" : "S") : deg >= 0 ? "E" : "W";
  return `${d}°${minFloat.toFixed(3)}' ${dir}`;
}

// Format degrees to decimal degrees (DD)
function formatDD(deg, isLat) {
  const dir = isLat ? (deg >= 0 ? "N" : "S") : deg >= 0 ? "E" : "W";
  return `${Math.abs(deg).toFixed(5)}° ${dir}`;
}

// Format coordinate based on current format setting
function formatCoord(deg, isLat) {
  switch (coordFormat) {
    case 0:
      return formatDMS(deg, isLat);
    case 1:
      return formatDDM(deg, isLat);
    case 2:
      return formatDD(deg, isLat);
    default:
      return formatDMS(deg, isLat);
  }
}

// Update position display with new data
function updatePositionBox(lat, lon, heading) {
  const box = document.getElementById("myr_position_box");
  if (!box) return;

  if (lat === null || lon === null) {
    box.style.display = "none";
    return;
  }

  lastRadarLat = lat;
  lastRadarLon = lon;
  if (heading !== null && heading !== undefined) {
    lastRadarHeading = heading;
  }

  // Update PPI with own-ship position for AIS vessel relative positioning
  if (ppi && lat !== null && lon !== null) {
    ppi.setOwnShipPosition(lat, lon);
  }

  const label = box.querySelector(".myr_pos_label");
  const coords = box.querySelectorAll(".myr_pos_coords");
  const hdgEl = box.querySelector(".myr_pos_heading");

  if (label) {
    label.textContent = isStationary ? "Stationary" : "Ship Pos";
  }

  if (coords.length >= 2) {
    coords[0].textContent = formatCoord(lat, true);
    coords[1].textContent = formatCoord(lon, false);
  }

  if (hdgEl && lastRadarHeading !== null) {
    hdgEl.textContent = `Hdg: ${lastRadarHeading.toFixed(1)}°`;
  }

  box.style.display = "block";
}

// Create the heading mode toggle button
function createHeadingModeToggle() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const toggleBtn = document.createElement("div");
  toggleBtn.id = "myr_heading_toggle";
  toggleBtn.className = "myr_heading_toggle myr_heading_disabled";
  toggleBtn.innerHTML = "H Up";
  toggleBtn.title = "Heading data required for North Up mode";

  toggleBtn.addEventListener("click", () => {
    // Don't allow switching if disabled (no heading data)
    if (toggleBtn.classList.contains("myr_heading_disabled")) {
      return;
    }

    if (headingMode === "headingUp") {
      headingMode = "northUp";
      toggleBtn.innerHTML = "N Up";
    } else {
      headingMode = "headingUp";
      toggleBtn.innerHTML = "H Up";
    }
    headingMode = updateHeadingDisplay(headingMode);
    if (headingMode === "headingUp") {
      toggleBtn.innerHTML = "H Up";
    } else {
      toggleBtn.innerHTML = "N Up";
    }
    ppi.redrawCanvas();
  });

  container.appendChild(toggleBtn);
}

// Called when heading data is received
function onHeadingReceived() {
  lastHeadingTime = Date.now();

  // Enable the heading toggle button
  const toggleBtn = document.getElementById("myr_heading_toggle");
  if (toggleBtn) {
    toggleBtn.classList.remove("myr_heading_disabled");
    toggleBtn.title = "Click to toggle: Heading Up / North Up";
  }

  // Clear existing timeout and set a new one
  if (headingTimeoutId) {
    clearTimeout(headingTimeoutId);
  }
  headingTimeoutId = setTimeout(onHeadingTimeout, HEADING_TIMEOUT_MS);
}

// Called when heading data times out (not received for 5 seconds)
function onHeadingTimeout() {
  headingTimeoutId = null;
  console.log("Heading data lost");
  lastLoggedHeading = null;

  // Revert to heading-up mode
  if (headingMode !== "headingUp") {
    headingMode = "headingUp";
    updateHeadingDisplay(headingMode);
    if (ppi) {
      ppi.redrawCanvas();
    }
  }

  // Disable the toggle button
  const toggleBtn = document.getElementById("myr_heading_toggle");
  if (toggleBtn) {
    toggleBtn.classList.add("myr_heading_disabled");
    toggleBtn.innerHTML = "H Up";
    toggleBtn.title = "Heading data required for North Up mode";
  }
}

// Create the power lozenge on the viewer
function createPowerLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_power_lozenge";
  lozenge.className = "myr_power_lozenge myr_power_off";
  lozenge.title = "Click power icon to toggle radar power";

  const powerBtn = document.createElement("button");
  powerBtn.className = "myr_power_lozenge_button";
  powerBtn.innerHTML = `<svg class="myr_power_icon" viewBox="0 0 24 24">
    <path d="M12 3v9"/>
    <path d="M18.4 6.6a9 9 0 1 1-12.8 0"/>
  </svg>`;
  powerBtn.addEventListener("click", () => {
    togglePower();
  });

  const nameDisplay = document.createElement("div");
  nameDisplay.id = "myr_power_lozenge_name";
  nameDisplay.className = "myr_power_lozenge_name";
  nameDisplay.textContent = getUserName() || "Radar";

  lozenge.appendChild(powerBtn);
  lozenge.appendChild(nameDisplay);
  container.appendChild(lozenge);
}

// Update the power lozenge state
function updatePowerLozenge(powerState, userName) {
  const lozenge = document.getElementById("myr_power_lozenge");
  if (!lozenge) return;

  if (powerState !== undefined) {
    lozenge.classList.remove(
      "myr_power_transmit",
      "myr_power_standby",
      "myr_power_off",
      "myr_power_disconnected",
    );
    if (powerState === "transmit") {
      lozenge.classList.add("myr_power_transmit");
    } else if (powerState === "standby") {
      lozenge.classList.add("myr_power_standby");
    } else if (powerState === "disconnected") {
      lozenge.classList.add("myr_power_disconnected");
    } else {
      lozenge.classList.add("myr_power_off");
    }
  }

  if (userName !== undefined) {
    const nameDisplay = document.getElementById("myr_power_lozenge_name");
    if (nameDisplay) {
      nameDisplay.textContent = userName || "Radar";
    }
  }
}

// Audio alerts state
let audioAlertsEnabled = true;
const audioCache = {};

// Create the speaker lozenge on the viewer (to the right of power lozenge)
function createSpeakerLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_speaker_lozenge";
  lozenge.className = "myr_speaker_lozenge myr_speaker_on";
  lozenge.title = "Click to toggle audio alerts";

  const speakerBtn = document.createElement("button");
  speakerBtn.className = "myr_speaker_lozenge_button";
  speakerBtn.innerHTML = `<svg class="myr_speaker_icon" viewBox="0 0 24 24">
    <path d="M11 5L6 9H2v6h4l5 4V5z"/>
    <path class="myr_speaker_waves" d="M15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14"/>
  </svg>`;
  speakerBtn.addEventListener("click", () => {
    toggleAudioAlerts();
  });

  lozenge.appendChild(speakerBtn);
  container.appendChild(lozenge);
}

// Toggle audio alerts on/off
function toggleAudioAlerts() {
  audioAlertsEnabled = !audioAlertsEnabled;
  updateSpeakerLozenge();
}

// Update the speaker lozenge visual state
function updateSpeakerLozenge() {
  const lozenge = document.getElementById("myr_speaker_lozenge");
  if (!lozenge) return;

  lozenge.classList.remove("myr_speaker_on", "myr_speaker_off");
  if (audioAlertsEnabled) {
    lozenge.classList.add("myr_speaker_on");
  } else {
    lozenge.classList.add("myr_speaker_off");
  }
}

// Play an audio alert
function playAudioAlert(alertName) {
  if (!audioAlertsEnabled) return;

  // Cache audio objects for reuse
  if (!audioCache[alertName]) {
    audioCache[alertName] = new Audio(`audio/${alertName}.mp3`);
  }

  const audio = audioCache[alertName];
  // Reset to start if already playing
  audio.currentTime = 0;
  audio.play().catch((e) => {
    console.warn(`Failed to play audio alert "${alertName}":`, e);
  });
}

// Create the range lozenge on the viewer
function createRangeLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_range_lozenge";
  lozenge.className = "myr_range_lozenge";
  lozenge.title = "Click + to zoom in, - to zoom out";

  const zoomInBtn = document.createElement("div");
  zoomInBtn.className = "myr_range_zoom";
  zoomInBtn.innerHTML = "+";
  zoomInBtn.addEventListener("click", () => {
    zoomIn();
  });

  const rangeDisplay = document.createElement("div");
  rangeDisplay.id = "myr_range_display";
  rangeDisplay.className = "myr_range_display";
  rangeDisplay.textContent = "";

  const zoomOutBtn = document.createElement("div");
  zoomOutBtn.className = "myr_range_zoom";
  zoomOutBtn.innerHTML = "−";
  zoomOutBtn.addEventListener("click", () => {
    zoomOut();
  });

  lozenge.appendChild(zoomInBtn);
  lozenge.appendChild(rangeDisplay);
  lozenge.appendChild(zoomOutBtn);
  container.appendChild(lozenge);
}

// Update the range display
function updateRangeDisplay() {
  const rangeDisplay = document.getElementById("myr_range_display");
  if (rangeDisplay) {
    rangeDisplay.textContent = getCurrentRangeDisplay();
  }
}

// Create the hamburger menu button and setup controls toggle
function createHamburgerMenu() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  // Create hamburger button
  const hamburgerBtn = document.createElement("button");
  hamburgerBtn.type = "button";
  hamburgerBtn.id = "myr_hamburger_button";
  hamburgerBtn.className = "myr_hamburger_button";
  hamburgerBtn.title = "Open radar controls";

  // Three lines for hamburger icon
  for (let i = 0; i < 3; i++) {
    const line = document.createElement("span");
    line.className = "myr_hamburger_line";
    hamburgerBtn.appendChild(line);
  }

  // Get references to controls panel and close button
  const controller = document.getElementById("myr_controller");
  const closeBtn = document.getElementById("myr_close_controls");

  // Toggle controls panel open
  hamburgerBtn.addEventListener("click", () => {
    if (controller) {
      controller.classList.add("myr_controller_open");
    }
  });

  // Close controls panel
  if (closeBtn) {
    closeBtn.addEventListener("click", () => {
      if (controller) {
        controller.classList.remove("myr_controller_open");
      }
    });
  }

  container.appendChild(hamburgerBtn);
}

// Check WebGPU availability
async function checkWebGPU() {
  if (!navigator.gpu) return false;
  try {
    const adapter = await navigator.gpu.requestAdapter();
    return !!adapter;
  } catch (e) {
    return false;
  }
}

// Check WebGL2 availability
function checkWebGL() {
  const canvas = document.createElement("canvas");
  const gl = canvas.getContext("webgl2");
  return !!gl;
}

// Select renderer based on query parameter and availability
// Returns "webgpu", "webgl", or null if neither available
async function selectRenderer(requested) {
  const webgpuAvailable = await checkWebGPU();
  const webglAvailable = checkWebGL();

  // If specific renderer requested, try to use it
  if (requested === "webgl") {
    if (webglAvailable) return "webgl";
    showRendererError("WebGL2");
    return null;
  }
  if (requested === "webgpu") {
    if (webgpuAvailable) return "webgpu";
    showRendererError("WebGPU");
    return null;
  }

  // Auto-select: prefer WebGPU, fallback to WebGL
  if (webgpuAvailable) return "webgpu";
  if (webglAvailable) return "webgl";

  // Neither available
  showRendererError("WebGPU or WebGL2");
  return null;
}

function showRendererError(rendererName) {
  const container = document.querySelector(".myr_container");
  if (!container) return;

  container.innerHTML = `
    <div class="myr_webgpu_error">
      <h2>${rendererName} Not Available</h2>
      <p class="myr_error_message">This display requires ${rendererName} which is not available in your browser.</p>

      <div class="myr_error_section">
        <h3>Possible Solutions</h3>
        <div class="myr_code_instructions">
          <p>Try one of the following:</p>
          <p>- Use a modern browser (Chrome, Firefox, Edge, Safari)</p>
          <p>- Enable hardware acceleration in browser settings</p>
          <p>- Update your graphics drivers</p>
          <p>- See the <a href="index.html" class="myr_flag_link">radar list page</a> for detailed setup instructions</p>
        </div>
      </div>

      <div class="myr_error_actions">
        <a href="index.html" class="myr_back_link">Back to Radar List</a>
        <button onclick="location.reload()" class="myr_retry_button">Retry</button>
      </div>
    </div>
  `;
}

function restart(id) {
  setTimeout(loadRadar, 15000, id);
}

// Pending radar data if callback arrives before PPI is ready
var pendingRadarData = null;

// r contains id, name, capabilities and spokeDataUrl
function radarLoaded(r) {
  capabilities = r.capabilities;
  let maxSpokeLength = capabilities.maxSpokeLength;
  let spokesPerRevolution = capabilities.spokesPerRevolution;
  let prev_angle = -1;

  // If PPI isn't ready yet, store data and return
  if (!ppi || !renderer || !renderer.ready) {
    pendingRadarData = r;
    return;
  }

  // Initialize PPI with radar capabilities
  ppi.setLegend(capabilities.legend);
  ppi.setSpokes(spokesPerRevolution, maxSpokeLength);

  // Also initialize renderer with spokes (for texture sizing)
  renderer.setSpokes(spokesPerRevolution, maxSpokeLength);

  // Set stationary mode from capabilities
  isStationary = capabilities.stationary || false;

  // Use provided spokeDataUrl or construct SignalK stream URL
  let spokeDataUrl = r.spokeDataUrl;
  if (
    !spokeDataUrl ||
    spokeDataUrl === "undefined" ||
    spokeDataUrl === "null"
  ) {
    const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    spokeDataUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${r.id}/stream`;
  } else {
    spokeDataUrl = spokeDataUrl.replace("{id}", r.id);
  }
  console.log("Connecting to radar stream:", spokeDataUrl);
  webSocket = new WebSocket(spokeDataUrl);
  webSocket.binaryType = "arraybuffer";

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  };
  webSocket.onclose = (e) => {
    console.log(
      "websocket close: code=" +
        e.code +
        ", reason=" +
        e.reason +
        ", wasClean=" +
        e.wasClean,
    );
    restart(r.id);
  };
  webSocket.onerror = (e) => {
    console.log("websocket error:", e);
  };
  webSocket.onmessage = (e) => {
    // Skip processing when page is not visible to prevent queueing up updates
    if (document.hidden) {
      return;
    }

    try {
      const dataSize = e.data?.byteLength || e.data?.length || 0;
      if (dataSize === 0) {
        console.warn("WS message received with 0 bytes");
        return;
      }
      if (!RadarMessage) {
        console.warn("RadarMessage not loaded yet, dropping message");
        return;
      }
      let buf = e.data;
      let bytes = new Uint8Array(buf);
      var message = RadarMessage.decode(bytes);
      if (message.spokes && message.spokes.length > 0) {
        let lastSpoke = null;
        for (let i = 0; i < message.spokes.length; i++) {
          let spoke = message.spokes[i];
          ppi.drawSpoke(spoke);
          prev_angle = spoke.angle;
          lastSpoke = spoke;
        }
        ppi.render();
        // Update position box with radar position from last spoke
        // Heading is derived from spoke.bearing - spoke.angle (in spokes units)
        if (
          lastSpoke &&
          lastSpoke.lat !== undefined &&
          lastSpoke.lon !== undefined
        ) {
          let heading = null;
          if (lastSpoke.bearing !== undefined) {
            // Convert from spokes to degrees: (bearing - angle) * 360 / spokesPerRevolution
            heading =
              ((lastSpoke.bearing - lastSpoke.angle) * 360) /
              spokesPerRevolution;
            // Normalize to 0-360
            if (heading < 0) heading += 360;
            if (heading >= 360) heading -= 360;
          }
          updatePositionBox(lastSpoke.lat, lastSpoke.lon, heading);
        }
      }
    } catch (err) {
      console.error("Error processing WebSocket message:", err);
    }
  };
}

function controlUpdate(controlId, value) {
  if (controlId === "power") {
    let powerState;
    if (value.value === "disconnected") {
      powerState = "disconnected";
      for (const targetId of knownTargets) {
        ppi.removeTarget(targetId);
      }
      knownTargets.clear();
    } else {
      const control = getControl(controlId);
      powerState = "off";
      if (control?.descriptions && value.value in control.descriptions) {
        powerState = control.descriptions[value.value].toLowerCase();
      }
    }
    if (ppi) {
      const time = getOperatingTime();
      ppi.setPowerMode(powerState, time.onTime, time.txTime);
    }
    updatePowerLozenge(powerState);
  } else if (controlId === "userName") {
    updatePowerLozenge(undefined, value.value);
  } else if (controlId === "guardZone1") {
    if (ppi) {
      ppi.setGuardZone(0, parseGuardZone(value));
    }
  } else if (controlId === "guardZone2") {
    if (ppi) {
      ppi.setGuardZone(1, parseGuardZone(value));
    }
  } else if (controlId.startsWith("exclusionZone")) {
    const index = parseInt(controlId.slice(-1)) - 1;
    if (index >= 0 && index < 4 && ppi) {
      ppi.setExclusionZone(index, parseGuardZone(value));
    }
  } else if (controlId.startsWith("exclusionRect")) {
    const index = parseInt(controlId.slice(-1)) - 1;
    if (index >= 0 && index < 4 && ppi) {
      ppi.setExclusionRect(index, parseExclusionRect(value));
    }
  } else if (controlId.startsWith("noTransmitSector")) {
    const index = parseInt(controlId.slice(-1)) - 1;
    if (index >= 0 && index < 4 && ppi) {
      ppi.setNoTransmitSector(index, parseNoTransmitSector(value));
    }
  } else if (controlId === "showAis") {
    const newShowAis = value.value === 1;
    // Subscribe or unsubscribe based on state change
    // showAis === null means first time receiving control from server
    if (newShowAis && (showAis === null || !showAis)) {
      subscribeToAis();
    } else if (!newShowAis && (showAis === null || showAis)) {
      unsubscribeFromAis();
      // Clear all AIS vessels from display when turning off
      if (ppi) {
        ppi.clearAisVessels();
      }
    }
    showAis = newShowAis;
    if (ppi) {
      ppi.setShowAis(showAis);
    }
  } else {
    const control = getControl(controlId);
    if (control?.name === "Range") {
      const range = typeof value === "object" ? value.value : value;
      ppi.setRange(range);
      updateRangeDisplay();
    }
  }
}

// Track known targets to detect new acquisitions
const knownTargets = new Set();

// Handle stream messages (targets, navigation, etc.)
function handleStreamMessage(path, value) {
  // Handle target updates: radars.{id}.targets.{targetId}
  if (path.includes(".targets.")) {
    const parts = path.split(".");
    if (parts.length >= 4 && parts[2] === "targets") {
      const targetId = parseInt(parts[3]);
      if (ppi) {
        if (value === null) {
          // Target lost
          ppi.removeTarget(targetId);
          knownTargets.delete(targetId);
        } else {
          // Check if this is a NEW automatic target (guard zone detection)
          if (!knownTargets.has(targetId) && value.sourceZone) {
            // Play audio alert for the specific guard zone
            if (value.sourceZone === 1) {
              playAudioAlert("guard_zone_1");
            } else if (value.sourceZone === 2) {
              playAudioAlert("guard_zone_2");
            }
          }
          knownTargets.add(targetId);

          // Target update
          ppi.updateTarget(targetId, value);
        }
      }
    }
  }

  // Handle AIS vessel updates: vessels.{mmsi}
  if (path.startsWith("vessels.") && !path.includes("radars")) {
    const parts = path.split(".");
    if (parts.length >= 2) {
      const mmsi = parts[1];
      if (ppi) {
        if (value === null || value.status === "Lost") {
          // Vessel lost
          ppi.removeAisVessel(mmsi);
        } else {
          // Vessel update
          ppi.updateAisVessel(mmsi, value);
        }
      }
    }
  }
}

// Parse guard zone control value into drawing parameters
function parseGuardZone(cv) {
  if (!cv || !cv.enabled) return null;
  return {
    startAngle: cv.value ?? 0,
    endAngle: cv.endValue ?? 0,
    startDistance: cv.startDistance ?? 0,
    endDistance: cv.endDistance ?? 0,
  };
}

// Parse no-transmit sector control value into drawing parameters
function parseNoTransmitSector(cv) {
  if (!cv || !cv.enabled) return null;
  return {
    startAngle: cv.value ?? 0,
    endAngle: cv.endValue ?? 0,
  };
}

// Parse exclusion rect control value into drawing parameters
function parseExclusionRect(cv) {
  if (!cv || !cv.enabled) return null;
  return {
    x1: cv.x1 ?? 0,
    y1: cv.y1 ?? 0,
    x2: cv.x2 ?? 0,
    y2: cv.y2 ?? 0,
    width: cv.width ?? 0,
    enabled: true,
  };
}

/**
 * Enable/disable zone edit mode with drag handles on the viewer
 */
function setZoneEditMode(controlId, editing, onDragEnd = null) {
  if (!ppi) return;

  if (!editing) {
    ppi.setEditingZone(null, null);
    return;
  }

  let zoneIndex = null;
  if (controlId === "guardZone1") {
    zoneIndex = 0;
  } else if (controlId === "guardZone2") {
    zoneIndex = 1;
  }

  if (zoneIndex === null) return;

  const wrappedCallback = onDragEnd ? (index, zone) => onDragEnd(zone) : null;

  ppi.setEditingZone(zoneIndex, wrappedCallback);
}

/**
 * Enable zone create mode - click-drag on PPI to define zone boundaries
 * @param {string} controlId - Control ID (e.g., "guardZone1", "exclusionZone1")
 * @param {boolean} creating - Whether to enable create mode
 * @param {function} onCreated - Callback (zone) when zone is created
 */
function setZoneCreateMode(controlId, creating, onCreated = null) {
  if (!ppi) return;

  if (!creating) {
    ppi.cancelCreating();
    return;
  }

  let zoneIndex = null;
  let zoneType = "guard";

  if (controlId === "guardZone1") {
    zoneIndex = 0;
  } else if (controlId === "guardZone2") {
    zoneIndex = 1;
  } else if (controlId === "exclusionZone1") {
    zoneIndex = 2;
    zoneType = "exclusion";
  } else if (controlId === "exclusionZone2") {
    zoneIndex = 3;
    zoneType = "exclusion";
  } else if (controlId === "exclusionZone3") {
    zoneIndex = 4;
    zoneType = "exclusion";
  } else if (controlId === "exclusionZone4") {
    zoneIndex = 5;
    zoneType = "exclusion";
  }

  if (zoneIndex === null) return;

  const wrappedCallback = onCreated ? (index, zone) => onCreated(zone) : null;

  ppi.setCreatingZone(zoneIndex, wrappedCallback, zoneType);
}

/**
 * Update a zone's parameters for live preview during editing.
 * Called when form fields change to show immediate visual feedback.
 */
function updateZoneForEditing(controlId, zone) {
  if (!ppi) return;

  if (controlId === "guardZone1") {
    ppi.setGuardZone(0, zone);
  } else if (controlId === "guardZone2") {
    ppi.setGuardZone(1, zone);
  } else if (controlId === "exclusionZone1") {
    ppi.setExclusionZone(0, zone);
  } else if (controlId === "exclusionZone2") {
    ppi.setExclusionZone(1, zone);
  } else if (controlId === "exclusionZone3") {
    ppi.setExclusionZone(2, zone);
  } else if (controlId === "exclusionZone4") {
    ppi.setExclusionZone(3, zone);
  }
}

/**
 * Enable rect create mode - click-drag on PPI to define rect boundaries
 * @param {string} controlId - Control ID (e.g., "exclusionRect1")
 * @param {boolean} creating - Whether to enable create mode
 * @param {function} onCreated - Callback (rect) when rect is created
 */
function setRectCreateMode(controlId, creating, onCreated = null) {
  if (!ppi) return;

  if (!creating) {
    ppi.cancelCreating();
    return;
  }

  let rectIndex = null;
  if (controlId === "exclusionRect1") {
    rectIndex = 0;
  } else if (controlId === "exclusionRect2") {
    rectIndex = 1;
  } else if (controlId === "exclusionRect3") {
    rectIndex = 2;
  } else if (controlId === "exclusionRect4") {
    rectIndex = 3;
  }

  if (rectIndex === null) return;

  const wrappedCallback = onCreated ? (index, rect) => onCreated(rect) : null;

  ppi.setCreatingRect(rectIndex, wrappedCallback);
}

/**
 * Update a rect's parameters for live preview during editing.
 */
function updateRectForEditing(controlId, rect) {
  if (!ppi) return;

  if (controlId === "exclusionRect1") {
    ppi.setExclusionRect(0, rect);
  } else if (controlId === "exclusionRect2") {
    ppi.setExclusionRect(1, rect);
  } else if (controlId === "exclusionRect3") {
    ppi.setExclusionRect(2, rect);
  } else if (controlId === "exclusionRect4") {
    ppi.setExclusionRect(3, rect);
  }
}

/**
 * Enable/disable sector edit mode with drag handles on the viewer
 */
function setSectorEditMode(controlId, editing, onDragEnd = null) {
  if (!ppi) return;

  if (!editing) {
    ppi.setEditingSector(null, null);
    return;
  }

  let sectorIndex = null;
  const match = controlId.match(/noTransmitSector(\d)/);
  if (match) {
    sectorIndex = parseInt(match[1]) - 1;
  }

  if (sectorIndex === null || sectorIndex < 0 || sectorIndex > 3) return;

  const wrappedCallback = onDragEnd
    ? (index, sector) => onDragEnd(sector)
    : null;

  ppi.setEditingSector(sectorIndex, wrappedCallback);
}
