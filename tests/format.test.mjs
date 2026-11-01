import test from "node:test";
import assert from "node:assert/strict";
import { formatBytes, formatRatio, formatDate } from "../src/lib/format.ts";

test("formatBytes 2516582 -> 2,4 MB; 0 -> 0 B", () => {
  assert.equal(formatBytes(2516582), "2,4 MB");
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(1023), "1023 B");
  assert.equal(formatBytes(1024), "1 KB");
});

test("formatRatio null packed -> em dash; size 0 guarded", () => {
  assert.equal(formatRatio(null, 10), "—");
  assert.equal(formatRatio(5, 0), "—");
  assert.equal(formatRatio(5, 10), "%50");
});

test("formatDate uses ms and Turkish locale", () => {
  const s = formatDate(Date.UTC(2026, 0, 5, 12, 0));
  assert.match(s, /2026/);
  assert.match(s, /05\.01\.2026/);
  assert.equal(formatDate(null), "—");
});

test("formatBytes bumps the unit when rounding reaches 1024", () => {
  assert.equal(formatBytes(1048575), "1 MB");
  assert.equal(formatBytes(1024 * 1024), "1 MB");
});
