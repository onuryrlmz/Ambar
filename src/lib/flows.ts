// The password, job and open sequences, with everything they touch injected, so Node can
// test them. Passwords live in local variables of these functions, and go nowhere but the
// injected call: nothing here returns or stores one.

import { safeName } from "./names.ts";
import { isArchiveName, selectedPaths } from "./tabs-core.ts";
import type {
  ArchiveError, ConflictChoice, ConflictEvent, DoneEvent, FileFailure, OpenResult, OpReport,
  ProgressEvent,
} from "./types.ts";

export { safeName, safeText } from "./names.ts";

/** Errors reject with `{ code, detail }`; anything else becomes a generic `io` error. */
export function toArchiveError(e: unknown): ArchiveError {
  if (e && typeof e === "object" && "code" in e && typeof (e as ArchiveError).code === "string") {
    const x = e as ArchiveError;
    return { code: x.code, detail: typeof x.detail === "string" ? x.detail : "" };
  }
  return { code: "io", detail: String(e) };
}

const isPasswordCode = (code: string) => code === "password_required" || code === "wrong_password";

export type OpenOutcome =
  | { kind: "opened"; result: OpenResult }
  | { kind: "cancelled" }
  | { kind: "failed"; error: ArchiveError };

export interface OpenDeps {
  open: (path: string, password: string | null) => Promise<OpenResult>;
  /** Resolves to null on cancel. */
  askPassword: (path: string, wrong: boolean) => Promise<string | null>;
}

/** Tries with no password first, so the backend's session cache gets its chance. */
export async function openWithPassword(path: string, deps: OpenDeps): Promise<OpenOutcome> {
  let password: string | null = null;
  for (;;) {
    try {
      return { kind: "opened", result: await deps.open(path, password) };
    } catch (e) {
      const error = toArchiveError(e);
      if (!isPasswordCode(error.code)) return { kind: "failed", error };
      password = await deps.askPassword(path, error.code === "wrong_password");
      if (password === null) return { kind: "cancelled" };
    }
  }
}

export interface JobSummary {
  kind: "ok" | "partial" | "failed" | "cancelled";
  ok: number;
  skipped: number;
  failures: FileFailure[];
  /** The job-level error, when it never produced a report. */
  error: ArchiveError | null;
  /** Files of this job may be in the destination, whatever `kind` says. */
  mayHaveWritten: boolean;
  /** The user closed a password prompt and nothing came of the job: no result to show. */
  dismissed?: boolean;
}

/** Any failure rules out `ok`: a failed operation is never reported as success. */
export function summarizeReport(report: OpReport): JobSummary {
  const failures = report.failed;
  const kind = failures.length === 0 ? "ok" : report.ok === 0 ? "failed" : "partial";
  return { kind, ok: report.ok, skipped: report.skipped, failures, error: null, mayHaveWritten: report.ok > 0 };
}

export function summarizeDone(done: Pick<DoneEvent, "report" | "error">): JobSummary {
  if (done.report) return summarizeReport(done.report);
  const nothing = { ok: 0, skipped: 0, failures: [], mayHaveWritten: false };
  if (done.error?.code === "cancelled") return { kind: "cancelled", ...nothing, error: null };
  return { kind: "failed", ...nothing, error: done.error ?? { code: "io", detail: "" } };
}

export interface ConflictAnswer {
  choice: ConflictChoice;
  applyAll: boolean;
}

export interface JobHandlers {
  onProgress?: (ev: ProgressEvent) => void;
  onConflict?: (ev: ConflictEvent) => void;
  onDone: (ev: DoneEvent) => void;
}

export interface JobDeps {
  newId: () => string;
  watch: (id: string, handlers: JobHandlers) => Promise<() => void>;
  start: (id: string, password: string | null) => Promise<void>;
  cancel: (id: string) => Promise<void>;
  answer: (id: string, choice: ConflictChoice) => Promise<void>;
  askPassword: (wrong: boolean) => Promise<string | null>;
  askConflict: (path: string) => Promise<ConflictAnswer>;
  onProgress?: (ev: ProgressEvent) => void;
  /** Hands over what the progress dialog's İptal calls. */
  onControl?: (control: { cancel: () => void }) => void;
}

