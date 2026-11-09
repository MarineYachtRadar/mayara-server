// Run: node --test dev/local-view.test.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { LocalView, refusesChanges, zoomSteps } from "../web/gui/local-view.js";

const PIXEL_COLORS = 16;
const DOPPLER = 16;
const NM = 1852;
const RANGES = [463, 926, 1389, 1852, 2778, 3704, 5556, 7408, 11112];

function viewing({ gain, sea, range = 3 * NM } = {}) {
  const view = new LocalView();
  view.radarReported({ id: "gain", value: 50, auto: true });
  view.radarReported({ id: "sea", value: 45, auto: true });
  view.radarReported({ id: "range", value: range });
  view.activate();
  if (gain !== undefined) view.set({ id: "gain", value: gain, auto: false });
  if (sea !== undefined) view.set({ id: "sea", value: sea, auto: false });
  return view;
}

/** One spoke 3 nm long whose pixels all hold `value` */
function spoke(value, length = 512) {
  return new Uint8Array(length).fill(value);
}

test("Only a refusal of the login switches to the local view", () => {
  assert.equal(refusesChanges(401), true);
  assert.equal(refusesChanges(403), true);
  assert.equal(refusesChanges(0), false, "mayara unreachable");
  assert.equal(refusesChanges(500), false);
});

test("Until the radar refuses a change, the picture is the radar's", () => {
  const view = new LocalView();
  view.radarReported({ id: "gain", value: 50, auto: true });
  view.set({ id: "gain", value: 0, auto: false });
  const data = spoke(9);
  view.adjust(data, 3 * NM, PIXEL_COLORS);
  assert.deepEqual(data, spoke(9));
});

test("Gain set here at the radar's own setting changes nothing", () => {
  const data = spoke(9);
  viewing({ gain: 50 }).adjust(data, 3 * NM, PIXEL_COLORS);
  assert.deepEqual(data, spoke(9));
});

test("Gain turned down here fades echoes; turned up, it lifts them", () => {
  const down = spoke(6);
  viewing({ gain: 0 }).adjust(down, 3 * NM, PIXEL_COLORS);
  assert.ok(down.every((v) => v === 0), "a weak echo is gone");

  const ship = spoke(15);
  viewing({ gain: 25 }).adjust(ship, 3 * NM, PIXEL_COLORS);
  assert.ok(ship.every((v) => v > 0 && v < 15), "a ship fades");

  const up = spoke(6);
  viewing({ gain: 100 }).adjust(up, 3 * NM, PIXEL_COLORS);
  assert.ok(up.every((v) => v > 6 && v <= 15));
});

test("Gain never lights empty water nor touches Doppler and trails", () => {
  const data = spoke(0);
  data[10] = DOPPLER;
  data[11] = DOPPLER + 1;
  viewing({ gain: 0 }).adjust(data, 3 * NM, PIXEL_COLORS);
  viewing({ gain: 100 }).adjust(data, 3 * NM, PIXEL_COLORS);
  assert.equal(data[10], DOPPLER);
  assert.equal(data[11], DOPPLER + 1);
  assert.equal(data[0], 0);
});

test("More sea clears close to the boat and leaves far echoes", () => {
  const data = spoke(6);
  viewing({ sea: 100 }).adjust(data, 3 * NM, PIXEL_COLORS);
  assert.equal(data[0], 0, "by the boat");
  assert.equal(data[data.length - 1], 6, "three miles out");
});

test("Less sea than the radar's cannot bring back what it cleared", () => {
  const data = spoke(6);
  viewing({ sea: 0 }).adjust(data, 3 * NM, PIXEL_COLORS);
  assert.deepEqual(data, spoke(6));
});

test("Auto here leaves the setting to the radar", () => {
  const view = viewing({ gain: 0 });
  view.set({ id: "gain", value: 0, auto: true });
  const data = spoke(9);
  view.adjust(data, 3 * NM, PIXEL_COLORS);
  assert.deepEqual(data, spoke(9));
});

test("This screen shows its own setting whatever the radar reports", () => {
  const view = viewing({ gain: 20 });
  assert.equal(view.radarReported({ id: "gain", value: 50, auto: true }).value, 20);
  assert.equal(view.radarReported({ id: "gain", value: 50, auto: true }).auto, false);
  assert.equal(view.radarReported({ id: "power", value: 2 }).value, 2);
});

test("Zoom stays within the range the radar sends", () => {
  const view = viewing();
  assert.equal(view.nextRange(RANGES, 3 * NM, false), 3704);
  assert.equal(view.nextRange(RANGES, 3 * NM, true), undefined);
  assert.equal(view.set({ id: "range", value: 6 * NM }).value, 3 * NM);
  assert.equal(view.nextRange(RANGES, 926, false), undefined, "8x is the limit");
  assert.deepEqual(zoomSteps(RANGES, 5556), [926, 1389, 1852, 2778, 3704, 5556]);
});

test("A radar range shorter than the one zoomed to wins", () => {
  const view = viewing();
  view.set({ id: "range", value: 3704 });
  assert.equal(view.radarReported({ id: "range", value: 3 * NM }).value, 3704);
  assert.equal(view.radarReported({ id: "range", value: NM }).value, NM);
});

test("A setting from a slider counts as the number it shows", () => {
  const view = viewing();
  view.set({ id: "gain", value: "0", auto: false });
  const data = spoke(6);
  view.adjust(data, 3 * NM, PIXEL_COLORS);
  assert.ok(data.every((v) => v === 0));
});
