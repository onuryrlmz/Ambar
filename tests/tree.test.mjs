import test from "node:test";
import assert from "node:assert/strict";
import { buildIndex, childrenOf, descendantPaths } from "../src/lib/tree.ts";

const f = (path, size = 1, packed = 1, extra = {}) => ({
  path, isDir: false, size, packed, modified: 1000, crc: 1, encrypted: false, ...extra,
});
const d = (path, extra = {}) => ({
  path, isDir: true, size: 0, packed: 0, modified: 2000, crc: null, encrypted: false, ...extra,
});

test("synthesizes implicit folders and sums sizes", () => {
  const idx = buildIndex([f("a/b/c.txt", 10, 4), f("a/d.txt", 5, 2)]);
  const root = childrenOf(idx, "");
  assert.equal(root.length, 1);
  assert.equal(root[0].name, "a");
  assert.equal(root[0].isDir, true);
  assert.equal(root[0].path, "a");
  assert.equal(root[0].size, 15);
  assert.equal(root[0].packed, 6);
  const a = childrenOf(idx, "a");
  assert.deepEqual(a.map((n) => n.name).sort(), ["b", "d.txt"]);
  assert.equal(a.find((n) => n.name === "b").size, 10);
});

test("packed sums to null if any descendant is null", () => {
  const idx = buildIndex([f("a/x", 1, null), f("a/y", 1, 3)]);
  const a = childrenOf(idx, "")[0];
  assert.equal(a.packed, null);
  assert.equal(a.size, 2);
  assert.ok(!Number.isNaN(a.size));
});

test("root children contain only top level", () => {
  const idx = buildIndex([f("x.txt"), f("a/b.txt"), f("a/c/d.txt")]);
  assert.deepEqual(childrenOf(idx, "").map((n) => n.name).sort(), ["a", "x.txt"]);
  assert.deepEqual(childrenOf(idx, "nope"), []);
});

test("descendantPaths expands folder", () => {
  const idx = buildIndex([f("a/b.txt"), f("a/c/d.txt"), f("z.txt")]);
  const out = descendantPaths(idx, ["a"]);
  assert.deepEqual(out.sort(), ["a/b.txt", "a/c/d.txt"]);
  assert.deepEqual(descendantPaths(idx, ["z.txt"]), ["z.txt"]);
});

test("explicit folder entry after children does not duplicate; its metadata applies", () => {
  const idx = buildIndex([f("a/b.txt", 3, 1), d("a", { modified: 5555 })]);
  const root = childrenOf(idx, "");
  assert.equal(root.length, 1);
  assert.equal(root[0].modified, 5555);
  assert.equal(root[0].size, 3);
});

test("file and folder with the same name give both nodes", () => {
  const idx = buildIndex([f("a", 2), f("a/b.txt", 3)]);
  const root = childrenOf(idx, "");
  assert.equal(root.length, 2);
  assert.deepEqual(root.map((n) => n.isDir).sort(), [false, true]);
  assert.equal(root.find((n) => n.isDir).size, 3);
});

test("duplicate file entries keep the later one", () => {
  const idx = buildIndex([f("a.txt", 1), f("a.txt", 9)]);
  const root = childrenOf(idx, "");
  assert.equal(root.length, 1);
  assert.equal(root[0].size, 9);
});

test("empty archive gives empty root", () => {
  const idx = buildIndex([]);
  assert.deepEqual(childrenOf(idx, ""), []);
  assert.deepEqual(descendantPaths(idx, ["x"]), []);
});

test("100k entries index under 500 ms and childrenOf under 50 ms", () => {
  const entries = [];
  for (let i = 0; i < 100000; i++) entries.push(f(`d${i % 100}/s${i % 7}/f${i}.txt`, i, i));
  let t0 = performance.now();
  const idx = buildIndex(entries);
  const build = performance.now() - t0;
  t0 = performance.now();
  const kids = childrenOf(idx, "d1/s1");
  const q = performance.now() - t0;
  assert.ok(kids.length > 0);
  assert.ok(build < 500, `build ${build}`);
  assert.ok(q < 50, `children ${q}`);
});
