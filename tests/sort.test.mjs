import test from "node:test";
import assert from "node:assert/strict";
import { sortNodes } from "../src/lib/sort.ts";

const n = (name, isDir = false, size = 0) => ({
  name, path: name, isDir, size, packed: null, modified: null, encrypted: false, crc: null,
});

test("Turkish sorting: c < ç < d, ı < i, folders first", () => {
  const out = sortNodes([n("d"), n("ç"), n("i"), n("c"), n("ı"), n("zdir", true)], "name", "asc");
  assert.deepEqual(out.map((x) => x.name), ["zdir", "c", "ç", "d", "ı", "i"]);
});

test("numeric sort: 2.txt before 10.txt", () => {
  const out = sortNodes([n("10.txt"), n("2.txt")], "name", "asc");
  assert.deepEqual(out.map((x) => x.name), ["2.txt", "10.txt"]);
});

test("desc keeps folders first and does not mutate input", () => {
  const input = [n("a"), n("b", true), n("c")];
  const out = sortNodes(input, "name", "desc");
  assert.deepEqual(out.map((x) => x.name), ["b", "c", "a"]);
  assert.deepEqual(input.map((x) => x.name), ["a", "b", "c"]);
});

test("size sort and null modified last", () => {
  const out = sortNodes([n("a", false, 5), n("b", false, 1)], "size", "asc");
  assert.deepEqual(out.map((x) => x.name), ["b", "a"]);
  const m = sortNodes([{ ...n("x"), modified: null }, { ...n("y"), modified: 5 }], "modified", "asc");
  assert.deepEqual(m.map((x) => x.name), ["y", "x"]);
});

test("crc and ratio sort keys, nulls last", () => {
  const x = (name, o) => ({ ...n(name), ...o });
  const c = sortNodes([x("a", { crc: 9 }), x("b", { crc: null }), x("c", { crc: 2 })], "crc", "asc");
  assert.deepEqual(c.map((v) => v.name), ["c", "a", "b"]);
  const r = sortNodes(
    [x("a", { size: 10, packed: 9 }), x("b", { size: 0, packed: 0 }), x("c", { size: 10, packed: 2 })],
    "ratio", "desc");
  assert.deepEqual(r.map((v) => v.name), ["a", "c", "b"]);
});

test("an unknown size sorts last in either direction", () => {
  const rows = [{ ...n("u"), sizeUnknown: true }, n("a", false, 5), n("b", false, 9)];
  assert.deepEqual(sortNodes(rows, "size", "asc").map((x) => x.name), ["a", "b", "u"]);
  assert.deepEqual(sortNodes(rows, "size", "desc").map((x) => x.name), ["b", "a", "u"]);
});