/** Runs one job to its end, prompting and re-running on a password error. */
export async function runJob(deps: JobDeps): Promise<JobSummary> {
  let password: string | null = null;
  let applyAll: ConflictChoice | null = null;
  let cancelled = false;
  let currentId: string | null = null;
  let progressed = false;
  // The last run that got files out, before a re-run for a password.
  let gotOut: OpReport | null = null;

  const sendCancel = () => {
    if (currentId !== null) void deps.cancel(currentId).catch(() => {});
  };
  deps.onControl?.({
    cancel: () => {
      cancelled = true;
      sendCancel();
    },
  });

  const conflict = async (id: string, path: string) => {
    let choice: ConflictChoice;
    if (cancelled) {
      choice = "cancel";
    } else if (applyAll) {
      choice = applyAll;
    } else {
      const a = await deps.askConflict(path);
      choice = a.choice;
      if (a.applyAll && choice !== "cancel") applyAll = choice;
    }
    try {
      await deps.answer(id, choice);
    } catch {
      // Unanswered, the job would wait forever.
      void deps.cancel(id).catch(() => {});
    }
  };

  const once = (given: string | null) =>
    new Promise<Pick<DoneEvent, "report" | "error">>((resolve) => {
      const id = deps.newId();
      currentId = id;
      let unwatch = () => {};
      deps
        .watch(id, {
          onProgress: (ev) => {
            progressed = true;
            deps.onProgress?.(ev);
          },
          onConflict: (ev) => void conflict(id, ev.path),
          onDone: resolve,
        })
        .then((u) => {
          unwatch = u;
          return deps.start(id, given);
        })
        .then(() => {
          // The backend ignores a cancel for a job it has not registered yet.
          if (cancelled) sendCancel();
        })
        .catch((e) => {
          unwatch();
          resolve({ report: null, error: toArchiveError(e) });
        });
    });

  for (;;) {
    progressed = false;
    const done = await once(password);
    if (done.report && done.report.ok > 0) gotOut = done.report;
    const code = done.error?.code ?? done.report?.failed.find((f) => isPasswordCode(f.error.code))?.error.code;
    if (code === undefined || !isPasswordCode(code) || cancelled) {
      const summary = summarizeDone(done);
      return { ...summary, mayHaveWritten: summary.mayHaveWritten || progressed || gotOut !== null };
    }
    password = await deps.askPassword(password !== null || code === "wrong_password");
    if (password === null) {
      // What an earlier run did get out still has to be reported.
      if (gotOut) return summarizeReport(gotOut);
      return { ...summarizeDone({ report: null, error: { code: "cancelled", detail: "" } }), dismissed: true };
    }
  }
}

export interface EntryDeps {
  open: (password: string | null, allowRisky: boolean) => Promise<void>;
  askPassword: (wrong: boolean) => Promise<string | null>;
  confirmRisky: () => Promise<boolean>;
}

export type ActOutcome = { kind: "opened" } | { kind: "cancelled" } | { kind: "failed"; error: ArchiveError };

/** Opening a file inside an archive: a risky type needs an explicit yes, once. */
export async function openEntryWithPrompts(deps: EntryDeps): Promise<ActOutcome> {
  let password: string | null = null;
  let allowRisky = false;
  for (;;) {
    try {
      await deps.open(password, allowRisky);
      return { kind: "opened" };
    } catch (e) {
      const error = toArchiveError(e);
      if (error.code === "risky_open" && !allowRisky) {
        if (!(await deps.confirmRisky())) return { kind: "cancelled" };
        allowRisky = true;
      } else if (isPasswordCode(error.code)) {
        password = await deps.askPassword(error.code === "wrong_password");
        if (password === null) return { kind: "cancelled" };
      } else {
        return { kind: "failed", error };
      }
    }
  }
}

