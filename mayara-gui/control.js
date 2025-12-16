/**
 * Capabilities-Driven Radar Control Panel
 *
 * Dynamically builds the control UI based on radar capabilities from the v5 API.
 * No hardcoded controls - everything is generated from the capability manifest.
 *
 * UI Design: Touch-friendly with sliders and buttons only (no dropdowns).
 */

export { loadRadar, registerRadarCallback, registerControlCallback, setCurrentRange, getPowerState, getOperatingHours, hasHoursCapability };

import van from "./van-1.5.2.js";
import { fetchRadarIds, fetchCapabilities, fetchState, setControl, detectMode, isStandaloneMode, saveInstallationSetting } from "./api.js";

const { div, label, input, button, span } = van.tags;

// State
let radarId = null;
let capabilities = null;
let radarState = null;
let statePollingInterval = null;
let callbacks = [];
let controlCallbacks = [];

// Current range (for viewer.js integration)
let currentRange = 1852;
let lastRangeUpdateTime = 0;
let rangeUpdateCount = {};  // Track how often each range value is seen
let userRequestedRangeIndex = -1;  // Track user's position in range table
let rangeFromSpokeData = false;  // True once we've received range from spoke data

// Track pending control changes to prevent polling from overwriting user input
// Maps controlId -> { value, timestamp }
let pendingControls = {};

function registerRadarCallback(callback) {
  callbacks.push(callback);
}

function registerControlCallback(callback) {
  controlCallbacks.push(callback);
}

// Called from viewer.js when spoke data contains range
// Uses majority voting to prevent flickering from mixed range values during transitions
function setCurrentRange(meters) {
  if (meters <= 0) return;

  const now = Date.now();

  // Reset counts if more than 2 seconds since last update
  if (now - lastRangeUpdateTime > 2000) {
    rangeUpdateCount = {};
  }
  lastRangeUpdateTime = now;

  // Count this range value
  rangeUpdateCount[meters] = (rangeUpdateCount[meters] || 0) + 1;

  // Find the most common range value (need at least 5 samples)
  let maxCount = 0;
  let dominantRange = currentRange;
  for (const [range, count] of Object.entries(rangeUpdateCount)) {
    if (count > maxCount) {
      maxCount = count;
      dominantRange = parseInt(range);
    }
  }

  // Only update if we have a clear majority (5+ samples) and it's different
  if (maxCount >= 5 && dominantRange !== currentRange) {
    currentRange = dominantRange;
    rangeFromSpokeData = true;  // Mark that we have real range from radar
    // Also update userRequestedRangeIndex to match spoke data
    const ranges = capabilities?.characteristics?.supportedRanges || [];
    const newIndex = ranges.findIndex(r => Math.abs(r - dominantRange) < 50);
    if (newIndex >= 0) {
      userRequestedRangeIndex = newIndex;
    }
    rangeUpdateCount = {};  // Reset after accepting new range
    updateRangeDisplay();
  }
}

// ============================================================================
// UI Building from Capabilities
// ============================================================================

/**
 * Build the entire control panel from capabilities
 */
