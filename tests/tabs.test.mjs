import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  archiveCrumbs, baseName, beginGesture, clampSplit, closeTab, emptyTabs, fileToShow, fitColumns, focusTab, isArchiveName, isCloseTabKey, isReloadKey,
  expandHome, keyCommand, listModel, parseShowHidden, visibleRows, localCrumbs, tabLabel, moveGesture, moveTo, navigate, newView, openTab, openedTab, parentOf, parseSplit, pressRow,
  pruneSelection, releaseGesture, rowKey, selectAll, selectClick, selectedPaths, selectionSummary, toggleCursor, toggleSort, updateTab,
} from "../src/lib/tabs-core.ts";

const list = (keys) => ({ keys, indexOf: new Map(keys.map((k, i) => [k, i])) });

const row = (path, isDir = false, size = 1) => ({
  name: path.split("/").pop(), path, isDir, size, packed: null, modified: null, encrypted: false, crc: null,
});

function three() {
  let s = emptyTabs();
  for (const p of ["/a.rar", "/b.zip", "/c.7z"]) s = openTab(s, p);
  return s;
}

test("opening the same path twice focuses the existing tab", () => {
  let s = three();
  const a = s.tabs[0];
  assert.equal(s.tabs.length, 3);
  assert.equal(s.activeId, s.tabs[2].id);
  s = openTab(s, "/a.rar");
  assert.equal(s.tabs.length, 3);
  assert.equal(s.activeId, a.id);
});

test("closing active tab focuses a neighbor", () => {
  let s = three();
  const [a, b, c] = s.tabs;
  s = focusTab(s, b.id);
  s = closeTab(s, b.id);
  assert.equal(s.activeId, c.id, "right neighbor first");
  s = closeTab(s, c.id);
  assert.equal(s.activeId, a.id, "left neighbor when last");
  s = closeTab(s, a.id);
  assert.equal(s.activeId, null);
  assert.deepEqual(s.tabs, []);
});

test("closing an inactive tab keeps the active one", () => {
  let s = three();
  const [a, , c] = s.tabs;
  s = closeTab(s, a.id);
  assert.equal(s.activeId, c.id);
  assert.equal(closeTab(s, 999), s, "unknown id is a no-op");
});

test("tab ids are never reused, so a late open result cannot land on another tab", () => {
  let s = openTab(emptyTabs(), "/a.rar");
  const first = s.tabs[0].id;
  s = closeTab(s, first);
  s = openTab(s, "/a.rar");
  assert.notEqual(s.tabs[0].id, first);
});

test("navigate into folder and back restores parent selection", () => {
  const keys = list(["d:docs", "f:a.txt", "f:b.txt"]);
  let v = newView("");
  v = selectClick(v, keys, 1, {});
  v = selectClick(v, keys, 2, { range: true });
  assert.deepEqual([...v.selection], ["f:a.txt", "f:b.txt"]);
  const inside = navigate(v, "docs");
  assert.equal(inside.cwd, "docs");
  assert.equal(inside.selection.size, 0);
  assert.equal(inside.cursor, null);
  const back = navigate(inside, parentOf("docs"), "docs");
  assert.equal(back.cwd, "");
  assert.deepEqual([...back.selection], ["f:a.txt", "f:b.txt"]);
  assert.equal(back.cursor, v.cursor);
});

test("going up to a folder never visited selects the folder we came from", () => {
  const v = navigate(newView("x/y"), "x", "x/y");
  assert.deepEqual([...v.selection], ["d:x/y"]);
  assert.equal(v.cursor, "d:x/y");
});

test("navigating to the same folder leaves the view untouched", () => {
  const v = newView("x");
  assert.equal(navigate(v, "x"), v);
});

test("a tab keeps its folder and selection across focus changes", () => {
  let s = three();
  const a = s.tabs[0];
  s = updateTab(s, a.id, (t) => navigate(t, "sub"));
  s = focusTab(s, s.tabs[1].id);
  s = focusTab(s, a.id);
  assert.equal(s.tabs[0].cwd, "sub");
  assert.equal(s.tabs[0].path, "/a.rar");
});

