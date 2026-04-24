/**
 * Capabilities-Driven Radar Control Panel
 *
 * Dynamically builds the control UI based on radar capabilities.
 * Control building and updating logic adapted from v1/gui/control.js.
 * WebSocket streaming uses Signal K v3 protocol.
 */

export {
  loadRadar,
  registerRadarCallback,
  registerControlCallback,
  registerStreamMessageCallback,
  setCurrentRange,
  getPowerState,
  getControl,
  getOperatingTime,
  isPlaybackMode,
  getUserName,
  togglePower,
  zoomIn,
  zoomOut,
  getCurrentRangeDisplay,
  isAcquireTargetMode,
  setAcquireTargetMode,
  acquireTargetAtPosition,
  registerAcquireTargetModeCallback,
  getRadarId,
  subscribeToAis,
  unsubscribeFromAis,
};

import van from "./vendor/van-1.5.2.js";
import { toUser } from "./units.js";
import {
  fetchRadars,
  fetchRadarIds,
  fetchCapabilities,
  setControl as apiSetControl,
  acquireTarget as apiAcquireTarget,
  detectMode,
  isStandaloneMode,
  isPlaybackRadar,
} from "./api.js";
import { setZoneEditMode, setZoneCreateMode, setRectCreateMode, setSectorEditMode, updateZoneForEditing, updateRectForEditing } from "./viewer.js";
import { SpokeProcessingMode } from "./spoke_processor.js";

const { div, label, input, button, select, option, span } = van.tags;

// State
let radarId = null;
let myr_capabilities = null;
let stateWebSocket = null;
let radarCallbacks = [];
let controlCallbacks = [];
let streamMessageCallbacks = [];
let playbackMode = false;
let acquireTargetMode = false;
let acquireTargetModeCallbacks = [];
let acquireTargetModeTimer = null;

// Control state (from v1)
let myr_control_values = {};
let myr_error_message = null;

// Current range (for viewer.js integration)
let currentRange = 1852;
let lastRangeUpdateTime = 0;
let rangeUpdateCount = {};
let rangeFromSpokeData = false;

const control_prefix = "myr_control_";
const auto_postfix = "_auto";
const end_postfix = "_end";
const enabled_postfix = "_enabled";

function registerRadarCallback(callback) {
  radarCallbacks.push(callback);
}

function registerControlCallback(callback) {
  controlCallbacks.push(callback);
}

function registerStreamMessageCallback(callback) {
  streamMessageCallbacks.push(callback);
}

// Called from viewer.js when spoke data contains range
function setCurrentRange(meters) {
  if (meters <= 0) return;

  const now = Date.now();
  if (now - lastRangeUpdateTime > 2000) {
    rangeUpdateCount = {};
  }
  lastRangeUpdateTime = now;
  rangeUpdateCount[meters] = (rangeUpdateCount[meters] || 0) + 1;

  let maxCount = 0;
  let dominantRange = currentRange;
  for (const [range, count] of Object.entries(rangeUpdateCount)) {
    if (count > maxCount) {
      maxCount = count;
      dominantRange = parseInt(range);
    }
  }

  if (maxCount >= 5 && dominantRange !== currentRange) {
    currentRange = dominantRange;
    rangeFromSpokeData = true;
    const ranges = myr_capabilities?.supportedRanges || [];
    const newIndex = ranges.findIndex((r) => Math.abs(r - dominantRange) < 50);
    if (newIndex >= 0) {
      userRequestedRangeIndex = newIndex;
    }
    rangeUpdateCount = {};
  }
}

// ============================================================================
// Helper Classes (from v1)
// ============================================================================

class TemporaryMessage {
  timeoutId;
  element;

  constructor(id) {
    this.element = document.getElementById(id);
    this.element.style.hidden = true;
  }

  raise(aMessage) {
    this.element.style.hidden = false;
    this.element.classList.remove("myr_vanish");
    this.element.innerHTML = aMessage;
    this.timeoutId = setTimeout(() => {
      this.cancel();
    }, 5000);
  }

  cancel() {
    if (typeof this.timeoutId === "number") {
      clearTimeout(this.timeoutId);
    }
    this.element.classList.add("myr_vanish");
  }
}

// ============================================================================
// Control Building (from v1, adapted for v3 CSS)
// ============================================================================

function convertControlsToUserUnits(controls) {
  const result = {};

  Object.entries(controls).forEach(([id, control]) => {
    result[id] = convertControlToUserUnits(id, control);
  });

  return result;
}

function convertControlToUserUnits(id, control) {
  const result = {};

  let cloned = { id, ...control };

  if (cloned.units) {
    let units = cloned.units;
    if (units === "m" && cloned.maxValue < 100) {
      // leave this in meters
    } else {
      ["minValue", "maxValue", "stepValue"].forEach((prop) => {
        if (prop in cloned) {
          [units, cloned[prop]] = toUser(cloned.units, cloned[prop]);
        }
      });
    }
    cloned.user_units = units;
  }

  return cloned;
}

/**
 * Rounds a number to a limited number of decimals, for user pleasure.
 */
function roundToStep(value, stepValue) {
  value = Number(value);
  if (!Number.isFinite(value) || !Number.isFinite(stepValue)) return NaN;

  if (Math.abs(stepValue - 0.1) < Number.EPSILON) {
    return Number((value + stepValue / 2).toFixed(1));
  }
  if (stepValue < 0.02) {
    return Number((value + stepValue / 2).toFixed(2));
  }
  if (stepValue <= 1) {
    return Number((value + stepValue / 2).toFixed(1));
  }

  const scale = 1 / stepValue;
  const scaledVal = Math.round(value * scale);
  const scaledStep = Math.round(stepValue * scale);
  const roundedInt = Math.round(scaledVal / scaledStep) * scaledStep;
  const rounded = roundedInt / scale;

  return rounded;
}

// V1-style control builders adapted with v3 CSS classes
const ReadOnlyValue = (id, name) =>
  div(
    { class: "myr_control myr_readonly myr_info_stacked" },
    div({ class: "myr_control_label" }, name),
    div({ class: "myr_info_value", id: control_prefix + id })
  );

const StringValue = (id, name) =>
  div(
    { class: "myr_control myr_string_control" },
    span({ class: "myr_control_label" }, name),
    input({ type: "text", id: control_prefix + id, size: 20 }),
    button({ type: "button", onclick: (e) => do_button(e) }, "Set")
  );

const NumericValue = (id, name) =>
  div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_numeric",
        id: control_prefix + id + "_display",
      })
    ),
    input({
      type: "number",
      id: control_prefix + id,
      onchange: (e) => do_change(e.target),
      oninput: (e) => do_input(e),
    })
  );

const RangeValue = (id, name, min, max, def) =>
  div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_description",
        id: control_prefix + id + "_desc",
      })
    ),
    input({
      type: "range",
      class: "myr_slider",
      id: control_prefix + id,
      min,
      max,
      value: def,
      onchange: (e) => do_change(e.target),
    })
  );

// Discrete slider with tick marks showing possible values
const DiscreteSliderValue = (id, name, min, max, def) => {
  const numSteps = max - min;
  const ticks = [];
  for (let i = 0; i <= numSteps; i++) {
    ticks.push(
      div({
        class: "myr_tick",
        "data-index": i,
      })
    );
  }

  return div(
    { class: "myr_control myr_number_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value myr_description",
        id: control_prefix + id + "_desc",
      })
    ),
    div(
      { class: "myr_discrete_slider", id: control_prefix + id + "_container" },
      div({ class: "myr_slider_track" }),
      div({ class: "myr_tick_container" }, ...ticks),
      input({
        type: "range",
        class: "myr_slider myr_slider_discrete",
        id: control_prefix + id,
        min,
        max,
        value: def,
        onchange: (e) => do_change(e.target),
        oninput: (e) => updateTickMarks(e.target),
      })
    )
  );
};