function buildControlsFromCapabilities() {
  const titleEl = document.getElementById("myr_title");
  const controlsEl = document.getElementById("myr_controls");

  if (!capabilities || !controlsEl) return;

  // Set title
  if (titleEl) {
    titleEl.innerHTML = "";
    van.add(titleEl, div(`${capabilities.make || ''} ${capabilities.model || ''} Controls`));
  }

  // Clear controls
  controlsEl.innerHTML = "";

  // Build radar info header showing model, serial, firmware, etc.
  const infoItems = [];
  if (capabilities.model) {
    infoItems.push({ label: "Model", value: capabilities.model });
  }
  if (capabilities.serialNumber) {
    infoItems.push({ label: "Serial", value: capabilities.serialNumber });
  }
  if (capabilities.firmwareVersion) {
    infoItems.push({ label: "Firmware", value: capabilities.firmwareVersion });
  }
  if (capabilities.characteristics?.maxRange) {
    const maxNm = (capabilities.characteristics.maxRange / 1852).toFixed(0);
    infoItems.push({ label: "Max Range", value: `${maxNm} nm` });
  }
  if (capabilities.characteristics?.hasDoppler) {
    infoItems.push({ label: "Doppler", value: "Yes" });
  }

  if (infoItems.length > 0) {
    const infoHeader = div({ class: "myr_radar_info_header" },
      ...infoItems.map(item =>
        div({ class: "myr_radar_info_item" },
          span({ class: "myr_info_label" }, item.label + ":"),
          span({ class: "myr_info_value" }, item.value)
        )
      )
    );
    van.add(controlsEl, infoHeader);
  }

  // Group controls by category
  const baseControls = [];
  const extendedControls = [];
  const configControls = [];
  const infoControls = [];

  for (const control of capabilities.controls || []) {
    if (control.readOnly) {
      infoControls.push(control);
    } else if (control.category === 'installation') {
      configControls.push(control);
    } else if (control.category === 'extended') {
      extendedControls.push(control);
    } else {
      baseControls.push(control);
    }
  }

  // Build base controls (power, range, gain, sea, rain)
  if (baseControls.length > 0) {
    const baseSection = div({ class: "myr_control_section" });

    // Power control first (special handling)
    const powerControl = baseControls.find(c => c.id === 'power');
    if (powerControl) {
      van.add(baseSection, buildPowerControl(powerControl));
    }

    // Range control (special handling with +/- buttons)
    const rangeControl = baseControls.find(c => c.id === 'range');
    if (rangeControl) {
      van.add(baseSection, buildRangeControl(rangeControl));
    }

    // Other base controls
    for (const control of baseControls) {
      if (control.id !== 'power' && control.id !== 'range') {
        van.add(baseSection, buildControl(control));
      }
    }

    van.add(controlsEl, baseSection);
  }

  // Build extended controls in a collapsible section
  if (extendedControls.length > 0) {
    const extSection = div({ class: "myr_control_section myr_extended_section" },
      div({ class: "myr_section_header" }, "Advanced Controls")
    );

    for (const control of extendedControls) {
      van.add(extSection, buildControl(control));
    }

    van.add(controlsEl, extSection);
  }

  // Build installation controls (config settings - rarely changed)
  if (configControls.length > 0) {
    const configSection = div({ class: "myr_control_section myr_installation_section" },
      div({ class: "myr_section_header" }, "Installation")
    );

    for (const control of configControls) {
      van.add(configSection, buildControl(control));
    }

    van.add(controlsEl, configSection);
  }

  // Build info controls (read-only)
  if (infoControls.length > 0) {
    const infoSection = div({ class: "myr_control_section myr_info_section" },
      div({ class: "myr_section_header" }, "Radar Information")
    );

    for (const control of infoControls) {
      van.add(infoSection, buildInfoControl(control));
    }

    van.add(controlsEl, infoSection);
  }

  // Apply initial state
  if (radarState) {
    applyStateToUI(radarState);
  }
}

/**
 * Build a control widget based on its type and schema
 */
function buildControl(control) {
  // Special case for dopplerMode - needs custom UI (enabled toggle + mode selector)
  if (control.id === 'dopplerMode') {
    return buildDopplerModeControl(control);
  }

  // Special case for noTransmitZones - needs custom UI (2 zone editors)
  if (control.id === 'noTransmitZones') {
    return buildNoTransmitZonesControl(control);
  }

  switch (control.type) {
    case 'boolean':
      return buildBooleanControl(control);
    case 'number':
      return buildNumberControl(control);
    case 'enum':
      return buildEnumControl(control);
    case 'compound':
      return buildCompoundControl(control);
    default:
      console.warn(`Unknown control type: ${control.type} for ${control.id}`);
      return div();
  }
}

/**
 * Power control - special Transmit/Standby buttons
 */
function buildPowerControl(control) {
  const currentValue = getControlValue('power') || 'standby';

  return div({ class: "myr_power_buttons" },
    button({
      type: "button",
      class: `myr_power_button myr_power_button_transmit ${currentValue === 'transmit' ? 'myr_power_active' : ''}`,
      onclick: () => sendControlValue('power', 'transmit'),
    }, "Transmit"),
    button({
      type: "button",
      class: `myr_power_button myr_power_button_standby ${currentValue === 'standby' ? 'myr_power_active' : ''}`,
      onclick: () => sendControlValue('power', 'standby'),
    }, "Standby")
  );
}

