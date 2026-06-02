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
} from "./control.js";
import {
  detectMode,
  fetchRadarIds,
  apiBase,
  wsBase,
} from "./api.js";
import "./vendor/protobuf.min.js";

import { WebGPURenderer } from "./render_webgpu.js";
import { WebGLRenderer } from "./render_webgl.js";
import { PPI } from "./ppi.js";

var webSocket;
var headingSocket;
var aisSocket;

// Exponential reconnect backoff for the heading + AIS WebSockets. Caps at
// 30s so the console doesn't log a reconnect attempt every five seconds
// while the server is down (we used to log >300 attempts/30min outage).
// The state stream in control.js has its own equivalent counter — keep
// them independent so a working AIS socket doesn't reset the others.
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;
let headingReconnectAttempts = 0;
let aisReconnectAttempts = 0;

function reconnectDelay(attempts) {
  return Math.min(RECONNECT_BASE_MS * Math.pow(2, attempts), RECONNECT_MAX_MS);
}
const aisVesselState = new Map(); // mmsi → aggregated vessel snapshot
// Own-ship identifiers learned from Signal K's REST snapshot (`tree.self`)
// and via the vessel.mmsi field. Deltas matching either are dropped so the
// operator's own MMSI never shows up as an AIS target on top of own ship.
const ownShipIdentifiers = new Set();
var RadarMessage;
var ppi; // The PPI display instance
var renderer; // The backend renderer (WebGPU or WebGL)
var capabilities;
var renderMethod = "webgpu"; // "webgpu" or "webgl"

// Heading mode: "headingUp" or "northUp"
var headingMode = "headingUp";
var trueHeading = 0; // in radians
var haveSignalKHeading = false; // true once a navigation.headingTrue delta lands
var magneticHeading = 0; // in radians, for the readout toggle only
var haveMagneticHeading = false; // true once a navigation.headingMagnetic delta lands
var lastLoggedHeading = null; // last heading value logged to console
var lastHeadingTime = 0; // Timestamp of last heading update

// Position display
var lastRadarLat = null;
var lastRadarLon = null;
var lastRadarHeading = null;
var isStationary = false; // Set from server config

// AIS overlay enabled state (client-only toggle, persists in localStorage)
var showAis = (() => {
  try {
    return localStorage.getItem("mayaraShowAis") !== "false";
  } catch {
    return true;
  }
})();

// Heading readout preference: show magnetic instead of true when both exist.
// Display-only — never affects plotting, which always uses the true heading.
var preferMagneticHeading = (() => {
  try {
    return localStorage.getItem("mayaraPreferMagnetic") === "true";
  } catch {
    return false;
  }
})();

// VRM/EBL marker localStorage keys. Hoisted to module scope so the
// initialization IIFEs below and the persistence helpers further down
// reference the same constants.
const VRM_EBL_STATE_KEY = "mayaraVrmEbl";
const VRM_EBL_VISIBLE_KEY = "mayaraVrmEblVisible";

// VRM/EBL markers - persisted between sessions as an array payload consumed
// by PPI.setVrmEblMarkers; bearing is in radians relative to bow.
var vrmEblState = (() => {
  try {
    const raw = localStorage.getItem(VRM_EBL_STATE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    return parsed;
  } catch {
    return null;
  }
})();

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);
registerStreamMessageCallback(handleStreamMessage);