function updateTickMarks(slider) {
  const container = slider.closest(".myr_discrete_slider");
  if (!container) return;

  const min = parseInt(slider.min);
  const max = parseInt(slider.max);
  const value = parseInt(slider.value);

  const ticks = container.querySelectorAll(".myr_tick");
  ticks.forEach((tick, i) => {
    const tickValue = min + i;
    tick.classList.toggle("myr_tick_active", tickValue === value);
  });
}

const ButtonValue = (id, name) =>
  div(
    { class: "myr_control myr_button_control" },
    button(
      {
        type: "button",
        class: "myr_action_button",
        id: control_prefix + id,
        onclick: (e) => do_change(e.target),
      },
      name
    )
  );

const AutoButton = (id) =>
  label(
    { class: "myr_auto_switch" },
    input({
      type: "checkbox",
      class: "myr_auto_toggle",
      id: control_prefix + id + auto_postfix,
      onchange: (e) => do_toggle_auto(e.target),
    }),
    span({ class: "myr_auto_slider" }),
    " Auto"
  );

const EnabledButton = (id) =>
  div(
    { class: "myr_enabled_button" },
    label(
      { class: "myr_checkbox_label" },
      input({
        type: "checkbox",
        class: "myr_enabled",
        id: control_prefix + id + enabled_postfix,
        onchange: (e) => do_change_enabled(e.target),
      }),
      " Enabled"
    )
  );

const SelectValue = (id, name, validValues, descriptions) => {
  return div(
    { class: "myr_control myr_enum_control" },
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      span({
        class: "myr_control_value",
        id: control_prefix + id + "_desc",
      })
    ),
    select(
      {
        class: "myr_select",
        id: control_prefix + id,
        onchange: (e) => do_change(e.target),
      },
      validValues.map((v) => option({ value: v }, descriptions[v]))
    )
  );
};

/**
 * Sector control - displays start and end angles with optional enabled checkbox
 * Server sends: value (start in radians), endValue (end in radians), enabled (optional)
 */
const SectorValue = (id, name, control) => {
  const prefix = `myr_control_${id}`;
  const hasEnabled = control.hasEnabled !== false;

  const min = control.minValue ?? -180;
  const max = control.maxValue ?? 180;

  function updateEditFields(sector) {
    // Update edit fields from sector data (radians)
    const startAngleDeg = Math.round((sector.startAngle * 180) / Math.PI);
    const endAngleDeg = Math.round((sector.endAngle * 180) / Math.PI);

    const startEl = document.getElementById(`${prefix}_edit_start`);
    const endEl = document.getElementById(`${prefix}_edit_end`);

    if (startEl) startEl.value = startAngleDeg;
    if (endEl) endEl.value = endAngleDeg;
  }

  function onDragEnd(newSector) {
    // Called when user finishes dragging a handle on the viewer
    updateEditFields(newSector);
  }

  function enterEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_sector_display");
    const editSection = container.querySelector(".myr_sector_edit");

    // Copy current values to edit fields
    const cv = myr_control_values[id] || {};
    const [, startAngle] = toUser(control.units, cv.value);
    const [, endAngle] = toUser(control.units, cv.endValue);

    document.getElementById(`${prefix}_edit_start`).value = startAngle ?? 0;
    document.getElementById(`${prefix}_edit_end`).value = endAngle ?? 0;
    document.getElementById(`${prefix}_edit_enabled`).checked = cv.enabled ?? false;

    displaySection.style.display = "none";
    editSection.style.display = "block";

    // Enable drag handles on the viewer
    setSectorEditMode(id, true, onDragEnd);
  }

  function exitEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_sector_display");
    const editSection = container.querySelector(".myr_sector_edit");

    displaySection.style.display = "block";
    editSection.style.display = "none";

    // Disable drag handles on the viewer
    setSectorEditMode(id, false);
  }

  function saveSector() {
    const startDeg = parseInt(document.getElementById(`${prefix}_edit_start`)?.value) || 0;
    const endDeg = parseInt(document.getElementById(`${prefix}_edit_end`)?.value) || 0;
    const enabledVal = document.getElementById(`${prefix}_edit_enabled`)?.checked ?? false;

    // Convert degrees to radians for server
    const startRad = (startDeg * Math.PI) / 180;
    const endRad = (endDeg * Math.PI) / 180;

    // Optimistically update local state for consistency
    myr_control_values[id] = {
      ...myr_control_values[id],
      value: startRad,
      endValue: endRad,
      enabled: enabledVal,
    };

    apiSetControl(radarId, id, {
      value: startRad,
      endValue: endRad,
      enabled: enabledVal,
    });

    exitEditMode();
  }

  return div(
    { class: "myr_control myr_sector_control", id: `myr_${id}` },
    // Hidden input for get_element_by_server_id to find
    input({ type: "hidden", id: `${control_prefix}${id}` }),
    // Header with label and Edit button
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      button(
        {
          type: "button",
          class: "myr_zone_edit_btn",
          onclick: enterEditMode,
        },
        "Edit"
      )
    ),
    // Read-only display section (clickable to enter edit mode)
    div(
      { class: "myr_sector_display", onclick: enterEditMode },
      div(
        { class: "myr_sector_summary" },
        div(
          { class: "myr_zone_summary_row" },
          span({ class: "myr_zone_summary_label" }, "Angle: "),
          span({ id: `${prefix}_display_angle` }, "0° - 0°")
        )
      )
    ),
    // Edit section (hidden by default)
    div(
      { class: "myr_sector_edit", style: "display: none;" },
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_start` }, "Start°"),
          input({
            type: "number",
            id: `${prefix}_edit_start`,
            min: min,
            max: max,
            value: 0,
          })
        ),
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_end` }, "End°"),
          input({
            type: "number",
            id: `${prefix}_edit_end`,
            min: min,
            max: max,
            value: 0,
          })
        )
      ),
      hasEnabled
        ? div(
            { class: "myr_zone_enabled" },
            label(
              { class: "myr_checkbox_label" },
              input({
                type: "checkbox",
                id: `${prefix}_edit_enabled`,
              }),
              " Enabled"
            )
          )
        : null,
      div(
        { class: "myr_zone_buttons" },
        button(
          {
            type: "button",
            class: "myr_zone_cancel_btn",
            onclick: exitEditMode,
          },
          "Cancel"
        ),
        button(
          {
            type: "button",
            class: "myr_zone_save_btn",
            onclick: saveSector,
          },
          "Save"
        )
      )
    )
  );
};

/**
 * Update sector control UI from server state (read-only display)
 */
function updateSectorUI(id, control, cv) {
  const prefix = `myr_control_${id}`;

  // Update display values
  const angleDisplay = document.getElementById(`${prefix}_display_angle`);

  if (angleDisplay) {
    let [, startAngle] = toUser(control.units, cv.value);
    let [, endAngle] = toUser(control.units, cv.endValue);
    if (control.stepValue) {
      startAngle = roundToStep(startAngle ?? 0, control.stepValue);
      endAngle = roundToStep(endAngle ?? 0, control.stepValue);
    }
    angleDisplay.textContent = `${startAngle ?? 0}° - ${endAngle ?? 0}°`;
  }

  // Hide display section when not enabled
  const container = document.getElementById(`myr_${id}`);
  const displaySection = container?.querySelector(".myr_sector_display");
  if (displaySection) {
    displaySection.style.display = cv.enabled ? "block" : "none";
  }
}