/**
 * Range control - +/- buttons with display
 */
function buildRangeControl(control) {
  // Get supported ranges from characteristics
  const ranges = capabilities.characteristics?.supportedRanges || [];

  return div({ class: "myr_range_buttons" },
    button({
      type: "button",
      class: "myr_range_button",
      onclick: () => changeRange(-1),
    }, "Range -"),
    button({
      type: "button",
      class: "myr_range_button",
      onclick: () => changeRange(1),
    }, "Range +")
  );
}

/**
 * Boolean control - toggle button
 */
function buildBooleanControl(control) {
  const currentValue = getControlValue(control.id) || control.default || false;

  return div({ class: "myr_control myr_boolean_control" },
    span({ class: "myr_control_label" }, control.name),
    button({
      type: "button",
      id: `myr_${control.id}`,
      class: `myr_toggle_button ${currentValue ? 'myr_toggle_active' : ''}`,
      onclick: (e) => {
        const isActive = e.target.classList.contains('myr_toggle_active');
        sendControlValue(control.id, !isActive);
      },
    }, currentValue ? "On" : "Off")
  );
}

/**
 * Number control - slider
 */
function buildNumberControl(control) {
  const range = control.range || { min: 0, max: 100 };
  let currentValue = getControlValue(control.id);

  // Handle compound values (objects with mode/value)
  if (typeof currentValue === 'object' && currentValue !== null) {
    currentValue = currentValue.value;
  }

  const value = currentValue !== undefined ? currentValue : (control.default || range.min);

  return div({ class: "myr_control myr_number_control" },
    div({ class: "myr_control_header" },
      span({ class: "myr_control_label" }, control.name),
      span({ id: `myr_${control.id}_value`, class: "myr_control_value" },
        formatNumberValue(value, control))
    ),
    input({
      type: "range",
      id: `myr_${control.id}`,
      class: "myr_slider",
      min: range.min,
      max: range.max,
      step: range.step || 1,
      value: value,
      oninput: (e) => {
        // Update display while dragging
        const valEl = document.getElementById(`myr_${control.id}_value`);
        if (valEl) {
          valEl.textContent = formatNumberValue(parseInt(e.target.value), control);
        }
      },
      onchange: (e) => {
        sendControlValue(control.id, parseInt(e.target.value));
      },
    })
  );
}

/**
 * Enum control - row of buttons (no dropdown per user request)
 */
function buildEnumControl(control) {
  const values = control.values || [];
  const currentValue = getControlValue(control.id) ?? control.default;

  return div({ class: "myr_control myr_enum_control" },
    span({ class: "myr_control_label" }, control.name),
    div({ class: "myr_button_group", id: `myr_${control.id}_group` },
      ...values.map(v => {
        // Compare as strings to handle number/string type differences
        const isActive = String(v.value) === String(currentValue);
        return button({
          type: "button",
          class: `myr_enum_button ${isActive ? 'myr_enum_active' : ''}`,
          "data-value": v.value,
          onclick: () => sendControlValue(control.id, v.value),
        }, v.label || v.value);
      })
    )
  );
}

/**
 * Compound control - mode selector + value slider (e.g., gain with auto/manual)
 */
