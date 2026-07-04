export { PPI };

import { SpokeProcessorFactory } from "./spoke_processor.js";

// Fraction of the shorter canvas axis used as the outer radar range ring radius.
// The PPI is a circle inscribed in the canvas so the full set range is visible
// in every direction; the remaining margin leaves room for range labels.
const RANGE_SCALE = 0.9;

const NAUTICAL_MILE = 1852.0;

// Exponential-smoothing factor for the radar-derived heading. The bearing
// field on each spoke is quantized, so the per-spoke `bearing - angle` heading
// wobbles by a fraction of a degree frame to frame even with a steady boat.
// The overlay (AIS/ARPA/zones/rose) uses this smoothed heading to convert
// between geographic and bow-relative angles, so the wobble would show up as
// targets jumping. Smoothing the heading on its unit vector (circular-mean
// safe across the 360->0 wrap) kills the jitter while staying responsive: at
// typical spoke rates the time constant is well under the timescale of a real
// turn. The north-up raster itself does NOT use the smoothed value — each
// spoke is written at its own raw `spoke.bearing`, which is exact per spoke.
const HEADING_SMOOTH_ALPHA = 0.1;

// Overlay redraw threshold for live heading motion, in radians (~0.5°).
// Below this the overlay keeps its last paint; above it a redraw is
// scheduled so head-up AIS/ARPA placement and north-up zones/rose track
// the boat's yaw even when no other repaint trigger fires (e.g. standby).
const HEADING_REDRAW_EPSILON = (0.5 * Math.PI) / 180;

// In north-up mode spokes are written at their geographic bearing, so the
// display row can skip forward beyond the sweep's own step when the heading
// steps between spokes (SK heading arrives at a few Hz). Skipped rows would
// keep one-rotation-stale pixels; heading-induced forward gaps up to this
// fraction of a revolution are filled with the current spoke. Only the gap
// in EXCESS of the relative-angle step counts — radars like Furuno report
// 8192 angles but send every 4th-14th, and that natural stride must not be
// filled (it isn't in head-up either; it would multiply processing cost per
// spoke by the stride). Larger jumps mean a heading-source change, not yaw.
const NORTH_UP_MAX_FILL_FRACTION = 1 / 32;

// How long the radar-derived heading stays authoritative after the last
// bearing-carrying spoke. lastHeading is only updated by drawSpoke, so when
// spokes stop (standby, disconnect) it freezes; past this age a live Signal
// K heading takes over so overlays keep tracking the boat's yaw.
const RADAR_HEADING_FRESH_MS = 5000;

// Consecutive spokes without any usable heading before north-up gives up
// and auto-reverts to head-up. In north-up a heading-less spoke cannot be
// placed (writing it at its relative angle would corrupt the geographic
// raster), so such spokes are dropped; a sustained run means the heading
// source died and a frozen geographic image would just get staler.
const NORTH_UP_HEADINGLESS_REVERT_SPOKES = 32;

function divides_near(a, b) {
  let remainder = a % b;
  return remainder <= 1.0 || remainder >= b - 1;
}

function is_metric(v) {
  if (v <= 100) {
    return divides_near(v, 25);
  } else if (v <= 750) {
    return divides_near(v, 50);
  }
  return divides_near(v, 500);
}

// Format a distance for a VRM/EBL readout. Unlike `formatRangeValue` (which
// snaps to nice radar-tick fractions such as 1/4 nm), this returns a smooth
// 2-decimal value in the unit the operator already sees: nm imperial, m/km
// metric. Rounding to .01 nm (~18 m) or 1 m keeps the readout precise enough
// for navigation without showing raw float noise like "0.073077... nm".
function formatVrmDistance(metric, v) {
  if (metric) {
    if (v >= 1000) {
      return (v / 1000).toFixed(2) + " km";
    }
    return Math.round(v) + " m";
  }
  return (v / NAUTICAL_MILE).toFixed(2) + " nm";
}

function formatRangeValue(metric, v) {
  if (metric) {
    v = Math.round(v);
    if (v >= 1000) {
      return v / 1000 + " km";
    } else {
      return v + " m";
    }
  } else {
    if (v >= NAUTICAL_MILE - 1) {
      if (divides_near(v, NAUTICAL_MILE)) {
        return Math.floor((v + 1) / NAUTICAL_MILE) + " nm";
      } else {
        return v / NAUTICAL_MILE + " nm";
      }
    } else if (divides_near(v, NAUTICAL_MILE / 2)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 2)) + "/2 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 4)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 4)) + "/4 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 8)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 8)) + "/8 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 16)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 16)) + "/16 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 32)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 32)) + "/32 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 64)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 64)) + "/64 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 128)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 128)) + "/128 nm";
    } else {
      return v / NAUTICAL_MILE + " nm";
    }
  }
}

/**
 * PPI (Plan Position Indicator) - Manages the radar display overlay and spoke processing
 * This class is renderer-agnostic and can work with WebGPU, WebGL, or Canvas2D backends
 */
class PPI {
  /**
   * @param {object} renderer - Backend renderer (WebGPU, WebGL, etc.) with renderSpoke/render methods
   * @param {HTMLCanvasElement} overlayCanvas - Canvas element for overlay graphics
   * @param {HTMLCanvasElement} backgroundCanvas - Canvas element for background graphics
   */
  constructor(renderer, overlayCanvas, backgroundCanvas) {
    this.renderer = renderer;
    this.overlay_dom = overlayCanvas;
    this.overlay_ctx = overlayCanvas ? overlayCanvas.getContext("2d") : null;
    this.background_dom = backgroundCanvas;
    this.background_ctx = backgroundCanvas ? backgroundCanvas.getContext("2d") : null;

    // Display dimensions
    this.width = 0;
    this.height = 0;
    this.center_x = 0;
    this.center_y = 0;
    this.beam_length = 0;

    // Radar state
    this.range = 0;
    this.spoke_range = 0;
    this.lastHeading = null;
    // Smoothed unit vector of the radar-derived heading; null until the first
    // spoke carries a bearing, so smoothing starts from a real value rather
    // than pulling toward an arbitrary north.
    this.headingSin = null;
    this.headingCos = null;
    this.headingMode = "headingUp";
    this.trueHeading = null;
    // Last display row written in north-up mode and the relative angle it
    // belonged to (both reported-spoke units); used to fill the
    // heading-induced part of forward display gaps.
    this.lastWriteAngle = null;
    this.lastRelAngle = null;
    // Timestamp of the last radar-derived heading update, for the
    // freshness window that decides whether it may outrank Signal K.
    this.lastHeadingAt = 0;
    // Run length of heading-less spokes dropped in north-up, for the
    // auto-revert to head-up.
    this.headinglessSpokeCount = 0;
    // Heading the overlay was last painted with, and the pending-redraw
    // latch for the rAF-throttled heading-motion refresh.
    this.lastOverlayHeading = null;
    this._overlayRedrawPending = false;

    // Spoke data
    this.data = null;
    this.spokesPerRevolution = 0;
    this.reportedSpokesPerRevolution = 0; // Original value from radar, preserved for mode switching
    this.maxspokelength = 0;
    this.legend = null;

    // Spoke processing strategy
    this.spokeProcessor = null;
    this.processingMode = "auto"; // "auto", "clean", "fill", "reduce", or "smooth"

    // Buffer flush - wait for full rotation after standby/range change
    this.waitForRotation = false;
    this.waitStartAngle = -1;
    this.seenAngleWrap = false;
    this.lastWaitAngle = 0;

    // Power mode state
    this.powerMode = "off";
    this.onTimeSeconds = null;
    this.txTimeSeconds = null;

    // Guard zones and no-transmit sectors
    this.guardZones = [null, null];
    this.noTransmitSectors = [null, null, null, null];

    // Zone edit mode state
    this.editingZoneIndex = null;
    this.dragState = null;
    this.hoveredHandle = null;
    this.onZoneDragEnd = null;
    this.onZoneDragMove = null;

    // Zone create mode state (click-drag to define zone)
    this.creatingZoneIndex = null;
    this.creatingZoneType = null; // 'guard' or 'exclusion'
    this.creatingRectIndex = null;
    this.createDragStart = null;
    this.onZoneCreated = null;
    this.onRectCreated = null;

    // 3-click rect creation state
    // clickCount: 0=waiting for click1, 1=waiting for click2, 2=waiting for click3
    this.rectCreateClickCount = 0;
    this.rectCreateCorner1 = null; // {x, y} first corner
    this.rectCreateCorner2 = null; // {x, y} second corner (defines edge with corner1)

    // Exclusion zones and rects for stationary installations
    this.exclusionZones = [null, null, null, null];
    this.exclusionRects = [null, null, null, null];

    // Sector edit mode state
    this.editingSectorIndex = null;
    this.onSectorDragEnd = null;

    // Drag handlers bound state
    this._dragHandlersInstalled = false;

    // ARPA targets: Map of targetId -> target data
    this.targets = new Map();

    // AIS vessels: Map of mmsi -> vessel data
    this.aisVessels = new Map();
    this.showAis = true; // Default to showing AIS vessels

    // Target acquisition mode
    this.acquireTargetMode = false;
    this.onTargetAcquire = null; // Callback: (bearing, distance) => void

    // VRM/EBL markers - two independent markers, each combining a Variable
    // Range Marker (range ring) and an Electronic Bearing Line. Each marker
    // defines a point at (bearing, distance) shown with a draggable ring and
    // line plus a readout of true bearing and distance.
    // Each entry: { enabled: bool, bearing: rad (relative to bow), distance: meters }
    // bearing is stored relative to bow so HU/NU mode switches don't relocate it.
    this.vrmEbl = [
      { enabled: false, bearing: 0, distance: 0 },
      { enabled: false, bearing: 0, distance: 0 },
    ];
    this.onVrmEblChange = null; // Callback: (markers) => void (for persistence)
    this._vrmEblDrag = null;    // { index, mode: 'ring'|'line'|'point', startBearing, startDistance }

    // Display zoom (1.0 = 100%, range 0.33 to 3.0)
    this.displayZoom = 1.0;
  }

  /**
   * Zoom in the display by 10% (max 300%)
   */
  zoomIn() {
    this.displayZoom = Math.min(3.0, this.displayZoom * 1.1);
    this.redrawCanvas();
  }

  /**
   * Zoom out the display by 10% (min 33%)
   */
  zoomOut() {
    this.displayZoom = Math.max(0.33, this.displayZoom / 1.1);
    this.redrawCanvas();
  }

  /**
   * Get current display zoom factor
   */
  getDisplayZoom() {
    return this.displayZoom;
  }

  // ============================================================
  // VRM/EBL public API
  // ============================================================

  setVrmEblEnabled(index, enabled) {
    const marker = this.vrmEbl[index];
    if (!marker) return;
    if (marker.enabled === enabled) return;

    if (enabled && marker.distance <= 0) {
      // First-time enable: pick a sensible default of half the displayed range
      // on a north-easterly bearing so it lands in clear space.
      const rangeMeters = this.range || this.spoke_range || 1852;
      marker.distance = rangeMeters * 0.5;
      marker.bearing = index === 0 ? Math.PI / 4 : -Math.PI / 4;
    }
    marker.enabled = enabled;
    this.#installVrmEblHandlersIfNeeded();
    this.redrawCanvas();
    if (this.onVrmEblChange) this.onVrmEblChange(this.vrmEbl);
  }

  // Restore marker positions from external storage. The onVrmEblChange
  // callback is intentionally not fired here: this is the inverse of
  // persistence, so notifying the callback would write the just-restored
  // values straight back to localStorage.
  setVrmEblMarkers(markers) {
    if (!Array.isArray(markers)) return;
    for (let i = 0; i < Math.min(2, markers.length); i++) {
      const m = markers[i];
      if (!m) continue;
      const target = this.vrmEbl[i];
      if (typeof m.bearing === "number") target.bearing = m.bearing;
      if (typeof m.distance === "number") target.distance = m.distance;
      if (typeof m.enabled === "boolean") target.enabled = m.enabled;
    }
    this.#installVrmEblHandlersIfNeeded();
    this.redrawCanvas();
  }

  getVrmEblMarkers() {
    return this.vrmEbl.map((m) => ({ ...m }));
  }

  /**
   * Initialize spoke data buffers
   */
  setSpokes(spokesPerRevolution, maxspokelength) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.reportedSpokesPerRevolution = spokesPerRevolution; // Store original for mode switching
    this.maxspokelength = maxspokelength;
    this.data = new Uint8Array(spokesPerRevolution * maxspokelength);
    this.#resetWriteTracking();

