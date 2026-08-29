/**
 * API adapter for Mayara Radar v5
 *
 * Automatically detects whether running in SignalK or standalone mode
 * and provides a unified API interface for the capabilities-driven v5 API.
 */

// The Radar API version this UI was built against, permeated from the server's
// api-version at build time (build.rs generates api-version.js). Used to enforce
// ^major.minor compatibility against whatever server the UI talks to.
import { RADAR_API_VERSION } from "./api-version.js";

// API endpoints for different modes
const SIGNALK_RADARS_API = "/signalk/v2/api/vessels/self/radars";
const STANDALONE_INTERFACES_API =
  "/signalk/v2/api/vessels/self/radars/interfaces";
const DIAGNOSTICS_API =
  "/signalk/v2/api/vessels/self/radars/diagnostics";
const ENDPOINT_API = "/signalk";

// Mount prefix to prepend to every API/WS path.
//
// Served directly by mayara, the GUI lives at `/gui/…` while its API lives at
// the origin root (`/signalk/…`), so the prefix is empty. Behind the Signal K
// reverse proxy the GUI is mounted at `/plugins/<id>/gui/…` and the proxy
// forwards everything under that mount to mayara — so the API must be reached
// at `/plugins/<id>/gui/signalk/…`, i.e. the prefix is whatever precedes
// `/gui` *plus* `/gui` itself. The discriminator is therefore whether anything
// precedes `/gui`: nothing → direct (empty); a sub-path → proxied.
export function basePrefix() {
  // Match the `…/gui` segment whether or not a trailing slash follows
  // (e.g. `/plugins/<id>/gui` with no slash still needs the prefix).
  const m = window.location.pathname.match(/^(.*\/gui)(?:\/|$)/);
  return m && m[1] !== "/gui" ? m[1] : "";
}

export function apiBase(path) {
  return basePrefix() + path;
}

export function wsBase(path) {
  const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${wsProtocol}//${window.location.host}${basePrefix()}${path}`;
}

// Every request carries a deadline. A browser `fetch` has no timeout of its
// own, so a stall anywhere below HTTP — an mDNS answer that never arrives for
// a `.local` host, a radar that dropped off the LAN, a Wi-Fi handover — leaves
// the promise pending forever. The GUI then shows a frozen control or an empty
// panel with nothing in the console, which is indistinguishable from a bug in
// mayara itself.
const REQUEST_TIMEOUT_MS = 10000;

// An upload carries a whole recording over the boat's Wi-Fi, so it gets a
// deadline of its own rather than the interactive one.
const UPLOAD_TIMEOUT_MS = 300000;

/**
 * `fetch` with a deadline. Rejects with a descriptive `Error` when the request
 * outlives `timeoutMs`, so callers report "timed out" rather than hanging or
 * reporting an opaque `AbortError`.
 *
 * @param {string} url - Request URL
 * @param {Object} [options] - `fetch` options
 * @param {number} [timeoutMs] - Deadline in milliseconds
 * @returns {Promise<Response>}
 */
export async function apiFetch(
  url,
  options = {},
  timeoutMs = REQUEST_TIMEOUT_MS
) {
  const deadline = AbortSignal.timeout(timeoutMs);
  // A caller's own signal is combined with the deadline rather than replaced,
  // so passing one still cancels the request instead of being ignored.
  const signal = options.signal
    ? AbortSignal.any([deadline, options.signal])
    : deadline;

  // The deadline covers reading the body too, so a response that starts
  // arriving and then stalls aborts inside `json()`/`blob()` rather than in
  // `fetch()`. Classify both the same way, or that case reports an opaque
  // AbortError instead of the timeout the caller is meant to see.
  const asTimeout = (e) => {
    if (e.name === "TimeoutError" || (e.name === "AbortError" && deadline.aborted)) {
      return new Error(
        `${options.method || "GET"} ${url} timed out after ${timeoutMs} ms`
      );
    }
    return e;
  };

  let response;
  try {
    response = await fetch(url, { ...options, signal });
  } catch (e) {
    throw asTimeout(e);
  }

  for (const read of ["json", "text", "blob", "arrayBuffer", "formData"]) {
    const original = response[read].bind(response);
    response[read] = async (...args) => {
      try {
        return await original(...args);
      } catch (e) {
        throw asTimeout(e);
      }
    };
  }
  return response;
}