function buildCompoundControl(control) {
  const modes = control.modes || ['auto', 'manual'];
  const currentState = getControlValue(control.id) || {};
  const currentMode = currentState.mode || control.defaultMode || modes[0];
  const currentValue = currentState.value !== undefined ? currentState.value : 50;

  // Get value range from properties
  const valueProps = control.properties?.value || {};
  const range = valueProps.range || { min: 0, max: 100 };

  const isAuto = currentMode === 'auto';

  return div({ class: "myr_control myr_compound_control", id: `myr_${control.id}_compound` },
    div({ class: "myr_compound_header" },
      span({ class: "myr_control_label" }, control.name),
      span({ id: `myr_${control.id}_value`, class: "myr_control_value" },
        isAuto ? "Auto" : currentValue)
    ),
    div({ class: "myr_compound_body" },
      // Mode buttons
      div({ class: "myr_mode_buttons" },
        ...modes.map(mode =>
          button({
            type: "button",
            class: `myr_mode_button ${mode === currentMode ? 'myr_mode_active' : ''}`,
            "data-mode": mode,
            onclick: () => {
              const slider = document.getElementById(`myr_${control.id}_slider`);
              const value = slider ? parseInt(slider.value) : currentValue;
              sendControlValue(control.id, { mode, value });
            },
          }, mode.charAt(0).toUpperCase() + mode.slice(1))
        )
      ),
      // Value slider (disabled when auto)
      input({
        type: "range",
        id: `myr_${control.id}_slider`,
        class: "myr_slider myr_compound_slider",
        min: range.min,
        max: range.max,
        step: range.step || 1,
        value: currentValue,
        disabled: isAuto,
        oninput: (e) => {
          const valEl = document.getElementById(`myr_${control.id}_value`);
          // Check current mode dynamically, not the captured isAuto
          const modeEl = document.querySelector(`#myr_${control.id}_compound .myr_mode_active`);
          const currentMode = modeEl?.dataset.mode || 'auto';
          if (valEl && currentMode !== 'auto') {
            valEl.textContent = e.target.value;
          }
        },
        onchange: (e) => {
          // Check current mode dynamically
          const modeEl = document.querySelector(`#myr_${control.id}_compound .myr_mode_active`);
          const mode = modeEl?.dataset.mode || 'manual';
          if (mode !== 'auto') {
            sendControlValue(control.id, { mode, value: parseInt(e.target.value) });
          }
        },
      })
    )
  );
}

/**
 * Doppler Mode control - 3 buttons: Off | Target | Rain
 * Furuno Target Analyzer: { enabled: bool, mode: "target" | "rain" }
 */
function buildDopplerModeControl(control) {
  const currentState = getControlValue(control.id) || { enabled: false, mode: 'target' };
  const enabled = currentState.enabled || false;
  const mode = currentState.mode || 'target';

  // Determine which button is active: off, target, or rain
  const activeBtn = !enabled ? 'off' : mode;

  return div({ class: "myr_control", id: `myr_${control.id}_compound` },
    span({ class: "myr_control_label" }, control.name),
    div({ class: "myr_mode_buttons myr_mode_buttons_3" },
      button({
        type: "button",
        class: `myr_mode_button ${activeBtn === 'off' ? 'myr_mode_active' : ''}`,
        "data-value": "off",
        onclick: () => sendControlValue(control.id, { enabled: false, mode: 'target' }),
      }, "Off"),
      button({
        type: "button",
        class: `myr_mode_button ${activeBtn === 'target' ? 'myr_mode_active' : ''}`,
        "data-value": "target",
        onclick: () => sendControlValue(control.id, { enabled: true, mode: 'target' }),
      }, "Target"),
      button({
        type: "button",
        class: `myr_mode_button ${activeBtn === 'rain' ? 'myr_mode_active' : ''}`,
        "data-value": "rain",
        onclick: () => sendControlValue(control.id, { enabled: true, mode: 'rain' }),
      }, "Rain")
    )
  );
}

/**
 * No-Transmit Zones control - 2 zone editors with enabled/start/end
 * Furuno Sector Blanking: { zones: [{ enabled, start, end }, { enabled, start, end }] }
 */