/**
 * Zone control - displays start/end angles and start/end distances
 * Shows read-only summary with Edit button; edit mode shows all fields with Cancel/Save
 * Server sends: value (start angle in radians), endValue (end angle in radians),
 *               startDistance (meters), endDistance (meters), enabled
 */
const ZoneValue = (id, name, control) => {
  const prefix = `myr_control_${id}`;

  const minAngle = control.minValue ?? -180;
  const maxAngle = control.maxValue ?? 180;
  const maxDist = control.maxDistance ?? 100000;

  function updateEditFields(zone) {
    // Update edit fields from zone data (radians/meters)
    const startAngleDeg = Math.round((zone.startAngle * 180) / Math.PI);
    const endAngleDeg = Math.round((zone.endAngle * 180) / Math.PI);

    const startAngleEl = document.getElementById(`${prefix}_edit_start_angle`);
    const endAngleEl = document.getElementById(`${prefix}_edit_end_angle`);
    const startDistEl = document.getElementById(`${prefix}_edit_start_dist`);
    const endDistEl = document.getElementById(`${prefix}_edit_end_dist`);

    if (startAngleEl) startAngleEl.value = startAngleDeg;
    if (endAngleEl) endAngleEl.value = endAngleDeg;
    if (startDistEl) startDistEl.value = Math.round(zone.startDistance);
    if (endDistEl) endDistEl.value = Math.round(zone.endDistance);
  }

  function onDragEnd(newZone) {
    // Called when user finishes dragging a handle on the viewer
    updateEditFields(newZone);
  }

  function onDragMove(newZone) {
    // Called during dragging - update edit fields in real time
    updateEditFields(newZone);
  }

  function onZoneCreated(zone) {
    // Called when user finishes drawing a zone via click-drag
    updateEditFields(zone);
    // Exit create mode, enter regular edit mode for adjustments
    setZoneCreateMode(id, false);
    setZoneEditMode(id, true, onDragEnd, onDragMove);
    // Enable the zone checkbox since user drew it
    document.getElementById(`${prefix}_edit_enabled`).checked = true;
  }

  function startDrawMode() {
    // First ensure we're in edit mode (shows the edit section)
    const container = document.getElementById(`myr_${id}`);
    const editSection = container.querySelector(".myr_zone_edit");
    if (editSection.style.display === "none") {
      enterEditMode();
    }
    // Disable regular edit handles, enable create mode
    setZoneEditMode(id, false);
    setZoneCreateMode(id, true, onZoneCreated);
  }

  function updateZonePreview() {
    // Called when edit fields change - update the zone preview on the viewer
    const startDeg = parseInt(document.getElementById(`${prefix}_edit_start_angle`)?.value) || 0;
    const endDeg = parseInt(document.getElementById(`${prefix}_edit_end_angle`)?.value) || 0;
    const startDist = parseInt(document.getElementById(`${prefix}_edit_start_dist`)?.value) || 0;
    const endDist = parseInt(document.getElementById(`${prefix}_edit_end_dist`)?.value) || 0;

    // Convert degrees to radians for the viewer
    const startRad = (startDeg * Math.PI) / 180;
    const endRad = (endDeg * Math.PI) / 180;

    updateZoneForEditing(id, {
      startAngle: startRad,
      endAngle: endRad,
      startDistance: startDist,
      endDistance: endDist,
      enabled: true, // Always draw during editing
    });
  }

  function enterEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_zone_display");
    const editSection = container.querySelector(".myr_zone_edit");

    // Copy current values to edit fields
    const cv = myr_control_values[id] || {};
    const [, startAngle] = toUser(control.units, cv.value);
    const [, endAngle] = toUser(control.units, cv.endValue);

    document.getElementById(`${prefix}_edit_start_angle`).value = startAngle ?? 0;
    document.getElementById(`${prefix}_edit_end_angle`).value = endAngle ?? 0;
    document.getElementById(`${prefix}_edit_start_dist`).value = Math.round(cv.startDistance ?? 0);
    document.getElementById(`${prefix}_edit_end_dist`).value = Math.round(cv.endDistance ?? 0);
    document.getElementById(`${prefix}_edit_enabled`).checked = cv.enabled ?? false;

    displaySection.style.display = "none";
    editSection.style.display = "block";

    // Enable drag handles on the viewer
    setZoneEditMode(id, true, onDragEnd, onDragMove);

    // Initialize zone preview
    updateZonePreview();
  }

  function exitEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_zone_display");
    const editSection = container.querySelector(".myr_zone_edit");

    displaySection.style.display = "block";
    editSection.style.display = "none";

    // Disable drag handles on the viewer
    setZoneEditMode(id, false);

    // Restore zone to server state
    const cv = myr_control_values[id] || {};
    if (cv.enabled) {
      updateZoneForEditing(id, {
        startAngle: cv.value ?? 0,
        endAngle: cv.endValue ?? 0,
        startDistance: cv.startDistance ?? 0,
        endDistance: cv.endDistance ?? 0,
        enabled: true,
      });
    } else {
      // Zone was not enabled, clear it
      updateZoneForEditing(id, null);
    }
  }

  function saveZone() {
    const startDeg = parseInt(document.getElementById(`${prefix}_edit_start_angle`)?.value) || 0;
    const endDeg = parseInt(document.getElementById(`${prefix}_edit_end_angle`)?.value) || 0;
    const startDist = parseInt(document.getElementById(`${prefix}_edit_start_dist`)?.value) || 0;
    const endDist = parseInt(document.getElementById(`${prefix}_edit_end_dist`)?.value) || 0;
    const enabledVal = document.getElementById(`${prefix}_edit_enabled`)?.checked ?? false;

    // Convert degrees to radians for server
    const startRad = (startDeg * Math.PI) / 180;
    const endRad = (endDeg * Math.PI) / 180;

    // Optimistically update local state so exitEditMode() uses the new values
    // The server will confirm via WebSocket, but this prevents the race condition
    // where exitEditMode() restores the old values before the WebSocket arrives
    myr_control_values[id] = {
      ...myr_control_values[id],
      value: startRad,
      endValue: endRad,
      startDistance: startDist,
      endDistance: endDist,
      enabled: enabledVal,
    };

    apiSetControl(radarId, id, {
      value: startRad,
      endValue: endRad,
      startDistance: startDist,
      endDistance: endDist,
      enabled: enabledVal,
    });

    exitEditMode();
  }

  return div(
    { class: "myr_control myr_zone_control", id: `myr_${id}` },
    // Hidden input for get_element_by_server_id to find
    input({ type: "hidden", id: `${control_prefix}${id}` }),
    // Header with label and Edit button
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      button(
        {
          type: "button",
          class: "myr_zone_edit_btn",
          onclick: enterEditMode,
        },
        "Edit"
      )
    ),
    // Read-only display section (clickable to enter edit mode)
    div(
      { class: "myr_zone_display", onclick: enterEditMode },
      div(
        { class: "myr_zone_summary" },
        div(
          { class: "myr_zone_summary_row" },
          span({ class: "myr_zone_summary_label" }, "Angle: "),
          span({ id: `${prefix}_display_angle` }, "0° - 0°")
        ),
        div(
          { class: "myr_zone_summary_row" },
          span({ class: "myr_zone_summary_label" }, "Distance: "),
          span({ id: `${prefix}_display_dist` }, "0 - 0 m")
        )
      )
    ),
    // Edit section (hidden by default)
    div(
      { class: "myr_zone_edit", style: "display: none;" },
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_start_angle` }, "Start°"),
          input({
            type: "number",
            id: `${prefix}_edit_start_angle`,
            min: minAngle,
            max: maxAngle,
            value: 0,
            oninput: updateZonePreview,
          })
        ),
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_end_angle` }, "End°"),
          input({
            type: "number",
            id: `${prefix}_edit_end_angle`,
            min: minAngle,
            max: maxAngle,
            value: 0,
            oninput: updateZonePreview,
          })
        )
      ),
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_start_dist` }, "Inner (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_start_dist`,
            min: 0,
            max: maxDist,
            value: 0,
            oninput: updateZonePreview,
          })
        ),
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_end_dist` }, "Outer (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_end_dist`,
            min: 0,
            max: maxDist,
            value: 0,
            oninput: updateZonePreview,
          })
        )
      ),
      div(
        { class: "myr_zone_enabled" },
        label(
          { class: "myr_checkbox_label" },
          input({
            type: "checkbox",
            id: `${prefix}_edit_enabled`,
          }),
          " Enabled"
        )
      ),
      div(
        { class: "myr_zone_buttons" },
        button(
          {
            type: "button",
            class: "myr_zone_draw_btn",
            onclick: startDrawMode,
            title: "Click and drag on radar to draw zone",
          },
          "Draw"
        ),
        button(
          {
            type: "button",
            class: "myr_zone_cancel_btn",
            onclick: exitEditMode,
          },
          "Cancel"
        ),
        button(
          {
            type: "button",
            class: "myr_zone_save_btn",
            onclick: saveZone,
          },
          "Save"
        )
      )
    )
  );
};