window.onload = async function () {
  const urlParams = new URLSearchParams(window.location.search);
  let id = urlParams.get("id");
  const requestedRenderer = urlParams.get("renderer");

  // Determine which renderer to use
  renderMethod = await selectRenderer(requestedRenderer);
  if (!renderMethod) {
    return; // Error message already shown
  }

  console.log(`Using ${renderMethod} renderer`);

  const badge = document.getElementById("myr_renderer_badge");
  if (badge) {
    badge.textContent = renderMethod;
  }

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

  // A bare or stale `?id` (missing, empty, or the legacy `0` placeholder) and
  // any id the server doesn't recognise would otherwise drive an endless
  // failing capabilities fetch. Resolve it against the discovered radars,
  // falling back to the first one, and send the operator to the index when
  // there are none.
  try {
    const ids = await fetchRadarIds();
    if (!id || !ids.includes(id)) {
      if (ids.length === 0) {
        window.location.href = "index.html";
        return;
      }
      id = ids[0];
      // Reflect the radar we actually loaded so a refresh or bookmark
      // doesn't reuse the bogus id.
      urlParams.set("id", id);
      history.replaceState(null, "", `?${urlParams}`);
    }
  } catch (e) {
    console.error(`Could not resolve radar id: ${e}`);
  }

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

  // Poll mayara's upstream-nav status and surface a banner when own-ship
  // position / AIS are unavailable (most often an unapproved SignalK token).
  pollUpstreamStatus();

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

  // Create AIS lozenge (bottom-left, toggles vessels.* subscription)
  createAisLozenge();

  // Create VRM/EBL lozenge (above AIS lozenge) - cycles through marker states
  createVrmEblLozenge();

  // Create range lozenge
  createRangeLozenge();

  window.onresize = function () {
    ppi.redrawCanvas();
  };

  // Chrome Mobile collapses/expands its URL bar without firing window.resize,
  // but does fire visualViewport.resize. Without this, the PPI canvas keeps
  // the old `100dvh` size from when the bar was in the other state, and the
  // WebGL/WebGPU backing buffer drifts out of sync with the overlay canvas —
  // the radar image then floats above or below the range rings. Same story
  // on orientation change for tablets.
  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", () => ppi.redrawCanvas());
  }
  if (screen.orientation && typeof screen.orientation.addEventListener === "function") {
    screen.orientation.addEventListener("change", () => ppi.redrawCanvas());
  } else {
    window.addEventListener("orientationchange", () => ppi.redrawCanvas());
  }

  // Keyboard shortcuts for display zoom
  document.addEventListener("keydown", (event) => {
    if (event.key === "i" || event.key === "I") {
      ppi.zoomIn();
    } else if (event.key === "o" || event.key === "O") {
      ppi.zoomOut();
    }
  });
};

