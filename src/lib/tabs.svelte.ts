// The open archives. Module state, so it survives the page's `{#key lang}` re-render.

import { openArchive } from "./api.ts";
import { askPassword } from "./dialogs/store.svelte.ts";
import { describeOpenFailures, openWithPassword, type OpenFailure } from "./flows.ts";
import { sortNodes } from "./sort.ts";
import * as core from "./tabs-core.ts";
import { childrenOf } from "./tree.ts";

let state = $state.raw(core.emptyTabs());
// Codes, not their text: the banner must follow a language change.
let openFailures = $state.raw<OpenFailure[]>([]);

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
export const archiveError = () => describeOpenFailures(openFailures);
export const setOpenFailures = (list: OpenFailure[]) => {
  openFailures = list;
};
export const dismissArchiveError = () => {
  openFailures = [];
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
 * Opens `path` in a tab, or focuses the tab it is already in, prompting for a password when
 * the archive needs one. `showing` says the archive is now on screen. On failure or a
 * cancelled prompt the tab goes away; the caller decides what to do with `failure`.
 */
export async function openTab(path: string): Promise<{ showing: boolean; failure: OpenFailure | null }> {
  const before = state.activeId;
  const existing = state.tabs.find((t) => t.path === path);
  state = core.openTab(state, path);
  if (existing) return { showing: !existing.loading, failure: null };
  const tab = state.tabs[state.tabs.length - 1];
  const alive = () => state.tabs.some((t) => t.id === tab.id);

  const outcome = await openWithPassword(path, {
    open: openArchive,
    // Closed while loading: nobody is waiting for this archive any more.
    askPassword: (p, wrong) => (alive() ? askPassword(core.baseName(p), wrong) : Promise.resolve(null)),
  });

  if (outcome.kind === "opened") {
    state = core.openedTab(state, tab.id, outcome.result);
    return { showing: state.activeId === tab.id, failure: null };
  }
  if (!alive()) return { showing: false, failure: null };
  const wasActive = state.activeId === tab.id;
  state = core.closeTab(state, tab.id);
  if (wasActive && before !== null) state = core.focusTab(state, before);
  if (outcome.kind === "cancelled") return { showing: false, failure: null };
  return { showing: false, failure: { name: core.baseName(path), code: outcome.error.code } };
}
