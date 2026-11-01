import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ArchiveError, ConflictChoice, ConflictEvent, DoneEvent, ExtractRequest, LocalEntry,
  OpenResult, ProgressEvent,
} from "./types.ts";

/** Errors reject with `{ code, detail }`; anything else becomes a generic `io` error. */
export function toArchiveError(e: unknown): ArchiveError {
  if (e && typeof e === "object" && "code" in e && typeof (e as ArchiveError).code === "string") {
    const x = e as ArchiveError;
    return { code: x.code, detail: typeof x.detail === "string" ? x.detail : "" };
  }
  return { code: "io", detail: String(e) };
}

/** The frontend owns job ids, so it can listen before the job starts. */
export function newJobId(): string {
  return crypto.randomUUID();
}

export interface JobHandlers {
  onProgress?: (ev: ProgressEvent) => void;
  onConflict?: (ev: ConflictEvent) => void;
  onDone: (ev: DoneEvent) => void;
}

const jobs = new Map<string, JobHandlers>();
let listening: Promise<void> | null = null;

/** Registers the three global listeners once. Safe to call repeatedly. */
export function initJobEvents(): Promise<void> {
  if (!listening) {
    listening = (async () => {
      await listen<ProgressEvent>("job-progress", (e) => jobs.get(e.payload.id)?.onProgress?.(e.payload));
      await listen<ConflictEvent>("job-conflict", (e) => jobs.get(e.payload.id)?.onConflict?.(e.payload));
      await listen<DoneEvent>("job-done", (e) => {
        const h = jobs.get(e.payload.id);
        jobs.delete(e.payload.id);
        h?.onDone(e.payload);
      });
    })().catch((err) => {
      listening = null;
      throw err;
    });
  }
  return listening;
}

/** Registers handlers for `id`; await it BEFORE calling startExtract/startTest. The
 * handlers are dropped after `job-done`. Returns an explicit unwatch function. */
export async function watchJob(id: string, handlers: JobHandlers): Promise<() => void> {
  await initJobEvents();
  jobs.set(id, handlers);
  return () => {
    jobs.delete(id);
  };
}

export const openArchive = (path: string, password: string | null = null) =>
  invoke<OpenResult>("open_archive", { path, password });

export const startExtract = (id: string, path: string, password: string | null, req: ExtractRequest) =>
  invoke<void>("start_extract", { id, path, password, req });

export const startTest = (id: string, path: string, password: string | null) =>
  invoke<void>("start_test", { id, path, password });

export const cancelJob = (id: string) => invoke<void>("cancel_job", { id });

export const answerConflict = (id: string, choice: ConflictChoice) =>
  invoke<void>("answer_conflict", { id, choice });

export const forgetPassword = (path: string) => invoke<void>("forget_password", { path });

export const listLocal = (path: string) => invoke<LocalEntry[]>("list_local", { path });

export const homeDir = () => invoke<string>("home_dir");

export const parentDir = (path: string) => invoke<string | null>("parent_dir", { path });

/** Rejects with code `risky_open` for risky types unless `allowRisky`. `quickLook` is macOS only. */
export const openEntry = (
  path: string,
  entry: string,
  password: string | null,
  quickLook: boolean,
  allowRisky: boolean,
) => invoke<void>("open_entry", { path, entry, password, quickLook, allowRisky });