// Subscribe to own-ship navigation.headingTrue and navigation.position via the
// mayara stream. This runs in every mode: mayara serves `vessels.self` nav from
// its upstream Signal K source regardless of whether the GUI is reached
// directly (mode "standalone") or behind the Signal K proxy. Relying on it
// instead of radar spokes means own-ship position and heading are available
// even while the radar is in standby.
function subscribeToHeading() {
  const streamUrl = wsBase("/signalk/v1/stream?subscribe=none");

  headingSocket = new WebSocket(streamUrl);

  headingSocket.onopen = () => {
    console.log("Heading WebSocket connected");
    headingReconnectAttempts = 0;
    // Also subscribe to own-ship position so the AIS overlay can place
    // other vessels relative to us without waiting for radar spokes
    // (which carry position metadata, but only while transmitting).
    const subscription = {
      context: "vessels.self",
      subscribe: [
        { path: "navigation.headingTrue", period: 200 },
        // Magnetic heading drives only the readout toggle; plotting uses the
        // true heading (which mayara derives from magnetic + variation when no
        // true heading is published).
        { path: "navigation.headingMagnetic", period: 200 },
        // Position drives AIS overlay placement — without this the
        // overlay falls back to spoke metadata, which is only updated
        // when the radar is transmitting and can disagree with the
        // chartplotter GPS, causing visible target jumps.
        { path: "navigation.position", period: 1000 },
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
                haveSignalKHeading = true;
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
              } else if (value.path === "navigation.headingMagnetic") {
                // Display-only: feeds the HdgT/HdgM readout toggle. Plotting
                // never uses this — it stays on the canonical true heading. A
                // null value means the magnetic source dropped out, so clear
                // the state to remove the HdgM option and toggle affordance.
                if (value.value != null) {
                  magneticHeading = value.value; // Already in radians
                  haveMagneticHeading = true;
                } else {
                  magneticHeading = 0;
                  haveMagneticHeading = false;
                }
                refreshPositionBoxHeading();
                updateHeadingDisplayToggle();
              } else if (
                value.path === "navigation.position" &&
                value.value?.latitude != null &&
                value.value?.longitude != null
              ) {
                // Routes through updatePositionBox so the on-screen
                // ship-position display, the PPI's AIS reference point,
                // and the lastRadarLat/Lon used by the AIS proximity
                // filter all stay in sync regardless of radar TX state.
                // Pass null for heading — the position delta tells us
                // nothing about heading; preserve whatever the spoke
                // handler (or a future heading source) has set.
                updatePositionBox(
                  value.value.latitude,
                  value.value.longitude,
                  null,
                );
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
    onHeadingLost();
    const delay = reconnectDelay(headingReconnectAttempts);
    headingReconnectAttempts += 1;
    console.log(
      `Heading WebSocket closed, reconnecting in ${delay}ms (attempt ${headingReconnectAttempts})`,
    );
    setTimeout(subscribeToHeading, delay);
  };
}

// ---------------------------------------------------------------------------
// Upstream-navigation status banner
//
// Own-ship position and AIS reach the GUI through mayara's own `/signalk`
// stream, which mayara relays from its upstream Signal K `-n` connection.
// When that upstream needs an unapproved device token, mayara runs anonymous
// (`tcp:`) — nav still flows but the authenticated AIS REST seed is skipped,
// so the overlay stays empty with no on-screen explanation. We poll the
// `nav` block mayara reports on `/signalk` and surface a non-blocking banner
// that auto-clears once the condition resolves.
// ---------------------------------------------------------------------------

const STATUS_POLL_OK_MS = 30000; // healthy: re-check occasionally
const STATUS_POLL_WAIT_MS = 5000; // a banner is showing: re-check sooner

function ensureStatusBanner() {
  let banner = document.getElementById("myr_status_banner");
  if (!banner) {
    const container = document.querySelector(".myr_ppi");
    if (!container) return null;
    banner = document.createElement("div");
    banner.id = "myr_status_banner";
    banner.className = "myr_status_banner";
    // Announce the message to screen readers when it appears/changes.
    banner.setAttribute("role", "status");
    banner.setAttribute("aria-live", "polite");
    banner.style.display = "none";
    container.appendChild(banner);
  }
  return banner;
}

function showStatusBanner(message) {
  const banner = ensureStatusBanner();
  if (!banner) return;
  banner.textContent = `⚠ ${message}`;
  banner.style.display = "block";
}

function hideStatusBanner() {
  const banner = document.getElementById("myr_status_banner");
  if (banner) banner.style.display = "none";
}

// Map mayara's reported `nav` block to a banner message, or null when nothing
// needs surfacing. `transport` of `static`/`nmea0183`/`udp` is a deliberate
// non-Signal-K nav source, so we never nag about a missing token/AIS there.
function navStatusMessage(nav) {
  if (!nav) return null;
  const skTransport =
    nav.transport === "ws" ||
    nav.transport === "wss" ||
    nav.transport === "mdns" ||
    nav.transport === "tcp";
  if (!skTransport) return null;

  // Upstream nav was once flowing but has gone stale (no fresh deltas) — the
  // connection dropped or the device token was revoked. mayara clears the
  // frozen value, so have_position is false; nav_age_seconds being set (not
  // null) is what distinguishes "we had it and lost it" from "never had it".
  if (!nav.nav_live && nav.token_present && nav.nav_age_seconds != null) {
    return "SignalK navigation lost — connection dropped or token revoked. Re-approve in Security → Access Requests.";
  }

  if (!nav.have_position) {
    return "No SignalK navigation yet — own-ship position & AIS unavailable.";
  }
  // Position is flowing but the authenticated AIS seed needs a token. `tcp:`
  // can't carry one, and ws/wss without a token means the request is still
  // pending or was denied.
  if (!nav.token_present && nav.ais_count === 0) {
    return "Waiting for SignalK token approval — open Security → Access Requests to enable AIS.";
  }
  return null;
}

async function pollUpstreamStatus() {
  let reachable = false;
  let message = null;
  try {
    const res = await fetch(apiBase("/signalk"), {
      headers: { Accept: "application/json" },
    });
    if (res.ok) {
      reachable = true;
      const data = await res.json();
      message = navStatusMessage(data?.nav);
    }
  } catch {
    // Network blip — often the same trouble the banner is reporting. Leave
    // any existing banner as-is rather than clearing it on a failed fetch.
  }

  // Only mutate the banner when we actually heard back from mayara, so a
  // transient fetch failure doesn't drop an active warning.
  if (reachable) {
    if (message) showStatusBanner(message);
    else hideStatusBanner();
  }

  // Re-check sooner while something is wrong (or unreachable) so the banner
  // clears promptly once resolved; back off only when all is well.
  const delay = message || !reachable ? STATUS_POLL_WAIT_MS : STATUS_POLL_OK_MS;
  setTimeout(pollUpstreamStatus, delay);
}

// Subscribe to vessels.* on Signal K and aggregate per-MMSI updates into the
// flat snapshot the PPI overlay expects. Works against both a real Signal K
// server (deltas like `vessels.{mmsi}.navigation.position`) and standalone
// mayara's `/signalk/v1/stream`, which serves `vessels.{mmsi}` with a flat
// value from its AIS store.
async function subscribeToAisViaSignalK() {
  if (aisSocket) {
    return; // already subscribed
  }

  // Re-render anything we still have in the local cache from a previous
  // session (the cache survives unsubscribe). The operator sees vessels
  // immediately while the prime fetch and WS deltas freshen the data.
  if (ppi) {
    for (const [mmsi, vessel] of aisVesselState) {
      ppi.updateAisVessel(mmsi, vessel);
    }
  }

  // Prime the local AIS cache from Signal K's REST snapshot before opening
  // the WS. Without this, vessels only appear as upstream pushes deltas
  // (~5-60s per vessel) so the operator sees the overlay fill in slowly.
  // The prime also populates `ownShipIdentifiers` so the WS handler can
  // filter own ship's MMSI-keyed deltas; awaiting here avoids a race where
  // a delta arrives before the prime has learned the operator's MMSI.
  // mayara standalone doesn't serve /v1/api/vessels/ — the fetch 404s and
  // we fall through to the WS-only path.
  await primeAisFromRestSnapshot();

  if (!showAis) {
    return; // operator turned AIS off while prime was in flight
  }

  const streamUrl = wsBase("/signalk/v1/stream?subscribe=none");

  // Hold a local reference so handlers that fire after `aisSocket` is
  // replaced (e.g. a late `onclose` from a previous socket) can detect
  // they're stale and skip teardown / reconnect work.
  const socket = new WebSocket(streamUrl);
  aisSocket = socket;

  socket.onopen = () => {
    console.log("AIS WebSocket connected");
    aisReconnectAttempts = 0;
    // `vessels.*` triggers mayara's AIS-store full-snapshot replay; the
    // remaining leaf paths drive Signal K's incremental delta delivery
    // under context = "vessels.*". Sending both lets one subscription work
    // against either backend without having to detect mode first.
    // mayara's stream subscriber rejects unknown bare-leaf paths (e.g.
    // "name") with CannotParseControlId, which short-circuits the AIS
    // snapshot. Stick to paths it knows: `vessels.*` triggers the full
    // snapshot replay (which already includes vessel names), and the
    // `navigation.*` leaves drive incremental deltas in plugin mode.
    const subscription = {
      context: "vessels.*",
      subscribe: [
        { path: "vessels.*", period: 1000 },
        { path: "navigation.position", period: 1000 },
        { path: "navigation.courseOverGroundTrue", period: 1000 },
        { path: "navigation.speedOverGround", period: 1000 },
        { path: "navigation.headingTrue", period: 1000 },
      ],
    };
    socket.send(JSON.stringify(subscription));
  };

  socket.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      if (data.updates) {
        for (const update of data.updates) {
          handleAisDelta(data.context, update);
        }
      }
    } catch (e) {
      // Ignore parse errors (e.g., hello message)
    }
  };

  socket.onerror = (e) => {
    console.log("AIS WebSocket error:", e);
  };

  socket.onclose = () => {
    if (aisSocket !== socket) return; // stale close from a replaced socket
    aisSocket = null;
    if (showAis) {
      const delay = reconnectDelay(aisReconnectAttempts);
      aisReconnectAttempts += 1;
      console.log(
        `AIS WebSocket closed, reconnecting in ${delay}ms (attempt ${aisReconnectAttempts})`,
      );
      setTimeout(() => {
        if (showAis) subscribeToAisViaSignalK();
      }, delay);
    }
  };
}