test("openedTab fills in the listing and detects password protection", () => {
  const info = { format: "zip", fileSize: 1, encryptedHeaders: false, solid: false, multiVolume: false, hasRecovery: false, locked: false, comment: null };
  const entry = { path: "d/a.txt", isDir: false, size: 3, packed: 1, modified: null, crc: 7, encrypted: true };
  let s = openTab(emptyTabs(), "/a.zip");
  assert.equal(s.tabs[0].loading, true);
  s = openedTab(s, s.tabs[0].id, { info, entries: [entry] });
  assert.equal(s.tabs[0].loading, false);
  assert.equal(s.tabs[0].info, info);
  assert.equal(s.tabs[0].locked, true);
  assert.ok(s.tabs[0].index.dirs.has("d"));
  assert.equal(openedTab(s, 999, { info, entries: [] }), s, "a closed tab is ignored");
});

test("a same-named file and folder are selected separately but pass as plain paths", () => {
  const f = row("x");
  const d = row("x", true);
  assert.notEqual(rowKey(f), rowKey(d));
  assert.deepEqual(selectedPaths(new Set([rowKey(f), rowKey(d), "f:y"])), ["x", "y"]);
});

test("click, toggle and range selection", () => {
  const keys = list(["a", "b", "c", "d"]);
  let v = selectClick(newView(""), keys, 1, {});
  assert.deepEqual([...v.selection], ["b"]);
  v = selectClick(v, keys, 3, { toggle: true });
  assert.deepEqual([...v.selection].sort(), ["b", "d"]);
  v = selectClick(v, keys, 3, { toggle: true });
  assert.deepEqual([...v.selection], ["b"]);
  v = selectClick(v, keys, 1, {});
  v = selectClick(v, keys, 3, { range: true });
  assert.deepEqual([...v.selection], ["b", "c", "d"]);
  v = selectClick(v, keys, 0, { range: true });
  assert.deepEqual([...v.selection], ["a", "b"], "the anchor stays where the range began");
});

test("keyboard moves clamp, and shift extends from the anchor", () => {
  const keys = list(["a", "b", "c"]);
  let v = moveTo(newView(""), keys, -5, false);
  assert.equal(v.cursor, "a");
  v = moveTo(v, keys, 99, true);
  assert.equal(v.cursor, "c");
  assert.deepEqual([...v.selection], ["a", "b", "c"]);
  v = moveTo(v, keys, 1, true);
  assert.deepEqual([...v.selection], ["a", "b"]);
  v = moveTo(v, keys, 2, false);
  assert.deepEqual([...v.selection], ["c"]);
  const none = newView("");
  assert.equal(moveTo(none, list([]), 1, false), none);
});

test("extending a selection looks the anchor up in the map, never scanning the keys", () => {
  const l = list(["a", "b", "c"]);
  l.keys.indexOf = () => assert.fail("scanned the keys");
  const v = selectClick(newView(""), l, 0, {});
  assert.deepEqual([...selectClick(v, l, 2, { range: true }).selection], ["a", "b", "c"]);
  assert.deepEqual([...moveTo(v, l, 1, true).selection], ["a", "b"]);
});

test("a plain press inside a multi-selection waits for the release, which collapses it", () => {
  const l = list(["a", "b", "c"]);
  const all = selectAll(newView(""), l.keys);
  const p = pressRow(all, l, 1, {});
  assert.equal(p.collapseOnRelease, true);
  assert.equal(p.view.selection.size, 3, "kept during the press so a drag can take them all");
  assert.equal(p.view.cursor, "b");
  assert.deepEqual([...selectClick(p.view, l, 1, {}).selection], ["b"], "the release");
});

test("any other press applies at once and has nothing to do on release", () => {
  const l = list(["a", "b", "c"]);
  const all = selectAll(newView(""), l.keys);
  const single = pressRow(selectClick(newView(""), l, 0, {}), l, 0, {});
  assert.equal(single.collapseOnRelease, false);
  assert.deepEqual([...single.view.selection], ["a"]);
  const outside = pressRow(selectClick(newView(""), l, 0, {}), l, 2, {});
  assert.equal(outside.collapseOnRelease, false);
  assert.deepEqual([...outside.view.selection], ["c"]);
  const toggled = pressRow(all, l, 1, { toggle: true });
  assert.equal(toggled.collapseOnRelease, false);
  assert.deepEqual([...toggled.view.selection], ["a", "c"]);
  const ranged = pressRow({ ...all, anchor: "a" }, l, 1, { range: true });
  assert.equal(ranged.collapseOnRelease, false);
  assert.deepEqual([...ranged.view.selection], ["a", "b"]);
});