/**
 * Update zone control UI from server state (read-only display)
 */
function updateZoneUI(id, control, cv) {
  const prefix = `myr_control_${id}`;

  // Update display values
  const angleDisplay = document.getElementById(`${prefix}_display_angle`);
  const distDisplay = document.getElementById(`${prefix}_display_dist`);

  if (angleDisplay) {
    let [, startAngle] = toUser(control.units, cv.value);
    let [, endAngle] = toUser(control.units, cv.endValue);
    if (control.stepValue) {
      startAngle = roundToStep(startAngle ?? 0, control.stepValue);
      endAngle = roundToStep(endAngle ?? 0, control.stepValue);
    }
    angleDisplay.textContent = `${startAngle ?? 0}° - ${endAngle ?? 0}°`;
  }
  if (distDisplay) {
    const startDist = Math.round(cv.startDistance ?? 0);
    const endDist = Math.round(cv.endDistance ?? 0);
    distDisplay.textContent = `${startDist} - ${endDist} m`;
  }

  // Hide display section when not enabled
  const container = document.getElementById(`myr_${id}`);
  const displaySection = container?.querySelector(".myr_zone_display");
  if (displaySection) {
    displaySection.style.display = cv.enabled ? "block" : "none";
  }
}

/**
 * Rect control - displays corner-based rectangular exclusion zones
 * Two corners (x1,y1), (x2,y2) define one edge, width is perpendicular
 * Shows read-only summary with Edit button; edit mode shows all fields with Cancel/Save
 */
const RectValue = (id, name, control) => {
  const prefix = `myr_control_${id}`;
  const maxDist = control.maxValue ?? 100000;

  function updateEditFields(rect) {
    const x1El = document.getElementById(`${prefix}_edit_x1`);
    const y1El = document.getElementById(`${prefix}_edit_y1`);
    const x2El = document.getElementById(`${prefix}_edit_x2`);
    const y2El = document.getElementById(`${prefix}_edit_y2`);
    const widthEl = document.getElementById(`${prefix}_edit_width`);

    if (x1El) x1El.value = Math.round(rect.x1 ?? 0);
    if (y1El) y1El.value = Math.round(rect.y1 ?? 0);
    if (x2El) x2El.value = Math.round(rect.x2 ?? 0);
    if (y2El) y2El.value = Math.round(rect.y2 ?? 0);
    if (widthEl) widthEl.value = Math.round(rect.width ?? 0);
  }

  function onRectCreated(rect) {
    updateEditFields(rect);
    setRectCreateMode(id, false);
    document.getElementById(`${prefix}_edit_enabled`).checked = true;
  }

  function startDrawMode() {
    const container = document.getElementById(`myr_${id}`);
    const editSection = container.querySelector(".myr_rect_edit");
    if (editSection.style.display === "none") {
      enterEditMode();
    }
    setRectCreateMode(id, true, onRectCreated);
  }

  function updateRectPreview() {
    const x1 = parseFloat(document.getElementById(`${prefix}_edit_x1`)?.value) || 0;
    const y1 = parseFloat(document.getElementById(`${prefix}_edit_y1`)?.value) || 0;
    const x2 = parseFloat(document.getElementById(`${prefix}_edit_x2`)?.value) || 0;
    const y2 = parseFloat(document.getElementById(`${prefix}_edit_y2`)?.value) || 0;
    const width = parseFloat(document.getElementById(`${prefix}_edit_width`)?.value) || 0;

    updateRectForEditing(id, {
      x1,
      y1,
      x2,
      y2,
      width,
      enabled: true,
    });
  }

  function enterEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_rect_display");
    const editSection = container.querySelector(".myr_rect_edit");

    const cv = myr_control_values[id] || {};
    document.getElementById(`${prefix}_edit_x1`).value = Math.round(cv.x1 ?? 0);
    document.getElementById(`${prefix}_edit_y1`).value = Math.round(cv.y1 ?? 0);
    document.getElementById(`${prefix}_edit_x2`).value = Math.round(cv.x2 ?? 0);
    document.getElementById(`${prefix}_edit_y2`).value = Math.round(cv.y2 ?? 0);
    document.getElementById(`${prefix}_edit_width`).value = Math.round(cv.width ?? 0);
    document.getElementById(`${prefix}_edit_enabled`).checked = cv.enabled ?? false;

    displaySection.style.display = "none";
    editSection.style.display = "block";

    updateRectPreview();
  }

  function exitEditMode() {
    const container = document.getElementById(`myr_${id}`);
    const displaySection = container.querySelector(".myr_rect_display");
    const editSection = container.querySelector(".myr_rect_edit");

    displaySection.style.display = "block";
    editSection.style.display = "none";

    setRectCreateMode(id, false);

    const cv = myr_control_values[id] || {};
    if (cv.enabled) {
      updateRectForEditing(id, {
        x1: cv.x1 ?? 0,
        y1: cv.y1 ?? 0,
        x2: cv.x2 ?? 0,
        y2: cv.y2 ?? 0,
        width: cv.width ?? 0,
        enabled: true,
      });
    } else {
      updateRectForEditing(id, null);
    }
  }

  function saveRect() {
    const x1 = parseFloat(document.getElementById(`${prefix}_edit_x1`)?.value) || 0;
    const y1 = parseFloat(document.getElementById(`${prefix}_edit_y1`)?.value) || 0;
    const x2 = parseFloat(document.getElementById(`${prefix}_edit_x2`)?.value) || 0;
    const y2 = parseFloat(document.getElementById(`${prefix}_edit_y2`)?.value) || 0;
    const width = parseFloat(document.getElementById(`${prefix}_edit_width`)?.value) || 0;
    const enabledVal = document.getElementById(`${prefix}_edit_enabled`)?.checked ?? false;

    myr_control_values[id] = {
      ...myr_control_values[id],
      x1,
      y1,
      x2,
      y2,
      width,
      enabled: enabledVal,
    };

    apiSetControl(radarId, id, {
      x1,
      y1,
      x2,
      y2,
      width,
      enabled: enabledVal,
    });

    exitEditMode();
  }

  return div(
    { class: "myr_control myr_rect_control", id: `myr_${id}` },
    input({ type: "hidden", id: `${control_prefix}${id}` }),
    div(
      { class: "myr_control_header" },
      span({ class: "myr_control_label" }, name),
      button(
        {
          type: "button",
          class: "myr_zone_edit_btn",
          onclick: enterEditMode,
        },
        "Edit"
      )
    ),
    div(
      { class: "myr_rect_display", onclick: enterEditMode },
      div(
        { class: "myr_zone_summary" },
        div(
          { class: "myr_zone_summary_row" },
          span({ class: "myr_zone_summary_label" }, "Edge: "),
          span({ id: `${prefix}_display_edge` }, "(0,0) - (0,0)")
        ),
        div(
          { class: "myr_zone_summary_row" },
          span({ class: "myr_zone_summary_label" }, "Width: "),
          span({ id: `${prefix}_display_width` }, "0 m")
        )
      )
    ),
    div(
      { class: "myr_rect_edit", style: "display: none;" },
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_x1` }, "X1 (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_x1`,
            min: -maxDist,
            max: maxDist,
            value: 0,
            oninput: updateRectPreview,
          })
        ),
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_y1` }, "Y1 (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_y1`,
            min: -maxDist,
            max: maxDist,
            value: 0,
            oninput: updateRectPreview,
          })
        )
      ),
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_x2` }, "X2 (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_x2`,
            min: -maxDist,
            max: maxDist,
            value: 0,
            oninput: updateRectPreview,
          })
        ),
        div(
          { class: "myr_zone_field" },
          label({ for: `${prefix}_edit_y2` }, "Y2 (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_y2`,
            min: -maxDist,
            max: maxDist,
            value: 0,
            oninput: updateRectPreview,
          })
        )
      ),
      div(
        { class: "myr_zone_row" },
        div(
          { class: "myr_zone_field myr_zone_field_wide" },
          label({ for: `${prefix}_edit_width` }, "Width (m)"),
          input({
            type: "number",
            id: `${prefix}_edit_width`,
            min: 0,
            max: maxDist,
            value: 0,
            oninput: updateRectPreview,
          })
        )
      ),
      div(
        { class: "myr_zone_enabled" },
        label(
          { class: "myr_checkbox_label" },
          input({
            type: "checkbox",
            id: `${prefix}_edit_enabled`,
          }),
          " Enabled"
        )
      ),
      div(
        { class: "myr_zone_buttons" },
        button(
          {
            type: "button",
            class: "myr_zone_draw_btn",
            onclick: startDrawMode,
            title: "Click 3 times on radar: corner 1, corner 2, then width",
          },
          "Draw"
        ),
        button(
          {
            type: "button",
            class: "myr_zone_cancel_btn",
            onclick: exitEditMode,
          },
          "Cancel"
        ),
        button(
          {
            type: "button",
            class: "myr_zone_save_btn",
            onclick: saveRect,
          },
          "Save"
        )
      )
    )
  );
};