// Fetch the full Signal K vessel tree once and populate the local AIS state
// before WebSocket deltas start trickling in. Fire-and-forget; failures are
// silent (mayara standalone doesn't serve this endpoint).
async function primeAisFromRestSnapshot() {
  try {
    const res = await fetch(apiBase("/signalk/v1/api/vessels/"));
    if (!res.ok) return;
    const tree = await res.json();
    const URN = "urn:mrn:imo:mmsi:";
    const SK_UUID = "urn:mrn:signalk:uuid:";
    // Any urn:mrn:signalk:uuid: entry represents own ship (or another
    // local vessel without an MMSI yet). Skip those and remember any MMSI
    // they carry so WS deltas keyed by that MMSI are filtered too.
    for (const [ctxKey, vessel] of Object.entries(tree)) {
      if (
        ctxKey.startsWith(SK_UUID) &&
        typeof vessel === "object" &&
        vessel !== null
      ) {
        ownShipIdentifiers.add(ctxKey);
        if (vessel.mmsi) ownShipIdentifiers.add(String(vessel.mmsi));
      }
    }
    for (const [ctxKey, vessel] of Object.entries(tree)) {
      if (typeof vessel !== "object" || vessel === null) continue;
      if (ownShipIdentifiers.has(ctxKey)) continue;
      let mmsi = ctxKey.startsWith(URN) ? ctxKey.slice(URN.length) : ctxKey;
      if (!/^\d+$/.test(mmsi)) continue; // only IMO-MMSI vessels
      if (ownShipIdentifiers.has(mmsi)) continue;
      const nav = vessel.navigation;
      const agg = aisVesselState.get(mmsi) || { mmsi, status: "Active" };
      if (vessel.name?.value) agg.name = vessel.name.value;
      if (nav?.position?.value) agg.position = nav.position.value;
      if (nav?.courseOverGroundTrue?.value != null)
        agg.cog = nav.courseOverGroundTrue.value;
      if (nav?.speedOverGround?.value != null)
        agg.sog = nav.speedOverGround.value;
      if (nav?.headingTrue?.value != null)
        agg.heading = nav.headingTrue.value;
      if (!agg.position) continue;
      aisVesselState.set(mmsi, agg);
      if (ppi) ppi.updateAisVessel(mmsi, agg);
    }
    console.log(
      `AIS primed from REST snapshot: ${aisVesselState.size} vessels`
    );
  } catch (e) {
    // Network error, JSON parse error, or REST endpoint absent — no-op.
  }
}