// Detected mode (null = not detected yet)
let detectedMode = null;

// Cache for capabilities (fetched once per radar)
const capabilitiesCache = new Map();

/**
 * Detect which API mode we're running in
 * @returns {Promise<string>} 'signalk' or 'standalone'
 */
export async function detectMode() {
  if (detectedMode) {
    return detectedMode;
  }

  // Try standalone first - check if / returns server mayara
  try {
    const response = await apiFetch(apiBase(ENDPOINT_API), {
      headers: { Accept: "application/json" },
    });
    const data = await response.json();
    if (data?.server?.id === "mayara") {
      detectedMode = "standalone";
      console.log("Detected standalone mode");
      return detectedMode;
    }
  } catch (e) {
    // Standalone not available
  }

  // Try SignalK - check if endpoint returns 200
  try {
    const response = await apiFetch(apiBase(SIGNALK_RADARS_API), {
      method: "HEAD",
    });
    if (response.ok) {
      detectedMode = "signalk";
      console.log("Detected SignalK mode");
      return detectedMode;
    }
  } catch (e) {
    // SignalK not available
  }

  // Default to standalone
  detectedMode = "standalone";
  console.log("Defaulting to standalone mode");
  return detectedMode;
}

/**
 * Get the radars API URL for current mode
 * @returns {string} API URL
 */
export function getRadarsPath() {
  return apiBase(SIGNALK_RADARS_API);
}

/**
 * Get the interfaces API URL (standalone only)
 * @returns {string|null} API URL or null if not available
 */
export function getInterfacesUrl() {
  return apiBase(STANDALONE_INTERFACES_API);
}

/**
 * Get the network-diagnostics download URL (gzipped JSON).
 * @returns {string} API URL
 */
export function getDiagnosticsUrl() {
  return apiBase(DIAGNOSTICS_API);
}

/**
 * Fetch list of radar IDs
 * @returns {Promise<string[]>} Array of radar IDs
 */
export async function fetchRadarIds() {
  await detectMode();

  const response = await apiFetch(getRadarsPath());
  const data = await response.json();
  assertCompatibleApiVersion(data);

  return Object.keys(radarsMap(data));
}

/**
 * Fetch list of radars (legacy compatibility)
 * @returns {Promise<Object>} Radars object keyed by ID
 */
export async function fetchRadars() {
  await detectMode();

  const response = await apiFetch(getRadarsPath());
  const data = await response.json();
  assertCompatibleApiVersion(data);
  return radarsMap(data);
}

/**
 * Ask the server whether it can store radar settings.
 *
 * Returns null when the answer cannot be had — an older mayara has no such
 * endpoint, and a proxy in front of it may not route one. A landing page that
 * cannot reach it simply shows no warning, rather than failing to load.
 *
 * @returns {Promise<{settingsStored: boolean, settingsPath?: string}|null>}
 */
export async function fetchServerStatus() {
  await detectMode();

  try {
    const response = await apiFetch(`${getRadarsPath()}/status`);
    if (!response.ok) return null;
    return await response.json();
  } catch (err) {
    console.log("Server status unavailable:", err);
    return null;
  }
}

/**
 * Unwrap the radar list to a plain `{ id: radar }` map. The Radar API response
 * is the `{ version, radars }` envelope (both mayara and signalk-server return
 * this); older/bare responses that are already a keyed map are passed through.
 * @param {Object} data - Parsed radar-list response
 * @returns {Object} Radars keyed by ID
 */
function radarsMap(data) {
  return data && typeof data.radars === "object" ? data.radars : data;
}

/** Parse a "X.Y.Z" version string into `[major, minor, patch]` numbers. */
function parseVersion(v) {
  return String(v)
    .split(".")
    .map((n) => parseInt(n, 10) || 0);
}