test("keys act only in their unmodified forms, Shift only where it extends", () => {
  const k = (key, m = {}) => keyCommand({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...m });
  for (const key of ["Enter", "Backspace", " "]) {
    assert.ok(k(key), key);
    for (const m of [{ shiftKey: true }, { altKey: true }, { metaKey: true }, { ctrlKey: true }]) {
      assert.equal(k(key, m), null, `${key} ${JSON.stringify(m)}`);
    }
  }
  for (const key of ["Home", "End", "PageUp", "PageDown"]) {
    assert.equal(k(key).extend, false, key);
    assert.equal(k(key, { shiftKey: true }).extend, true, key);
    for (const m of [{ altKey: true }, { metaKey: true }, { ctrlKey: true }]) {
      assert.equal(k(key, m), null, `${key} ${JSON.stringify(m)}`);
    }
  }
  assert.deepEqual(k("ArrowDown"), { kind: "move", step: 1, extend: false });
  assert.deepEqual(k("ArrowUp", { shiftKey: true }), { kind: "move", step: -1, extend: true });
  assert.deepEqual(k("ArrowDown", { metaKey: true }), { kind: "open" });
  assert.deepEqual(k("ArrowUp", { ctrlKey: true }), { kind: "up" });
  assert.equal(k("ArrowUp", { altKey: true }), null);
  assert.equal(k("ArrowUp", { metaKey: true, shiftKey: true }), null);
  assert.deepEqual(k("a", { metaKey: true }), { kind: "all" });
  assert.deepEqual(k("A", { ctrlKey: true }), { kind: "all" });
  assert.equal(k("a"), null);
  assert.equal(k("a", { metaKey: true, shiftKey: true }), null);
  assert.equal(k("x"), null);
});

test("reload shortcuts are recognized", () => {
  const k = (key, m = {}) => isReloadKey({ key, metaKey: false, ctrlKey: false, ...m });
  assert.ok(k("F5"));
  assert.ok(k("r", { metaKey: true }));
  assert.ok(k("R", { ctrlKey: true }));
  assert.ok(!k("r"));
  assert.ok(!k("F4"));
});

test("select all and toggling the cursor", () => {
  const keys = ["a", "b"];
  let v = selectAll(newView(""), keys);
  assert.equal(v.selection.size, 2);
  v = { ...v, cursor: "a" };
  v = toggleCursor(v);
  assert.deepEqual([...v.selection], ["b"]);
  v = toggleCursor(v);
  assert.equal(v.selection.size, 2);
});

test("a refresh drops selection and cursor that no longer exist", () => {
  const keys = ["a"];
  const v = { ...newView(""), selection: new Set(["a", "gone"]), cursor: "gone", anchor: "gone" };
  const p = pruneSelection(v, new Map(keys.map((k, i) => [k, i])));
  assert.deepEqual([...p.selection], ["a"]);
  assert.equal(p.cursor, null);
  assert.equal(p.anchor, null);
});

test("selectionSummary counts and adds up only selected rows", () => {
  const m = listModel([row("a", false, 10), row("b", false, 20), row("c", false, 40)]);
  assert.deepEqual(selectionSummary(m, new Set(["f:a", "f:c", "f:gone"])), { count: 2, size: 50 });
});

test("sorting the same column flips direction, a new column starts ascending", () => {
  let v = { sortKey: "name", sortDir: "asc" };
  v = toggleSort(v, "name");
  assert.equal(v.sortDir, "desc");
  v = toggleSort(v, "size");
  assert.deepEqual(v, { sortKey: "size", sortDir: "asc" });
});

test("archive breadcrumbs map to prefixes and stay short on hostile depth", () => {
  assert.deepEqual(archiveCrumbs(""), []);
  assert.deepEqual(archiveCrumbs("a/b"), [{ label: "a", path: "a" }, { label: "b", path: "a/b" }]);
  const deep = archiveCrumbs(Array.from({ length: 600 }, (_, i) => `s${i}`).join("/"));
  assert.ok(deep.length <= 13);
  assert.equal(deep[0].label, "s0");
  assert.equal(deep.at(-1).label, "s599");
  assert.equal(deep[1].label, "…");
  assert.equal(deep[1].path, null);
});