function buildNoTransmitZonesControl(control) {
  const currentState = getControlValue(control.id) || { zones: [] };
  const zones = currentState.zones || [];

  // Default zones (2 for Furuno)
  const zone1 = zones[0] || { enabled: false, start: 0, end: 0 };
  const zone2 = zones[1] || { enabled: false, start: 0, end: 0 };

  // Read current zone values from DOM (to avoid stale closure values)
  function getZoneFromDOM(zoneNum) {
    const prefix = `myr_ntz_zone${zoneNum}`;
    const enabledEl = document.getElementById(`${prefix}_enabled`);
    const startEl = document.getElementById(`${prefix}_start`);
    const endEl = document.getElementById(`${prefix}_end`);
    return {
      enabled: enabledEl?.checked || false,
      start: parseInt(startEl?.value) || 0,
      end: parseInt(endEl?.value) || 0
    };
  }

  function sendCurrentZones() {
    const z1 = getZoneFromDOM(1);
    const z2 = getZoneFromDOM(2);
    console.log('NTZ: Sending zones:', { z1, z2 });
    sendControlValue(control.id, { zones: [z1, z2] });
  }

  function buildZoneEditor(zoneNum, zone) {
    const prefix = `myr_ntz_zone${zoneNum}`;
    return div({ class: "myr_ntz_zone" },
      div({ class: "myr_ntz_zone_header" },
        label({ class: "myr_checkbox_label" },
          input({
            type: "checkbox",
            id: `${prefix}_enabled`,
            checked: zone.enabled,
            onchange: () => sendCurrentZones()
          }),
          ` Zone ${zoneNum}`
        )
      ),
      div({ class: "myr_ntz_angles" },
        div({ class: "myr_ntz_angle" },
          label({ for: `${prefix}_start` }, "Start°"),
          input({
            type: "number",
            id: `${prefix}_start`,
            min: 0,
            max: 359,
            value: zone.start,
            disabled: !zone.enabled,
            onchange: () => sendCurrentZones()
          })
        ),
        div({ class: "myr_ntz_angle" },
          label({ for: `${prefix}_end` }, "End°"),
          input({
            type: "number",
            id: `${prefix}_end`,
            min: 0,
            max: 359,
            value: zone.end,
            disabled: !zone.enabled,
            onchange: () => sendCurrentZones()
          })
        )
      )
    );
  }

  return div({ class: "myr_control myr_ntz_control", id: `myr_${control.id}_compound` },
    span({ class: "myr_control_label" }, control.name),
    div({ class: "myr_ntz_zones" },
      buildZoneEditor(1, zone1),
      buildZoneEditor(2, zone2)
    )
  );
}

/**
 * Read-only info control
 */
function buildInfoControl(control) {
  const value = getControlValue(control.id) || '-';

  return div({ class: "myr_control myr_info_control" },
    span({ class: "myr_control_label" }, control.name),
    span({ id: `myr_${control.id}`, class: "myr_info_value" }, formatInfoValue(value, control))
  );
}

// ============================================================================
// Control Value Helpers
// ============================================================================

function getControlValue(controlId) {
  return radarState?.controls?.[controlId];
}

function formatNumberValue(value, control) {
  // Handle compound values (objects with mode/value)
  let numValue = value;
  if (typeof value === 'object' && value !== null) {
    if (value.mode === 'auto') {
      return 'Auto';
    }
    numValue = value.value !== undefined ? value.value : 0;
  }

  const unit = control?.range?.unit || '';
  if (unit === 'percent') {
    return `${numValue}%`;
  }
  return unit ? `${numValue} ${unit}` : String(numValue);
}

function formatInfoValue(value, control) {
  if (control.id === 'operatingHours' && typeof value === 'number') {
    return `${value.toFixed(1)} hrs`;
  }
  return String(value);
}

function formatRange(meters) {
  const nm = meters / 1852;
  if (nm >= 1) {
    if (nm === 1.5) return "1.5 nm";
    return Math.round(nm) + " nm";
  } else if (nm >= 0.7) {
    return "3/4 nm";
  } else if (nm >= 0.4) {
    return "1/2 nm";
  } else if (nm >= 0.2) {
    return "1/4 nm";
  } else if (nm >= 0.1) {
    return "1/8 nm";
  } else {
    return "1/16 nm";
  }
}

function updateRangeDisplay() {
  const display = document.getElementById("myr_range_display");
  if (display) {
    display.textContent = formatRange(currentRange);
  }
}

// ============================================================================
// Control Commands
// ============================================================================

