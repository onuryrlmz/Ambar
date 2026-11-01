// Pure tab, folder and selection logic, so Node can test it. Everything returns new
// objects and never mutates: `tabs.svelte.ts` and `local.svelte.ts` hold the result in
// `$state.raw`, because a deep proxy over a 100,000-entry index would be far too slow.

import { safeName } from "./names.ts";
import { buildIndex, type ArchiveIndex } from "./tree.ts";
import type { ArchiveInfo, OpenResult, SortDir, SortKey, TreeNode } from "./types.ts";

interface SavedView {
  selection: Set<string>;
  cursor: string | null;
  anchor: string | null;
}

/** One pane's position in a folder tree. Selection, cursor and anchor hold `rowKey`s. */
export interface PaneView extends SavedView {
  cwd: string;
  /** What each visited folder looked like when we left it. */
  memory: Map<string, SavedView>;
}

export interface SortState {
  sortKey: SortKey;
  sortDir: SortDir;
}

export interface Tab extends PaneView, SortState {
  id: number;
  path: string;
  /** Null until the listing arrives. */
  info: ArchiveInfo | null;
  index: ArchiveIndex | null;
  /** Holds a password-protected archive. `info.locked` is the unrelated RAR lock flag. */
  locked: boolean;
  loading: boolean;
}

export interface TabsState {
  tabs: Tab[];
  activeId: number | null;
  nextId: number;
}

export const emptyTabs = (): TabsState => ({ tabs: [], activeId: null, nextId: 1 });

export const newView = (cwd: string): PaneView => ({
  cwd, selection: new Set(), cursor: null, anchor: null, memory: new Map(),
});

/** Opening a path that is already open focuses its tab. */
export function openTab(state: TabsState, path: string): TabsState {
  const open = state.tabs.find((t) => t.path === path);
  if (open) return { ...state, activeId: open.id };
  const tab: Tab = {
    ...newView(""), id: state.nextId, path, info: null, index: null,
    locked: false, loading: true, sortKey: "name", sortDir: "asc",
  };
  return { tabs: [...state.tabs, tab], activeId: tab.id, nextId: state.nextId + 1 };
}

/** Closing the active tab focuses its right neighbor, or the left one if it was last. */
export function closeTab(state: TabsState, id: number): TabsState {
  const i = state.tabs.findIndex((t) => t.id === id);
  if (i < 0) return state;
  const tabs = state.tabs.filter((t) => t.id !== id);
  const activeId = state.activeId === id ? (tabs[Math.min(i, tabs.length - 1)]?.id ?? null) : state.activeId;
  return { ...state, tabs, activeId };
}

export function focusTab(state: TabsState, id: number): TabsState {
  return state.tabs.some((t) => t.id === id) ? { ...state, activeId: id } : state;
}

export function updateTab(state: TabsState, id: number, f: (t: Tab) => Tab): TabsState {
  if (!state.tabs.some((t) => t.id === id)) return state;
  return { ...state, tabs: state.tabs.map((t) => (t.id === id ? f(t) : t)) };
}

/** Stores a finished listing. A tab closed in the meantime is ignored. */
export function openedTab(state: TabsState, id: number, res: OpenResult): TabsState {
  return updateTab(state, id, (t) => ({
    ...t,
    info: res.info,
    index: buildIndex(res.entries),
    locked: res.info.encryptedHeaders || res.entries.some((e) => e.encrypted),
    loading: false,
  }));
}

/** A file and a folder may share a path, so the kind is part of the key. */
export const rowKey = (r: { isDir: boolean; path: string }): string => (r.isDir ? "d:" : "f:") + r.path;

/** The backend takes plain paths; a same-named file and folder collapse into one. */
export function selectedPaths(selection: Iterable<string>): string[] {
  const out = new Set<string>();
  for (const k of selection) out.add(k.slice(2));
  return [...out];
}

/**
 * Moves to `dir`. A folder seen before comes back as we left it; otherwise `cameFrom`
 * (the child folder being left, when going up) gets the cursor.
 */
