// The left pane's folder. Module state, so it survives the page's `{#key lang}` re-render.

import { homeDir, listLocal, parentDir, toArchiveError } from "./api.ts";
import { errorText, t } from "./i18n/index.ts";
import { sortNodes } from "./sort.ts";
import * as core from "./tabs-core.ts";
import type { LocalEntry, SortKey, TreeNode } from "./types.ts";

export interface LocalRow extends TreeNode {
  hidden: boolean;
  isSymlink: boolean;
}

const toRow = (e: LocalEntry): LocalRow => ({
  name: e.name, path: e.path, isDir: e.isDir, size: e.size, packed: null, modified: e.modified,
  encrypted: false, crc: null, hidden: e.hidden, isSymlink: e.isSymlink,
});

let view = $state.raw(core.newView(""));
let rows = $state.raw<LocalRow[]>([]);
let sort = $state.raw<core.SortState>({ sortKey: "name", sortDir: "asc" });
let listError = $state.raw<string | null>(null);
let started = false;
let latest = 0;

const model = $derived(core.listModel(sortNodes(rows, sort.sortKey, sort.sortDir)));

export const localView = () => view;
export const localModel = () => model;
export const localSort = () => sort;
export const localError = () => listError;
export const dismissLocalError = () => {
  listError = null;
};
export const setLocalView = (v: core.PaneView) => {
  view = v;
};
export const toggleLocalSort = (key: SortKey) => {
  sort = core.toggleSort(sort, key);
};

/**
 * Shows `path`. If it cannot be listed the error shows in the pane, and the folder on
 * screen stays as it was, still usable. Only the newest of overlapping calls wins.
 */
export async function goTo(path: string, cameFrom?: string): Promise<void> {
  const mine = ++latest;
  try {
    const entries = await listLocal(path);
    if (mine !== latest) return;
    view = core.navigate(view, path, cameFrom);
    rows = entries.map(toRow);
    view = core.pruneSelection(view, model.indexOf);
    listError = null;
  } catch (e) {
    if (mine !== latest) return;
    listError = t("pane.listFailed", { name: path, message: errorText(toArchiveError(e).code) });
  }
}

export async function goHome(): Promise<void> {
  try {
    await goTo(await homeDir());
  } catch (e) {
    listError = errorText(toArchiveError(e).code);
  }
}

export async function goUp(): Promise<void> {
  const here = view.cwd;
  if (!here) return;
  const parent = await parentDir(here);
  if (parent !== null) await goTo(parent, here);
}

export const refreshLocal = () => (view.cwd ? goTo(view.cwd) : goHome());

/** First mount only: a language re-render keeps the folder. */
export function startLocal() {
  if (started) return;
  started = true;
  void goHome();
}