/**
 * After `open_archive` on a local file: whether it goes to its default app. Only a plain
 * non-archive does. Text named `sahte.rar` would otherwise go to whatever handles `.rar`,
 * and the user asked for an archive, so they hear that it is not one.
 */
export const localFileGoesToItsApp = (name: string, code: string | undefined): boolean =>
  code === "not_archive" && !isArchiveName(name);

/** Opening a local file with its default app. */
export async function openLocalWithPrompt(deps: {
  open: (allowRisky: boolean) => Promise<void>;
  confirmRisky: () => Promise<boolean>;
}): Promise<ActOutcome> {
  let allowRisky = false;
  for (;;) {
    try {
      await deps.open(allowRisky);
      return { kind: "opened" };
    } catch (e) {
      const error = toArchiveError(e);
      if (error.code !== "risky_open" || allowRisky) return { kind: "failed", error };
      if (!(await deps.confirmRisky())) return { kind: "cancelled" };
      allowRisky = true;
    }
  }
}

export type ExtractScope = "selected" | "folder";

/**
 * What an extraction takes. The backend reads an empty selection as the whole archive,
 * which is what "all" means only at the root. In a subfolder, it is that folder.
 */
export function extractTarget(
  cwd: string,
  selection: Iterable<string>,
  scope: ExtractScope,
): { selection: string[]; base: string } {
  const picked = scope === "selected" ? selectedPaths(selection) : [];
  if (picked.length > 0) return { selection: picked, base: cwd };
  return { selection: cwd === "" ? [] : [cwd], base: cwd };
}

/** A null percent means an indeterminate bar: the total is not known. */
export function progressView(ev: ProgressEvent | null): {
  current: string;
  done: number;
  total: number;
  percent: number | null;
} {
  if (!ev) return { current: "", done: 0, total: 0, percent: null };
  const percent = ev.totalBytes > 0 ? Math.min(100, Math.floor((ev.doneBytes / ev.totalBytes) * 100)) : null;
  return { current: ev.current, done: ev.doneBytes, total: ev.totalBytes, percent };
}

const trimSeparators = (p: string) => (/^[\\/]+$/.test(p) ? p : p.replace(/[\\/]+$/, ""));

export const pathsEqual = (a: string, b: string): boolean => trimSeparators(a) === trimSeparators(b);

export interface OpenFailure {
  name: string;
  code: string;
}

export type OpenFailureText =
  | { kind: "one"; name: string; code: string }
  | { kind: "many"; n: number; names: string; code: string | null };

const NAMES_SHOWN = 4;

/** One banner, however many files a drop or a multi-open could not open. */
export function describeOpenFailures(list: OpenFailure[]): OpenFailureText | null {
  if (list.length === 0) return null;
  if (list.length === 1) return { kind: "one", name: safeName(list[0].name), code: list[0].code };
  const shown = list.slice(0, NAMES_SHOWN).map((f) => safeName(f.name));
  if (list.length > NAMES_SHOWN) shown.push("…");
  const same = list.every((f) => f.code === list[0].code);
  return { kind: "many", n: list.length, names: shown.join(", "), code: same ? list[0].code : null };
}

/**
 * Up one folder. Only a real move takes a navigation number: a root has no parent, and
 * that must not outrank a navigation in flight. A navigation that started during the
 * lookup wins over it, success or failure.
 */
export async function goUpFlow(
  here: string,
  seq: { current: () => number; take: () => number },
  deps: {
    parent: (path: string) => Promise<string | null>;
    show: (mine: number, parent: string, cameFrom: string) => Promise<void>;
    fail: (error: ArchiveError) => void;
  },
): Promise<void> {
  const seen = seq.current();
  let parent: string | null;
  try {
    parent = await deps.parent(here);
  } catch (e) {
    if (seen === seq.current()) deps.fail(toArchiveError(e));
    return;
  }
  if (parent !== null && seen === seq.current()) await deps.show(seq.take(), parent, here);
}
