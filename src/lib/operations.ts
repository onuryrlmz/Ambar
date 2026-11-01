// The flows, connected to the real backend, the dialogs and the panes.

import {
  answerConflict, archiveNeedsPassword, cancelJob, newJobId, openEntry, openLocal, planBatch, quickLookLocal, runKey,
  startExtract, startTest, watchJob,
} from "./api.ts";
import { runBatch } from "./batch.ts";
import {
  askBatchExtract, askConflict, askExtract, askPassword, beginProgress, confirmRisky, endProgress, setProgressArchive,
  setProgressControl, setProgressEvent, showBatchResult, showResult, type ProgressState,
} from "./dialogs/store.svelte.ts";
import {
  extractTarget, localFileGoesToItsApp, openEntryWithPrompts, openLocalWithPrompt, pathsEqual, runJob, safeName, toArchiveError, type JobSummary,
  type OpenFailure,
} from "./flows.ts";
import { localView, refreshLocal } from "./local.svelte.ts";
import { baseName, type Tab } from "./tabs-core.ts";
import { openTab, setOpenFailures } from "./tabs.svelte.ts";
import type { BatchPlan, ExtractRequest, TreeNode } from "./types.ts";

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

/** Çıkart with files selected on the left: all of them, one archive at a time. */
export async function extractBatchWithDialog(paths: string[]): Promise<void> {
  setOpenFailures([]);
  let plan: BatchPlan;
  try {
    plan = await planBatch(paths);
  } catch (e) {
    setOpenFailures([{ name: baseName(paths[0]), code: toArchiveError(e).code }]);
    return;
  }
  const choice = await askBatchExtract(plan.archives.length, localView().cwd);
  if (!choice) return;
  const req: ExtractRequest = {
    dest: choice.dest,
    ...extractTarget("", [], "folder"),
    keepPaths: choice.keepPaths,
    overwrite: choice.overwrite,
    keepBroken: choice.keepBroken,
    protected: plan.protected,
  };
  const folders = new Map(plan.archives.map((a) => [a.path, a.folder]));
  const ordered = plan.archives.map((a) => a.path);
  beginProgress("extract", baseName(ordered[0]));
  try {
    const summary = await runBatch(ordered, {
      newId: newJobId,
      watch: watchJob,
      start: (id, path, password) =>
        startExtract(id, path, password, req, choice.separate ? { name: folders.get(path) ?? null } : null),
      cancel: cancelJob,
      answer: answerConflict,
      needsPassword: archiveNeedsPassword,
      runKey,
      askPassword,
      askConflict,
      onProgress: setProgressEvent,
      onArchive: setProgressArchive,
      onControl: setProgressControl,
    }, plan.joined);
    endProgress();
    // Even a failed or cancelled run may have written files.
    if (pathsEqual(choice.dest, localView().cwd)) void refreshLocal();
    await showBatchResult(summary);
  } finally {
    endProgress();
  }
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
  return showResult("open", name, { kind: "failed", ok: 0, skipped: 0, failures: [], decrypted: 0, error, mayHaveWritten: false });
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
  if (!localFileGoesToItsApp(name, r.failure?.code)) {
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

/** Space on a local file on the Mac. */
export async function previewLocalFile(path: string): Promise<void> {
  setOpenFailures([]);
  try {
    await quickLookLocal(path);
  } catch (e) {
    setOpenFailures([{ name: baseName(path), code: toArchiveError(e).code }]);
  }
}