/**
 * Update rect control UI from server state (read-only display)
 */
function updateRectUI(id, control, cv) {
  const prefix = `myr_control_${id}`;

  const edgeDisplay = document.getElementById(`${prefix}_display_edge`);
  const widthDisplay = document.getElementById(`${prefix}_display_width`);

  if (edgeDisplay) {
    const x1 = Math.round(cv.x1 ?? 0);
    const y1 = Math.round(cv.y1 ?? 0);
    const x2 = Math.round(cv.x2 ?? 0);
    const y2 = Math.round(cv.y2 ?? 0);
    edgeDisplay.textContent = `(${x1},${y1}) - (${x2},${y2})`;
  }
  if (widthDisplay) {
    const width = Math.round(cv.width ?? 0);
    widthDisplay.textContent = `${width} m`;
  }

  const container = document.getElementById(`myr_${id}`);
  const displaySection = container?.querySelector(".myr_rect_display");
  if (displaySection) {
    displaySection.style.display = cv.enabled ? "block" : "none";
  }
}

function buildControls() {
  let controlsEl = document.getElementById("myr_controls");
  if (!controlsEl) return;
  controlsEl.innerHTML = "";

  // First, collect all controls and sort by id
  const sortById = (a, b) => (a.id || 0) - (b.id || 0);
  const allControls = Object.entries(myr_capabilities.controls)
    .filter(([k]) => k !== "power" && k !== "range")
    .map(([k, v]) => ({ ...v, controlId: k }))
    .sort(sortById);

  // Group controls by category, preserving order of first occurrence
  const categories = {};
  const categoryOrder = [];

  for (const control of allControls) {
    const category = control.category || "basic";

    if (!categories[category]) {
      categories[category] = [];
      categoryOrder.push(category);
    }
    categories[category].push(control);
  }

  // Build sections for each category in order
  for (const category of categoryOrder) {
    const categoryTitle = category.charAt(0).toUpperCase() + category.slice(1);
    const section = div(
      { class: `myr_control_section myr_${category}_section` },
      div({ class: "myr_section_header" }, categoryTitle)
    );
    van.add(controlsEl, section);

    for (const control of categories[category]) {
      const k = control.controlId;
      const v = control;

      van.add(section, buildSingleControl(k, v));

      // Add auto/enabled buttons
      if (v.hasAuto) {
        van.add(get_element_by_server_id(k).parentNode, AutoButton(k));
      }
      if (v.hasEnabled && !v.isReadOnly && v.dataType !== "sector" && v.dataType !== "zone") {
        van.add(get_element_by_server_id(k).parentNode, EnabledButton(k));
      }
    }

    // Add "Acquire Target" button to the targets section
    if (category === "targets") {
      van.add(section, buildAcquireTargetControl());
    }
  }
}

/**
 * Build the Acquire Target control with button and status text
 */
function buildAcquireTargetControl() {
  return div(
    { class: "myr_control myr_acquire_target_control" },
    button(
      {
        type: "button",
        class: "myr_action_button myr_acquire_target_btn",
        id: "myr_acquire_target_btn",
        onclick: () => setAcquireTargetMode(!acquireTargetMode),
      },
      "Acquire targets"
    ),
    div(
      {
        class: "myr_acquire_target_status",
        id: "myr_acquire_target_status",
        style: "display: none;",
      },
      ""
    )
  );
}

