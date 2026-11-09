// For a viewer who may watch the radar but not change it, such as a guest on a
// Signal K server that allows read-only access. Gain, sea and range then act
// on this screen only: they reshape the picture the radar sends and leave the
// radar, and every other screen, as they are.

/** The controls a viewer can still set for their own screen. */
const LOCAL_CONTROLS = new Set(["gain", "sea", "range"]);

/** Where gain neither lifts nor sinks the picture, and where auto sits. */
const NEUTRAL = 50;
const FULL_SCALE = 100;
/** Share of the strongest echo that full gain adds, and no gain takes away */
const GAIN_LIFT = 0.4;
const GAIN_CUT = 0.8;
/** Share of the strongest echo that full sea takes off right by the boat */
const SEA_CUT_AT_BOAT = 1.6;
/** How far out, in meters, the sea's cut falls to about a third */
const SEA_CUT_RANGE = 500;
/** Zooming in further than this shows the radar's pixels as blocks */
const MAX_ZOOM = 8;

/** An answer that says this login may not change the radar */
function refusesChanges(status) {
  return status === 401 || status === 403;
}

function gainShift(gain) {
  const off = gain - NEUTRAL;
  return off >= 0
    ? (off / (FULL_SCALE - NEUTRAL)) * GAIN_LIFT
    : (off / NEUTRAL) * GAIN_CUT;
}

/** The setting a radar control acts at: auto counts as the middle */
function level(cv) {
  const value = Number(cv?.value);
  return cv?.auto || !Number.isFinite(value) ? NEUTRAL : value;
}

/**
 * The ranges this screen can zoom to: the radar's own steps, up to the range
 * it sends and not so far in that its pixels show.
 */
function zoomSteps(validValues, radarRange) {
  return validValues
    .map(Number)
    .filter((r) => r <= radarRange && r * MAX_ZOOM >= radarRange);
}

class LocalView {
  constructor() {
    this.active = false;
    // The settings as the radar reports them, and as this screen sets them
    this.radar = new Map();
    this.local = new Map();
  }

  handles(controlId) {
    return LOCAL_CONTROLS.has(controlId);
  }

  activate() {
    this.active = true;
  }

  /**
   * A value the radar reported. Returns what this screen shows for the
   * control: the radar's value, or the one set here.
   */
  radarReported(cv) {
    if (!this.handles(cv.id)) return cv;
    this.radar.set(cv.id, { ...cv });
    const local = this.local.get(cv.id);
    if (!this.active || !local) return cv;
    if (cv.id === "range") {
      // The radar sends no picture beyond its own range
      return { ...cv, value: Math.min(local.value, cv.value) };
    }
    return { ...cv, ...local };
  }

  /** A setting made on this screen */
  set(cv) {
    // A slider hands its value over as text
    const set = "value" in cv ? { ...cv, value: Number(cv.value) } : { ...cv };
    const radarRange = this.radar.get("range")?.value;
    if (cv.id === "range" && radarRange) {
      set.value = Math.min(set.value, radarRange);
    }
    this.local.set(cv.id, set);
    return set;
  }

  /** The shorter or longer range next to `current` this screen can show */
  nextRange(validValues, current, longer) {
    const radarRange = this.radar.get("range")?.value;
    if (!radarRange) return undefined;
    const steps = zoomSteps(validValues, radarRange);
    const next = longer
      ? steps.find((r) => r > current)
      : steps.findLast((r) => r < current);
    return next;
  }

  /**
   * Reshapes a spoke's echoes in place for the gain and sea set here, as
   * changes from what the radar was set to. Sea can only clear more: what
   * the radar's own sea control took away does not come back.
   *
   * @param {Uint8Array} data - the spoke's pixels
   * @param {number} spokeRange - meters to its last pixel
   * @param {number} pixelColors - values from here up are not echoes
   *   (Doppler, trails) and stay as they are
   */
  adjust(data, spokeRange, pixelColors) {
    if (!this.active) return;
    const strongest = pixelColors - 1;
    const gain = this.#delta("gain", (cv) => gainShift(level(cv)));
    const sea = Math.max(0, this.#delta("sea", (cv) => level(cv) / FULL_SCALE));
    if (gain === 0 && sea === 0) return;

    const lift = gain * strongest;
    const seaCut = sea * SEA_CUT_AT_BOAT * strongest;
    const metersPerPixel = spokeRange / data.length;
    for (let i = 0; i < data.length; i++) {
      const v = data[i];
      if (v === 0 || v >= pixelColors) continue;
      const cut = Math.floor(seaCut * Math.exp((-i * metersPerPixel) / SEA_CUT_RANGE));
      const shown = Math.round(v + lift) - cut;
      data[i] = shown < 1 ? 0 : Math.min(shown, strongest);
    }
  }

  #delta(id, effect) {
    const local = this.local.get(id);
    // Auto here leaves the setting to the radar
    if (!local || local.auto) return 0;
    const radar = this.radar.get(id);
    return effect(local) - effect(radar);
  }
}

/** The one view of this page */
const localView = new LocalView();

export { LocalView, localView, refusesChanges, zoomSteps };
