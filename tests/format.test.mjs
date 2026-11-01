import test from "node:test";
import assert from "node:assert/strict";
import { formatBytes, formatRatio, formatDate, formatCrc, formatCount } from "../src/lib/format.ts";

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

test("formatCrc is eight uppercase hex digits, em dash when absent", () => {
  assert.equal(formatCrc(0xabc), "00000ABC");
  assert.equal(formatCrc(0), "00000000");
  assert.equal(formatCrc(null), "—");
});

test("formatCount groups digits by language", () => {
  assert.equal(formatCount(100000), "100.000");
});
