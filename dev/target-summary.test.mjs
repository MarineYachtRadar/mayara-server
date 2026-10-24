// Run: node --test dev/target-summary.test.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DANGER_CPA_M,
  DANGER_TCPA_S,
  isDangerous,
  bearingDistance,
  cpaTcpa,
  motion,
  rankInteresting,
} from "../web/gui/target-summary.js";

const KNOT = 1852 / 3600;
const EAST = Math.PI / 2;
const WEST = (3 * Math.PI) / 2;
const NORTH = 0;

test("A close pass in the near future is a danger; a passed or distant one is not", () => {
  assert.equal(isDangerous(DANGER_CPA_M - 1, DANGER_TCPA_S - 1), true);
  assert.equal(isDangerous(DANGER_CPA_M - 1, -60), false);
  assert.equal(isDangerous(DANGER_CPA_M - 1, DANGER_TCPA_S + 1), false);
  assert.equal(isDangerous(DANGER_CPA_M + 1, 60), false);
  assert.equal(isDangerous(null, 60), false);
});

test("Dangers come first, then the rest nearest first, capped", () => {
  const targets = [
    { label: "far", range: 3000 },
    { label: "near", range: 500 },
    { label: "danger far", range: 2500, cpa: 100, tcpa: 120 },
    { label: "danger near", range: 1500, cpa: 200, tcpa: 300 },
    { label: "middle", range: 1000, cpa: 2000, tcpa: 300 },
  ];
  const ranked = rankInteresting(targets, 4).map((t) => t.label);
  assert.deepEqual(ranked, ["danger near", "danger far", "near", "middle"]);
});

test("A tracked target outranks a nearer one still acquiring or lost", () => {
  const targets = [
    { label: "acquiring", status: "acquiring", range: 300 },
    { label: "lost", status: "lost", range: 400 },
    { label: "tracked", status: "tracking", range: 2000 },
  ];
  const ranked = rankInteresting(targets, 2).map((t) => t.label);
  assert.deepEqual(ranked, ["tracked", "acquiring"]);
});

test("A stopped boat needs no course for a CPA; a moving one does", () => {
  assert.deepEqual(motion(null, 0), { course: 0, speed: 0 });
  assert.equal(motion(null, 3), null);
  assert.equal(motion(EAST, null), null);
  assert.deepEqual(motion(EAST, 3), { course: EAST, speed: 3 });

  // At anchor with no course, a ship 1 km east heading west passes through us
  const result = cpaTcpa(EAST, 1000, motion(WEST, 10 * KNOT), motion(null, 0));
  assert.ok(result.cpa < 1e-6);
});

test("Beyond the displayed range only a danger earns a box", () => {
  const targets = [
    { label: "outside", range: 5000 },
    { label: "closing fast", range: 6000, cpa: 50, tcpa: 400 },
    { label: "inside", range: 800 },
  ];
  const ranked = rankInteresting(targets, 5, 1852);
  assert.deepEqual(ranked.map((t) => t.label), ["closing fast", "inside"]);
  assert.deepEqual(ranked.map((t) => t.danger), [true, false]);
});

test("One minute of longitude east on the equator is a nautical mile due east", () => {
  const { bearing, distance } = bearingDistance(0, 0, 0, 1 / 60);
  assert.ok(Math.abs(bearing - EAST) < 1e-9);
  assert.ok(Math.abs(distance - 1853) < 2);
});

test("A ship crossing ahead from starboard has its CPA where it crosses our track", () => {
  // Target 1000 m east, 1000 m north, heading west at 10 kn; we are stopped.
  const bearing = Math.atan2(1000, 1000);
  const distance = Math.hypot(1000, 1000);
  const result = cpaTcpa(bearing, distance, { course: WEST, speed: 10 * KNOT }, { course: NORTH, speed: 0 });
  assert.ok(Math.abs(result.cpa - 1000) < 1e-6);
  assert.ok(Math.abs(result.tcpa - 1000 / (10 * KNOT)) < 1e-6);
});

test("A ship keeping station with us has no CPA", () => {
  const motion = { course: NORTH, speed: 5 * KNOT };
  assert.equal(cpaTcpa(EAST, 1000, motion, motion), null);
});
