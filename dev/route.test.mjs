// Run: node --test dev/route.test.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { routeId, routePoints, coursePlot } from "../web/gui/route.js";

const ROUTE = {
  name: "Into Suva",
  feature: {
    type: "Feature",
    geometry: {
      type: "LineString",
      coordinates: [
        [178.3, -18.2],
        [178.4, -18.15],
        [178.42, -18.13],
      ],
    },
    properties: {
      coordinatesMeta: [{ name: "Offshore" }, { name: "Reef pass" }, { name: "Anchorage" }],
    },
  },
};

const course = (entries) => new Map(Object.entries(entries));
const at = (latitude, longitude) => ({ position: { latitude, longitude } });

test("A route href names its route; anything else names none", () => {
  assert.equal(routeId("/resources/routes/ac3a3b2d-07e8-4f25-92bc-98e7c92f7f1a"), "ac3a3b2d-07e8-4f25-92bc-98e7c92f7f1a");
  assert.equal(routeId("/resources/waypoints/abc"), null);
  assert.equal(routeId("/resources/routes/harbour.entry"), "harbour.entry");
  assert.equal(routeId("/resources/routes/.."), null);
  assert.equal(routeId("/resources/routes/../../vessels"), null);
  assert.equal(routeId(null), null);
});

test("Route points come in sailing order, reversed when the route is", () => {
  const ahead = routePoints(ROUTE, false);
  assert.deepEqual(ahead[0], { latitude: -18.2, longitude: 178.3, name: "Offshore" });
  assert.equal(ahead.length, 3);

  const back = routePoints(ROUTE, true);
  assert.deepEqual(back.map((p) => p.name), ["Anchorage", "Reef pass", "Offshore"]);
  assert.equal(ROUTE.feature.geometry.coordinates[0][0], 178.3, "the route itself is left alone");

  assert.deepEqual(routePoints({}, false), []);
});

test("No next point is no course to draw", () => {
  assert.equal(coursePlot(course({}), null), null);
  assert.equal(coursePlot(course({ "navigation.course.previousPoint": at(-18, 178) }), null), null);
});

test("A destination without a route is drawn as its leg alone", () => {
  const plot = coursePlot(
    course({
      "navigation.course.nextPoint": at(-18.1, 178.4),
      "navigation.course.previousPoint": at(-18.2, 178.3),
    }),
    null
  );
  assert.deepEqual(plot, {
    next: { latitude: -18.1, longitude: 178.4, name: null },
    previous: { latitude: -18.2, longitude: 178.3 },
    route: [],
    nextIndex: null,
  });
});

test("On a route, the next point is the one the Course API is steering for", () => {
  const points = routePoints(ROUTE, false);
  const plot = coursePlot(
    course({
      "navigation.course.activeRoute": { href: "/resources/routes/x", pointIndex: 1, pointTotal: 3 },
      "navigation.course.nextPoint": at(-18.15, 178.4),
      "navigation.course.previousPoint": at(-18.2, 178.3),
    }),
    points
  );
  assert.equal(plot.nextIndex, 1);
  assert.equal(plot.next.name, "Reef pass");
  assert.equal(plot.route, points);
});

test("Route points that do not fit the point index are not drawn", () => {
  const plot = coursePlot(
    course({
      "navigation.course.activeRoute": { href: "/resources/routes/x", pointIndex: 5 },
      "navigation.course.nextPoint": at(-18.15, 178.4),
    }),
    routePoints(ROUTE, false)
  );
  assert.deepEqual(plot.route, []);
  assert.equal(plot.nextIndex, null);
  assert.equal(plot.previous, null);
});

test("A route point at the index that is not the next point is not taken for it", () => {
  const points = routePoints(ROUTE, false);
  const plot = coursePlot(
    course({
      "navigation.course.activeRoute": { href: "/resources/routes/x", pointIndex: 1 },
      "navigation.course.nextPoint": { ...at(-18.13, 178.42), name: "Anchorage" },
    }),
    points
  );
  assert.equal(plot.route, points);
  assert.equal(plot.nextIndex, null);
  assert.equal(plot.next.name, "Anchorage");
});