function unsubscribeFromAisViaSignalK() {
  if (aisSocket) {
    aisSocket.close();
    aisSocket = null;
  }
  aisReconnectAttempts = 0;
  // Intentionally keep `aisVesselState` and `ownShipIdentifiers` populated
  // across the toggle. When the operator re-enables AIS, the previous
  // snapshot is rendered instantly and WS deltas (plus the on-connect REST
  // prime) refresh it — better than starting empty and waiting for
  // upstream to push each vessel again.
  if (ppi) {
    ppi.clearAisVessels();
  }
}

// Apply a single Signal K delta update to the per-MMSI aggregate, then push
// the merged snapshot to the PPI. Handles two delta shapes:
//   - Real Signal K: context = `vessels.{mmsi}`, values[].path = leaf path
//     (e.g. "navigation.position"). Aggregator promotes leaves to flat fields.
//   - Mayara standalone: context = "vessels.*" wildcard, values[].path =
//     `vessels.{mmsi}`, value = whole flat snapshot. Merged directly.
function handleAisDelta(context, update) {
  if (!update.values) return;

  for (const item of update.values) {
    let mmsi = null;
    let leafPath = null;

    if (item.path && item.path.startsWith("vessels.")) {
      // Mayara-style: path = "vessels.{mmsi}", value = flat snapshot
      mmsi = item.path.slice("vessels.".length);
    } else if (context && context.startsWith("vessels.")) {
      // Signal K-style: context = "vessels.{mmsi}", path = leaf
      mmsi = context.slice("vessels.".length);
      leafPath = item.path;
    } else {
      continue;
    }

    if (!mmsi || mmsi === "self" || mmsi === "*") continue;

    // Strip the Signal K URN prefix so the bare MMSI shows in labels and acts
    // as a stable map key across delta shapes.
    const URN_PREFIX = "urn:mrn:imo:mmsi:";
    if (mmsi.startsWith(URN_PREFIX)) {
      mmsi = mmsi.slice(URN_PREFIX.length);
    }

    if (ownShipIdentifiers.has(mmsi)) continue;

    const agg = aisVesselState.get(mmsi) || { mmsi, status: "Active" };

    if (leafPath === null) {
      // Mayara style: value is the full flat object
      if (typeof item.value !== "object" || item.value === null) continue;
      Object.assign(agg, item.value);
    } else {
      // Signal K style: promote leaf to flat field
      switch (leafPath) {
        case "navigation.position":
          agg.position = item.value;
          break;
        case "navigation.courseOverGroundTrue":
          agg.cog = item.value;
          break;
        case "navigation.speedOverGround":
          agg.sog = item.value;
          break;
        case "navigation.headingTrue":
          agg.heading = item.value;
          break;
        case "name":
          agg.name = item.value;
          break;
        default:
          continue; // unknown leaf — don't push a redundant update
      }
    }

    // Position-proximity own-ship filter: if a vessel reports a position
    // within ~75m of our last-known own-ship location, treat it as us and
    // suppress. Picks up the operator's own AIS transmission that other
    // receivers on the network echo back as a separate MMSI-keyed vessel.
    // The threshold is generous enough to cover the gap between AIS
    // (antenna) and GPS (chartplotter) positions on the same boat.
    if (
      agg.position &&
      lastRadarLat != null &&
      lastRadarLon != null &&
      isSamePosition(agg.position, lastRadarLat, lastRadarLon)
    ) {
      ownShipIdentifiers.add(mmsi);
      // If this MMSI was already on the PPI from earlier deltas
      // (before it moved into the own-ship proximity bubble), evict it
      // now — otherwise the stale target sticks until the operator
      // moves and `evictOwnShipFromAis` runs.
      if (aisVesselState.delete(mmsi) && ppi) {
        ppi.removeAisVessel(mmsi);
      }
      continue;
    }

    aisVesselState.set(mmsi, agg);
    if (ppi) {
      ppi.updateAisVessel(mmsi, agg);
    }
  }
}

