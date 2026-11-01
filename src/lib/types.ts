// Mirrors the Rust types (engine/mod.rs, jobs.rs, error.rs, local_fs.rs). Keep in sync.

export type Format =
  | "rar" | "zip" | "sevenZ" | "tar" | "tarGz" | "tarBz2" | "tarXz" | "gz" | "bz2" | "xz";

/** Mirrors `ArchiveError::code()`. */
export type ErrorCode =
  | "not_archive" | "password_required" | "wrong_password" | "missing_volume" | "corrupt"
  | "disk_full" | "permission" | "unsafe_path" | "dest_conflict" | "unsupported"
  | "cancelled" | "risky_open" | "io";

export interface ArchiveError {
  code: ErrorCode;
  detail: string;
}

export interface Entry {
  path: string;
  isDir: boolean;
  size: number;
  packed: number | null;
  /** Unix SECONDS. `formatDate` takes milliseconds: multiply by 1000. */
  modified: number | null;
  crc: number | null;
  encrypted: boolean;
}

export interface ArchiveInfo {
  format: Format;
  fileSize: number;
  encryptedHeaders: boolean;
  solid: boolean;
  multiVolume: boolean;
  hasRecovery: boolean;
  locked: boolean;
  comment: string | null;
}

export interface OpenResult {
  info: ArchiveInfo;
  entries: Entry[];
}

export type OverwritePolicy = "ask" | "overwrite" | "skip" | "rename";
export type ConflictChoice = "overwrite" | "skip" | "rename" | "cancel";

export interface ExtractRequest {
  dest: string;
  selection: string[];
  base: string;
  keepPaths: boolean;
  overwrite: OverwritePolicy;
  keepBroken: boolean;
}

export interface FileFailure {
  path: string;
  error: ArchiveError;
}

export interface OpReport {
  ok: number;
  skipped: number;
  failed: FileFailure[];
}

export interface ProgressEvent {
  id: string;
  current: string;
  doneBytes: number;
  totalBytes: number;
}

export interface ConflictEvent {
  id: string;
  path: string;
}

export interface DoneEvent {
  id: string;
  report: OpReport | null;
  error: ArchiveError | null;
}

export interface LocalEntry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  hidden: boolean;
  size: number;
  /** Unix SECONDS. */
  modified: number | null;
}

/** A row in an archive pane: a real or synthesized entry. */
export interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  packed: number | null;
  /** Unix SECONDS, as in `Entry`. Null for synthesized folders. */
  modified: number | null;
  encrypted: boolean;
  crc: number | null;
}

export type SortKey = "name" | "size" | "packed" | "modified";
export type SortDir = "asc" | "desc";