function buildSingleControl(k, v) {
  if (v.isReadOnly || v.readOnly) {
    return ReadOnlyValue(k, v.name);
  } else if (v.dataType === "button") {
    return ButtonValue(k, v.name);
  } else if (v.dataType === "string") {
    return StringValue(k, v.name);
  } else if (v.dataType === "sector") {
    return SectorValue(k, v.name, v);
  } else if (v.dataType === "zone") {
    return ZoneValue(k, v.name, v);
  } else if (v.dataType === "rect") {
    return RectValue(k, v.name, v);
  } else if ("validValues" in v && "descriptions" in v) {
    return SelectValue(k, v.name, v.validValues, v.descriptions);
  } else if (
    "maxValue" in v &&
    v.maxValue <= 100 &&
    (!v.units || (v.units !== "m/s" && v.units !== "m" && v.units !== "deg"))
  ) {
    const min = v.minValue || 0;
    const max = v.maxValue;
    const numSteps = max - min;
    // Use discrete slider with tick marks for controls with few values (2-10)
    if (numSteps >= 1 && numSteps <= 9) {
      return DiscreteSliderValue(k, v.name, min, max, 0);
    }
    return RangeValue(k, v.name, min, max, 0);
  } else {
    return NumericValue(k, v.name);
  }
}

// ============================================================================
// Control Value Setting (from v1 setControl)
// ============================================================================

function setControlValue(cv) {
  myr_control_values[cv.id] = cv;

  let i = get_element_by_server_id(cv.id);
  let control = getControl(cv.id);
  let units = undefined;
  var value;

  // Update DOM elements if they exist
  if (i && control) {
    if (control.hasAutoAdjustable && cv.auto) {
      value = cv.autoValue;
    } else {
      value = cv.value;
    }

    let html = value;
    if (control.units && cv.id !== "range") {
      [units, value] = toUser(control.units, value);
      if (control.stepValue) {
        value = roundToStep(value, control.stepValue);
      }
      // Floor time values displayed in hours (operating time, transmit time)
      if (units === "h") {
        value = Math.floor(value);
      }
      html = value + " " + units;
    }

    // For read-only controls, update the element directly (it's a span with myr_info_value)
    if (control.isReadOnly || control.readOnly) {
      i.innerHTML = html;
    } else if (control && control.dataType === "sector") {
      updateSectorUI(cv.id, control, cv);
    } else if (control && control.dataType === "zone") {
      updateZoneUI(cv.id, control, cv);
    } else if (control && control.dataType === "rect") {
      updateRectUI(cv.id, control, cv);
    } else {
      // Update numeric display
      let n = document.getElementById(control_prefix + cv.id + "_display");
      if (!n) {
        n = i.parentNode.querySelector(".myr_numeric");
      }
      if (n) {
        n.innerHTML = html;
      }

      // Update description display
      let d = document.getElementById(control_prefix + cv.id + "_desc");
      if (!d) {
        d = i.parentNode.querySelector(".myr_description");
      }
      if (d) {
        let description = control.descriptions
          ? control.descriptions[value]
          : undefined;
        if (!description && control.hasAutoAdjustable) {
          if (cv.auto) {
            description =
              "A" + (value > 0 ? "+" + value : "") + (value < 0 ? value : "");
            i.min = control.autoAdjustMinValue;
            i.max = control.autoAdjustMaxValue;
          } else {
            i.min = control.minValue;
            i.max = control.maxValue;
          }
        }
        if (!description) {
          description = html;
        }
        d.innerHTML = description;
      }

      // Set input value after setting min/max
      i.value = value;

      // Update tick marks for discrete sliders
      if (i.classList.contains("myr_slider_discrete")) {
        updateTickMarks(i);
      }

      // Handle auto toggle switch
      if (control.hasAuto && "auto" in cv) {
        let autoCheckbox = i.parentNode.querySelector(".myr_auto_toggle");
        if (autoCheckbox) {
          autoCheckbox.checked = cv.auto;
        }
        let display = cv.auto && !control.hasAutoAdjustable ? "none" : "block";
        if (n) n.style.display = display;
        if (d) d.style.display = display;
        i.style.display = display;
      }

      // Handle enabled checkbox
      if ("enabled" in cv) {
        let checkbox = i.parentNode.querySelector(".myr_enabled");
        if (checkbox) {
          checkbox.checked = cv.enabled;
        }
        let display = cv.enabled ? "block" : "none";
        if (n) n.style.display = display;
        if (d) d.style.display = display;
        i.style.display = display;
      }

      // Special handling for Spoke Processing control - update PPI
      if (cv.id === "spokeProcessing" && window.ppi?.setProcessingMode) {
        window.ppi.setProcessingMode(SpokeProcessingMode.fromIndex(cv.value));
      }

      // Handle allowed/disallowed state
      if (cv.hasOwnProperty("allowed")) {
        let p = i.parentNode;
        if (!cv.allowed) {
          p.classList.add("myr_readonly");
          i.disabled = true;
        } else {
          p.classList.remove("myr_readonly");
          i.disabled = false;
        }
      }
    }

    // Show error if present
    if (cv.error && myr_error_message) {
      myr_error_message.raise(cv.error);
    }
  }

  // Always notify control callbacks (even if no DOM element exists)
  controlCallbacks.forEach((cb) => {
    cb(cv.id, cv);
  });
}

// ============================================================================
// Event Handlers (from v1)
// ============================================================================

function do_change(v) {
  let id = html_to_server_id(v.id);

  let control = getControl(id);
  let update = myr_control_values[id] ?? { id };
  let message = {};
  let value = v.value;

  if ("user_units" in control && id !== "range") {
    message.units = control.user_units;
    value = Number(value);
  }

  // Check if auto mode is active from current control state
  let auto = update.auto || false;
  update.auto = auto;
  message.auto = auto;
  if (auto && control.hasAutoAdjustable) {
    update.autoValue = value;
    message.autoValue = value;
  } else {
    update.value = value;
    message.value = value;
  }

  let checkbox = document.getElementById(v.id + enabled_postfix);
  if (checkbox) {
    update.enabled = checkbox.checked;
    message.enabled = checkbox.checked;
  }

  setControlValue(update);
  sendControlToServer(id, message);
}

function do_toggle_auto(checkbox) {
  let id = html_to_server_id(checkbox.id);

  let update = myr_control_values[id] || { id: id };
  update.auto = checkbox.checked;
  setControlValue(update);

  sendControlToServer(id, { id: id, auto: checkbox.checked });
}

function do_change_enabled(checkbox) {
  let v = document.getElementById(html_to_value_id(checkbox.id));
  do_change(v);
}

function do_button(e) {
  let v = e.target.previousElementSibling;
  let id = html_to_server_id(v.id);
  sendControlToServer(id, { id: id, value: v.value });
}

function do_input() {
  // Real-time feedback while dragging (optional)
}

async function sendControlToServer(controlId, message) {
  if (playbackMode) {
    console.log(`Playback mode: ignoring control ${controlId}`);
    return;
  }

  console.log(`Sending control: ${controlId} = ${JSON.stringify(message)}`);

  const success = await apiSetControl(radarId, controlId, message);
}

// ============================================================================
// ID Conversion Helpers
// ============================================================================

function get_element_by_server_id(id) {
  let did = control_prefix + id;
  return document.getElementById(did);
}

function html_to_server_id(id) {
  let r = id;
  if (r.startsWith(control_prefix)) {
    r = r.substr(control_prefix.length);
  }
  return html_to_value_id(r);
}

function html_to_value_id(id) {
  let r = id;
  if (r.endsWith(auto_postfix)) {
    r = r.substr(0, r.length - auto_postfix.length);
  }
  if (r.endsWith(enabled_postfix)) {
    r = r.substr(0, r.length - enabled_postfix.length);
  }
  return r;
}

// ============================================================================
// WebSocket State Streaming (v2 Signal K protocol)
// ============================================================================