async function sendControlValue(controlId, value) {
  if (!radarId) return;

  console.log(`Sending control: ${controlId} = ${JSON.stringify(value)}`);

  // Mark as pending to prevent polling from overwriting
  pendingControls[controlId] = { value, timestamp: Date.now() };

  // Optimistic UI update immediately
  updateControlUI(controlId, value);

  const success = await setControl(radarId, controlId, value);

  if (success) {
    // Notify callbacks
    const control = capabilities?.controls?.find(c => c.id === controlId);
    controlCallbacks.forEach(cb => cb(control, { id: controlId, value }));

    // Persist Installation category controls (write-only settings like bearingAlignment)
    // Use capabilities.key (e.g., "Furuno-RD003212") for storage - compatible with WASM SignalK plugin
    if (control?.category === 'installation') {
      const storageKey = capabilities?.key || radarId;
      saveInstallationSetting(storageKey, controlId, value);
    }
  }
}

function changeRange(direction) {
  const ranges = capabilities?.characteristics?.supportedRanges || [];
  if (ranges.length === 0) return;

  // Use tracked index if valid, otherwise find from current range
  if (userRequestedRangeIndex < 0 || userRequestedRangeIndex >= ranges.length) {
    userRequestedRangeIndex = ranges.findIndex(r => Math.abs(r - currentRange) < 50);
    if (userRequestedRangeIndex < 0) userRequestedRangeIndex = 0;
  }

  const newIndex = Math.max(0, Math.min(ranges.length - 1, userRequestedRangeIndex + direction));
  const newRange = ranges[newIndex];

  // Always update index to track user's position
  userRequestedRangeIndex = newIndex;

  sendControlValue('range', newRange);
}

// ============================================================================
// UI Updates from State
// ============================================================================

function updateControlUI(controlId, value) {
  // Update local state
  if (radarState?.controls) {
    radarState.controls[controlId] = value;
  }

  // Update UI based on control type
  const control = capabilities?.controls?.find(c => c.id === controlId);
  if (!control) return;

  // Special case for dopplerMode
  if (controlId === 'dopplerMode') {
    updateDopplerModeUI(controlId, value);
    return;
  }

  // Special case for noTransmitZones
  if (controlId === 'noTransmitZones') {
    updateNoTransmitZonesUI(value);
    return;
  }

  switch (control.type) {
    case 'boolean':
      updateBooleanUI(controlId, value);
      break;
    case 'number':
      updateNumberUI(controlId, value, control);
      break;
    case 'enum':
      updateEnumUI(controlId, value);
      break;
    case 'compound':
      updateCompoundUI(controlId, value, control);
      break;
  }

  // Special handling for power
  if (controlId === 'power') {
    updatePowerUI(value);
  }
}

function updatePowerUI(value) {
  const transmitBtn = document.querySelector('.myr_power_button_transmit');
  const standbyBtn = document.querySelector('.myr_power_button_standby');

  if (transmitBtn) {
    transmitBtn.classList.toggle('myr_power_active', value === 'transmit');
  }
  if (standbyBtn) {
    standbyBtn.classList.toggle('myr_power_active', value === 'standby');
  }
}

function updateBooleanUI(controlId, value) {
  const btn = document.getElementById(`myr_${controlId}`);
  if (btn) {
    btn.classList.toggle('myr_toggle_active', value);
    btn.textContent = value ? "On" : "Off";
  }
}

function updateNumberUI(controlId, value, control) {
  const slider = document.getElementById(`myr_${controlId}`);
  const valueEl = document.getElementById(`myr_${controlId}_value`);

  if (slider) {
    slider.value = value;
  }
  if (valueEl) {
    valueEl.textContent = formatNumberValue(value, control);
  }
}

function updateEnumUI(controlId, value) {
  const group = document.getElementById(`myr_${controlId}_group`);
  if (group) {
    // Convert value to string for comparison (dataset values are always strings)
    const valueStr = String(value);
    group.querySelectorAll('.myr_enum_button').forEach(btn => {
      btn.classList.toggle('myr_enum_active', btn.dataset.value === valueStr);
    });
  }
}