/**
 * Assert the server's Radar API `version` satisfies `^<built version>` — the
 * same major with an equal-or-newer minor. Future minor updates are assumed to
 * be additive and compatible; an older API (or a missing version, i.e. a
 * pre-envelope server) or a new major (breaking) is refused with a thrown error
 * that surfaces in the UI.
 * @param {Object} data - Parsed `GET /radars` response (the `{ version, radars }` envelope)
 * @returns {string} the accepted server version
 */
function assertCompatibleApiVersion(data) {
  const [reqMajor, reqMinor] = parseVersion(RADAR_API_VERSION);
  const serverVersion =
    data && typeof data.version === "string" ? data.version : null;
  if (!serverVersion) {
    throw new Error(
      `Radar API too old: this UI requires ^${reqMajor}.${reqMinor} but the server reported no version.`
    );
  }
  const [major, minor] = parseVersion(serverVersion);
  if (major !== reqMajor || minor < reqMinor) {
    throw new Error(
      `Incompatible Radar API version ${serverVersion}: this UI requires ` +
        `^${reqMajor}.${reqMinor} (>= ${reqMajor}.${reqMinor}.0, < ${reqMajor + 1}.0.0).`
    );
  }
  return serverVersion;
}

/**
 * Fetch radar capabilities (v5 API)
 * Returns the capability manifest with controls schema, characteristics, etc.
 * @param {string} radarId - The radar ID
 * @returns {Promise<Object>} Capability manifest
 */
