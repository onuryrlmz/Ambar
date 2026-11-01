// A batch of archives: one job at a time through `runJob`, with everything injected so Node
// can test it. The order comes from the backend's `plan_batch`. The remembered password lives
// in a local of `runBatch` and goes nowhere but the injected calls.

import { isPasswordCode, runJob, type JobDeps, type JobSummary } from "./flows.ts";
import { baseName } from "./tabs-core.ts";
import type { ArchiveError, ConflictChoice } from "./types.ts";

/** The dialog's list stays small however broken an archive is. The counts stay true. */
export const FAILURES_PER_ARCHIVE = 200;

/** One line of the result list: a failed entry, or a whole archive when `path` is null. */
export interface BatchRow {
  archive: string;
  path: string | null;
  error: ArchiveError | null;
  /** Its password prompt was closed, so it never ran. */
  dismissed?: boolean;
  /** Stands for this many failures of the archive that are not listed. */
  more?: number;
}

export interface BatchSummary {
  kind: "ok" | "partial" | "failed" | "cancelled";
  total: number;
  okArchives: number;
  failedArchives: number;
  dismissedArchives: number;
  /** Cancelled under way, or never started. */
  notFinished: number;
  ok: number;
  skipped: number;
  /** Failed entries, plus archives that failed as a whole. */
  failed: number;
  rows: BatchRow[];
  mayHaveWritten: boolean;
}

export interface BatchDeps extends Pick<JobDeps, "newId" | "watch" | "cancel" | "answer" | "askConflict" | "onProgress"> {
  start: (id: string, path: string, password: string | null) => Promise<void>;
  /** Asked before a job starts, so a run never has to be repeated for a first password. */
  needsPassword: (path: string) => Promise<boolean>;
  /** Resolves to null when closed. */
  askPassword: (name: string, wrong: boolean) => Promise<string | null>;
  /** `index` counts from 1. */
  onArchive?: (index: number, total: number, name: string) => void;
  onControl?: (control: { cancel: () => void }) => void;
}

const passwordTrouble = (s: JobSummary): boolean =>
  !!s.dismissed || (s.error !== null && isPasswordCode(s.error.code)) || s.failures.some((f) => isPasswordCode(f.error.code));

/** Runs the archives in the given order. A failure is that archive's alone; İptal ends the run. */
export async function runBatch(paths: string[], deps: BatchDeps): Promise<BatchSummary> {
  const out: BatchSummary = {
    kind: "ok", total: paths.length, okArchives: 0, failedArchives: 0, dismissedArchives: 0,
    notFinished: 0, ok: 0, skipped: 0, failed: 0, rows: [], mayHaveWritten: false,
  };
  let cancelled = false;
  let cancelCurrent: (() => void) | null = null;
  deps.onControl?.({
    cancel: () => {
      cancelled = true;
      cancelCurrent?.();
    },
  });
  const applyAll: { choice: ConflictChoice | null } = { choice: null };
  // What the user last typed and no archive has turned down: parts of one set share it.
  let remembered: string | null = null;

  for (const [i, path] of paths.entries()) {
    if (cancelled) break;
    const name = baseName(path);
    deps.onArchive?.(i + 1, paths.length, name);
    const locked = await deps.needsPassword(path).catch(() => false); // the job reports the real trouble
    if (cancelled) break;
    let typed: string | null = null;
    let first: string | null = null;
    if (locked) {
      first = remembered ?? (typed = await deps.askPassword(name, false));
      if (first === null) {
        out.dismissedArchives++;
        out.rows.push({ archive: name, path: null, error: null, dismissed: true });
        continue;
      }
    }
    const summary = await runJob({
      newId: deps.newId,
      watch: deps.watch,
      cancel: deps.cancel,
      answer: deps.answer,
      askConflict: deps.askConflict,
      onProgress: deps.onProgress,
      start: (id, password) => deps.start(id, path, password),
      initialPassword: first,
      applyAll,
      askPassword: async (wrong) => {
        const given = await deps.askPassword(name, wrong);
        if (given !== null) typed = given;
        return given;
      },
      onControl: (control) => {
        cancelCurrent = control.cancel;
      },
    });
    cancelCurrent = null;
    if (passwordTrouble(summary)) remembered = null;
    else if (typed !== null) remembered = typed;

    if (summary.dismissed) {
      out.dismissedArchives++;
      out.rows.push({ archive: name, path: null, error: null, dismissed: true });
      continue;
    }
    out.ok += summary.ok;
    out.skipped += summary.skipped;
    out.mayHaveWritten ||= summary.mayHaveWritten;
    if (summary.kind === "cancelled") {
      cancelled = true;
      break;
    }
    if (summary.kind === "ok") {
      out.okArchives++;
      continue;
    }
    out.failedArchives++;
    if (summary.error) {
      out.failed++;
      out.rows.push({ archive: name, path: null, error: summary.error });
    }
    out.failed += summary.failures.length;
    for (const f of summary.failures.slice(0, FAILURES_PER_ARCHIVE)) {
      out.rows.push({ archive: name, path: f.path, error: f.error });
    }
    if (summary.failures.length > FAILURES_PER_ARCHIVE) {
      out.rows.push({ archive: name, path: null, error: null, more: summary.failures.length - FAILURES_PER_ARCHIVE });
    }
  }

  out.notFinished = out.total - out.okArchives - out.failedArchives - out.dismissedArchives;
  if (cancelled) out.kind = "cancelled";
  else if (out.failedArchives === 0 && out.dismissedArchives === 0) out.kind = "ok";
  else out.kind = out.ok > 0 || out.skipped > 0 || out.okArchives > 0 ? "partial" : "failed";
  return out;
}
