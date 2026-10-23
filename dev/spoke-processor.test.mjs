// Run: node --test dev/spoke-processor.test.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  SpokeProcessorFactory,
  SpokeProcessingMode,
} from "../web/gui/spoke_processor.js";

const LEGEND = { lowReturn: 1, mediumReturn: 2, strongReturn: 3, specialStart: 16 };
const SPOKES = 2048;

function fillAfterGap(sparseSpokes) {
  const buffer = new Uint8Array(SPOKES);
  const fill = SpokeProcessorFactory.create(
    SpokeProcessingMode.FILL,
    SPOKES,
    LEGEND,
    sparseSpokes
  );
  fill.processSpoke(buffer, { angle: 100, data: new Uint8Array([1]) }, SPOKES, 1);
  fill.processSpoke(buffer, { angle: 108, data: new Uint8Array([2]) }, SPOKES, 1);
  return Array.from(buffer.subarray(101, 109));
}

test("Fill repeats a spoke over the angles a sparse radar skipped", () => {
  assert.deepEqual(fillAfterGap(true), Array(8).fill(2));
});

test("Fill keeps the last sweep where a radar that sends every spoke lost some", () => {
  assert.deepEqual(fillAfterGap(false), [0, 0, 0, 0, 0, 0, 0, 2]);
});