// Great-circle distance under ~75m. Approximates with equirectangular
// projection — fine at this scale and avoids trig per delta.
function isSamePosition(pos, refLat, refLon) {
  const dLatM = (pos.latitude - refLat) * 111320;
  const dLonM =
    (pos.longitude - refLon) *
    111320 *
    Math.cos((refLat * Math.PI) / 180);
  return dLatM * dLatM + dLonM * dLonM < 75 * 75;
}

// Remove any AIS vessel whose last-known position is close enough to be us.
// Used after own-ship position becomes known to retroactively prune vessels
// that were added before the proximity filter could run.
function evictOwnShipFromAis() {
  if (lastRadarLat == null || lastRadarLon == null) return;
  for (const [mmsi, vessel] of aisVesselState) {
    if (
      vessel.position &&
      isSamePosition(vessel.position, lastRadarLat, lastRadarLon)
    ) {
      ownShipIdentifiers.add(mmsi);
      aisVesselState.delete(mmsi);
      if (ppi) ppi.removeAisVessel(mmsi);
    }
  }
}

// Update PPI with current heading
function updateHeadingDisplay(mode) {
  if (ppi) {
    ppi.setTrueHeading(trueHeading);
    if (mode) {
      return ppi.setHeadingMode(mode);
    }
  }
  // Refresh the position box's HdgT field so it tracks the live
  // Signal K heading even when the position itself isn't changing.
  refreshPositionBoxHeading();
  return mode || headingMode;
}

// Resolve the heading to show in the position box as { deg, label }. With both
// true and magnetic available, honor the readout preference; with only one,
// show it; otherwise fall back to the radar-derived true heading (smoothed).
// The haveSignalKHeading / haveMagneticHeading guards reject the 0-radian
// initial values. Returns null when no heading exists.
function positionBoxHeadingDeg() {
  const haveTrue =
    haveSignalKHeading && trueHeading != null && !Number.isNaN(trueHeading);
  const haveMag =
    haveMagneticHeading &&
    magneticHeading != null &&
    !Number.isNaN(magneticHeading);
  if (haveMag && (preferMagneticHeading || !haveTrue)) {
    return { deg: (magneticHeading * 180) / Math.PI, label: "HdgM" };
  }
  if (haveTrue) {
    return { deg: (trueHeading * 180) / Math.PI, label: "HdgT" };
  }
  if (lastRadarHeading != null && !Number.isNaN(lastRadarHeading)) {
    return { deg: lastRadarHeading, label: "HdgT" };
  }
  return null;
}