test("local breadcrumbs handle POSIX and Windows paths", () => {
  assert.deepEqual(localCrumbs("/"), [{ label: "/", path: "/" }]);
  assert.deepEqual(localCrumbs("/Users/ali"), [
    { label: "/", path: "/" }, { label: "Users", path: "/Users" }, { label: "ali", path: "/Users/ali" },
  ]);
  assert.deepEqual(localCrumbs("C:\\Users\\ali"), [
    { label: "C:", path: "C:\\" }, { label: "Users", path: "C:\\Users" }, { label: "ali", path: "C:\\Users\\ali" },
  ]);
});

test("baseName takes the last segment of either separator", () => {
  assert.equal(baseName("/a/b.rar"), "b.rar");
  assert.equal(baseName("C:\\x\\y.zip"), "y.zip");
  assert.equal(baseName("/"), "/");
});

test("parentOf walks archive folders up to the root", () => {
  assert.equal(parentOf("a/b"), "a");
  assert.equal(parentOf("a"), "");
  assert.equal(parentOf(""), null);
});

test("archive names are recognized for the icon, case-insensitively", () => {
  for (const n of ["a.rar", "A.ZIP", "b.7z", "c.tar", "d.tar.gz", "e.tgz", "f.tbz2", "g.txz", "h.gz", "i.bz2", "j.xz", "k.part1.rar"]) {
    assert.ok(isArchiveName(n), n);
  }
  for (const n of ["a.txt", "rar", ".DS_Store", "zip.pdf"]) assert.ok(!isArchiveName(n), n);
});

test("the saved split ratio is parsed defensively and clamped to the window", () => {
  assert.equal(parseSplit("0.3"), 0.3);
  for (const bad of [null, "", "abc", "NaN", "Infinity", "-1", "2"]) assert.equal(parseSplit(bad), 0.5, String(bad));
  assert.equal(clampSplit(0.5, 1000), 0.5);
  assert.equal(clampSplit(0.01, 1000), 0.26);
  assert.equal(clampSplit(0.99, 1000), 0.74);
  assert.equal(clampSplit(0.1, 400), 0.5, "too narrow for both minimums: split evenly");
});

test("columns that do not fit drop by priority and the name keeps its minimum", () => {
  const cols = [
    { key: "size", width: 70, priority: 1 },
    { key: "packed", width: 70, priority: 3 },
    { key: "modified", width: 120, priority: 2 },
  ];
  const keys = (w) => fitColumns(cols, w, 150).map((c) => c.key);
  assert.deepEqual(keys(1000), ["size", "packed", "modified"], "original order kept");
  assert.deepEqual(keys(345), ["size", "modified"]);
  assert.deepEqual(keys(225), ["size"]);
  assert.deepEqual(keys(100), []);
  assert.deepEqual(keys(300), ["size", "packed"], "packed still fits after modified did not");
});

test("a press inside a multi-selection collapses on a plain release", () => {
  const order = list(["f:a", "f:b", "f:c"]);
  const view = { ...newView(""), selection: new Set(["f:a", "f:b"]), cursor: "f:a", anchor: "f:a" };
  const pressed = pressRow(view, order, 1, {});
  assert.equal(pressed.collapseOnRelease, true);
  const g = beginGesture(100, 100, 1);
  assert.equal(releaseGesture(g, 1), 1, "released on the same row, no drag");
  assert.equal(releaseGesture(g, 2), null, "released on another row");
});

test("a real drag cancels the collapse, so the multi-selection survives", () => {
  let g = beginGesture(100, 100, 1);
  const small = moveGesture(g, 102, 101);
  assert.equal(small.started, false, "a shaky click is not a drag");
  assert.equal(releaseGesture(small.gesture, 1), 1);
  const far = moveGesture(small.gesture, 112, 100);
  assert.equal(far.started, true);
  g = far.gesture;
  assert.equal(releaseGesture(g, 1), null, "dragged back to the same row and released: no collapse");
  const again = moveGesture(g, 200, 200);
  assert.equal(again.started, false, "the drag starts once");
});

test("Close Tab key: Ctrl+W off the Mac only, and never with Shift", () => {
  const k = (o) => ({ key: "w", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...o });
  assert.equal(isCloseTabKey(k({ ctrlKey: true }), false), true);
  assert.equal(isCloseTabKey(k({ ctrlKey: true, key: "W" }), false), true);
  assert.equal(isCloseTabKey(k({ ctrlKey: true, shiftKey: true, key: "W" }), false), false, "that is Close Window");
  assert.equal(isCloseTabKey(k({ metaKey: true }), true), false, "the menu owns it on the Mac");
  assert.equal(isCloseTabKey(k({}), false), false);
});

