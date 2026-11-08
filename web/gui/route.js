// The own ship's active course from the Signal K Course API, turned into the
// points the PPI draws. No DOM access, so node tests can import it.

export { COURSE_PATHS, routeId, routePoints, coursePlot };

const COURSE_PATHS = [
  "navigation.course.activeRoute",
  "navigation.course.nextPoint",
  "navigation.course.previousPoint",
];

/**
 * The route id in an `activeRoute.href` (`/resources/routes/<id>`), or null.
 * @param {string|null|undefined} href
 */
function routeId(href) {
  const match = /^\/resources\/routes\/([A-Za-z0-9._-]+)$/.exec(href ?? "");
  return match && !/^\.+$/.test(match[1]) ? match[1] : null;
}

/**
 * The waypoints of a Signal K route resource, in the order the boat sails
 * them: a route followed in reverse is sailed from its last point, and the
 * Course API's `pointIndex` counts in that order.
 * @param {object} route - Resources API route (`feature.geometry` a LineString)
 * @param {boolean} reverse - `activeRoute.reverse`
 * @returns {{latitude: number, longitude: number, name: string|null}[]}
 */
function routePoints(route, reverse) {
  const coordinates = route?.feature?.geometry?.coordinates;
  if (!Array.isArray(coordinates)) return [];
  const meta = route.feature.properties?.coordinatesMeta;
  const points = coordinates
    .map((c, i) => ({
      longitude: c?.[0],
      latitude: c?.[1],
      name: (Array.isArray(meta) && meta[i]?.name) || null,
    }))
    .filter((p) => Number.isFinite(p.latitude) && Number.isFinite(p.longitude));
  return reverse ? points.reverse() : points;
}

function position(point) {
  const p = point?.position;
  return p && Number.isFinite(p.latitude) && Number.isFinite(p.longitude)
    ? { latitude: p.latitude, longitude: p.longitude }
    : null;
}

/**
 * What to draw for the current course.
 *
 * `next` is the point the boat steers for, and `leg` the line it runs on,
 * from `previous` to `next`; both come from the Course API itself, so they are
 * right even while the route's own points are not known. `route` holds the
 * whole route when it is, and `nextIndex` says which of its points is `next`.
 *
 * @param {Map<string, any>} course - Course API values by path
 * @param {{latitude: number, longitude: number, name: string|null}[]|null} points
 *   the active route's points in sailing order, from `routePoints`
 * @returns {null | {
 *   next: {latitude: number, longitude: number, name: string|null},
 *   previous: {latitude: number, longitude: number}|null,
 *   route: {latitude: number, longitude: number, name: string|null}[],
 *   nextIndex: number|null,
 * }}
 */
function coursePlot(course, points) {
  const nextPoint = course.get("navigation.course.nextPoint");
  const next = position(nextPoint);
  if (!next) return null;

  const activeRoute = course.get("navigation.course.activeRoute");
  const index = activeRoute?.pointIndex;
  const onRoute =
    activeRoute != null &&
    Array.isArray(points) &&
    Number.isInteger(index) &&
    index >= 0 &&
    index < points.length;

  return {
    next: { ...next, name: (onRoute && points[index].name) || nextPoint?.name || null },
    previous: position(course.get("navigation.course.previousPoint")),
    route: onRoute ? points : [],
    nextIndex: onRoute ? index : null,
  };
}