export function navigate<V extends PaneView>(view: V, dir: string, cameFrom?: string): V {
  if (dir === view.cwd) return view;
  const memory = new Map(view.memory);
  memory.set(view.cwd, { selection: view.selection, cursor: view.cursor, anchor: view.anchor });
  const saved = memory.get(dir);
  if (saved) return { ...view, cwd: dir, memory, ...saved };
  const key = cameFrom === undefined ? null : "d:" + cameFrom;
  return { ...view, cwd: dir, memory, selection: new Set(key ? [key] : []), cursor: key, anchor: key };
}

/** The parent of an archive folder ("" is the root, which has none). */
export function parentOf(cwd: string): string | null {
  if (cwd === "") return null;
  const i = cwd.lastIndexOf("/");
  return i < 0 ? "" : cwd.slice(0, i);
}

const between = (keys: string[], a: number, b: number) =>
  new Set(keys.slice(Math.min(a, b), Math.max(a, b) + 1));

/** Row order, with an index so that per-keypress lookups do not scan the keys. */
export type RowOrder = Pick<ListModel<unknown>, "keys" | "indexOf">;

/** Applies a click on row `i`: plain replaces, `toggle` flips, `range` extends from the anchor. */
export function selectClick<V extends PaneView>(
  view: V,
  order: RowOrder,
  i: number,
  mods: { toggle?: boolean; range?: boolean },
): V {
  const key = order.keys[i];
  if (key === undefined) return view;
  if (mods.range) {
    const from = view.anchor === null ? undefined : order.indexOf.get(view.anchor);
    if (from !== undefined) return { ...view, selection: between(order.keys, from, i), cursor: key };
  } else if (mods.toggle) {
    const selection = new Set(view.selection);
    if (!selection.delete(key)) selection.add(key);
    return { ...view, selection, cursor: key, anchor: key };
  }
  return { ...view, selection: new Set([key]), cursor: key, anchor: key };
}

/**
 * A mouse press on row `i`. A plain press inside a multi-selection keeps it for now, so
 * a drag can take all of it; `collapseOnRelease` says a release without a drag must then
 * apply the plain click.
 */
export function pressRow<V extends PaneView>(
  view: V,
  order: RowOrder,
  i: number,
  mods: { toggle?: boolean; range?: boolean },
): { view: V; collapseOnRelease: boolean } {
  const key = order.keys[i];
  if (key !== undefined && !mods.toggle && !mods.range && view.selection.has(key) && view.selection.size > 1) {
    return { view: { ...view, cursor: key, anchor: key }, collapseOnRelease: true };
  }
  return { view: selectClick(view, order, i, mods), collapseOnRelease: false };
}

/** A mouse gesture on a row: from the press until the release. */
export interface RowGesture {
  x: number;
  y: number;
  /** From `pressRow`: the row a release without a drag narrows the selection to. */
  collapse: number | null;
  dragging: boolean;
}

export const DRAG_THRESHOLD = 5;

export const beginGesture = (x: number, y: number, collapse: number | null): RowGesture => ({
  x, y, collapse, dragging: false,
});

/** `started` is true on the one move that turns the press into a drag. A drag never collapses. */
export function moveGesture(g: RowGesture, x: number, y: number): { gesture: RowGesture; started: boolean } {
  if (g.dragging || Math.hypot(x - g.x, y - g.y) < DRAG_THRESHOLD) return { gesture: g, started: false };
  return { gesture: { ...g, dragging: true, collapse: null }, started: true };
}

/** The row to narrow the selection to when the gesture ends on row `i`, if any. */
export const releaseGesture = (g: RowGesture, i: number): number | null => (g.collapse === i ? i : null);

/** Keyboard move to row `i` (clamped); `extend` grows the selection from the anchor. */
export function moveTo<V extends PaneView>(view: V, order: RowOrder, i: number, extend: boolean): V {
  const { keys } = order;
  if (keys.length === 0) return view;
  const idx = Math.min(Math.max(0, i), keys.length - 1);
  const key = keys[idx];
  if (extend) {
    const anchor = view.anchor ?? view.cursor ?? key;
    const from = anchor === key ? idx : order.indexOf.get(anchor);
    if (from !== undefined) return { ...view, selection: between(keys, from, idx), cursor: key, anchor };
  }
  return { ...view, selection: new Set([key]), cursor: key, anchor: key };
}

export const selectAll = <V extends PaneView>(view: V, keys: string[]): V => ({ ...view, selection: new Set(keys) });