test("fileToShow: the one selected file, or the cursor's file when nothing is selected", () => {
  const model = listModel([row("alt", true), row("a.txt"), row("b.txt")]);
  const v = newView("");
  assert.equal(fileToShow(v, model), null);
  assert.equal(fileToShow({ ...v, cursor: "f:a.txt" }, model).path, "a.txt");
  assert.equal(fileToShow({ ...v, cursor: "d:alt" }, model), null, "a folder is not shown");
  assert.equal(fileToShow({ ...v, selection: new Set(["f:b.txt"]), cursor: "f:a.txt" }, model).path, "b.txt");
  assert.equal(fileToShow({ ...v, selection: new Set(["d:alt"]) }, model), null);
  assert.equal(fileToShow({ ...v, selection: new Set(["f:a.txt", "f:b.txt"]) }, model), null, "two is ambiguous");
  assert.equal(fileToShow({ ...v, selection: new Set(["f:gone"]) }, model), null);
});

test("breadcrumbs and tab labels mark bidi overrides, and paths stay exact for actions", () => {
  const spoof = "fatura\u202Etxt.command";
  assert.deepEqual(archiveCrumbs(`a/${spoof}`), [
    { label: "a", path: "a" },
    { label: "fatura\uFFFDtxt.command", path: `a/${spoof}` },
  ]);
  assert.deepEqual(localCrumbs(`/x/${spoof}`).at(-1), { label: "fatura\uFFFDtxt.command", path: `/x/${spoof}` });
  assert.deepEqual(localCrumbs(`C:\\${spoof}`).at(-1), { label: "fatura\uFFFDtxt.command", path: `C:\\${spoof}` });
  assert.equal(tabLabel(`/x/${spoof}.rar`), "fatura\uFFFDtxt.command.rar");
  assert.equal(tabLabel("/x/türkçe 😀.rar"), "türkçe 😀.rar");
});

test("the file list and the panes show names only through safeName", () => {
  const read = (f) => readFileSync(new URL(`../src/lib/${f}`, import.meta.url), "utf8");
  const list = read("FileList.svelte");
  assert.ok(!/\{r\.name\}/.test(list), "FileList shows a raw row name");
  assert.ok(/title=\{safeName\(r\.name\)\}/.test(list));
  assert.ok(/<span class="text">\{safeName\(r\.name\)\}<\/span>/.test(list));
  const pane = read("ArchivePane.svelte");
  assert.ok(!/\{baseName\(/.test(pane), "ArchivePane shows a raw archive name");
  assert.ok(!/title=\{(x|tab)\.path\}/.test(pane), "ArchivePane shows a raw archive path");
  assert.ok(!/name: failure\.path,/.test(read("LocalPane.svelte")), "LocalPane shows a raw path");
});

test("selectionSummary has no size when a selected row's size is unknown", () => {
  const m = listModel([
    { name: "a", path: "a", isDir: false, size: 5 },
    { name: "u", path: "u", isDir: false, size: 0, sizeUnknown: true },
  ]);
  assert.deepEqual(selectionSummary(m, new Set(["f:a"])), { count: 1, size: 5 });
  assert.deepEqual(selectionSummary(m, new Set(["f:a", "f:u"])), { count: 2, size: null });
});

test("hidden rows are left out unless asked for, and that is off by default", () => {
  const rows = [{ name: ".git", hidden: true }, { name: "a.txt", hidden: false }];
  assert.deepEqual(visibleRows(rows, false).map((r) => r.name), ["a.txt"]);
  assert.deepEqual(visibleRows(rows, true).map((r) => r.name), [".git", "a.txt"]);
  assert.equal(parseShowHidden(null), false);
  assert.equal(parseShowHidden("garbage"), false);
  assert.equal(parseShowHidden("false"), false);
  assert.equal(parseShowHidden("true"), true);
});

test("expandHome handles ~ and ~/x only", () => {
  assert.equal(expandHome("~", "/Users/o"), "/Users/o");
  assert.equal(expandHome("~/Masaüstü", "/Users/o"), "/Users/o/Masaüstü");
  assert.equal(expandHome("~\\x", "C:\\Users\\o"), "C:\\Users\\o\\x");
  assert.equal(expandHome("~kim/x", "/Users/o"), "~kim/x");
  assert.equal(expandHome("/a/~", "/Users/o"), "/a/~");
  assert.equal(expandHome("göreli", "/Users/o"), "göreli");
});
