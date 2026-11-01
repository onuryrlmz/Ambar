import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toArchiveError } from "./flows.ts";
import type {
  BatchPlan, ConflictChoice, ConflictEvent, DoneEvent, ExtractRequest, LocalEntry,
  OpenResult, ProgressEvent,
} from "./types.ts";

export { toArchiveError };

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
      const undo: (() => void)[] = [];
      try {
        undo.push(await listen<ProgressEvent>("job-progress", (e) => jobs.get(e.payload.id)?.onProgress?.(e.payload)));
        undo.push(await listen<ConflictEvent>("job-conflict", (e) => jobs.get(e.payload.id)?.onConflict?.(e.payload)));
        undo.push(await listen<DoneEvent>("job-done", (e) => {
          const h = jobs.get(e.payload.id);
          jobs.delete(e.payload.id);
          h?.onDone(e.payload);
        }));
      } catch (err) {
        for (const u of undo) u(); // a retry must not double-fire
        throw err;
      }
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

/** `ownFolder` puts this archive in its own folder under `req.dest`, made by the backend. */
export const startExtract = (
  id: string,
  path: string,
  password: string | null,
  req: ExtractRequest,
  ownFolder: { name: string | null } | null = null,
) => invoke<void>("start_extract", { id, path, password, req, ownFolder });

export const planBatch = (paths: string[]) => invoke<BatchPlan>("plan_batch", { paths });

/** What identifies this archive's run, or null while its header stays locked. */
export const runKey = (path: string, password: string | null) => invoke<string | null>("run_key", { path, password });

export const archiveNeedsPassword = (path: string) => invoke<boolean>("archive_needs_password", { path });

export const startTest = (id: string, path: string, password: string | null) =>
  invoke<void>("start_test", { id, path, password });

export const cancelJob = (id: string) => invoke<void>("cancel_job", { id });

export const answerConflict = (id: string, choice: ConflictChoice) =>
  invoke<void>("answer_conflict", { id, choice });

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

/** Opens a local file with its default app. Rejects with `risky_open` like `openEntry`. */
export const openLocal = (path: string, allowRisky: boolean) => invoke<void>("open_local", { path, allowRisky });

/** Previews a local file with Quick Look, never launching it. macOS only. */
export const quickLookLocal = (path: string) => invoke<void>("quick_look_local", { path });

/** Rebuilds the native menu in this language, with its hidden-files check mark. */
export const setMenuLanguage = (lang: string, showHidden: boolean) =>
  invoke<void>("set_menu_language", { lang, showHidden });
