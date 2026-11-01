// The open archives. Module state, so it survives the page's `{#key lang}` re-render.

import { openArchive, toArchiveError } from "./api.ts";
import { sortNodes } from "./sort.ts";
import * as core from "./tabs-core.ts";
import { childrenOf } from "./tree.ts";

let state = $state.raw(core.emptyTabs());
// The code, not its text: the banner must follow a language change.
let openError = $state.raw<{ name: string; code: string } | null>(null);

const active = $derived(state.tabs.find((t) => t.id === state.activeId) ?? null);

// Read one by one: selecting a row replaces the tab object, and that must not re-sort.
const index = $derived(active?.index ?? null);
const cwd = $derived(active?.cwd ?? "");
const sortKey = $derived(active?.sortKey ?? "name");
const sortDir = $derived(active?.sortDir ?? "asc");
const model = $derived(core.listModel(index ? sortNodes(childrenOf(index, cwd), sortKey, sortDir) : []));

export const allTabs = () => state.tabs;
export const activeTab = () => active;
export const activeModel = () => model;
export const archiveError = () => openError;
export const dismissArchiveError = () => {
  openError = null;
};

export function focusTab(id: number) {
  state = core.focusTab(state, id);
}

export function closeTab(id: number) {
  state = core.closeTab(state, id);
}

/** Replaces the active tab, as `f` returns it. */
export function updateActive(f: (t: core.Tab) => core.Tab) {
  if (active) state = core.updateTab(state, active.id, f);
}

/**
 * Opens `path` in a tab, or focuses the tab it is already in. Resolves to whether the
 * archive is now showing. On failure the tab goes away and the error shows in the banner.
 */
export async function openTab(path: string, password: string | null = null): Promise<boolean> {
  const before = state.activeId;
  const existing = state.tabs.find((t) => t.path === path);
  state = core.openTab(state, path);
  if (existing) return !existing.loading;
  const tab = state.tabs[state.tabs.length - 1];
  openError = null;
  try {
    const res = await openArchive(path, password);
    state = core.openedTab(state, tab.id, res);
    return state.activeId === tab.id;
  } catch (e) {
    // Closed while loading: nobody is waiting for this archive any more.
    if (!state.tabs.some((t) => t.id === tab.id)) return false;
    const wasActive = state.activeId === tab.id;
    state = core.closeTab(state, tab.id);
    if (wasActive && before !== null) state = core.focusTab(state, before);
    openError = { name: core.baseName(path), code: toArchiveError(e).code };
    return false;
  }
}
