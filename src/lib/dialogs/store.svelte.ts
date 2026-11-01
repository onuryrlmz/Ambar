// The one modal the window can show. Prompts wait in a queue and go in front of the
// progress dialog, which comes back when they are answered.

import type { ConflictAnswer, ExtractScope, JobSummary } from "../flows.ts";
import type { ArchiveInfo, OverwritePolicy, ProgressEvent } from "../types.ts";

export interface ExtractChoice {
  dest: string;
  scope: ExtractScope;
  keepPaths: boolean;
  keepBroken: boolean;
  overwrite: OverwritePolicy;
}

export type ResultOp = "extract" | "test" | "open";

export type Dialog =
  | { kind: "password"; name: string; wrong: boolean; done: (v: string | null) => void }
  | {
      kind: "extract";
      cwd: string;
      selectedCount: number;
      dest: string;
      done: (v: ExtractChoice | null) => void;
    }
  | { kind: "conflict"; path: string; done: (v: ConflictAnswer) => void }
  | { kind: "risky"; name: string; done: (v: boolean) => void }
  | { kind: "result"; op: ResultOp; name: string; summary: JobSummary; done: (v: void) => void }
  | { kind: "info"; path: string; info: ArchiveInfo; done: (v: void) => void }
  | { kind: "about"; done: (v: void) => void };

/** Everything about a dialog but its `done`, which `ask` supplies. */
type Request<D> = D extends { done: unknown } ? Omit<D, "done"> : never;
type Answer<D> = D extends { done: (v: infer V) => void } ? V : never;

export interface ProgressState {
  op: "extract" | "test";
  name: string;
  ev: ProgressEvent | null;
  cancelling: boolean;
  cancel: (() => void) | null;
}

let queue = $state.raw<Dialog[]>([]);
let progress = $state.raw<ProgressState | null>(null);

export const currentDialog = (): Dialog | null => queue[0] ?? null;
export const currentProgress = (): ProgressState | null => progress;
export const modalOpen = (): boolean => queue.length > 0 || progress !== null;

function ask<K extends Dialog["kind"]>(
  request: Request<Extract<Dialog, { kind: K }>>,
): Promise<Answer<Extract<Dialog, { kind: K }>>> {
  return new Promise((resolve) => {
    const dialog = {
      ...request,
      done: (v: Answer<Extract<Dialog, { kind: K }>>) => {
        queue = queue.filter((d) => d !== dialog);
        resolve(v);
      },
    } as unknown as Dialog;
    queue = [...queue, dialog];
  });
}

export const askPassword = (name: string, wrong: boolean) => ask<"password">({ kind: "password", name, wrong });

export const askExtract = (cwd: string, selectedCount: number, dest: string) =>
  ask<"extract">({ kind: "extract", cwd, selectedCount, dest });

export const askConflict = (path: string) => ask<"conflict">({ kind: "conflict", path });

export const confirmRisky = (name: string) => ask<"risky">({ kind: "risky", name });

export const showResult = (op: ResultOp, name: string, summary: JobSummary) =>
  ask<"result">({ kind: "result", op, name, summary });

export const showInfo = (path: string, info: ArchiveInfo) => ask<"info">({ kind: "info", path, info });

export const showAbout = () => ask<"about">({ kind: "about" });

export function beginProgress(op: ProgressState["op"], name: string) {
  progress = { op, name, ev: null, cancelling: false, cancel: null };
}

export function setProgressEvent(ev: ProgressEvent) {
  if (progress) progress = { ...progress, ev };
}

export function setProgressControl(control: { cancel: () => void }) {
  if (progress) progress = { ...progress, cancel: control.cancel };
}

export function cancelProgress() {
  if (!progress || progress.cancelling) return;
  progress.cancel?.();
  progress = { ...progress, cancelling: true };
}

export function endProgress() {
  progress = null;
}