    // Create spoke processor if legend is available
    if (this.legend) {
      this.#createSpokeProcessor();
    }
  }

  setRange(range) {
    this.range = range;
    if (this.data) {
      this.data.fill(0);
    }
    this.#resetWriteTracking();
    // Update renderer scale when control range changes
    if (this.renderer && this.renderer.setRangeScale) {
      this.renderer.setRangeScale(this.range, this.spoke_range || this.range);
    }
    this.redrawCanvas();
  }

  setHeadingMode(mode) {
    if (mode === this.headingMode) {
      return mode;
    }
    // North-up needs a heading; reverting to head-up must always work
    // (it is the forced fallback when the heading source dies).
    if (mode === "northUp" && !this.hasHeading()) {
      return "headingUp";
    }
    // The raster write frame changes with the mode (relative angle vs
    // geographic bearing), so rotate the existing rows by the raster
    // frame's heading to keep the image continuous instead of blanking it
    // for a full sweep. All write tracking is in the old frame — drop it.
    const heading = this.#displayHeadingRad();
    this.headingMode = mode;
    if (heading !== null) {
      this.#remapRaster(mode === "northUp" ? heading : -heading);
    } else if (this.data) {
      // No heading to re-base with (source just died): the old rows'
      // frame is unknowable, so clear rather than show a mixed frame.
      this.data.fill(0);
    }
    this.#resetWriteTracking();
    // redrawCanvas re-uploads the (remapped) raster after the renderer
    // transform update, so the switch repaints without a blank frame.
    this.redrawCanvas();
    return mode;
  }

  setTrueHeading(heading) {
    this.trueHeading = heading;
    this.#maybeRefreshOverlayForHeading();
  }

  getTrueHeading() {
    return this.trueHeading;
  }

  // True while the radar-derived heading is recent enough to describe the
  // boat's current attitude. It only updates while bearing-carrying
  // spokes arrive, so it freezes in standby/disconnect and must then
  // yield to a live Signal K heading.
  #radarHeadingFresh() {
    return (
      this.lastHeading !== null &&
      performance.now() - this.lastHeadingAt < RADAR_HEADING_FRESH_MS
    );
  }

  // Heading of the raster's write frame, in radians. The spoke raster is
  // placed with the radar's own heading (per-spoke `bearing - angle`), so
  // anything that must stay glued to the echoes — display rotation of
  // relative geometry, ARPA targets (server-computed in spoke-bearing
  // frame), MARPA acquire clicks — uses this. Preference: fresh
  // radar-derived, then live Signal K, then a stale radar-derived value
  // (better than nothing on boats with no other source). Null when no
  // source exists.
  #displayHeadingRad() {
    if (this.#radarHeadingFresh()) return (this.lastHeading * Math.PI) / 180;
    if (this.trueHeading !== null) return this.trueHeading;
    if (this.lastHeading !== null) return (this.lastHeading * Math.PI) / 180;
    return null;
  }

  // The boat's true heading, in radians, for converting TRUE-frame data
  // (AIS bearings computed from lat/lon, vessel COG/heading) into the
  // relative frame. Prefers the live Signal K true heading; falls back to
  // the radar-derived heading. Distinct from #displayHeadingRad so a
  // radar fed a magnetic or misaligned heading doesn't drag the AIS
  // overlay with it. Null when no source is available.
  #trueHeadingRad() {
    if (this.trueHeading !== null) return this.trueHeading;
    return this.#displayHeadingRad();
  }

  // Screen rotation applied to bow-relative angles, evaluated live at
  // draw/hit-test time. In north-up mode the raster rows are geographic,
  // so relative geometry (guard zones, sectors, VRM/EBL) rotates by the
  // current heading; in head-up mode the raster is bow-up and nothing
  // rotates. This replaces the old frozen `headingRotation` field that
  // was only refreshed on redrawCanvas and made the display drift.
  #displayRotation() {
    if (this.headingMode !== "northUp") return 0;
    return this.#displayHeadingRad() ?? 0;
  }

  // Drop all write-frame tracking (PPI-side and processor-side). Call
  // whenever the raster is cleared, reallocated, or its frame changes.
  #resetWriteTracking() {
    this.lastWriteAngle = null;
    this.lastRelAngle = null;
    this.headinglessSpokeCount = 0;
    if (this.spokeProcessor) {
      this.spokeProcessor.resetWriteTracking();
    }
  }

  // Rotate every raster row by the given angle (radians clockwise) so a
  // heading-mode switch re-bases the existing image into the new write
  // frame in place. The renderers re-upload the full buffer on the next
  // draw, so no renderer-side support is needed.
  #remapRaster(offsetRad) {
    if (!this.data || !this.spokesPerRevolution || !this.maxspokelength) return;
    const rows = this.spokesPerRevolution;
    const len = this.maxspokelength;
    let shift = Math.round((offsetRad / (2 * Math.PI)) * rows) % rows;
    if (shift < 0) shift += rows;
    if (shift === 0) return;
    const src = this.data.slice();
    for (let i = 0; i < rows; i++) {
      const dst = ((i + shift) % rows) * len;
      this.data.set(src.subarray(i * len, i * len + len), dst);
    }
  }

  // Repaint the overlay when the live heading has moved materially since
  // the overlay was last painted, or when heading availability flipped
  // (AIS/ARPA drawing is gated on having one). rAF-throttled: drawSpoke
  // calls this at spoke rate and Signal K heading deltas at a few Hz.
  #maybeRefreshOverlayForHeading() {
    const h = this.#displayHeadingRad();
    if (h === null && this.lastOverlayHeading === null) return;
    if (h !== null && this.lastOverlayHeading !== null) {
      let diff = Math.abs(h - this.lastOverlayHeading) % (2 * Math.PI);
      if (diff > Math.PI) diff = 2 * Math.PI - diff;
      if (diff < HEADING_REDRAW_EPSILON) return;
    }
    if (this._overlayRedrawPending) return;
    this._overlayRedrawPending = true;
    requestAnimationFrame(() => {
      this._overlayRedrawPending = false;
      this.#drawOverlay();
    });
  }

  // True when any heading reference is available — the SignalK true
  // heading or the radar-derived heading. AIS placement needs one of
  // these; the UI grays the AIS toggle when neither is present.
  hasHeading() {
    return this.trueHeading !== null || this.lastHeading !== null;
  }

  getHeadingMode() {
    return this.headingMode;
  }

  /**
   * Set acquire target mode
   * @param {boolean} enabled - Whether acquire mode is active
   * @param {function} callback - Callback function(bearing, distance) when target is acquired
   */
  setAcquireTargetMode(enabled, callback = null) {
    console.log(`PPI: setAcquireTargetMode(${enabled}), callback=${callback ? 'provided' : 'null'}`);
    this.acquireTargetMode = enabled;
    this.onTargetAcquire = callback;
    // Update pointer events to enable/disable click handling
    this.#updatePointerEvents();
  }

  setPowerMode(powerMode, onTimeSeconds, txTimeSeconds) {
    const isStandby = powerMode !== "transmit";
    const wasStandby = this.powerMode !== "transmit";
    this.powerMode = powerMode;
    this.onTimeSeconds = onTimeSeconds ?? null;
    this.txTimeSeconds = txTimeSeconds ?? null;

    if (isStandby && !wasStandby) {
      this.clearRadarDisplay();
    } else if (!isStandby && wasStandby) {
      this.clearRadarDisplay();
    }

    this.redrawCanvas();
  }

  clearRadarDisplay() {
    if (this.data) {
      this.data.fill(0);
    }
    this.#resetWriteTracking();
    if (this.spokeProcessor) {
      this.spokeProcessor.reset();
    }

    if (this.spokeProcessor && this.spokeProcessor.needsRotationWait()) {
      this.waitForRotation = true;
      this.waitStartAngle = -1;
      this.seenAngleWrap = false;
    } else {
      this.waitForRotation = false;
    }

    // Tell renderer to clear its display
    if (this.renderer && this.renderer.clearDisplay) {
      this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
    }
  }

  setProcessingMode(mode) {
    if (this.processingMode !== mode) {
      console.log(
        `PPI: setProcessingMode ${this.processingMode} -> ${mode}, ` +
          `spokes: ${this.spokesPerRevolution}, reported: ${this.reportedSpokesPerRevolution}`
      );

      // If switching away from reduce mode, restore original spoke count
      if (
        this.spokesPerRevolution !== this.reportedSpokesPerRevolution &&
        this.reportedSpokesPerRevolution > 0
      ) {
        console.log(
          `PPI: Restoring spoke count from ${this.spokesPerRevolution} to ${this.reportedSpokesPerRevolution}`
        );
        this.spokesPerRevolution = this.reportedSpokesPerRevolution;
        this.data = new Uint8Array(this.spokesPerRevolution * this.maxspokelength);

        if (this.renderer && this.renderer.setSpokes) {
          this.renderer.setSpokes(this.spokesPerRevolution, this.maxspokelength);
        }
      }

      this.processingMode = mode;
      this.#createSpokeProcessor();
      this.clearRadarDisplay();
    }
  }

  setGuardZone(index, zone) {
    if (index >= 0 && index < 2) {
      this.guardZones[index] = zone;
      this.redrawCanvas();
    }
  }

  setExclusionZone(index, zone) {
    if (index >= 0 && index < 4) {
      this.exclusionZones[index] = zone;
      this.redrawCanvas();
    }
  }

  setExclusionRect(index, rect) {
    if (index >= 0 && index < 4) {
      this.exclusionRects[index] = rect;
      this.redrawCanvas();
    }
  }

  setNoTransmitSector(index, sector) {
    if (index >= 0 && index < 4) {
      this.noTransmitSectors[index] = sector;
      this.redrawCanvas();
    }
  }

  setEditingZone(index, onDragEnd = null, onDragMove = null) {
    this.editingZoneIndex = index;
    this.onZoneDragEnd = onDragEnd;
    this.onZoneDragMove = onDragMove;
    this.dragState = null;
    this.hoveredHandle = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  setEditingSector(index, onDragEnd = null) {
    this.editingSectorIndex = index;
    this.onSectorDragEnd = onDragEnd;
    this.dragState = null;
    this.hoveredHandle = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  /**
   * Enter zone creation mode - click-drag to define a new zone
   * @param {number} index - Zone index (0-3 for guard zones, or exclusion zone index)
   * @param {function} onCreated - Callback (index, zone) when zone is created
   * @param {string} zoneType - 'guard' or 'exclusion' (default: 'guard')
   */
  setCreatingZone(index, onCreated = null, zoneType = "guard") {
    this.creatingZoneIndex = index;
    this.creatingZoneType = zoneType;
    this.creatingRectIndex = null;
    this.editingZoneIndex = null;
    this.editingSectorIndex = null;
    this.onZoneCreated = onCreated;
    this.dragState = null;
    this.createDragStart = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  /**
   * Enter rectangle creation mode - 3 clicks to define a rectangle
   * Click 1: first corner, Click 2: second corner (defines edge), Click 3: width
   * @param {number} index - Rectangle index
   * @param {function} onCreated - Callback (index, rect) when rect is created
   */
  setCreatingRect(index, onCreated = null) {
    this.creatingRectIndex = index;
    this.creatingZoneIndex = null;
    this.creatingZoneType = null;
    this.editingZoneIndex = null;
    this.editingSectorIndex = null;
    this.onRectCreated = onCreated;
    this.dragState = null;
    this.createDragStart = null;
    // Reset 3-click state
    this.rectCreateClickCount = 0;
    this.rectCreateCorner1 = null;
    this.rectCreateCorner2 = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  /**
   * Exit creation mode without creating anything
   */
  cancelCreating() {
    this.creatingZoneIndex = null;
    this.creatingZoneType = null;
    this.creatingRectIndex = null;
    this.createDragStart = null;
    this.dragState = null;
    // Reset 3-click state
    this.rectCreateClickCount = 0;
    this.rectCreateCorner1 = null;
    this.rectCreateCorner2 = null;

    this.#updatePointerEvents();
    this.redrawCanvas();
  }

  /**
   * Update or add an ARPA target
   * @param {number} id - Target ID
   * @param {object} data - Target data from server (ArpaTargetApi format)
   */
  updateTarget(id, data) {
    this.targets.set(id, data);
    this.#drawOverlay();
  }

  /**
   * Remove an ARPA target (target lost)
   * @param {number} id - Target ID
   */
  removeTarget(id) {
    this.targets.delete(id);
    this.#drawOverlay();
  }

  /**
   * Update or add an AIS vessel
   * @param {string} mmsi - Vessel MMSI
   * @param {object} data - AIS vessel data from server
   */
  updateAisVessel(mmsi, data) {
    this.aisVessels.set(mmsi, data);
    this.#drawOverlay();
  }

  /**
   * Remove an AIS vessel (lost)
   * @param {string} mmsi - Vessel MMSI
   */
  removeAisVessel(mmsi) {
    this.aisVessels.delete(mmsi);
    this.#drawOverlay();
  }

  /**
   * Set whether to show AIS vessels
   * @param {boolean} show - Whether to show AIS vessels
   */
  setShowAis(show) {
    this.showAis = show;
    this.#drawOverlay();
  }

  /**
   * Clear all AIS vessels from the display
   */
  clearAisVessels() {
    this.aisVessels.clear();
    this.#drawOverlay();
  }

  setLegend(legend) {
    this.legend = this.#convertServerLegend(legend);

    // Create spoke processor now that we have legend
    if (this.spokesPerRevolution) {
      this.#createSpokeProcessor();
    }

    // Pass legend to renderer for color table
    if (this.renderer && this.renderer.setLegend) {
      this.renderer.setLegend(this.legend);
    }
  }

  #convertServerLegend(serverLegend) {
    const colors = new Array(256);

    for (let i = 0; i < 256; i++) {
      colors[i] = [255, 0, 0, 255];
    }

    for (let i = 0; i < serverLegend.pixels.length && i < 256; i++) {
      const entry = serverLegend.pixels[i];
      if (entry.color) {
        colors[i] = this.#hexToRGBA(entry.color);
      }
    }

    return {
      colors: colors,
      lowReturn: serverLegend.lowReturn,
      mediumReturn: serverLegend.mediumReturn,
      strongReturn: serverLegend.strongReturn,
      specialStart: serverLegend.pixelColors,
    };
  }

  #hexToRGBA(hex) {
    let a = [];
    for (let i = 1; i < hex.length; i += 2) {
      a.push(parseInt(hex.slice(i, i + 2), 16));
    }
    while (a.length < 3) {
      a.push(0);
    }
    while (a.length < 4) {
      a.push(255);
    }
    return a;
  }

  #createSpokeProcessor() {
    if (!this.legend || !this.spokesPerRevolution) {
      return;
    }
    this.spokeProcessor = SpokeProcessorFactory.create(
      this.processingMode,
      this.spokesPerRevolution,
      this.legend
    );

    // Set up calibration callback for reduce processor
    // Use setTimeout to defer resize until after current spoke processing completes
    if (this.spokeProcessor.setCalibrationCallback) {
      this.spokeProcessor.setCalibrationCallback((actualSpokes) => {
        setTimeout(() => this.#resizeBufferForActualSpokes(actualSpokes), 0);
      });
    }
  }

  /**
   * Resize buffer when reduce processor determines actual spoke count
   */
  #resizeBufferForActualSpokes(actualSpokes) {
    console.log(`PPI: Resizing buffer from ${this.spokesPerRevolution} to ${actualSpokes} spokes`);

    // Store original values for reference
    const originalSpokes = this.spokesPerRevolution;

    // Update spoke count
    this.spokesPerRevolution = actualSpokes;

    // Resize data buffer
    this.data = new Uint8Array(actualSpokes * this.maxspokelength);
    this.#resetWriteTracking();

    // Notify renderer of new dimensions
    if (this.renderer && this.renderer.setSpokes) {
      this.renderer.setSpokes(actualSpokes, this.maxspokelength);
    }

    // Clear display with new buffer
    if (this.renderer && this.renderer.clearDisplay) {
      this.renderer.clearDisplay(this.data, actualSpokes, this.maxspokelength);
    }

    console.log(`PPI: Buffer resized from ${originalSpokes} to ${actualSpokes} spokes`);
  }

  /**
   * Process and draw a spoke
   */
  drawSpoke(spoke) {
    if (!this.data || !this.legend || !this.spokeProcessor) return;

    // Extract heading from spoke if available
    // Heading = bearing - angle (geographic bearing minus relative angle)
    // angle/bearing stay in the radar's reported units even after the
    // reduce processor shrinks the buffer, so use the reported count here.
    const angleUnits = this.reportedSpokesPerRevolution || this.spokesPerRevolution;
    if (spoke.bearing !== undefined && spoke.angle !== undefined) {
      const heading = (spoke.bearing + angleUnits - spoke.angle) % angleUnits;
      const headingRad = (heading * 2 * Math.PI) / angleUnits;
      const sin = Math.sin(headingRad);
      const cos = Math.cos(headingRad);
      if (this.headingSin === null) {
        // First valid heading: seed the smoother so it converges from the
        // real value, not from north.
        this.headingSin = sin;
        this.headingCos = cos;
      } else {
        this.headingSin += (sin - this.headingSin) * HEADING_SMOOTH_ALPHA;
        this.headingCos += (cos - this.headingCos) * HEADING_SMOOTH_ALPHA;
      }
      const smoothed = Math.atan2(this.headingSin, this.headingCos);
      this.lastHeading = ((smoothed * 180) / Math.PI + 360) % 360;
      this.lastHeadingAt = performance.now();
    } else if (!this.#radarHeadingFresh()) {
      // Only drop the radar-derived heading once it has gone stale: a
      // marginal heading feed can flap bearing present/absent between
      // spokes, and nulling immediately would bounce every consumer
      // (and the north-up write frame) to another source mid-rotation.
      this.lastHeading = null;
      this.headingSin = null;
      this.headingCos = null;
    }
    this.#maybeRefreshOverlayForHeading();

    // Don't draw spokes in standby mode
    if (this.powerMode !== "transmit") {
      if (this.spokeProcessor.needsRotationWait()) {
        this.waitForRotation = true;
        this.waitStartAngle = -1;
        this.seenAngleWrap = false;
      }
      return;
    }

    // Check spoke angle bounds - but reduce processor handles angle scaling internally,
    // so it may receive angles larger than the current buffer size
    const maxValidAngle =
      "reportedSpokesPerRevolution" in this.spokeProcessor
        ? this.spokeProcessor.reportedSpokesPerRevolution
        : this.spokesPerRevolution;
    if (spoke.angle >= maxValidAngle) {
      console.error(`Bad spoke angle: ${spoke.angle} >= ${maxValidAngle}`);
      return;
    }

    // Wait for full rotation
    if (this.waitForRotation) {
      if (this.waitStartAngle < 0) {
        this.waitStartAngle = spoke.angle;
        this.lastWaitAngle = spoke.angle;
        return;
      }

      if (
        !this.seenAngleWrap &&
        spoke.angle < this.lastWaitAngle - this.spokesPerRevolution / 2
      ) {
        this.seenAngleWrap = true;
      }

      if (this.seenAngleWrap && spoke.angle >= this.waitStartAngle) {
        this.waitForRotation = false;
        if (this.spokeProcessor) {
          this.spokeProcessor.reset();
        }
        if (this.data) this.data.fill(0);
        this.#resetWriteTracking();
        if (this.renderer && this.renderer.clearDisplay) {
          this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
        }
      } else {
        this.lastWaitAngle = spoke.angle;
        return;
      }
    }

    // Handle range changes
    if (this.spoke_range !== spoke.range) {
      const wasInitialRange = this.spoke_range === 0;
      this.spoke_range = spoke.range;
      this.data.fill(0);
      this.#resetWriteTracking();
      if (this.spokeProcessor) {
        this.spokeProcessor.reset();
      }
      // Update renderer scale when spoke range changes
      if (this.renderer && this.renderer.setRangeScale) {
        this.renderer.setRangeScale(this.range || this.spoke_range, this.spoke_range);
      }
      this.redrawCanvas();

      if (!wasInitialRange && this.spokeProcessor.needsRotationWait()) {
        this.waitForRotation = true;
        this.waitStartAngle = -1;
        this.seenAngleWrap = false;
        if (this.renderer && this.renderer.clearDisplay) {
          this.renderer.clearDisplay(this.data, this.spokesPerRevolution, this.maxspokelength);
        }
        return;
      }
    }

    // Update rotation tracking — always on the relative spoke.angle: the
    // physical sweep is monotonic while the geographic bearing is not.
    this.spokeProcessor.updateRotationTracking(spoke.angle, this.spokesPerRevolution);

    // North-up: write the spoke at its geographic bearing so every echo
    // lands (and stays) at its true direction regardless of yaw. The
    // per-spoke `bearing` from the radar is exact; fall back to rotating
    // the relative angle by the raster-frame heading when a spoke lacks
    // it (same frame as the bearing, so a flapping bearing feed doesn't
    // double-expose the image). writeAngle stays in reported units — the
    // reduce processor scales it to its shrunken buffer the same way it
    // scales spoke.angle.
    let writeAngle = null;
    if (this.headingMode === "northUp") {
      if (spoke.bearing !== undefined) {
        writeAngle = spoke.bearing % angleUnits;
      } else {
        const headingRad = this.#displayHeadingRad();
        if (headingRad !== null) {
          const headingSpokes = Math.round(
            (headingRad / (2 * Math.PI)) * angleUnits
          );
          writeAngle = (spoke.angle + headingSpokes + angleUnits) % angleUnits;
        }
      }

      if (writeAngle === null) {
        // No usable heading: this spoke cannot be placed geographically.
        // Writing it at its relative angle would mix write frames in one
        // raster, so drop it — and if the heading source stays dead,
        // revert to head-up rather than freeze under a "N Up" label.
        // (setHeadingMode with no heading clears the raster; the viewer
        // re-syncs its toggle from getHeadingMode() on spoke batches.)
        this.headinglessSpokeCount++;
        if (this.headinglessSpokeCount >= NORTH_UP_HEADINGLESS_REVERT_SPOKES) {
          this.setHeadingMode("headingUp");
        }
        return;
      }
      this.headinglessSpokeCount = 0;

      // Fill the heading-induced part of a forward display gap with this
      // spoke so no row keeps stale pixels when the boat yaws. The
      // sweep's own step (relStep — >1 on radars that report more angles
      // than they send, e.g. Furuno's 4-14 stride) is NOT a gap: those
      // rows aren't written in head-up either, and filling them would
      // multiply processing cost per spoke by the stride.
      if (this.lastWriteAngle !== null && this.lastRelAngle !== null) {
        const relStep =
          (spoke.angle - this.lastRelAngle + angleUnits) % angleUnits;
        const expected = (this.lastWriteAngle + relStep) % angleUnits;
        const gap = (writeAngle - expected + angleUnits) % angleUnits;
        const maxFill = angleUnits * NORTH_UP_MAX_FILL_FRACTION;
        if (gap > 0 && gap <= maxFill && relStep <= maxFill) {
          for (let a = 0; a < gap; a++) {
            this.spokeProcessor.processSpoke(
              this.data,
              spoke,
              this.spokesPerRevolution,
              this.maxspokelength,
              (expected + a) % angleUnits
            );
          }
        }
      }
      this.lastWriteAngle = writeAngle;
      this.lastRelAngle = spoke.angle;
    }

    // Process spoke using current strategy
    this.spokeProcessor.processSpoke(
      this.data,
      spoke,
      this.spokesPerRevolution,
      this.maxspokelength,
      writeAngle ?? undefined
    );
  }

  /**
   * Render the current spoke data to the display
   */
  render() {
    if (this.renderer && this.renderer.render) {
      this.renderer.render(this.data, this.spokesPerRevolution, this.maxspokelength);
    }
  }

  /**
   * Resize and redraw the canvas
   */
  redrawCanvas() {
    const parent = this.overlay_dom?.parentNode;
    if (!parent) return;

    const styles = getComputedStyle(parent);
    const w = parseInt(styles.getPropertyValue("width"), 10);
    const h = parseInt(styles.getPropertyValue("height"), 10);

    // Assigning to canvas.width / .height clears the bitmap regardless
    // of whether the value actually changed. Without the size-changed
    // guard, every control update (guard-zone save, exclusion edit,
    // no-transmit sector change, range/zoom update) that fires
    // redrawCanvas wipes the overlay/background canvases, producing a
    // visible flash of "spokes disappear, then re-paint over the next
    // sweep". Only touch the canvas dimensions when the parent
    // actually resized. The renderer applies the same guard internally
    // so transform-only updates (range scale, beam length) don't clear
    // the spoke canvas either.
    const sizeChanged = this.width !== w || this.height !== h;

    if (sizeChanged) {
      if (this.overlay_dom) {
        this.overlay_dom.width = w;
        this.overlay_dom.height = h;
      }
      if (this.background_dom) {
        this.background_dom.width = w;
        this.background_dom.height = h;
      }
    }

    this.width = w;
    this.height = h;
    this.center_x = w / 2;
    this.center_y = h / 2;
    this.beam_length = Math.trunc(Math.min(this.center_x, this.center_y) * RANGE_SCALE * this.displayZoom);

    // Draw overlay
    this.#drawOverlay();

    // Notify renderer of resize. The shader rotation is permanently 0:
    // north-up is achieved by writing spokes at their geographic bearing,
    // not by rotating the whole (mixed-age) image, which drifted as the
    // boat yawed between redraws.
    if (this.renderer && this.renderer.resize) {
      this.renderer.resize(w, h, this.beam_length, 0);
      // WebGL's transform update clears its canvas without redrawing
      // (WebGPU keeps the previous frame) — re-upload the current raster
      // so a redraw never leaves a blank or stale spoke image behind,
      // even when no spokes are flowing (standby, mode switch).
      if (this.renderer.clearDisplay && this.data) {
        this.renderer.clearDisplay(
          this.data,
          this.spokesPerRevolution,
          this.maxspokelength
        );
      }
    }
  }

  // ============================================================
  // Overlay drawing
  // ============================================================

  #drawOverlay() {
    if (!this.overlay_ctx) return;

    const ctx = this.overlay_ctx;
    const range = this.range;
    // Remember the heading this paint used so the heading-motion refresh
    // knows when the overlay is materially out of date.
    this.lastOverlayHeading = this.#displayHeadingRad();

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.width, this.height);

    // Draw no-transmit sectors
    for (const sector of this.noTransmitSectors) {
      this.#drawNoTransmitSector(ctx, sector, "rgba(255, 255, 200, 0.25)", "rgba(200, 200, 0, 0.6)");
    }

    // Draw guard zones
    this.#drawGuardZone(ctx, this.guardZones[0], "rgba(144, 238, 144, 0.25)", "rgba(0, 128, 0, 0.6)");
    this.#drawGuardZone(ctx, this.guardZones[1], "rgba(173, 216, 230, 0.25)", "rgba(0, 0, 255, 0.6)");

    // Draw all exclusion zones and rects as a single composite shape
    // to avoid overlapping transparency issues
    this.#drawAllExclusions(ctx);

    // Draw drag handles if editing
    if (this.editingZoneIndex !== null) {
      const zone = this.guardZones[this.editingZoneIndex];
      this.#drawDragHandles(ctx, zone);
    }
    if (this.editingSectorIndex !== null) {
      const sector = this.noTransmitSectors[this.editingSectorIndex];
      this.#drawSectorDragHandles(ctx, sector);
    }

    // Draw ARPA targets
    this.#drawTargets(ctx, range);

    // Draw AIS vessels
    if (this.showAis) {
      this.#drawAisVessels(ctx, range);
    }

    // Draw standby overlay
    if (this.powerMode !== "transmit") {
      this.#drawStandbyOverlay(ctx);
    }

    // Draw range rings
    this.#drawRangeRings(ctx, range);

    // Draw compass rose
    this.#drawCompassRose(ctx);

    // Draw VRM/EBL markers on top so labels stay readable
    this.#drawVrmEbl(ctx, range);
  }

  #drawStandbyOverlay(ctx) {
    ctx.save();

    ctx.fillStyle = "white";
    ctx.font = "bold 36px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    ctx.shadowColor = "black";
    ctx.shadowBlur = 4;
    ctx.shadowOffsetX = 2;
    ctx.shadowOffsetY = 2;

    if (this.powerMode === "disconnected") {
      ctx.fillText("DISCONNECTED", this.center_x, this.center_y);
      ctx.restore();
      return;
    }

    const hasOnTime = this.onTimeSeconds !== null;
    const hasTxTime = this.txTimeSeconds !== null;
    const showTimes =
      this.powerMode === "standby" && (hasOnTime || hasTxTime);
    const standbyY = showTimes ? this.center_y - 40 : this.center_y;

    ctx.fillText(this.powerMode.toUpperCase(), this.center_x, standbyY);

    if (showTimes) {
      ctx.font = "bold 20px/1 Verdana, Geneva, sans-serif";
      let yOffset = this.center_y + 10;

      if (hasOnTime) {
        const onTimeStr = this.#formatSecondsAsTimeZero(this.onTimeSeconds);
        ctx.fillText("ON-TIME: " + onTimeStr, this.center_x, yOffset);
        yOffset += 30;
      }

      if (hasTxTime) {
        const txTimeStr = this.#formatSecondsAsTimeZero(this.txTimeSeconds);
        ctx.fillText("TX-TIME: " + txTimeStr, this.center_x, yOffset);
      }
    }

    ctx.restore();
  }

  #formatSecondsAsTimeZero(totalSeconds) {
    totalSeconds = Math.floor(totalSeconds);
    const days = Math.floor(totalSeconds / 86400);
    const remainingAfterDays = totalSeconds % 86400;
    const hours = Math.floor(remainingAfterDays / 3600);
    const minutes = Math.floor((remainingAfterDays % 3600) / 60);
    const seconds = remainingAfterDays % 60;

    const hh = hours.toString().padStart(2, "0");
    const mm = minutes.toString().padStart(2, "0");
    const ss = seconds.toString().padStart(2, "0");

    return `${days}.${hh}:${mm}:${ss}`;
  }

  #drawNoTransmitSector(ctx, sector, fillColor, strokeColor) {
    if (!sector) return;

    const radius = this.beam_length * 3;
    if (radius <= 0) return;

    // Sector angles are bow-relative; rotate by the live heading in north-up
    const rot = this.#displayRotation();
    const startAngle = sector.startAngle + rot - Math.PI / 2;
    const endAngle = sector.endAngle + rot - Math.PI / 2;
    const isCircle = Math.abs(sector.endAngle - sector.startAngle) < 0.001;

    ctx.beginPath();
    if (isCircle) {
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
    } else {
      ctx.moveTo(this.center_x, this.center_y);
      ctx.arc(this.center_x, this.center_y, radius, startAngle, endAngle);
      ctx.closePath();
    }

    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  #drawGuardZone(ctx, zone, fillColor, strokeColor) {
    if (!zone) return;

    // Use the Control Range for UI drawing
    if (!this.range || this.range <= 0) return;

    const pixelsPerMeter = this.beam_length / this.range;
    const innerRadius = zone.startDistance * pixelsPerMeter;
    const outerRadius = zone.endDistance * pixelsPerMeter;

    if (outerRadius <= 0) return;

    // Zone angles are bow-relative; rotate by the live heading in north-up
    const rot = this.#displayRotation();
    const startAngle = zone.startAngle + rot - Math.PI / 2;
    const endAngle = zone.endAngle + rot - Math.PI / 2;
    const isCircle = Math.abs(zone.endAngle - zone.startAngle) < 0.001;

    ctx.beginPath();
    if (isCircle) {
      ctx.arc(this.center_x, this.center_y, outerRadius, 0, 2 * Math.PI);
      if (innerRadius > 0) {
        ctx.moveTo(this.center_x + innerRadius, this.center_y);
        ctx.arc(this.center_x, this.center_y, innerRadius, 0, 2 * Math.PI, true);
      }
    } else {
      ctx.arc(this.center_x, this.center_y, outerRadius, startAngle, endAngle);
      if (innerRadius > 0) {
        ctx.arc(this.center_x, this.center_y, innerRadius, endAngle, startAngle, true);
      } else {
        ctx.lineTo(this.center_x, this.center_y);
      }
      ctx.closePath();
    }

    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  /**
   * Draw all exclusion zones and rects as a single composite shape.
   * This prevents overlapping transparency from making overlapped areas lighter.
   */
  #drawAllExclusions(ctx) {
    if (!this.range || this.range <= 0) return;

    const pixelsPerMeter = this.beam_length / this.range;
    const rot = this.#displayRotation();
    const cos_h = Math.cos(rot);
    const sin_h = Math.sin(rot);

    // Collect all shapes to draw
    const shapes = [];

    // Add sector exclusion zones
    for (const zone of this.exclusionZones) {
      if (!zone) continue;

      const innerRadius = zone.startDistance * pixelsPerMeter;
      const outerRadius = zone.endDistance * pixelsPerMeter;
      if (outerRadius <= 0) continue;

      const startAngle = zone.startAngle + rot - Math.PI / 2;
      const endAngle = zone.endAngle + rot - Math.PI / 2;
      const isCircle = Math.abs(zone.endAngle - zone.startAngle) < 0.001;

      shapes.push({ type: 'sector', innerRadius, outerRadius, startAngle, endAngle, isCircle });
    }

    // Add rectangular exclusion zones
    for (const rect of this.exclusionRects) {
      if (!rect || !rect.enabled) continue;

      const dx = rect.x2 - rect.x1;
      const dy = rect.y2 - rect.y1;
      const edgeLen = Math.sqrt(dx * dx + dy * dy);
      if (edgeLen < 0.001 || (rect.width ?? 0) < 0.001) continue;

      const perpX = dy / edgeLen;
      const perpY = -dx / edgeLen;

      const corners = [
        { x: rect.x1, y: rect.y1 },
        { x: rect.x2, y: rect.y2 },
        { x: rect.x2 + perpX * rect.width, y: rect.y2 + perpY * rect.width },
        { x: rect.x1 + perpX * rect.width, y: rect.y1 + perpY * rect.width },
      ];

      const canvasCorners = corners.map(c => {
        const rx = c.x * cos_h - c.y * sin_h;
        const ry = c.x * sin_h + c.y * cos_h;
        return {
          x: this.center_x + rx * pixelsPerMeter,
          y: this.center_y - ry * pixelsPerMeter,
        };
      });

      shapes.push({ type: 'rect', corners: canvasCorners });
    }

    if (shapes.length === 0) return;

    // Draw all shapes as a single composite path for fill
    ctx.beginPath();
    for (const shape of shapes) {
      if (shape.type === 'sector') {
        if (shape.isCircle) {
          ctx.moveTo(this.center_x + shape.outerRadius, this.center_y);
          ctx.arc(this.center_x, this.center_y, shape.outerRadius, 0, 2 * Math.PI);
          if (shape.innerRadius > 0) {
            ctx.moveTo(this.center_x + shape.innerRadius, this.center_y);
            ctx.arc(this.center_x, this.center_y, shape.innerRadius, 0, 2 * Math.PI, true);
          }
        } else {
          const outerStartX = this.center_x + shape.outerRadius * Math.cos(shape.startAngle);
          const outerStartY = this.center_y + shape.outerRadius * Math.sin(shape.startAngle);
          ctx.moveTo(outerStartX, outerStartY);
          ctx.arc(this.center_x, this.center_y, shape.outerRadius, shape.startAngle, shape.endAngle);
          if (shape.innerRadius > 0) {
            ctx.arc(this.center_x, this.center_y, shape.innerRadius, shape.endAngle, shape.startAngle, true);
          } else {
            ctx.lineTo(this.center_x, this.center_y);
          }
          ctx.closePath();
        }
      } else if (shape.type === 'rect') {
        ctx.moveTo(shape.corners[0].x, shape.corners[0].y);
        for (let i = 1; i < shape.corners.length; i++) {
          ctx.lineTo(shape.corners[i].x, shape.corners[i].y);
        }
        ctx.closePath();
      }
    }

    // Single fill for all shapes
    ctx.fillStyle = "rgba(180, 180, 180, 0.25)";
    ctx.fill();

    // Draw strokes for each shape individually
    ctx.strokeStyle = "rgba(120, 120, 120, 0.6)";
    ctx.lineWidth = 1;
    for (const shape of shapes) {
      ctx.beginPath();
      if (shape.type === 'sector') {
        if (shape.isCircle) {
          ctx.arc(this.center_x, this.center_y, shape.outerRadius, 0, 2 * Math.PI);
          if (shape.innerRadius > 0) {
            ctx.moveTo(this.center_x + shape.innerRadius, this.center_y);
            ctx.arc(this.center_x, this.center_y, shape.innerRadius, 0, 2 * Math.PI);
          }
        } else {
          ctx.arc(this.center_x, this.center_y, shape.outerRadius, shape.startAngle, shape.endAngle);
          if (shape.innerRadius > 0) {
            ctx.arc(this.center_x, this.center_y, shape.innerRadius, shape.endAngle, shape.startAngle, true);
          } else {
            ctx.lineTo(this.center_x, this.center_y);
          }
          ctx.closePath();
        }
      } else if (shape.type === 'rect') {
        ctx.moveTo(shape.corners[0].x, shape.corners[0].y);
        for (let i = 1; i < shape.corners.length; i++) {
          ctx.lineTo(shape.corners[i].x, shape.corners[i].y);
        }
        ctx.closePath();
      }
      ctx.stroke();
    }
  }

  #drawRangeRings(ctx, range) {
    ctx.strokeStyle = "#00ff00";
    ctx.lineWidth = 1.5;
    ctx.fillStyle = "#00ff00";
    ctx.font = "bold 14px/1 Verdana, Geneva, sans-serif";

    for (let i = 1; i <= 4; i++) {
      const radius = (i * this.beam_length) / 4;
      ctx.beginPath();
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
      ctx.stroke();

      if (range) {
        const text = formatRangeValue(is_metric(range), (range * i) / 4);
        const labelX = this.center_x + radius * 0.707;
        const labelY = this.center_y - radius * 0.707;
        ctx.fillText(text, labelX + 5, labelY - 5);
      }
    }
  }

  #drawTargets(ctx, range) {
    // Use control range for target drawing - the radar image is scaled to match
    // (renderer applies spoke_range/range scale factor to the radar image)
    if (!range || range <= 0 || this.targets.size === 0) return;

    const pixelsPerMeter = this.beam_length / range;

    for (const [id, target] of this.targets) {
      this.#drawTarget(ctx, id, target, pixelsPerMeter);
    }
  }

  #drawTarget(ctx, id, target, pixelsPerMeter) {
    if (!target.position) return;
    // Don't draw targets until we have a heading reference. ARPA target
    // bearings are computed server-side from spoke bearings, i.e. they
    // live in the raster's write frame — use the same heading source as
    // the raster so targets stay glued to their echoes.
    const heading = this.#displayHeadingRad();
    if (heading === null) return;

    // Calculate screen position from bearing and distance
    // Target bearing is geographic (true bearing from radar to target) in radians
    const bearingRad = target.position.bearing; // Already in radians from API
    const distance = target.position.distance;
    const pixelDist = distance * pixelsPerMeter;

    // In North-Up mode the raster rows are geographic, so the geographic
    // bearing is displayed directly (rot equals heading, same live source).
    // In Heading-Up mode (rot = 0) convert geographic to relative by
    // subtracting heading: screenAngle = geographicBearing - heading + rot.
    const rot = this.#displayRotation();
    const adjustedBearing = bearingRad - heading + rot;

    // Convert polar to cartesian (bearing is clockwise from north)
    const x = this.center_x + pixelDist * Math.sin(adjustedBearing);
    const y = this.center_y - pixelDist * Math.cos(adjustedBearing);

    // Determine color based on status and danger
    // Dangerous: CPA < 0.5 NM (926m) and TCPA < 10 min (600s)
    const isDangerous =
      target.danger &&
      target.danger.cpa < 926 &&
      target.danger.tcpa > 0 &&
      target.danger.tcpa < 600;

    let color;
    let fillColor;
    if (isDangerous && target.status === "tracking") {
      color = "#ff0000"; // Red for dangerous
      fillColor = "rgba(255, 0, 0, 0.3)";
    } else {
      switch (target.status) {
        case "tracking":
          color = "#ffffff"; // White for active tracking
          fillColor = "rgba(255, 255, 255, 0.2)";
          break;
        case "acquiring":
          color = "#ffff00"; // Yellow for acquiring
          fillColor = "rgba(255, 255, 0, 0.2)";
          break;
        case "lost":
          color = "#ffa500"; // Orange for lost
          fillColor = "rgba(255, 165, 0, 0.2)";
          break;
        default:
          color = "#ffffff"; // White for unknown
          fillColor = "rgba(255, 255, 255, 0.2)";
      }
    }

    ctx.save();

    // Draw target symbol (small circle)
    const targetRadius = 6;

    // Filled circle
    ctx.beginPath();
    ctx.arc(x, y, targetRadius, 0, 2 * Math.PI);
    ctx.fillStyle = fillColor;
    ctx.fill();
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.stroke();

    // Draw velocity vector if we have motion data
    if (target.motion && target.status === "tracking") {
      // Course is geographic (already in radians from API), apply same transformation as bearing
      const courseRad = target.motion.course - heading + rot;
      const speed = target.motion.speed; // m/s

      // Scale vector length: 1 minute of travel
      const vectorLength = speed * 60 * pixelsPerMeter;
      const maxVectorLength = 100; // Cap at 100 pixels
      const scaledLength = Math.min(vectorLength, maxVectorLength);

      if (scaledLength > 5) {
        const vx = x + scaledLength * Math.sin(courseRad);
        const vy = y - scaledLength * Math.cos(courseRad);

        ctx.beginPath();
        ctx.moveTo(x, y);
        ctx.lineTo(vx, vy);
        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.stroke();

        // Arrowhead
        const arrowSize = 6;
        const arrowAngle = Math.atan2(vy - y, vx - x);
        ctx.beginPath();
        ctx.moveTo(vx, vy);
        ctx.lineTo(
          vx - arrowSize * Math.cos(arrowAngle - Math.PI / 6),
          vy - arrowSize * Math.sin(arrowAngle - Math.PI / 6)
        );
        ctx.moveTo(vx, vy);
        ctx.lineTo(
          vx - arrowSize * Math.cos(arrowAngle + Math.PI / 6),
          vy - arrowSize * Math.sin(arrowAngle + Math.PI / 6)
        );
        ctx.stroke();
      }
    }

    // Draw target ID label
    ctx.fillStyle = color;
    ctx.font = "bold 11px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    ctx.fillText(`T${id}`, x + targetRadius + 4, y - targetRadius);

    // Draw CPA/TCPA info if CPA < 2 NM (3704m) and TCPA < 15 min (900s)
    if (target.danger && target.status === "tracking") {
      const cpa = target.danger.cpa;
      const tcpa = target.danger.tcpa;

      // Only show if within thresholds
      if (cpa > 0 && cpa < 3704 && tcpa > 0 && tcpa < 900) {
        ctx.font = "10px/1 Verdana, Geneva, sans-serif";
        let yOffset = 13;

        // Format CPA (in meters or nm)
        let cpaText;
        if (cpa >= 1852) {
          cpaText = `CPA: ${(cpa / 1852).toFixed(2)} nm`;
        } else {
          cpaText = `CPA: ${Math.round(cpa)} m`;
        }
        ctx.fillText(cpaText, x + targetRadius + 4, y - targetRadius + yOffset);
        yOffset += 11;

        // Format TCPA (in minutes:seconds)
        const minutes = Math.floor(tcpa / 60);
        const seconds = Math.round(tcpa % 60);
        const tcpaText = `TCPA: ${minutes}:${seconds.toString().padStart(2, "0")}`;
        ctx.fillText(tcpaText, x + targetRadius + 4, y - targetRadius + yOffset);
      }
    }

    ctx.restore();
  }

  #drawAisVessels(ctx, range) {
    if (!range || range <= 0 || this.aisVessels.size === 0) return;

    const pixelsPerMeter = this.beam_length / range;

    for (const [mmsi, vessel] of this.aisVessels) {
      this.#drawAisVessel(ctx, mmsi, vessel, pixelsPerMeter);
    }
  }

  #drawAisVessel(ctx, mmsi, vessel, pixelsPerMeter) {
    if (!vessel.position) return;
    // Don't draw vessels until we have a heading reference. An AIS target
    // and its own radar echo are the same vessel, so the symbol must land
    // on the echo — that means rotating the true geographic bearing into
    // the raster's write frame with the SAME heading the raster uses
    // (#displayHeadingRad, via #displayRotation below). Using true heading
    // for the subtraction and display heading for the rotation would
    // offset every AIS symbol from its echo by the difference between the
    // two sources.
    const heading = this.#displayHeadingRad();
    if (heading === null) return;

    // Calculate screen position from lat/lon
    // We need own-ship position to calculate relative position
    // For now, we use bearing/distance if provided, or calculate from lat/lon
    const lat = vessel.position.latitude;
    const lon = vessel.position.longitude;

    // Get own-ship position from the radar (stored in trueHeading context)
    // If no position available, we can't draw AIS vessels
    // TODO: Get own-ship position from navigation data
    // For now, assume vessels come with relative bearing/distance or we skip

    // Calculate distance and bearing from own ship
    // Since AIS vessels come from Signal K which has absolute positions,
    // we need own-ship position. For now, let's use a simple approach:
    // The server should send position relative to own-ship, or we calculate here.

    // Assuming we have own-ship lat/lon (needs to be added to PPI state)
    if (!this.ownShipLat || !this.ownShipLon) return;

    // Haversine formula for distance and bearing
    const R = 6371000; // Earth radius in meters
    const lat1 = this.ownShipLat * Math.PI / 180;
    const lat2 = lat * Math.PI / 180;
    const dLat = (lat - this.ownShipLat) * Math.PI / 180;
    const dLon = (lon - this.ownShipLon) * Math.PI / 180;

    const a = Math.sin(dLat / 2) * Math.sin(dLat / 2) +
              Math.cos(lat1) * Math.cos(lat2) *
              Math.sin(dLon / 2) * Math.sin(dLon / 2);
    const c = 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
    const distance = R * c;

    // Calculate bearing from own ship to vessel
    const y = Math.sin(dLon) * Math.cos(lat2);
    const x = Math.cos(lat1) * Math.sin(lat2) -
              Math.sin(lat1) * Math.cos(lat2) * Math.cos(dLon);
    const bearingRad = Math.atan2(y, x); // Bearing in radians, 0 = North

    const pixelDist = distance * pixelsPerMeter;

    // Don't draw if outside display range
    if (pixelDist > this.beam_length * 1.5) return;

    // Apply heading rotation for display mode (heading resolved above);
    // in north-up rot equals heading so this collapses to the geographic
    // bearing, matching the geographically-placed raster.
    const rot = this.#displayRotation();
    const adjustedBearing = bearingRad - heading + rot;

    // Convert polar to cartesian (bearing is clockwise from north)
    const screenX = this.center_x + pixelDist * Math.sin(adjustedBearing);
    const screenY = this.center_y - pixelDist * Math.cos(adjustedBearing);

    ctx.save();

    // Determine vessel orientation: use heading if available, otherwise COG
    let vesselOrientation = vessel.heading ?? vessel.cog;
    if (vesselOrientation === undefined || vesselOrientation === null) {
      vesselOrientation = 0;
    }
    // Adjust for heading mode
    const displayOrientation = vesselOrientation - heading + rot;

    // Draw IMO-style vessel symbol
    this.#drawImoVesselSymbol(ctx, screenX, screenY, displayOrientation, vessel);

    // Draw COG vector (10 minutes of travel)
    if (vessel.sog !== undefined && vessel.sog > 0 && vessel.cog !== undefined) {
      const vectorLengthMeters = vessel.sog * 600; // 10 minutes in seconds
      const vectorLengthPixels = vectorLengthMeters * pixelsPerMeter;
      const maxVectorLength = 150; // Cap vector length
      const scaledVectorLength = Math.min(vectorLengthPixels, maxVectorLength);

      if (scaledVectorLength > 5) {
        const cogDisplay = vessel.cog - heading + rot;
        const vx = screenX + scaledVectorLength * Math.sin(cogDisplay);
        const vy = screenY - scaledVectorLength * Math.cos(cogDisplay);

        ctx.beginPath();
        ctx.moveTo(screenX, screenY);
        ctx.lineTo(vx, vy);
        ctx.strokeStyle = "#00ffff";
        ctx.lineWidth = 1.5;
        ctx.stroke();
      }
    }

    // Draw vessel name or MMSI
    const label = vessel.name || mmsi;
    ctx.fillStyle = "#00ffff";
    ctx.font = "11px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    ctx.fillText(label, screenX + 15, screenY - 5);

    ctx.restore();
  }

  /**
   * Draw IMO-style vessel symbol (isoceles triangle pointing in direction of travel)
   */
  #drawImoVesselSymbol(ctx, x, y, orientation, vessel) {
    const length = 20; // Symbol length in pixels
    const width = 10;  // Symbol width at base

    ctx.save();
    ctx.translate(x, y);
    ctx.rotate(orientation);

    // Draw isoceles triangle pointing up (bow direction)
    ctx.beginPath();
    ctx.moveTo(0, -length / 2);        // Bow (top)
    ctx.lineTo(-width / 2, length / 2); // Port stern
    ctx.lineTo(width / 2, length / 2);  // Starboard stern
    ctx.closePath();

    // Fill and stroke
    ctx.fillStyle = "rgba(0, 255, 255, 0.3)";
    ctx.fill();
    ctx.strokeStyle = "#00ffff";
    ctx.lineWidth = 2;
    ctx.stroke();

    // Draw center dot
    ctx.beginPath();
    ctx.arc(0, 0, 2, 0, 2 * Math.PI);
    ctx.fillStyle = "#00ffff";
    ctx.fill();

    ctx.restore();
  }

  /**
   * Set own-ship position for AIS vessel relative positioning
   */
  setOwnShipPosition(lat, lon) {
    this.ownShipLat = lat;
    this.ownShipLon = lon;
  }

  #drawCompassRose(ctx) {
    const degreeRingRadius = (3 * this.beam_length) / 4;
    const tickLength = 8;
    const majorTickLength = 12;

    ctx.font = "bold 12px/1 Verdana, Geneva, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.strokeStyle = "#00ff00";
    ctx.fillStyle = "#00ff00";

    // Head-up: rose counter-rotates by the live heading so its labels stay
    // geographic. North-up: rose is fixed with N at the top (the raster is
    // already geographic). Uses the raster frame's heading so rose bearings
    // line up with the echoes. Strict null check — 0° due north is a valid
    // heading, not a missing one.
    const headingRad = this.#displayHeadingRad();
    const roseRotationDeg =
      this.headingMode === "headingUp" && headingRad !== null
        ? -(headingRad * 180) / Math.PI
        : 0;

    for (let deg = 0; deg < 360; deg += 10) {
      const displayDeg = deg + roseRotationDeg;
      const radians = ((90 - displayDeg) * Math.PI) / 180;

      const cos = Math.cos(radians);
      const sin = Math.sin(radians);

      const isMajor = deg % 30 === 0;
      const tick = isMajor ? majorTickLength : tickLength;

      const innerRadius = degreeRingRadius - tick / 2;
      const outerRadius = degreeRingRadius + tick / 2;

      const x1 = this.center_x + innerRadius * cos;
      const y1 = this.center_y - innerRadius * sin;
      const x2 = this.center_x + outerRadius * cos;
      const y2 = this.center_y - outerRadius * sin;

      ctx.beginPath();
      ctx.moveTo(x1, y1);
      ctx.lineTo(x2, y2);
      ctx.stroke();

      if (isMajor) {
        const labelRadius = degreeRingRadius + majorTickLength + 10;
        const labelX = this.center_x + labelRadius * cos;
        const labelY = this.center_y - labelRadius * sin;
        ctx.fillText(deg.toString(), labelX, labelY);
      }
    }

    // Draw North indicator
    const northDeg = roseRotationDeg;
    const northRadians = ((90 - northDeg) * Math.PI) / 180;
    const northRadius = degreeRingRadius + majorTickLength + 25;
    const northX = this.center_x + northRadius * Math.cos(northRadians);
    const northY = this.center_y - northRadius * Math.sin(northRadians);
    ctx.font = "bold 14px/1 Verdana, Geneva, sans-serif";
    ctx.fillText("N", northX, northY);
  }

  // ============================================================
  // VRM/EBL drawing
  // ============================================================

  static VRM_EBL_COLORS = ["#00ffff", "#ff00ff"];

  #drawVrmEbl(ctx, range) {
    if (!range || range <= 0) return;
    const pixelsPerMeter = this.beam_length / range;

    for (let i = 0; i < this.vrmEbl.length; i++) {
      const marker = this.vrmEbl[i];
      if (!marker.enabled) continue;
      this.#drawVrmEblMarker(ctx, i, marker, pixelsPerMeter, range);
    }
  }

  #drawVrmEblMarker(ctx, index, marker, pixelsPerMeter, range) {
    const color = PPI.VRM_EBL_COLORS[index];
    const screenAngle = marker.bearing + this.#displayRotation();
    const radius = marker.distance * pixelsPerMeter;
    // Line extends to the edge of the PPI so it's always visible even when the
    // VRM is outside the displayed range.
    const lineLength = Math.max(radius, this.beam_length);

    const dirX = Math.sin(screenAngle);
    const dirY = -Math.cos(screenAngle);
    const lineEndX = this.center_x + lineLength * dirX;
    const lineEndY = this.center_y + lineLength * dirY;
    const pointX = this.center_x + radius * dirX;
    const pointY = this.center_y + radius * dirY;

    ctx.save();

    // EBL: dashed bearing line from centre out
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.setLineDash([6, 4]);
    ctx.beginPath();
    ctx.moveTo(this.center_x, this.center_y);
    ctx.lineTo(lineEndX, lineEndY);
    ctx.stroke();
    ctx.setLineDash([]);

    // VRM: range ring at distance
    if (radius > 0 && radius < this.beam_length * 3) {
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
      ctx.stroke();
    }

    // Intersection handle - the point this marker defines
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(pointX, pointY, 6, 0, 2 * Math.PI);
    ctx.fill();
    ctx.strokeStyle = "#000";
    ctx.lineWidth = 1;
    ctx.stroke();

    // Readout: true bearing + distance, anchored just outside the ring along
    // the bearing line. Offset perpendicularly so two markers don't overlap.
    this.#drawVrmEblReadout(ctx, index, marker, screenAngle, radius, color, range);

    ctx.restore();
  }

  #drawVrmEblReadout(ctx, index, marker, screenAngle, radius, color, range) {
    // Anchor 24 px beyond the intersection along the bearing line.
    const anchorRadius = radius + 24;
    const ax = this.center_x + anchorRadius * Math.sin(screenAngle);
    const ay = this.center_y - anchorRadius * Math.cos(screenAngle);

    let bearingText;
    const headingRad = this.#trueHeadingRad();
    if (headingRad !== null) {
      let trueBearing = marker.bearing + headingRad;
      while (trueBearing < 0) trueBearing += 2 * Math.PI;
      while (trueBearing >= 2 * Math.PI) trueBearing -= 2 * Math.PI;
      bearingText = `${((trueBearing * 180) / Math.PI).toFixed(1)}° T`;
    } else {
      let rel = marker.bearing;
      while (rel < 0) rel += 2 * Math.PI;
      while (rel >= 2 * Math.PI) rel -= 2 * Math.PI;
      bearingText = `${((rel * 180) / Math.PI).toFixed(1)}° R`;
    }

    const distanceText = formatVrmDistance(is_metric(range), marker.distance);

    const label = `VRM/EBL ${index + 1}`;
    const lines = [label, bearingText, distanceText];

    ctx.font = "bold 11px/1 Verdana, Geneva, sans-serif";
    let maxWidth = 0;
    for (const line of lines) {
      const w = ctx.measureText(line).width;
      if (w > maxWidth) maxWidth = w;
    }
    const padX = 6;
    const padY = 4;
    const lineHeight = 13;
    const boxW = maxWidth + padX * 2;
    const boxH = lineHeight * lines.length + padY * 2;

    // Place box so it sits beside (not on top of) the bearing line. Bias to
    // the side away from screen centre and clamp to viewport.
    let boxX = ax - boxW / 2;
    let boxY = ay - boxH / 2;
    if (Math.sin(screenAngle) > 0) boxX += 12;
    else boxX -= boxW + 12;
    if (-Math.cos(screenAngle) > 0) boxY += 12;
    else boxY -= 12;
    boxX = Math.max(4, Math.min(this.width - boxW - 4, boxX));
    boxY = Math.max(4, Math.min(this.height - boxH - 4, boxY));

    ctx.fillStyle = "rgba(0, 0, 0, 0.75)";
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.rect(boxX, boxY, boxW, boxH);
    ctx.fill();
    ctx.stroke();

    ctx.fillStyle = color;
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    for (let i = 0; i < lines.length; i++) {
      ctx.fillText(lines[i], boxX + padX, boxY + padY + i * lineHeight);
    }
  }

  // ============================================================
  // Drag handles for zone/sector editing
  // ============================================================

  #updatePointerEvents() {
    const isCreating =
      this.creatingZoneIndex !== null || this.creatingRectIndex !== null;
    const isEditing =
      this.editingZoneIndex !== null || this.editingSectorIndex !== null;
    const hasVrmEbl = this.vrmEbl.some((m) => m.enabled);
    const needsPointerEvents =
      isCreating || isEditing || this.acquireTargetMode || hasVrmEbl;

    if (needsPointerEvents && this.overlay_dom) {
      this.overlay_dom.style.pointerEvents = "auto";
      // Use crosshair for create mode and acquire mode
      this.overlay_dom.style.cursor =
        isCreating || this.acquireTargetMode ? "crosshair" : "default";
      this.#setupDragHandlers();
    } else if (this.overlay_dom) {
      this.overlay_dom.style.pointerEvents = "none";
      this.overlay_dom.style.cursor = "default";
      this.#removeDragHandlers();
    }
  }

  #installVrmEblHandlersIfNeeded() {
    this.#updatePointerEvents();
  }

  #setupDragHandlers() {
    if (this._dragHandlersInstalled) return;
    this._dragHandlersInstalled = true;

    this._onMouseDown = this.#handleMouseDown.bind(this);
    this._onMouseMove = this.#handleMouseMove.bind(this);
    this._onMouseUp = this.#handleMouseUp.bind(this);
    this._onTouchStart = this.#handleTouchStart.bind(this);
    this._onTouchMove = this.#handleTouchMove.bind(this);
    this._onTouchEnd = this.#handleTouchEnd.bind(this);

    this.overlay_dom.addEventListener("mousedown", this._onMouseDown);
    this.overlay_dom.addEventListener("mousemove", this._onMouseMove);
    this.overlay_dom.addEventListener("mouseup", this._onMouseUp);
    this.overlay_dom.addEventListener("mouseleave", this._onMouseUp);
    this.overlay_dom.addEventListener("touchstart", this._onTouchStart);
    this.overlay_dom.addEventListener("touchmove", this._onTouchMove);
    this.overlay_dom.addEventListener("touchend", this._onTouchEnd);
  }

  #removeDragHandlers() {
    if (!this._dragHandlersInstalled) return;
    this._dragHandlersInstalled = false;

    this.overlay_dom.removeEventListener("mousedown", this._onMouseDown);
    this.overlay_dom.removeEventListener("mousemove", this._onMouseMove);
    this.overlay_dom.removeEventListener("mouseup", this._onMouseUp);
    this.overlay_dom.removeEventListener("mouseleave", this._onMouseUp);
    this.overlay_dom.removeEventListener("touchstart", this._onTouchStart);
    this.overlay_dom.removeEventListener("touchmove", this._onTouchMove);
    this.overlay_dom.removeEventListener("touchend", this._onTouchEnd);
  }

  #getCanvasCoords(event) {
    const rect = this.overlay_dom.getBoundingClientRect();
    return {
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
    };
  }

  #pixelToRadarCoords(x, y) {
    const dx = x - this.center_x;
    const dy = y - this.center_y;
    // Calculate angle in screen coordinates
    let angle = Math.atan2(dx, -dy);
    // Subtract the live display rotation to convert from screen to radar
    // coordinates (screen angle = radar angle + rotation). Evaluated live
    // so clicks/drags in north-up land where the user sees them even
    // while the boat yaws.
    angle -= this.#displayRotation();
    // Normalize angle to [-PI, PI]
    while (angle > Math.PI) angle -= 2 * Math.PI;
    while (angle < -Math.PI) angle += 2 * Math.PI;
    const pixelDist = Math.sqrt(dx * dx + dy * dy);
    // Use the Control Range for UI coordinate conversion, not spoke_range
    const pixelsPerMeter = this.range > 0 ? this.beam_length / this.range : 1;
    const distance = pixelDist / pixelsPerMeter;
    return { angle, distance };
  }

  #getHandlePositions(zone) {
    if (!zone) return null;

    // Use the Control Range for UI positioning
    if (!this.range || this.range <= 0) return null;

    const pixelsPerMeter = this.beam_length / this.range;
    const innerRadius = zone.startDistance * pixelsPerMeter;
    const outerRadius = zone.endDistance * pixelsPerMeter;
    const midRadius = (innerRadius + outerRadius) / 2;

    // Rotate bow-relative zone angles by the live display rotation — must
    // match #drawGuardZone and #pixelToRadarCoords or the handles detach
    // from the drawn zone.
    const rot = this.#displayRotation();
    const rotatedStartAngle = zone.startAngle + rot;
    const rotatedEndAngle = zone.endAngle + rot;

    let midAngle = (zone.startAngle + zone.endAngle) / 2;
    if (zone.endAngle < zone.startAngle) {
      midAngle = (zone.startAngle + zone.endAngle + 2 * Math.PI) / 2;
      if (midAngle > Math.PI) midAngle -= 2 * Math.PI;
    }
    const rotatedMidAngle = midAngle + rot;

    const startAngleX = this.center_x + midRadius * Math.sin(rotatedStartAngle);
    const startAngleY = this.center_y - midRadius * Math.cos(rotatedStartAngle);
    const endAngleX = this.center_x + midRadius * Math.sin(rotatedEndAngle);
    const endAngleY = this.center_y - midRadius * Math.cos(rotatedEndAngle);

    const innerDistX = this.center_x + innerRadius * Math.sin(rotatedMidAngle);
    const innerDistY = this.center_y - innerRadius * Math.cos(rotatedMidAngle);
    const outerDistX = this.center_x + outerRadius * Math.sin(rotatedMidAngle);
    const outerDistY = this.center_y - outerRadius * Math.cos(rotatedMidAngle);

    return {
      startAngle: { x: startAngleX, y: startAngleY, angle: rotatedStartAngle },
      endAngle: { x: endAngleX, y: endAngleY, angle: rotatedEndAngle },
      innerDist: { x: innerDistX, y: innerDistY, radius: innerRadius, midAngle: rotatedMidAngle },
      outerDist: { x: outerDistX, y: outerDistY, radius: outerRadius, midAngle: rotatedMidAngle },
    };
  }

  #getSectorHandlePositions(sector) {
    if (!sector) return null;

    const handleRadius = this.beam_length * 0.5;
    if (handleRadius <= 0) return null;

    // Rotate bow-relative sector angles by the live display rotation
    const rot = this.#displayRotation();
    const rotatedStartAngle = sector.startAngle + rot;
    const rotatedEndAngle = sector.endAngle + rot;

    const startAngleX = this.center_x + handleRadius * Math.sin(rotatedStartAngle);
    const startAngleY = this.center_y - handleRadius * Math.cos(rotatedStartAngle);
    const endAngleX = this.center_x + handleRadius * Math.sin(rotatedEndAngle);
    const endAngleY = this.center_y - handleRadius * Math.cos(rotatedEndAngle);

    return {
      startAngle: { x: startAngleX, y: startAngleY, angle: rotatedStartAngle },
      endAngle: { x: endAngleX, y: endAngleY, angle: rotatedEndAngle },
    };
  }

  #hitTestHandles(x, y) {
    const hitRadius = 15;

    if (this.editingZoneIndex !== null) {
      const zone = this.guardZones[this.editingZoneIndex];
      if (zone) {
        const handles = this.#getHandlePositions(zone);
        if (handles) {
          for (const [name, pos] of Object.entries(handles)) {
            const dx = x - pos.x;
            const dy = y - pos.y;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
              return { type: "zone", handle: name };
            }
          }
        }
      }
    }

    if (this.editingSectorIndex !== null) {
      const sector = this.noTransmitSectors[this.editingSectorIndex];
      if (sector) {
        const handles = this.#getSectorHandlePositions(sector);
        if (handles) {
          for (const [name, pos] of Object.entries(handles)) {
            const dx = x - pos.x;
            const dy = y - pos.y;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
              return { type: "sector", handle: name };
            }
          }
        }
      }
    }

    return null;
  }

  #hitTestVrmEbl(x, y) {
    if (!this.range || this.range <= 0) return null;
    const pixelsPerMeter = this.beam_length / this.range;
    const pointRadius = 12;     // intersection handle priority area
    const ringTolerance = 8;    // px from ring to grab range
    const lineTolerance = 8;    // px from line to grab bearing

    // Live display rotation — must match #drawVrmEblMarker so the grab
    // areas sit on the drawn marker.
    const rot = this.#displayRotation();

    // Check intersection handles first (highest priority)
    let best = null;
    for (let i = 0; i < this.vrmEbl.length; i++) {
      const m = this.vrmEbl[i];
      if (!m.enabled) continue;
      const screenAngle = m.bearing + rot;
      const radius = m.distance * pixelsPerMeter;
      const px = this.center_x + radius * Math.sin(screenAngle);
      const py = this.center_y - radius * Math.cos(screenAngle);
      const dx = x - px;
      const dy = y - py;
      const d2 = dx * dx + dy * dy;
      if (d2 <= pointRadius * pointRadius) {
        if (!best || d2 < best.d2) {
          best = { index: i, mode: "point", d2 };
        }
      }
    }
    if (best) return { index: best.index, mode: best.mode };

    // Then rings: closeness to the ring circumference.
    for (let i = 0; i < this.vrmEbl.length; i++) {
      const m = this.vrmEbl[i];
      if (!m.enabled) continue;
      const radius = m.distance * pixelsPerMeter;
      if (radius <= 0) continue;
      const dx = x - this.center_x;
      const dy = y - this.center_y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (Math.abs(dist - radius) <= ringTolerance) {
        return { index: i, mode: "ring" };
      }
    }

    // Then bearing lines: perpendicular distance from the line segment.
    for (let i = 0; i < this.vrmEbl.length; i++) {
      const m = this.vrmEbl[i];
      if (!m.enabled) continue;
      const screenAngle = m.bearing + rot;
      const dirX = Math.sin(screenAngle);
      const dirY = -Math.cos(screenAngle);
      const dx = x - this.center_x;
      const dy = y - this.center_y;
      // Project onto direction; only count points on the positive ray.
      const t = dx * dirX + dy * dirY;
      if (t < 0) continue;
      const perp = Math.abs(dx * dirY - dy * dirX);
      if (perp <= lineTolerance && t <= this.beam_length * 1.2) {
        return { index: i, mode: "line" };
      }
    }

    return null;
  }

  #updateVrmEblFromDrag(x, y) {
    const drag = this._vrmEblDrag;
    if (!drag) return;
    const marker = this.vrmEbl[drag.index];
    if (!marker) return;
    const radarCoords = this.#pixelToRadarCoords(x, y);

    if (drag.mode === "ring") {
      marker.distance = Math.max(0, radarCoords.distance);
    } else if (drag.mode === "line") {
      marker.bearing = radarCoords.angle;
    } else if (drag.mode === "point") {
      marker.bearing = radarCoords.angle;
      marker.distance = Math.max(0, radarCoords.distance);
    }
    this.redrawCanvas();
    if (this.onVrmEblChange) this.onVrmEblChange(this.vrmEbl);
  }

  #handleMouseDown(event) {
    const coords = this.#getCanvasCoords(event);

    // Record click start position for acquire mode
    this._clickStart = { x: coords.x, y: coords.y };

    // If in zone create mode (drag-based), start drag
    if (this.creatingZoneIndex !== null) {
      const radarCoords = this.#pixelToRadarCoords(coords.x, coords.y);
      this.createDragStart = radarCoords;
      this.dragState = {
        type: "createZone",
        startX: coords.x,
        startY: coords.y,
      };
      this.#initializeCreatingZone(radarCoords);
      event.preventDefault();
      return;
    }

    // If in rect create mode (3-click based), handle click
    // This is click-based, not drag-based, so we don't use dragState
    if (this.creatingRectIndex !== null) {
      const radarCoords = this.#pixelToRadarCoords(coords.x, coords.y);
      const p = this.#polarToCartesian(radarCoords.angle, radarCoords.distance);

      if (this.rectCreateClickCount === 0) {
        // First click - initialize rect with first corner
        this.#initializeCreatingRect(radarCoords);
        // rectCreateClickCount is now 1, mouse move will update x2/y2
      } else if (this.rectCreateClickCount === 1) {
        // Second click - lock second corner, start width definition
        const rect = this.exclusionRects[this.creatingRectIndex];
        if (rect) {
          rect.x2 = p.x;
          rect.y2 = p.y;
          this.rectCreateCorner2 = { x: p.x, y: p.y };
          this.rectCreateClickCount = 2;
          // Mouse move will now update width
        }
      } else if (this.rectCreateClickCount === 2) {
        // Third click - finalize width and complete
        this.#finalizeRectCreation();
      }
      event.preventDefault();
      this.redrawCanvas();
      return;
    }

    const hit = this.#hitTestHandles(coords.x, coords.y);

    if (hit) {
      if (hit.type === "zone" && this.editingZoneIndex !== null) {
        const zone = this.guardZones[this.editingZoneIndex];
        this.dragState = {
          type: "zone",
          handle: hit.handle,
          startX: coords.x,
          startY: coords.y,
          originalZone: { ...zone },
        };
      } else if (hit.type === "sector" && this.editingSectorIndex !== null) {
        const sector = this.noTransmitSectors[this.editingSectorIndex];
        this.dragState = {
          type: "sector",
          handle: hit.handle,
          startX: coords.x,
          startY: coords.y,
          originalSector: { ...sector },
        };
      }
      this.overlay_dom.style.cursor = "grabbing";
      event.preventDefault();
      return;
    }

    const vrmHit = this.#hitTestVrmEbl(coords.x, coords.y);
    if (vrmHit) {
      this._vrmEblDrag = vrmHit;
      this.dragState = { type: "vrmEbl" };
      this.overlay_dom.style.cursor = "grabbing";
      event.preventDefault();
    }
  }

  #handleMouseMove(event) {
    const coords = this.#getCanvasCoords(event);

    // Handle 3-click rect creation: track mouse regardless of dragState
    if (this.creatingRectIndex !== null && this.rectCreateClickCount > 0) {
      this.#updateRectFromCreateDrag(coords.x, coords.y);
      event.preventDefault();
      return;
    }

    if (this.dragState) {
      if (this.dragState.type === "createZone") {
        this.#updateZoneFromCreateDrag(coords.x, coords.y);
        event.preventDefault();
      } else if (this.dragState.type === "zone") {
        this.#updateZoneFromDrag(coords.x, coords.y);
        event.preventDefault();
      } else if (this.dragState.type === "sector") {
        this.#updateSectorFromDrag(coords.x, coords.y);
        event.preventDefault();
      } else if (this.dragState.type === "vrmEbl") {
        this.#updateVrmEblFromDrag(coords.x, coords.y);
        event.preventDefault();
      }
    } else {
      const hit = this.#hitTestHandles(coords.x, coords.y);
      const newHovered = hit ? hit.handle : null;
      let cursor = "default";
      if (hit) {
        cursor = "grab";
      } else {
        const vrmHit = this.#hitTestVrmEbl(coords.x, coords.y);
        if (vrmHit) cursor = "grab";
      }
      if (newHovered !== this.hoveredHandle) {
        this.hoveredHandle = newHovered;
        this.overlay_dom.style.cursor = cursor;
        this.redrawCanvas();
      } else {
        this.overlay_dom.style.cursor = cursor;
      }
    }
  }

  #handleMouseUp(event) {
    // For 3-click rect creation, don't do anything special on mouseup
    // The clicks are handled in mousedown, not as drag operations
    if (this.creatingRectIndex !== null && this.rectCreateClickCount > 0) {
      this._clickStart = null;
      return;
    }

    if (this.dragState) {
      if (this.dragState.type === "createZone") {
        this.#finalizeZoneCreation();
      } else if (this.dragState.type === "zone") {
        const zoneIndex = this.editingZoneIndex;
        const newZone = this.guardZones[zoneIndex];
        if (this.onZoneDragEnd && newZone) {
          this.onZoneDragEnd(zoneIndex, newZone);
        }
      } else if (this.dragState.type === "sector") {
        const sectorIndex = this.editingSectorIndex;
        const newSector = this.noTransmitSectors[sectorIndex];
        if (this.onSectorDragEnd && newSector) {
          this.onSectorDragEnd(sectorIndex, newSector);
        }
      } else if (this.dragState.type === "vrmEbl") {
        this._vrmEblDrag = null;
        if (this.onVrmEblChange) this.onVrmEblChange(this.vrmEbl);
      }
      this.dragState = null;
      // Re-hit-test under the released pointer so the cursor reflects the
      // current target (VRM/EBL or zone/sector handle), not the stale
      // hoveredHandle that only tracks zone/sector handles.
      const releaseCoords = this.#getCanvasCoords(event);
      const stillOverHandle = this.#hitTestHandles(
        releaseCoords.x,
        releaseCoords.y,
      );
      const stillOverVrm = this.#hitTestVrmEbl(releaseCoords.x, releaseCoords.y);
      this.overlay_dom.style.cursor =
        stillOverHandle || stillOverVrm ? "grab" : "default";
    } else if (this.acquireTargetMode && this._clickStart) {
      // Handle click for target acquisition
      const coords = this.#getCanvasCoords(event);
      const dx = coords.x - this._clickStart.x;
      const dy = coords.y - this._clickStart.y;
      const clickDistance = Math.sqrt(dx * dx + dy * dy);

      // Only treat as click if mouse didn't move much (not a drag)
      if (clickDistance < 5) {
        const status = document.getElementById("myr_acquire_target_status");
        // MARPA acquire targets the server's blob detector, which works
        // in the spoke-bearing frame — use the raster frame's heading:
        // #pixelToRadarCoords subtracts #displayRotation() (same source)
        // in north-up, so the click round-trips exactly to the bearing
        // frame the server tracks in.
        const headingRad = this.#displayHeadingRad();
        if (headingRad === null) {
          console.warn("Acquire target: no heading data, cannot compute true bearing");
          if (status) {
            status.textContent = "No heading data, cannot acquire";
            status.style.display = "block";
          }
        } else {
          // Clear any prior error so a successful click doesn't leave the
          // stale "No heading data" message visible.
          if (status) {
            status.textContent = "Click in PPI to acquire";
          }
          const radarCoords = this.#pixelToRadarCoords(coords.x, coords.y);
          // Keep bearing in radians, add true heading to get bearing true
          let bearingRad = radarCoords.angle + headingRad;
          // Normalize to [0, 2π)
          while (bearingRad < 0) bearingRad += 2 * Math.PI;
          while (bearingRad >= 2 * Math.PI) bearingRad -= 2 * Math.PI;

          const bearingDeg = (bearingRad * 180) / Math.PI;
          console.log(`Acquire target click: bearing=${bearingDeg.toFixed(1)}° (${bearingRad.toFixed(3)} rad), distance=${radarCoords.distance.toFixed(0)}m`);

          if (this.onTargetAcquire) {
            // Send bearing in radians to match API format
            this.onTargetAcquire(bearingRad, radarCoords.distance);
          }
        }
      }
    }
    this._clickStart = null;
  }

  #handleTouchStart(event) {
    if (event.touches.length === 1) {
      const touch = event.touches[0];
      const rect = this.overlay_dom.getBoundingClientRect();
      const x = touch.clientX - rect.left;
      const y = touch.clientY - rect.top;
      const hit = this.#hitTestHandles(x, y);

      if (hit) {
        if (hit.type === "zone" && this.editingZoneIndex !== null) {
          const zone = this.guardZones[this.editingZoneIndex];
          this.dragState = {
            type: "zone",
            handle: hit.handle,
            startX: x,
            startY: y,
            originalZone: { ...zone },
          };
          event.preventDefault();
        } else if (hit.type === "sector" && this.editingSectorIndex !== null) {
          const sector = this.noTransmitSectors[this.editingSectorIndex];
          this.dragState = {
            type: "sector",
            handle: hit.handle,
            startX: x,
            startY: y,
            originalSector: { ...sector },
          };
          event.preventDefault();
        }
        return;
      }

      const vrmHit = this.#hitTestVrmEbl(x, y);
      if (vrmHit) {
        this._vrmEblDrag = vrmHit;
        this.dragState = { type: "vrmEbl" };
        event.preventDefault();
      }
    }
  }

  #handleTouchMove(event) {
    if (this.dragState && event.touches.length === 1) {
      const touch = event.touches[0];
      const rect = this.overlay_dom.getBoundingClientRect();
      const x = touch.clientX - rect.left;
      const y = touch.clientY - rect.top;
      if (this.dragState.type === "zone") {
        this.#updateZoneFromDrag(x, y);
      } else if (this.dragState.type === "sector") {
        this.#updateSectorFromDrag(x, y);
      } else if (this.dragState.type === "vrmEbl") {
        this.#updateVrmEblFromDrag(x, y);
      }
      event.preventDefault();
    }
  }

  #handleTouchEnd(event) {
    if (this.dragState) {
      if (this.dragState.type === "zone") {
        const zoneIndex = this.editingZoneIndex;
        const newZone = this.guardZones[zoneIndex];
        if (this.onZoneDragEnd && newZone) {
          this.onZoneDragEnd(zoneIndex, newZone);
        }
      } else if (this.dragState.type === "sector") {
        const sectorIndex = this.editingSectorIndex;
        const newSector = this.noTransmitSectors[sectorIndex];
        if (this.onSectorDragEnd && newSector) {
          this.onSectorDragEnd(sectorIndex, newSector);
        }
      } else if (this.dragState.type === "vrmEbl") {
        this._vrmEblDrag = null;
        if (this.onVrmEblChange) this.onVrmEblChange(this.vrmEbl);
      }
      this.dragState = null;
    }
  }

  #updateZoneFromDrag(x, y) {
    if (!this.dragState || this.editingZoneIndex === null) return;

    const zone = this.guardZones[this.editingZoneIndex];
    if (!zone) return;

    const radarCoords = this.#pixelToRadarCoords(x, y);

    switch (this.dragState.handle) {
      case "startAngle":
        zone.startAngle = radarCoords.angle;
        break;
      case "endAngle":
        zone.endAngle = radarCoords.angle;
        break;
      case "innerDist":
        zone.startDistance = Math.max(0, radarCoords.distance);
        if (zone.startDistance > zone.endDistance - 50) {
          zone.startDistance = zone.endDistance - 50;
        }
        break;
      case "outerDist":
        zone.endDistance = Math.max(50, radarCoords.distance);
        if (zone.endDistance < zone.startDistance + 50) {
          zone.endDistance = zone.startDistance + 50;
        }
        break;
    }

    // Call onDragMove callback if provided
    if (this.onZoneDragMove) {
      this.onZoneDragMove(this.editingZoneIndex, zone);
    }

    this.redrawCanvas();
  }

  #updateSectorFromDrag(x, y) {
    if (!this.dragState || this.editingSectorIndex === null) return;

    const sector = this.noTransmitSectors[this.editingSectorIndex];
    if (!sector) return;

    const radarCoords = this.#pixelToRadarCoords(x, y);

    switch (this.dragState.handle) {
      case "startAngle":
        sector.startAngle = radarCoords.angle;
        break;
      case "endAngle":
        sector.endAngle = radarCoords.angle;
        break;
    }

    this.redrawCanvas();
  }

  // Zone creation via click-drag

  #initializeCreatingZone(startCoords) {
    if (this.creatingZoneIndex !== null) {
      // Determine which array to use based on index
      // 0-1 are guard zones, 2+ would be exclusion zones
      const zoneArray = this.creatingZoneIndex < 2 ? this.guardZones : this.exclusionZones;
      const arrayIndex = this.creatingZoneIndex < 2 ? this.creatingZoneIndex : this.creatingZoneIndex - 2;
      zoneArray[arrayIndex] = {
        startAngle: startCoords.angle,
        endAngle: startCoords.angle,
        startDistance: startCoords.distance,
        endDistance: startCoords.distance,
        enabled: true
      };
    }
    this.redrawCanvas();
  }

  #initializeCreatingRect(startCoords) {
    if (this.creatingRectIndex !== null) {
      const p = this.#polarToCartesian(startCoords.angle, startCoords.distance);

      if (this.rectCreateClickCount === 0) {
        // First click - store first corner
        this.rectCreateCorner1 = { x: p.x, y: p.y };
        this.rectCreateClickCount = 1;
        // Initialize rect - x2/y2 will track mouse until second click
        // width starts small for visibility, will be set on third click
        this.exclusionRects[this.creatingRectIndex] = {
          x1: p.x,
          y1: p.y,
          x2: p.x,
          y2: p.y,
          width: 10,  // Small default width for visibility during edge definition
          enabled: true
        };
      }
    }
    this.redrawCanvas();
  }

  #updateZoneFromCreateDrag(x, y) {
    if (!this.createDragStart || this.creatingZoneIndex === null) return;

    const endCoords = this.#pixelToRadarCoords(x, y);
    const start = this.createDragStart;

    // Determine which array to use
    const zoneArray = this.creatingZoneIndex < 2 ? this.guardZones : this.exclusionZones;
    const arrayIndex = this.creatingZoneIndex < 2 ? this.creatingZoneIndex : this.creatingZoneIndex - 2;
    const zone = zoneArray[arrayIndex];
    if (!zone) return;

    // Normalize angles to [-PI, PI] and assign min/max
    let a1 = this.#normalizeAngle(start.angle);
    let a2 = this.#normalizeAngle(endCoords.angle);
    zone.startAngle = Math.min(a1, a2);
    zone.endAngle = Math.max(a1, a2);

    // Assign distances so startDistance < endDistance
    zone.startDistance = Math.max(0, Math.min(start.distance, endCoords.distance));
    zone.endDistance = Math.max(start.distance, endCoords.distance);

    // Enforce minimum size (50m)
    if (zone.endDistance - zone.startDistance < 50) {
      zone.endDistance = zone.startDistance + 50;
    }

    this.redrawCanvas();
  }

  #updateRectFromCreateDrag(x, y) {
    if (this.creatingRectIndex === null) return;

    const currentCoords = this.#pixelToRadarCoords(x, y);
    const p = this.#polarToCartesian(currentCoords.angle, currentCoords.distance);

    const rect = this.exclusionRects[this.creatingRectIndex];
    if (!rect) return;

    if (this.rectCreateClickCount === 1) {
      // After first click, show edge being defined
      rect.x2 = p.x;
      rect.y2 = p.y;
      rect.width = 10; // Small default width for visibility
    } else if (this.rectCreateClickCount === 2) {
      // After second click, adjust width based on perpendicular distance
      const dx = rect.x2 - rect.x1;
      const dy = rect.y2 - rect.y1;
      const edgeLen = Math.sqrt(dx * dx + dy * dy);
      if (edgeLen > 0.001) {
        // Calculate perpendicular distance from cursor to the edge line
        // Perpendicular unit vector (rotated 90 degrees clockwise)
        const perpX = dy / edgeLen;
        const perpY = -dx / edgeLen;
        // Vector from corner1 to current point
        const toP = { x: p.x - rect.x1, y: p.y - rect.y1 };
        // Project onto perpendicular to get width
        rect.width = Math.abs(toP.x * perpX + toP.y * perpY);
        // Minimum width
        if (rect.width < 5) rect.width = 5;
      }
    }

    this.redrawCanvas();
  }

  #normalizeAngle(angle) {
    while (angle > Math.PI) angle -= 2 * Math.PI;
    while (angle < -Math.PI) angle += 2 * Math.PI;
    return angle;
  }

  #polarToCartesian(angle, distance) {
    return {
      x: distance * Math.sin(angle),  // East is +X
      y: distance * Math.cos(angle)   // North is +Y
    };
  }

  #finalizeZoneCreation() {
    if (this.creatingZoneIndex !== null && this.onZoneCreated) {
      const zoneArray = this.creatingZoneIndex < 2 ? this.guardZones : this.exclusionZones;
      const arrayIndex = this.creatingZoneIndex < 2 ? this.creatingZoneIndex : this.creatingZoneIndex - 2;
      this.onZoneCreated(this.creatingZoneIndex, zoneArray[arrayIndex]);
    }
    this.dragState = null;
    this.createDragStart = null;
  }

  #finalizeRectCreation() {
    if (this.creatingRectIndex !== null && this.onRectCreated) {
      this.onRectCreated(this.creatingRectIndex, this.exclusionRects[this.creatingRectIndex]);
    }
    // Reset all create state
    this.creatingRectIndex = null;
    this.dragState = null;
    this.createDragStart = null;
    this.rectCreateClickCount = 0;
    this.rectCreateCorner1 = null;
    this.rectCreateCorner2 = null;
    // Reset cursor
    this.overlay_dom.style.cursor = "default";
  }

  #drawDragHandles(ctx, zone) {
    if (!zone) return;

    const handles = this.#getHandlePositions(zone);
    if (!handles) return;

    const handleRadius = 12;

    for (const [name, pos] of Object.entries(handles)) {
      const isHovered = this.hoveredHandle === name;
      const isDragging = this.dragState?.handle === name;

      ctx.save();

      ctx.beginPath();
      ctx.arc(pos.x, pos.y, handleRadius, 0, 2 * Math.PI);

      if (isDragging) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
      } else if (isHovered) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.7)";
      } else {
        ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
      }
      ctx.fill();

      ctx.strokeStyle = "rgba(100, 100, 100, 0.8)";
      ctx.lineWidth = 2;
      ctx.stroke();

      ctx.translate(pos.x, pos.y);

      if (name === "startAngle" || name === "endAngle") {
        ctx.rotate(pos.angle + Math.PI / 2);
      } else {
        ctx.rotate(handles.innerDist.midAngle);
      }

      ctx.strokeStyle = "rgba(50, 50, 50, 0.9)";
      ctx.lineWidth = 2;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      const arrowSize = 5;
      const arrowLength = 6;

      ctx.beginPath();
      ctx.moveTo(-arrowLength, 0);
      ctx.lineTo(arrowLength, 0);
      ctx.moveTo(arrowLength - arrowSize, -arrowSize);
      ctx.lineTo(arrowLength, 0);
      ctx.lineTo(arrowLength - arrowSize, arrowSize);
      ctx.moveTo(-arrowLength + arrowSize, -arrowSize);
      ctx.lineTo(-arrowLength, 0);
      ctx.lineTo(-arrowLength + arrowSize, arrowSize);
      ctx.stroke();

      ctx.restore();
    }
  }

  #drawSectorDragHandles(ctx, sector) {
    if (!sector) return;

    const handles = this.#getSectorHandlePositions(sector);
    if (!handles) return;

    const handleRadius = 12;

    for (const [name, pos] of Object.entries(handles)) {
      const isHovered = this.hoveredHandle === name;
      const isDragging = this.dragState?.handle === name;

      ctx.save();

      ctx.beginPath();
      ctx.arc(pos.x, pos.y, handleRadius, 0, 2 * Math.PI);

      if (isDragging) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
      } else if (isHovered) {
        ctx.fillStyle = "rgba(255, 255, 255, 0.7)";
      } else {
        ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
      }
      ctx.fill();

      ctx.strokeStyle = "rgba(100, 100, 100, 0.8)";
      ctx.lineWidth = 2;
      ctx.stroke();

      ctx.translate(pos.x, pos.y);
      ctx.rotate(pos.angle + Math.PI / 2);

      ctx.strokeStyle = "rgba(50, 50, 50, 0.9)";
      ctx.lineWidth = 2;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      const arrowSize = 5;
      const arrowLength = 6;

      ctx.beginPath();
      ctx.moveTo(-arrowLength, 0);
      ctx.lineTo(arrowLength, 0);
      ctx.moveTo(arrowLength - arrowSize, -arrowSize);
      ctx.lineTo(arrowLength, 0);
      ctx.lineTo(arrowLength - arrowSize, arrowSize);
      ctx.moveTo(-arrowLength + arrowSize, -arrowSize);
      ctx.lineTo(-arrowLength, 0);
      ctx.lineTo(-arrowLength + arrowSize, arrowSize);
      ctx.stroke();

      ctx.restore();
    }
  }
}