/** Space on Windows and Linux: flips the row under the cursor. */
export function toggleCursor<V extends PaneView>(view: V): V {
  if (view.cursor === null) return view;
  const selection = new Set(view.selection);
  if (!selection.delete(view.cursor)) selection.add(view.cursor);
  return { ...view, selection };
}

export const clearSelection = <V extends PaneView>(view: V): V =>
  view.selection.size === 0 ? view : { ...view, selection: new Set() };

/** After a refresh: forgets rows that are gone. */
export function pruneSelection<V extends PaneView>(view: V, indexOf: Map<string, number>): V {
  const selection = new Set([...view.selection].filter((k) => indexOf.has(k)));
  const cursor = view.cursor !== null && indexOf.has(view.cursor) ? view.cursor : null;
  const anchor = view.anchor !== null && indexOf.has(view.anchor) ? view.anchor : null;
  if (selection.size === view.selection.size && cursor === view.cursor && anchor === view.anchor) return view;
  return { ...view, selection, cursor, anchor };
}

/** Sorted rows with their keys. */
export interface ListModel<R> {
  rows: R[];
  keys: string[];
  indexOf: Map<string, number>;
}

export function listModel<R extends { isDir: boolean; path: string }>(rows: R[]): ListModel<R> {
  const keys = rows.map(rowKey);
  return { rows, keys, indexOf: new Map(keys.map((k, i) => [k, i])) };
}

/** What Göster acts on: the one selected file, or the cursor's file when nothing is selected. */
export function fileToShow<R extends { isDir: boolean }>(view: SavedView, model: ListModel<R>): R | null {
  let key = view.cursor;
  if (view.selection.size > 1) return null;
  for (const only of view.selection) key = only;
  const i = key === null ? undefined : model.indexOf.get(key);
  const r = i === undefined ? undefined : model.rows[i];
  return r && !r.isDir ? r : null;
}

export function selectionSummary(
  model: ListModel<{ size: number; sizeUnknown?: boolean }>,
  selection: Set<string>,
): { count: number; size: number | null } {
  let count = 0;
  let size = 0;
  let unknown = false;
  for (const k of selection) {
    const i = model.indexOf.get(k);
    if (i === undefined) continue;
    count++;
    size += model.rows[i].size;
    unknown ||= !!model.rows[i].sizeUnknown;
  }
  return { count, size: unknown ? null : size };
}

export function toggleSort<V extends SortState>(v: V, key: SortKey): V {
  return v.sortKey === key
    ? { ...v, sortDir: v.sortDir === "asc" ? "desc" : "asc" }
    : { ...v, sortKey: key, sortDir: "asc" };
}

/**
 * A breadcrumb segment; `path` is null on the "…" that stands in for hidden segments.
 * `label` is for display, with invisible characters marked; `path` is the exact one.
 */
export interface Crumb {
  label: string;
  path: string | null;
}

const MAX_CRUMBS = 12;

function capCrumbs(all: Crumb[]): Crumb[] {
  if (all.length <= MAX_CRUMBS) return all;
  return [all[0], { label: "…", path: null }, ...all.slice(-(MAX_CRUMBS - 1))];
}

export function archiveCrumbs(cwd: string): Crumb[] {
  if (cwd === "") return [];
  const out: Crumb[] = [];
  let at = 0;
  for (;;) {
    const end = cwd.indexOf("/", at);
    const stop = end < 0 ? cwd.length : end;
    out.push({ label: safeName(cwd.slice(at, stop)), path: cwd.slice(0, stop) });
    if (end < 0) return capCrumbs(out);
    at = end + 1;
  }
}

export function localCrumbs(path: string): Crumb[] {
  if (path === "") return [];
  const out: Crumb[] = [];
  if (path.startsWith("/")) {
    out.push({ label: "/", path: "/" });
    let cur = "";
    for (const part of path.split("/")) {
      if (!part) continue;
      cur += "/" + part;
      out.push({ label: safeName(part), path: cur });
    }
    return capCrumbs(out);
  }
  const unc = path.startsWith("\\\\");
  let cur = "";
  for (const part of path.split(/[\\/]/)) {
    if (!part) continue;
    if (cur === "") cur = unc ? "\\\\" + part : part + "\\";
    else cur += (cur.endsWith("\\") ? "" : "\\") + part;
    out.push({ label: safeName(part), path: cur });
  }
  return capCrumbs(out);
}