function updateCompoundUI(controlId, value, control) {
  const compound = document.getElementById(`myr_${controlId}_compound`);
  if (!compound) return;

  const mode = value?.mode || 'auto';
  const val = value?.value;

  // Update mode buttons
  compound.querySelectorAll('.myr_mode_button').forEach(btn => {
    btn.classList.toggle('myr_mode_active', btn.dataset.mode === mode);
  });

  // Update slider
  const slider = compound.querySelector('.myr_compound_slider');
  const valueEl = document.getElementById(`myr_${controlId}_value`);

  const isAuto = mode === 'auto';
  if (slider) {
    slider.disabled = isAuto;
    if (val !== undefined) {
      slider.value = val;
    }
  }
  if (valueEl) {
    valueEl.textContent = isAuto ? "Auto" : (val !== undefined ? val : '-');
  }
}

function updateDopplerModeUI(controlId, value) {
  const compound = document.getElementById(`myr_${controlId}_compound`);
  if (!compound) return;

  const enabled = value?.enabled || false;
  const mode = value?.mode || 'target';
  const activeBtn = !enabled ? 'off' : mode;

  // Update buttons (Off / Target / Rain)
  compound.querySelectorAll('.myr_mode_button').forEach(btn => {
    btn.classList.toggle('myr_mode_active', btn.dataset.value === activeBtn);
  });
}

function updateNoTransmitZonesUI(value) {
  const zones = value?.zones || [];
  const zone1 = zones[0] || { enabled: false, start: 0, end: 0 };
  const zone2 = zones[1] || { enabled: false, start: 0, end: 0 };

  // Update zone 1
  const z1Enabled = document.getElementById('myr_ntz_zone1_enabled');
  const z1Start = document.getElementById('myr_ntz_zone1_start');
  const z1End = document.getElementById('myr_ntz_zone1_end');
  if (z1Enabled) z1Enabled.checked = zone1.enabled;
  if (z1Start) {
    z1Start.value = zone1.start;
    z1Start.disabled = !zone1.enabled;
  }
  if (z1End) {
    z1End.value = zone1.end;
    z1End.disabled = !zone1.enabled;
  }

  // Update zone 2
  const z2Enabled = document.getElementById('myr_ntz_zone2_enabled');
  const z2Start = document.getElementById('myr_ntz_zone2_start');
  const z2End = document.getElementById('myr_ntz_zone2_end');
  if (z2Enabled) z2Enabled.checked = zone2.enabled;
  if (z2Start) {
    z2Start.value = zone2.start;
    z2Start.disabled = !zone2.enabled;
  }
  if (z2End) {
    z2End.value = zone2.end;
    z2End.disabled = !zone2.enabled;
  }
}

function applyStateToUI(state) {
  if (!state?.controls) return;

  for (const [controlId, value] of Object.entries(state.controls)) {
    // Skip controls with pending changes until server confirms the same value
    const pending = pendingControls[controlId];
    if (pending) {
      // Check if server has confirmed our pending value
      const serverValue = JSON.stringify(value);
      const pendingValue = JSON.stringify(pending.value);
      if (serverValue === pendingValue) {
        // Server confirmed, clear pending
        delete pendingControls[controlId];
      } else {
        // Server hasn't confirmed yet, keep user's value
        continue;
      }
    }
    updateControlUI(controlId, value);
  }

  // Update range display and initialize range index
  // Skip if we already have range from spoke data (more accurate than state API)
  if (state.controls.range && !rangeFromSpokeData) {
    currentRange = state.controls.range;
    // Initialize userRequestedRangeIndex from actual radar range
    const ranges = capabilities?.characteristics?.supportedRanges || [];
    userRequestedRangeIndex = ranges.findIndex(r => Math.abs(r - currentRange) < 50);
    if (userRequestedRangeIndex < 0) userRequestedRangeIndex = 0;
    updateRangeDisplay();
  }
}

// ============================================================================
// State Polling
// ============================================================================

let pollFailCount = 0;
const MAX_POLL_INTERVAL = 30000; // Max 30s between polls on repeated failures
const BASE_POLL_INTERVAL = 2000; // Normal 2s polling