export async function fetchCapabilities(radarId) {
  await detectMode();

  // Don't cache capabilities - model info may be updated after TCP connects
  // The radar model is identified via TCP $N96 response, which happens after
  // initial discovery. Caching would return stale "Unknown" model.

  const url = `${getRadarsPath()}/${radarId}/capabilities`;
  console.log(`Fetching capabilities: GET ${url}`);

  const response = await apiFetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch capabilities: ${response.status}`);
  }

  return response.json();
}

/**
 * Fetch list of interfaces
 * @returns {Promise<Object|null>} Interfaces object or null
 */
export async function fetchInterfaces() {
  await detectMode();

  const url = getInterfacesUrl();
  if (!url) {
    return null;
  }

  const response = await apiFetch(url);
  return response.json();
}

/**
 * Check if we're in SignalK mode
 * @returns {boolean}
 */
export function isSignalKMode() {
  return detectedMode === "signalk";
}

/**
 * Check if we're in standalone mode
 * @returns {boolean}
 */
export function isStandaloneMode() {
  return detectedMode === "standalone";
}

/**
 * Acquire a target at the specified bearing and distance from radar
 *
 * @param {string} radarId - The radar ID
 * @param {number} bearing - Target bearing in radians true [0, 2π)
 * @param {number} distance - Target distance in meters
 * @returns {Promise<{targetId: number, radarId: string}|null>} Target info or null on failure
 */
export async function acquireTarget(radarId, bearing, distance) {
  await detectMode();

  const url = `${getRadarsPath()}/${radarId}/targets`;
  const body = { bearing, distance };

  console.log(`Acquiring target: POST ${url}`, body);

  try {
    const response = await apiFetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });

    if (response.ok) {
      const result = await response.json();
      console.log(`Target acquired: ${result.targetId}`);
      return result;
    } else {
      const errorText = await response.text();
      console.error(
        `Target acquisition failed: ${response.status} ${response.statusText}`,
        errorText
      );
      return null;
    }
  } catch (e) {
    console.error(`Target acquisition error: ${e}`);
    return null;
  }
}

/**
 * Send a control command to a radar via REST API (v5 format)
 *
 * SignalK Radar API v5 format:
 *   PUT /signalk/v2/api/vessels/self/radars/{radarId}/controls/{controlId}
 *   Body: { value: ..., units: ... }
 *
 * @param {string} radarId - The radar ID
 * @param {string} controlId - The control ID (e.g., "power", "gain", "range")
 * @param {any} body - The value to set (type depends on control)
 * @returns {Promise<boolean>} True if successful
 */
export async function setControl(radarId, controlId, body) {
  await detectMode();

  const url = `${getRadarsPath()}/${radarId}/controls/${controlId}`;
  const bodyStr = JSON.stringify(body);

  console.log(`Setting control: PUT ${url}`, bodyStr);

  try {
    const response = await apiFetch(url, {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
      },
      body: bodyStr,
    });

    if (response.ok) {
      console.log(`Control ${controlId} set successfully`);
      return true;
    } else {
      const errorText = await response.text();
      console.error(
        `Control command failed: ${response.status} ${response.statusText} for ${url}`,
        errorText
      );
      return false;
    }
  } catch (e) {
    console.error(`Control command error: ${e}`);
    return false;
  }
}

// ============================================================================
// Playback Detection
// ============================================================================

/**
 * Check if a radar is a playback radar (virtual radar from recording playback)
 * @param {string} radarId - The radar ID
 * @returns {boolean} True if this is a playback radar
 */
export function isPlaybackRadar(radarId) {
  return radarId && radarId.startsWith("playback-");
}

// ============================================================================
// Recordings API
// ============================================================================

const RECORDINGS_API = apiBase("/v2/api/vessels/self/radars/recordings");

/**
 * List available recordings
 * @param {string} [subdirectory] - Optional subdirectory to list
 * @returns {Promise<Object[]>} Array of recording info objects
 */
export async function listRecordings(subdirectory) {
  const url = subdirectory
    ? `${RECORDINGS_API}/files?dir=${encodeURIComponent(subdirectory)}`
    : `${RECORDINGS_API}/files`;
  const response = await apiFetch(url);
  if (!response.ok) {
    throw new Error(`Failed to list recordings: ${response.status}`);
  }
  const data = await response.json();
  // Server returns { recordings: [...], totalCount, totalSize }
  return data.recordings || [];
}

/**
 * Get recording file info
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Recording info object
 */
export async function getRecordingInfo(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : "";
  const response = await apiFetch(
    `${RECORDINGS_API}/files/${encodeURIComponent(filename)}${params}`
  );
  if (!response.ok) {
    throw new Error(`Failed to get recording info: ${response.status}`);
  }
  return response.json();
}

/**
 * Delete a recording
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<boolean>} True if successful
 */
export async function deleteRecording(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : "";
  const response = await apiFetch(
    `${RECORDINGS_API}/files/${encodeURIComponent(filename)}${params}`,
    {
      method: "DELETE",
    }
  );
  return response.ok;
}

/**
 * Rename a recording file
 * @param {string} oldFilename - Current filename
 * @param {string} newFilename - New filename
 * @returns {Promise<Object>} Result with new filename
 */
export async function renameRecording(oldFilename, newFilename) {
  const response = await apiFetch(
    `${RECORDINGS_API}/files/${encodeURIComponent(oldFilename)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ newName: newFilename }),
    }
  );

  if (!response.ok) {
    const error = await response
      .json()
      .catch(() => ({ error: "Rename failed" }));
    throw new Error(error.error || "Rename failed");
  }

  // Server returns empty body on success
  return { success: true, newFilename };
}

/**
 * Upload a recording file (.mrr or .mrr.gz)
 * @param {File} file - The file to upload
 * @returns {Promise<Object>} Upload result with filename and size
 */
export async function uploadRecording(file) {
  const response = await apiFetch(
    `${RECORDINGS_API}/files/upload`,
    {
      method: "POST",
      headers: {
        "Content-Disposition": `attachment; filename="${file.name}"`,
      },
      body: file,
    },
    UPLOAD_TIMEOUT_MS
  );

  if (!response.ok) {
    const error = await response
      .json()
      .catch(() => ({ error: "Upload failed" }));
    throw new Error(error.error || "Upload failed");
  }

  return await response.json();
}

/**
 * Get the download URL for a recording (returns compressed .mrr.gz file)
 * @param {string} filename - Recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {string} Download URL
 */
