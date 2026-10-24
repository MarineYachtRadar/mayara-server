// Pure target maths and ranking shared by the PPI overlay and the target
// data boxes. No DOM access, so node tests can import it.

export {
  DANGER_CPA_M,
  DANGER_TCPA_S,
  isDangerous,
  bearingDistance,
  cpaTcpa,
  motion,
  rankInteresting,
};

const NAUTICAL_MILE = 1852.0;
const EARTH_RADIUS_M = 6371000;

// A target is a collision danger when it will pass within half a nautical
// mile in the next ten minutes. The PPI paints such targets red and the data
// boxes list them first, so both must use the same rule.
const DANGER_CPA_M = NAUTICAL_MILE / 2;
const DANGER_TCPA_S = 10 * 60;

// Below this relative speed (m/s) the vessels keep their distance and a CPA
// is meaningless: the "closest point" is anywhere along an endless drift.
const MIN_RELATIVE_SPEED = 0.01;

/**
 * @param {number|null|undefined} cpa - metres
 * @param {number|null|undefined} tcpa - seconds; negative once passed
 */
function isDangerous(cpa, tcpa) {
  return cpa != null && tcpa != null && cpa < DANGER_CPA_M && tcpa > 0 && tcpa < DANGER_TCPA_S;
}

/**
 * Great-circle bearing (radians true, clockwise from north) and distance
 * (metres) from one lat/lon (degrees) to another.
 */
function bearingDistance(fromLat, fromLon, toLat, toLon) {
  const lat1 = (fromLat * Math.PI) / 180;
  const lat2 = (toLat * Math.PI) / 180;
  const dLat = ((toLat - fromLat) * Math.PI) / 180;
  const dLon = ((toLon - fromLon) * Math.PI) / 180;

  const a =
    Math.sin(dLat / 2) * Math.sin(dLat / 2) +
    Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLon / 2) * Math.sin(dLon / 2);
  const distance = EARTH_RADIUS_M * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));

  const y = Math.sin(dLon) * Math.cos(lat2);
  const x = Math.cos(lat1) * Math.sin(lat2) - Math.sin(lat1) * Math.cos(lat2) * Math.cos(dLon);
  return { bearing: Math.atan2(y, x), distance };
}

/**
 * CPA (metres) and TCPA (seconds, negative once passed) of a target at
 * `bearing`/`distance` from own ship, both vessels moving at constant
 * course (radians true) and speed (m/s). Null when there is no relative
 * motion.
 */
function cpaTcpa(bearing, distance, target, own) {
  const px = distance * Math.sin(bearing);
  const py = distance * Math.cos(bearing);
  const vx = target.speed * Math.sin(target.course) - own.speed * Math.sin(own.course);
  const vy = target.speed * Math.cos(target.course) - own.speed * Math.cos(own.course);
  const v2 = vx * vx + vy * vy;
  if (v2 < MIN_RELATIVE_SPEED * MIN_RELATIVE_SPEED) return null;

  const tcpa = -(px * vx + py * vy) / v2;
  const cpa = Math.hypot(px + vx * tcpa, py + vy * tcpa);
  return { cpa, tcpa };
}

/**
 * A vessel's course (radians true) and speed (m/s) as `cpaTcpa` takes them,
 * or null while either is unknown. A vessel that is not moving needs no
 * course: a boat at anchor often reports none, and its CPA to a ship coming
 * at it still matters.
 */
function motion(course, speed) {
  if (speed == null) return null;
  if (speed < MIN_RELATIVE_SPEED) return { course: 0, speed: 0 };
  return course == null ? null : { course, speed };
}

// A target whose track is still forming or has been lost
function unsettled(target) {
  return target.status != null && target.status !== "tracking";
}

/**
 * The `max` most interesting targets: collision dangers first, then tracked
 * targets before ones still acquiring or lost, each group nearest first. A target beyond `maxRange` only makes
 * the list when it is a danger — a fast ship closing from outside the
 * displayed range still deserves a box.
 *
 * @param {Array<{range:number, cpa?:number|null, tcpa?:number|null}>} targets
 */
function rankInteresting(targets, max, maxRange = Infinity) {
  return targets
    .map((t) => ({ t, danger: isDangerous(t.cpa, t.tcpa) }))
    .filter(({ t, danger }) => danger || t.range <= maxRange)
    .sort(
      (a, b) =>
        b.danger - a.danger || unsettled(a.t) - unsettled(b.t) || a.t.range - b.t.range
    )
    .slice(0, max)
    .map(({ t, danger }) => ({ ...t, danger }));
}
