import test from "node:test";
import assert from "node:assert/strict";
import { visibleRange } from "../src/lib/virtual.ts";

test("visibleRange clamps at ends and adds overscan", () => {
  assert.deepEqual(visibleRange(0, 100, 10, 1000, 3), { start: 0, end: 13 });
  assert.deepEqual(visibleRange(500, 100, 10, 1000, 3), { start: 47, end: 63 });
  assert.deepEqual(visibleRange(99999, 100, 10, 1000, 3), { start: 987, end: 1000 });
  assert.deepEqual(visibleRange(0, 100, 10, 0, 3), { start: 0, end: 0 });
  assert.deepEqual(visibleRange(-50, 100, 10, 5, 3), { start: 0, end: 5 });
});

test("visibleRange renders under 100 rows for 100k items at 800px", () => {
  const r = visibleRange(123456, 800, 24, 100000, 10);
  assert.ok(r.end - r.start < 100, `${r.end - r.start}`);
});
