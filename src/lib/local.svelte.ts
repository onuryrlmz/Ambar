// The left pane's folder. Module state, so it survives the page's `{#key lang}` re-render.

import { homeDir, listLocal, parentDir, toArchiveError } from "./api.ts";
import { goUpFlow } from "./flows.ts";
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
// The code, not its text: the message must follow a language change.
let listError = $state.raw<{ path: string | null; code: string } | null>(null);
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
 * Each call takes its number before its first await, and only the newest may touch
 * state, so a slow Home or Up cannot override a later click.
 */
async function show(mine: number, path: string, cameFrom?: string): Promise<void> {
  try {
    const entries = await listLocal(path);
    if (mine !== latest) return;
    view = core.navigate(view, path, cameFrom);
    rows = entries.map(toRow);
    view = core.pruneSelection(view, model.indexOf);
    listError = null;
  } catch (e) {
    // The folder on screen stays as it was, still usable.
    if (mine === latest) listError = { path, code: toArchiveError(e).code };
  }
}

export const goTo = (path: string, cameFrom?: string): Promise<void> => show(++latest, path, cameFrom);

export async function goHome(): Promise<void> {
  const mine = ++latest;
  try {
    const home = await homeDir();
    if (mine === latest) await show(mine, home);
  } catch (e) {
    if (mine === latest) listError = { path: null, code: toArchiveError(e).code };
  }
}

export async function goUp(): Promise<void> {
  const here = view.cwd;
  if (!here) return;
  await goUpFlow(
    here,
    { current: () => latest, take: () => ++latest },
    {
      parent: parentDir,
      show: (mine, parent, cameFrom) => show(mine, parent, cameFrom),
      fail: (error) => {
        listError = { path: here, code: error.code };
      },
    },
  );
}

export const refreshLocal = () => (view.cwd ? goTo(view.cwd) : goHome());

/** First mount only: a language re-render keeps the folder. */
export function startLocal() {
  if (started) return;
  started = true;
  void goHome();
}