let reconnectAttempts = 0;
let reconnectTimer = null;
const MAX_RECONNECT_DELAY = 30000;
const BASE_RECONNECT_DELAY = 1000;

function connectStateStream(streamUrl, radarIdParam) {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  // Only show DISCONNECTED if the state stream wasn't already open.
  // When loadRadar restarts the spoke stream it also calls connectStateStream,
  // which may replace a working state connection. Showing DISCONNECTED in that
  // case causes a visible flash while waiting for the server to re-send power
  // state — Firefox is slower to complete this round-trip than Chrome/Safari.
  const wasConnected = stateWebSocket?.readyState === WebSocket.OPEN;
  if (stateWebSocket) {
    stateWebSocket.close();
    stateWebSocket = null;
  }

  const streamUrlWithParams = streamUrl.includes("?")
    ? `${streamUrl}&subscribe=none`
    : `${streamUrl}?subscribe=none`;

  console.log(`Connecting to state stream: ${streamUrlWithParams}`);

  const ws = new WebSocket(streamUrlWithParams);
  stateWebSocket = ws;

  ws.onopen = () => {
    console.log("State stream connected");
    reconnectAttempts = 0;
    // Power state will be updated when we receive the first control values
  };

  if (!wasConnected) {
    notifyDisconnected();
  }

  ws.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data);

      if (message.updates) {
        for (const update of message.updates) {
          if (update.meta) {
            for (const item of update.meta) {
              const pathParts = item.path.split(".");
              if (
                pathParts.length === 4 &&
                pathParts[0] === "radars" &&
                pathParts[1] === radarIdParam &&
                pathParts[2] === "controls"
              ) {
                const controlId = pathParts[pathParts.length - 1];

                let control = convertControlToUserUnits(controlId, item.value);
                let newc = JSON.stringify(control);
                let oldc = JSON.stringify(myr_capabilities.controls[controlId]);
                if (oldc != newc) {
                  console.log(
                    `meta data changed: ${controlId} from ${oldc} to ${newc}`
                  );
                  myr_capabilities.controls[controlId] = control;
                } else {
                  console.log(`No change to meta data for ${controlId}`);
                }
              }
            }
          }
          if (update.values) {
            for (const item of update.values) {
              const pathParts = item.path.split(".");
              if (
                pathParts.length === 4 &&
                pathParts[0] === "radars" &&
                pathParts[1] === radarIdParam &&
                pathParts[2] === "controls"
              ) {
                const controlId = pathParts[pathParts.length - 1];

                console.log(
                  `Receiving control value: ${controlId} = ${JSON.stringify(
                    item.value
                  )}`
                );

                const cv = { ...item.value, id: controlId };
                setControlValue(cv);
              }
              // Notify stream message callbacks for all values (targets, etc.)
              streamMessageCallbacks.forEach((cb) => {
                cb(item.path, item.value);
              });
            }
          }
        }
      } else if (message.name && message.version) {
        console.log("Connected to " + message.name + " v" + message.version);

        const subscription = {
          subscribe: [
            {
              path: `radars.${radarIdParam}.controls.*`,
              policy: "instant",
            },
            {
              path: `radars.${radarIdParam}.targets.*`,
              policy: "instant",
            },
          ],
        };

        console.log("Subscribing to radar controls and targets:", subscription);
        ws.send(JSON.stringify(subscription));
      }
    } catch (err) {
      console.error("Error processing state stream message:", err);
    }
  };

  ws.onerror = (error) => {
    console.error("State stream error:", error);
    notifyDisconnected();
  };

  ws.onclose = () => {
    if (stateWebSocket !== ws) {
      return; // Superseded by a newer connection
    }
    console.log("State stream closed");
    stateWebSocket = null;
    notifyDisconnected();

    reconnectAttempts++;
    const delay = Math.min(
      BASE_RECONNECT_DELAY * Math.pow(2, reconnectAttempts - 1),
      MAX_RECONNECT_DELAY
    );

    console.log(
      `Reconnecting state stream in ${delay}ms (attempt ${reconnectAttempts})`
    );
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (radarId) {
        connectStateStream(streamUrl, radarIdParam);
      }
    }, delay);
  };
}

function disconnectStateStream() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (stateWebSocket) {
    stateWebSocket.close();
    stateWebSocket = null;
  }
  reconnectAttempts = 0;
}

/**
 * Notify callbacks that connection to the server is lost
 */
function notifyDisconnected() {
  controlCallbacks.forEach((cb) => {
    cb("power", { id: "power", value: "disconnected" });
  });
}

// ============================================================================
// Initialization
// ============================================================================

setTimeout(() => {
  if (radarCallbacks.length === 0) {
    window.onload = function () {
      const urlParams = new URLSearchParams(window.location.search);
      const id = urlParams.get("id");
      loadRadar(id);
    };
  }
}, 0);

