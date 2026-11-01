// The flows, connected to the real backend, the dialogs and the panes.

import {
  answerConflict, cancelJob, newJobId, openEntry, openLocal, startExtract, startTest, watchJob,
} from "./api.ts";
import {
  askConflict, askExtract, askPassword, beginProgress, confirmRisky, endProgress, setProgressControl,
  setProgressEvent, showResult, type ProgressState,
} from "./dialogs/store.svelte.ts";
import {
  extractTarget, openEntryWithPrompts, openLocalWithPrompt, pathsEqual, runJob, safeName, type JobSummary,
  type OpenFailure,
} from "./flows.ts";
import { localView, refreshLocal } from "./local.svelte.ts";
import { baseName, type Tab } from "./tabs-core.ts";
import { openTab, setOpenFailures } from "./tabs.svelte.ts";
import type { ExtractRequest, TreeNode } from "./types.ts";

async function job(
  op: ProgressState["op"],
  path: string,
  start: (id: string, password: string | null) => Promise<void>,
): Promise<JobSummary> {
  const name = baseName(path);
  beginProgress(op, name);
  try {
    return await runJob({
      newId: newJobId,
      watch: watchJob,
      start,
      cancel: cancelJob,
      answer: answerConflict,
      askPassword: (wrong) => askPassword(name, wrong),
      askConflict,
      onProgress: setProgressEvent,
      onControl: setProgressControl,
    });
  } finally {
    endProgress();
  }
}

async function runExtract(path: string, req: ExtractRequest): Promise<void> {
  const summary = await job("extract", path, (id, password) => startExtract(id, path, password, req));
  if (summary.dismissed) return;
  // Even a failed or cancelled run may have written files.
  if (pathsEqual(req.dest, localView().cwd)) void refreshLocal();
  await showResult("extract", baseName(path), summary);
}

/** Çıkart. With nothing selected, the dialog says that all of the current folder goes. */
export async function extractWithDialog(tab: Tab): Promise<void> {
  const choice = await askExtract(tab.cwd, tab.selection.size, localView().cwd);
  if (!choice) return;
  await runExtract(tab.path, {
    dest: choice.dest,
    ...extractTarget(tab.cwd, tab.selection, choice.scope),
    keepPaths: choice.keepPaths,
    overwrite: choice.overwrite,
    keepBroken: choice.keepBroken,
  });
}

/** Rows dragged to the left pane: its folder, with the Ask policy. */
export async function extractDragged(tab: Tab, dest: string): Promise<void> {
  await runExtract(tab.path, {
    dest,
    ...extractTarget(tab.cwd, tab.selection, "selected"),
    keepPaths: true,
    overwrite: "ask",
    keepBroken: false,
  });
}

/** Sına. */
export async function testArchive(tab: Tab): Promise<void> {
  const summary = await job("test", tab.path, (id, password) => startTest(id, tab.path, password));
  if (!summary.dismissed) await showResult("test", baseName(tab.path), summary);
}

function openFailed(name: string, error: JobSummary["error"]) {
  return showResult("open", name, { kind: "failed", ok: 0, skipped: 0, failures: [], error, mayHaveWritten: false });
}

/** Göster, double-click, and Space on the Mac (`quickLook`). */
export async function showEntry(tab: Tab, row: TreeNode, quickLook: boolean): Promise<void> {
  const name = safeName(row.name);
  const outcome = await openEntryWithPrompts({
    open: (password, allowRisky) => openEntry(tab.path, row.path, password, quickLook, allowRisky),
    askPassword: (wrong) => askPassword(baseName(tab.path), wrong),
    confirmRisky: () => confirmRisky(name),
  });
  if (outcome.kind === "failed") await openFailed(name, outcome.error);
}

/**
 * Opens each path that is an archive, in order, and puts every failure in one banner.
 * `quiet` leaves out the files that are simply not archives. True if any is on screen.
 */
export async function openPaths(paths: string[], quiet = false): Promise<boolean> {
  setOpenFailures([]);
  const failures: OpenFailure[] = [];
  let showing = false;
  for (const path of paths) {
    const r = await openTab(path);
    showing ||= r.showing;
    if (r.failure && !(quiet && r.failure.code === "not_archive")) failures.push(r.failure);
  }
  setOpenFailures(failures);
  return showing;
}

/** Enter or double-click in the left pane: an archive opens here, anything else in its own app. */
export async function openLocalFile(path: string): Promise<boolean> {
  setOpenFailures([]);
  const name = baseName(path);
  const r = await openTab(path);
  if (r.failure?.code !== "not_archive") {
    if (r.failure) setOpenFailures([r.failure]);
    return r.showing;
  }
  const outcome = await openLocalWithPrompt({
    open: (allowRisky) => openLocal(path, allowRisky),
    confirmRisky: () => confirmRisky(safeName(name)),
  });
  if (outcome.kind === "failed") setOpenFailures([{ name, code: outcome.error.code }]);
  return false;
}