export function getRecordingDownloadUrl(filename, subdirectory) {
  const params = subdirectory ? `?dir=${encodeURIComponent(subdirectory)}` : "";
  return `${RECORDINGS_API}/files/${encodeURIComponent(
    filename
  )}/download${params}`;
}

/**
 * Get list of radars available for recording
 * @returns {Promise<Object[]>} Array of radar info objects
 */
export async function getRecordableRadars() {
  const response = await apiFetch(`${RECORDINGS_API}/radars`);
  if (!response.ok) {
    throw new Error(`Failed to get recordable radars: ${response.status}`);
  }
  return response.json();
}

/**
 * Start recording from a radar
 * @param {string} radarId - The radar ID to record
 * @param {string} [filename] - Optional filename (auto-generated if not provided)
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Recording status
 */
export async function startRecording(radarId, filename, subdirectory) {
  const body = { radarId };
  if (filename) body.filename = filename;
  if (subdirectory) body.subdirectory = subdirectory;

  const response = await apiFetch(`${RECORDINGS_API}/record/start`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const error = await response.text();
    throw new Error(`Failed to start recording: ${error}`);
  }
  return response.json();
}

/**
 * Stop the current recording
 * @returns {Promise<Object>} Final recording status
 */
export async function stopRecording() {
  const response = await apiFetch(`${RECORDINGS_API}/record/stop`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to stop recording: ${response.status}`);
  }
  return response.json();
}

/**
 * Get current recording status
 * @returns {Promise<Object>} Recording status
 */
export async function getRecordingStatus() {
  const response = await apiFetch(`${RECORDINGS_API}/record/status`);
  if (!response.ok) {
    throw new Error(`Failed to get recording status: ${response.status}`);
  }
  return response.json();
}

/**
 * Load a recording for playback
 * @param {string} filename - The recording filename
 * @param {string} [subdirectory] - Optional subdirectory
 * @returns {Promise<Object>} Playback status with radarId
 */
export async function loadPlayback(filename, subdirectory) {
  const body = { filename };
  if (subdirectory) body.subdirectory = subdirectory;

  const response = await apiFetch(`${RECORDINGS_API}/playback/load`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const error = await response.text();
    throw new Error(`Failed to load recording: ${error}`);
  }
  return response.json();
}

/**
 * Start/resume playback
 * @returns {Promise<Object>} Playback status
 */
export async function playPlayback() {
  const response = await apiFetch(`${RECORDINGS_API}/playback/play`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to start playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Pause playback
 * @returns {Promise<Object>} Playback status
 */
export async function pausePlayback() {
  const response = await apiFetch(`${RECORDINGS_API}/playback/pause`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to pause playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Stop playback and unload
 * @returns {Promise<Object>} Playback status
 */
export async function stopPlayback() {
  const response = await apiFetch(`${RECORDINGS_API}/playback/stop`, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to stop playback: ${response.status}`);
  }
  return response.json();
}

/**
 * Seek to position in playback
 * @param {number} positionMs - Position in milliseconds
 * @returns {Promise<Object>} Playback status
 */
export async function seekPlayback(positionMs) {
  const response = await apiFetch(`${RECORDINGS_API}/playback/seek`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ positionMs }),
  });
  if (!response.ok) {
    throw new Error(`Failed to seek: ${response.status}`);
  }
  return response.json();
}

/**
 * Update playback settings
 * @param {Object} settings - Settings object { speed?, loopPlayback? }
 * @returns {Promise<Object>} Playback status
 */
export async function setPlaybackSettings(settings) {
  const response = await apiFetch(`${RECORDINGS_API}/playback/settings`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    throw new Error(`Failed to update playback settings: ${response.status}`);
  }
  return response.json();
}

/**
 * Get current playback status
 * @returns {Promise<Object>} Playback status
 */
export async function getPlaybackStatus() {
  const response = await apiFetch(`${RECORDINGS_API}/playback/status`);
  if (!response.ok) {
    throw new Error(`Failed to get playback status: ${response.status}`);
  }
  return response.json();
}