async function loadRadar(id) {
  try {
    await detectMode();

    if (!id) {
      const ids = await fetchRadarIds();
      if (ids.length > 0) {
        id = ids[0];
      }
    }

    if (!id) {
      console.error("No radar found");
      showError("No radar found. Please check connection.");
      setTimeout(() => loadRadar(null), 10000);
      return;
    }

    radarId = id;
    playbackMode = isPlaybackRadar(id);
    console.log(
      `Loading radar: ${radarId}${playbackMode ? " (playback mode)" : ""}`
    );

    const radars = await fetchRadars();
    const radarInfo = radars[radarId];

    myr_capabilities = await fetchCapabilities(radarId);
    console.log("Capabilities:", myr_capabilities);

    // Convert to user units
    myr_capabilities.controls = convertControlsToUserUnits(
      myr_capabilities.controls || {}
    );
    myr_error_message = new TemporaryMessage("myr_error");

    // Build UI
    buildControls();

    // Connect to state stream
    let controlStreamUrl = radarInfo?.streamUrl;
    if (!controlStreamUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      if (isStandaloneMode()) {
        controlStreamUrl = `${wsProtocol}//${window.location.host}/v3/api/stream`;
      } else {
        controlStreamUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radar/stream`;
      }
    }
    connectStateStream(controlStreamUrl, radarId);

    // Get spokeDataUrl
    let spokeDataUrl = radarInfo?.spokeDataUrl;
    if (!spokeDataUrl) {
      const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      spokeDataUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${radarId}/stream`;
    }

    // Notify callbacks
    radarCallbacks.forEach((cb) =>
      cb({
        id: radarId,
        name: `${myr_capabilities.make} ${myr_capabilities.model}`,
        capabilities: myr_capabilities,
        spokeDataUrl: spokeDataUrl,
      })
    );
  } catch (err) {
    console.error("Failed to load radar:", err);
    showError(`Failed to load radar: ${err.message}`);
    setTimeout(() => loadRadar(id), 10000);
  }
}

function showError(message) {
  const errorEl = document.getElementById("myr_error");
  if (errorEl) {
    errorEl.textContent = message;
    errorEl.style.visibility = "visible";
    setTimeout(() => {
      errorEl.style.visibility = "hidden";
    }, 5000);
  }
}

// ============================================================================
// Exported Helper Functions
// ============================================================================

function getControl(controlId) {
  return myr_capabilities.controls[controlId];
}

function getPowerState() {
  return myr_control_values.power?.value || 0;
}

function convertTimeToSeconds(value, units) {
  switch (units) {
    case "h":
      return value * 3600;
    case "min":
      return value * 60;
    case "s":
    default:
      return value;
  }
}

function getOperatingTime() {
  const onTimeUnits = getControl("operatingTime")?.units || "s";
  const txTimeUnits = getControl("transmitTime")?.units || "s";

  return {
    onTime: convertTimeToSeconds(
      myr_control_values.operatingTime?.value || 0,
      onTimeUnits
    ),
    txTime: convertTimeToSeconds(
      myr_control_values.transmitTime?.value || 0,
      txTimeUnits
    ),
  };
}

function isPlaybackMode() {
  return playbackMode;
}

function getUserName() {
  return myr_control_values.userName?.value || "";
}

function nextValidValue(controlId, currentValue) {
  const control = getControl(controlId);
  if (!control) return currentValue;

  // If control has explicit validValues, cycle through those
  if (control.validValues && control.validValues.length > 0) {
    const validValues = control.validValues;

    // Find the index of current value in validValues (handle type mismatch by comparing as numbers)
    const currentIndex = validValues.findIndex(
      (v) => Number(v) === Number(currentValue)
    );

    // Cycle to next value in validValues
    // If current value is not in validValues, start at first valid value
    const nextIndex =
      currentIndex < 0 ? 0 : (currentIndex + 1) % validValues.length;

    return validValues[nextIndex];
  }

  // Otherwise use minValue/maxValue/stepValue
  const min = control.minValue ?? 0;
  const max = control.maxValue ?? 1;
  const step = control.stepValue ?? 1;

  let nextValue = Number(currentValue) + step;
  if (nextValue > max) {
    nextValue = min;
  }

  return nextValue;
}

function togglePower() {
  const currentValue = myr_control_values.power?.value ?? 0;
  const nextValue = nextValidValue("power", currentValue);

  // Send the control update
  sendControlToServer("power", { value: nextValue });
}

// ============================================================================
// Range Zoom Functions
// ============================================================================

/**
 * Get current range value and valid values
 */
function getRangeInfo() {
  const controlId = "range";

  const control = myr_capabilities.controls[controlId];
  const currentValue = myr_control_values[controlId]?.value;
  const validValues = control?.validValues || [];

  return { controlId, control, currentValue, validValues };
}

/**
 * Zoom in - go to shorter range (previous in validValues, smaller value)
 */
function zoomIn() {
  const info = getRangeInfo();
  if (!info || info.validValues.length === 0) return;

  const { controlId, currentValue, validValues } = info;

  // Find current index
  const currentIndex = validValues.findIndex(
    (v) => Number(v) === Number(currentValue)
  );

  // Go to previous (shorter) range
  if (currentIndex > 0) {
    const newValue = validValues[currentIndex - 1];
    sendControlToServer(controlId, { value: newValue });
  }
}

/**
 * Zoom out - go to longer range (next in validValues, larger value)
 */
function zoomOut() {
  const info = getRangeInfo();
  if (!info || info.validValues.length === 0) return;

  const { controlId, currentValue, validValues } = info;

  // Find current index
  const currentIndex = validValues.findIndex(
    (v) => Number(v) === Number(currentValue)
  );

  // Go to next (longer) range
  if (currentIndex < validValues.length - 1) {
    const newValue = validValues[currentIndex + 1];
    sendControlToServer(controlId, { value: newValue });
  }
}

/**
 * Get current range display text
 */
function getCurrentRangeDisplay() {
  const info = getRangeInfo();
  if (!info) return "";

  const { control, currentValue } = info;
  if (control?.descriptions && control.descriptions[currentValue]) {
    return control.descriptions[currentValue];
  }
  return currentValue ? `${currentValue} m` : "";
}

// ============================================================================
// Target Acquisition Mode
// ============================================================================

/**
 * Get the current radar ID
 */
function getRadarId() {
  return radarId;
}

/**
 * Check if acquire target mode is active
 */
function isAcquireTargetMode() {
  return acquireTargetMode;
}

/**
 * Set acquire target mode
 * @param {boolean} enabled - Whether to enable acquire target mode
 */
function setAcquireTargetMode(enabled) {
  acquireTargetMode = enabled;
  console.log(`setAcquireTargetMode: ${enabled}, ${acquireTargetModeCallbacks.length} callbacks registered`);

  // Clear any existing auto-revert timer
  if (acquireTargetModeTimer) {
    clearTimeout(acquireTargetModeTimer);
    acquireTargetModeTimer = null;
  }

  // Set auto-revert timer when enabling
  if (enabled) {
    acquireTargetModeTimer = setTimeout(() => {
      setAcquireTargetMode(false);
    }, 20000);
  }

  // Update button state
  const btn = document.getElementById("myr_acquire_target_btn");
  if (btn) {
    btn.classList.toggle("myr_acquire_active", enabled);
  }

  // Update status text
  const status = document.getElementById("myr_acquire_target_status");
  if (status) {
    status.textContent = enabled ? "Click in PPI to acquire" : "";
    status.style.display = enabled ? "block" : "none";
  }

  // Notify callbacks
  acquireTargetModeCallbacks.forEach((cb) => cb(enabled));
}

/**
 * Register callback for acquire target mode changes
 * @param {function} callback - Callback function(enabled: boolean)
 */
function registerAcquireTargetModeCallback(callback) {
  acquireTargetModeCallbacks.push(callback);
}

// ============================================================================
// AIS Subscription Management
// ============================================================================

/**
 * Subscribe to AIS vessel updates
 */
function subscribeToAis() {
  if (!stateWebSocket || stateWebSocket.readyState !== WebSocket.OPEN) {
    console.warn("Cannot subscribe to AIS: WebSocket not connected");
    return;
  }

  const subscription = {
    subscribe: [
      {
        path: "vessels.*",
        policy: "instant",
      },
    ],
  };

  console.log("Subscribing to AIS vessels:", subscription);
  stateWebSocket.send(JSON.stringify(subscription));
}

/**
 * Unsubscribe from AIS vessel updates
 */
function unsubscribeFromAis() {
  if (!stateWebSocket || stateWebSocket.readyState !== WebSocket.OPEN) {
    console.warn("Cannot unsubscribe from AIS: WebSocket not connected");
    return;
  }

  const desubscription = {
    desubscribe: [
      {
        path: "vessels.*",
      },
    ],
  };

  console.log("Desubscribing from AIS vessels:", desubscription);
  stateWebSocket.send(JSON.stringify(desubscription));
}

/**
 * Acquire a target at the specified bearing and distance from radar
 * @param {number} bearing - Bearing in radians true [0, 2π)
 * @param {number} distance - Distance in meters
 * @returns {Promise<{targetId: number, radarId: string}|null>}
 */
async function acquireTargetAtPosition(bearing, distance) {
  if (!radarId) {
    console.error("No radar loaded");
    return null;
  }

  if (playbackMode) {
    console.log("Playback mode: ignoring target acquisition");
    return null;
  }

  const bearingDeg = (bearing * 180) / Math.PI;
  console.log(`Acquiring target at bearing ${bearingDeg.toFixed(1)}° (${bearing.toFixed(3)} rad), distance ${distance.toFixed(0)}m`);

  const result = await apiAcquireTarget(radarId, bearing, distance);

  // Reset the auto-revert timer after each target acquisition
  if (acquireTargetMode && acquireTargetModeTimer) {
    clearTimeout(acquireTargetModeTimer);
    acquireTargetModeTimer = setTimeout(() => {
      setAcquireTargetMode(false);
    }, 20000);
  }

  return result;
}