async function pollState() {
  if (!radarId) return;

  try {
    const newState = await fetchState(radarId);
    if (newState) {
      radarState = newState;
      applyStateToUI(radarState);

      // Reset fail count on success and restore normal polling
      if (pollFailCount > 0) {
        pollFailCount = 0;
        startStatePolling(); // Restart with normal interval
      }
    }
  } catch (err) {
    pollFailCount++;
    if (pollFailCount <= 3) {
      console.warn(`State poll failed (${pollFailCount}):`, err.message);
    } else if (pollFailCount === 4) {
      console.warn("State polling failing, backing off...");
    }
    // Exponential backoff: restart polling with longer interval
    if (pollFailCount > 2) {
      startStatePolling();
    }
  }
}

function startStatePolling() {
  if (statePollingInterval) {
    clearInterval(statePollingInterval);
  }

  // Exponential backoff on failures
  const interval = Math.min(BASE_POLL_INTERVAL * Math.pow(2, pollFailCount), MAX_POLL_INTERVAL);
  statePollingInterval = setInterval(pollState, interval);
}

function stopStatePolling() {
  if (statePollingInterval) {
    clearInterval(statePollingInterval);
    statePollingInterval = null;
  }
}

// ============================================================================
// Initialization (for standalone control.html only)
// ============================================================================

// For control.html: auto-initialize on load
// For viewer.html: viewer.js imports this module and calls loadRadar() itself
// We detect standalone mode by checking if viewer.js has NOT registered a callback
// (viewer.js calls registerRadarCallback before window.onload)
setTimeout(() => {
  // If no callbacks registered after module evaluation, we're in standalone mode
  if (callbacks.length === 0) {
    window.onload = function() {
      const urlParams = new URLSearchParams(window.location.search);
      const id = urlParams.get("id");
      loadRadar(id);
    };
  }
}, 0);

async function loadRadar(id) {
  try {
    await detectMode();

    // If no ID provided, get first radar
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
    console.log(`Loading radar: ${radarId}`);

    // Fetch capabilities
    capabilities = await fetchCapabilities(radarId);
    console.log("Capabilities:", capabilities);

    // Fetch initial state
    radarState = await fetchState(radarId);
    console.log("Initial state:", radarState);

    // Build UI
    buildControlsFromCapabilities();

    // Start polling for state updates
    startStatePolling();

    // Notify callbacks (viewer.js expects these properties)
    const chars = capabilities.characteristics || {};

    // Build streamUrl based on mode
    const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    let streamUrl;
    if (isStandaloneMode()) {
      // Standalone mode: use /v1/api/radars/{id}/spokes
      streamUrl = `${wsProtocol}//${window.location.host}/v1/api/radars/${radarId}/spokes`;
    } else {
      // SignalK mode: use /signalk/v2/api/vessels/self/radars/{id}/stream
      streamUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${radarId}/stream`;
    }

    callbacks.forEach(cb => cb({
      id: radarId,
      name: `${capabilities.make} ${capabilities.model}`,
      maxSpokeLen: chars.maxSpokeLength || 512,
      spokesPerRevolution: chars.spokesPerRevolution || 2048,
      controls: capabilities.controls || [],
      capabilities,
      state: radarState,
      streamUrl,
    }));

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

/**
 * Get current power state
 * @returns {string} 'standby' | 'transmit' | 'off' | 'warming'
 */
function getPowerState() {
  return radarState?.controls?.power || 'standby';
}

/**
 * Get operating hours from radar state
 * @returns {{ onTime: number, txTime: number }} Operating hours
 */
function getOperatingHours() {
  const controls = radarState?.controls || {};
  return {
    onTime: controls.operatingHours || 0,
    txTime: controls.transmitHours || 0
  };
}

/**
 * Check if radar has hours capability (operatingHours or transmitHours)
 * @returns {{ hasOnTime: boolean, hasTxTime: boolean }}
 */
function hasHoursCapability() {
  const controls = capabilities?.controls || [];
  return {
    hasOnTime: controls.some(c => c.id === 'operatingHours'),
    hasTxTime: controls.some(c => c.id === 'transmitHours')
  };
}