/** The last segment of a local or archive path. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** An archive's name as a tab shows it. */
export const tabLabel = (path: string): string => safeName(baseName(path));

const ARCHIVE_NAME = /\.(rar|zip|7z|tar|tgz|tbz2?|txz|gz|bz2|xz)$/i;

/** For the row icon only. Opening goes by the file's content, not its name. */
export const isArchiveName = (name: string): boolean => ARCHIVE_NAME.test(name);

interface KeyMods {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export type KeyCommand =
  | { kind: "move"; step: 1 | -1; extend: boolean }
  | { kind: "page"; step: 1 | -1; extend: boolean }
  | { kind: "edge"; to: "start" | "end"; extend: boolean }
  | { kind: "open" | "up" | "space" | "all" };

/** What a key does in a file list. A modifier the command has no use for means no command. */
export function keyCommand(e: KeyMods): KeyCommand | null {
  const mod = e.metaKey || e.ctrlKey;
  if (e.altKey) return null;
  if (mod) {
    if (e.shiftKey) return null;
    if (e.key === "ArrowDown") return { kind: "open" };
    if (e.key === "ArrowUp") return { kind: "up" };
    if (e.key === "a" || e.key === "A") return { kind: "all" };
    return null;
  }
  const extend = e.shiftKey;
  switch (e.key) {
    case "ArrowDown":
      return { kind: "move", step: 1, extend };
    case "ArrowUp":
      return { kind: "move", step: -1, extend };
    case "PageDown":
      return { kind: "page", step: 1, extend };
    case "PageUp":
      return { kind: "page", step: -1, extend };
    case "Home":
      return { kind: "edge", to: "start", extend };
    case "End":
      return { kind: "edge", to: "end", extend };
  }
  if (extend) return null;
  if (e.key === "Enter") return { kind: "open" };
  if (e.key === "Backspace") return { kind: "up" };
  if (e.key === " ") return { kind: "space" };
  return null;
}

export function isReloadKey(e: Pick<KeyMods, "key" | "metaKey" | "ctrlKey">): boolean {
  return e.key === "F5" || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r");
}

/** Off the Mac the page handles Ctrl+W itself. Ctrl+Shift+W is Close Window, the menu's. */
export function isCloseTabKey(e: KeyMods, mac: boolean): boolean {
  return !mac && (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "w";
}

export const MIN_PANE_PX = 260;

/** `raw` is what `localStorage` held for `ambar.showHidden`. Off unless it says otherwise. */
export const parseShowHidden = (raw: string | null): boolean => raw === "true";

export const visibleRows = <R extends { hidden: boolean }>(rows: R[], showHidden: boolean): R[] =>
  showHidden ? rows : rows.filter((r) => !r.hidden);

/** A typed `~` or `~/x` under the home folder. `~kim` is left alone, and refused as relative. */
export function expandHome(typed: string, home: string): string {
  return typed === "~" || /^~[\\/]/.test(typed) ? home + typed.slice(1) : typed;
}

/** `raw` is what `localStorage` held for `ambar.split`. */
export function parseSplit(raw: string | null): number {
  const v = raw === null ? NaN : Number(raw);
  return Number.isFinite(v) && v > 0 && v < 1 ? v : 0.5;
}

/** Keeps both panes at their minimum width in a window `width` pixels wide. */
export function clampSplit(ratio: number, width: number): number {
  if (width < MIN_PANE_PX * 2) return 0.5;
  const min = MIN_PANE_PX / width;
  return Math.min(1 - min, Math.max(min, ratio));
}

/** Keeps the columns that fit beside the name, lowest `priority` number first; a
 * narrower column may still fit after a wider one did not. */
export function fitColumns<C extends { width: number; priority: number }>(
  cols: C[],
  available: number,
  minName: number,
): C[] {
  let left = available - minName;
  const kept = new Set<C>();
  for (const c of [...cols].sort((a, b) => a.priority - b.priority)) {
    if (c.width > left) continue;
    left -= c.width;
    kept.add(c);
  }
  return cols.filter((c) => kept.has(c));
}