function refreshPositionBoxHeading() {
  const box = document.getElementById("myr_position_box");
  if (!box) return;
  const hdgEl = box.querySelector(".myr_pos_heading");
  const hdg = positionBoxHeadingDeg();
  if (hdgEl && hdg != null) {
    hdgEl.textContent = `${hdg.label}: ${hdg.deg.toFixed(1)}°`;
  }
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
    <div class="myr_pos_heading">HdgT: ---°</div>
  `;
  box.addEventListener("click", cycleCoordFormat);
  // Clicking the heading line toggles the true/magnetic readout (when both
  // exist) without also cycling the coordinate format.
  const hdgEl = box.querySelector(".myr_pos_heading");
  if (hdgEl) {
    hdgEl.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleHeadingDisplay();
    });
  }
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

// Toggle the heading readout between true and magnetic. Inert unless both are
// available (nothing to switch to otherwise). Display-only: never touches the
// heading used for plotting. Persists the choice per browser.
function toggleHeadingDisplay() {
  const haveTrue = haveSignalKHeading && !Number.isNaN(trueHeading);
  const haveMag = haveMagneticHeading && !Number.isNaN(magneticHeading);
  if (!(haveTrue && haveMag)) return;
  preferMagneticHeading = !preferMagneticHeading;
  try {
    localStorage.setItem("mayaraPreferMagnetic", String(preferMagneticHeading));
  } catch {
    // localStorage unavailable (private mode) — preference is session-only.
  }
  refreshPositionBoxHeading();
  updateHeadingDisplayToggle();
}

// Show the click affordance on the heading line only when both true and
// magnetic headings exist, i.e. when switching is actually possible.
function updateHeadingDisplayToggle() {
  const box = document.getElementById("myr_position_box");
  if (!box) return;
  const hdgEl = box.querySelector(".myr_pos_heading");
  if (!hdgEl) return;
  const canToggle =
    haveSignalKHeading &&
    !Number.isNaN(trueHeading) &&
    haveMagneticHeading &&
    !Number.isNaN(magneticHeading);
  hdgEl.classList.toggle("myr_pos_heading_toggleable", canToggle);
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
  // AIS deltas that arrived before own-ship position was known couldn't be
  // checked against the proximity filter. Re-check them now and evict any
  // that turn out to be us.
  evictOwnShipFromAis();

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

  const hdg = positionBoxHeadingDeg();
  if (hdgEl && hdg != null) {
    hdgEl.textContent = `${hdg.label}: ${hdg.deg.toFixed(1)}°`;
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

// Called when heading data is received. Mayara's heading stream is
// event-driven (initial snapshot + broadcast on change), so silence means
// "heading is stable" — not "heading is stale". Don't disable the toggle
// on silence; let the socket-close path handle real loss-of-source.
function onHeadingReceived() {
  lastHeadingTime = Date.now();

  const toggleBtn = document.getElementById("myr_heading_toggle");
  if (toggleBtn) {
    toggleBtn.classList.remove("myr_heading_disabled");
    toggleBtn.title = "Click to toggle: Heading Up / North Up";
  }
  updateAisLozenge();
  updateHeadingDisplayToggle();
}

// Called when the heading WebSocket closes — true loss of source.
function onHeadingLost() {
  console.log("Heading source lost");
  lastLoggedHeading = null;

  if (headingMode !== "headingUp") {
    headingMode = "headingUp";
    updateHeadingDisplay(headingMode);
    if (ppi) {
      ppi.redrawCanvas();
    }
  }

  const toggleBtn = document.getElementById("myr_heading_toggle");
  if (toggleBtn) {
    toggleBtn.classList.add("myr_heading_disabled");
    toggleBtn.innerHTML = "H Up";
    toggleBtn.title = "Heading data required for North Up mode";
  }
  updateAisLozenge();
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

// Create the AIS overlay toggle lozenge (bottom-left, mirrors the speaker
// lozenge in the top-left). Click toggles vessels.* subscription on/off.
function createAisLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_ais_lozenge";
  lozenge.className =
    "myr_ais_lozenge " + (showAis ? "myr_ais_on" : "myr_ais_off");
  lozenge.title = "Click to toggle AIS overlay";

  const aisBtn = document.createElement("button");
  aisBtn.className = "myr_ais_lozenge_button";
  // Two-vessel pictogram (filled triangle + smaller outline triangle)
  aisBtn.innerHTML = `<svg class="myr_ais_icon" viewBox="0 0 24 24">
    <path d="M8 4 L13 17 L8 14 L3 17 Z"/>
    <path class="myr_ais_outline" d="M17 9 L20 17 L17 15 L14 17 Z" fill="none"/>
  </svg>`;
  aisBtn.addEventListener("click", toggleAisOverlay);

  lozenge.appendChild(aisBtn);
  container.appendChild(lozenge);

  if (showAis) {
    subscribeToAisViaSignalK();
  }
  if (ppi) {
    ppi.setShowAis(showAis);
  }
  updateAisLozenge();
}

function toggleAisOverlay() {
  // Inert while no heading reference is available — vessels can't be
  // placed, so toggling on would show nothing.
  if (ppi && !ppi.hasHeading()) return;
  showAis = !showAis;
  try {
    localStorage.setItem("mayaraShowAis", String(showAis));
  } catch {
    // ignore quota errors
  }
  if (showAis) {
    subscribeToAisViaSignalK();
  } else {
    unsubscribeFromAisViaSignalK();
  }
  if (ppi) {
    ppi.setShowAis(showAis);
  }
  updateAisLozenge();
}

function updateAisLozenge() {
  const lozenge = document.getElementById("myr_ais_lozenge");
  if (!lozenge) return;
  lozenge.classList.remove("myr_ais_on", "myr_ais_off");
  lozenge.classList.add(showAis ? "myr_ais_on" : "myr_ais_off");

  const noHeading = ppi ? !ppi.hasHeading() : false;
  lozenge.classList.toggle("myr_ais_disabled", noHeading);
  lozenge.title = noHeading
    ? "AIS unavailable — no heading source to position targets"
    : "Click to toggle AIS overlay";
}

// VRM/EBL lozenge - cycles through marker visibility states. Click steps
// forward through {none, #1 only, #2 only, both}; right-click steps backward.
// Two colored dots on the icon mirror the marker colors so the operator can
// see at a glance which marker is active.
// Persisted visible-state bitmask: bit 0 = marker 1, bit 1 = marker 2.
var vrmEblVisible = (() => {
  try {
    const raw = localStorage.getItem(VRM_EBL_VISIBLE_KEY);
    if (raw === null) return 0;
    const n = parseInt(raw, 10);
    return Number.isFinite(n) ? n & 0x3 : 0;
  } catch {
    return 0;
  }
})();

function createVrmEblLozenge() {
  const container = document.querySelector(".myr_ppi");
  if (!container) return;

  const lozenge = document.createElement("div");
  lozenge.id = "myr_vrmebl_lozenge";
  lozenge.className = "myr_vrmebl_lozenge";
  lozenge.title =
    "VRM/EBL — click to cycle markers, right-click to step back. " +
    "Drag the ring to set range, the line to set bearing, the dot to set both.";

  const btn = document.createElement("button");
  btn.className = "myr_vrmebl_lozenge_button";
  btn.innerHTML = `<svg class="myr_vrmebl_icon" viewBox="0 0 24 24">
    <circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="1.5"/>
    <line x1="12" y1="3" x2="12" y2="21" stroke="currentColor" stroke-width="1.5"/>
    <line x1="3" y1="12" x2="21" y2="12" stroke="currentColor" stroke-width="1.5"/>
    <circle class="myr_vrmebl_dot1" cx="6" cy="6" r="2.4"/>
    <circle class="myr_vrmebl_dot2" cx="18" cy="18" r="2.4"/>
  </svg>`;
  btn.addEventListener("click", (e) => {
    e.preventDefault();
    cycleVrmEbl(1);
  });
  btn.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    cycleVrmEbl(-1);
  });

  lozenge.appendChild(btn);
  container.appendChild(lozenge);

  if (vrmEblState && ppi) {
    ppi.setVrmEblMarkers(vrmEblState);
  }
  if (ppi) {
    ppi.setVrmEblEnabled(0, (vrmEblVisible & 0x1) !== 0);
    ppi.setVrmEblEnabled(1, (vrmEblVisible & 0x2) !== 0);
    ppi.onVrmEblChange = persistVrmEbl;
  }
  updateVrmEblLozenge();
}

const VRM_EBL_CYCLE = [0b00, 0b01, 0b10, 0b11];

function cycleVrmEbl(direction) {
  if (!ppi) return;
  const idx = VRM_EBL_CYCLE.indexOf(vrmEblVisible);
  const cur = idx >= 0 ? idx : 0;
  const next =
    (cur + (direction >= 0 ? 1 : VRM_EBL_CYCLE.length - 1)) %
    VRM_EBL_CYCLE.length;
  vrmEblVisible = VRM_EBL_CYCLE[next];
  ppi.setVrmEblEnabled(0, (vrmEblVisible & 0x1) !== 0);
  ppi.setVrmEblEnabled(1, (vrmEblVisible & 0x2) !== 0);
  try {
    localStorage.setItem(VRM_EBL_VISIBLE_KEY, String(vrmEblVisible));
  } catch {
    // ignore quota errors
  }
  updateVrmEblLozenge();
}

function updateVrmEblLozenge() {
  const lozenge = document.getElementById("myr_vrmebl_lozenge");
  if (!lozenge) return;
  lozenge.classList.remove(
    "myr_vrmebl_off",
    "myr_vrmebl_m1",
    "myr_vrmebl_m2",
    "myr_vrmebl_both",
  );
  if (vrmEblVisible === 0) lozenge.classList.add("myr_vrmebl_off");
  else if (vrmEblVisible === 0b11) lozenge.classList.add("myr_vrmebl_both");
  else if (vrmEblVisible & 0b01) lozenge.classList.add("myr_vrmebl_m1");
  else lozenge.classList.add("myr_vrmebl_m2");
}

function persistVrmEbl(markers) {
  try {
    localStorage.setItem(VRM_EBL_STATE_KEY, JSON.stringify(markers));
  } catch {
    // ignore quota errors
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
    spokeDataUrl = wsBase(
      `/signalk/v2/api/vessels/self/radars/${r.id}/spokes`
    );
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
        // When the upstream has no navigation.headingTrue, onHeadingReceived()
        // never fires from the heading socket, but the radar-derived heading
        // from spokes is still a valid reference. Enable the heading-dependent
        // UI (H-Up toggle, AIS lozenge) the first time any heading appears.
        // Gated on the toggle still being disabled so it runs once on
        // transition, not every spoke; enable-only, so brief radar gaps don't
        // flip the toggle back off.
        const headingToggle = document.getElementById("myr_heading_toggle");
        if (
          ppi.hasHeading() &&
          headingToggle?.classList.contains("myr_heading_disabled")
        ) {
          onHeadingReceived();
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

  // AIS vessels are handled by subscribeToAisViaSignalK on a dedicated
  // Signal K WebSocket, not via the radar state stream.
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
