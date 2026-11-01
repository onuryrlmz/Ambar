import type { Entry, TreeNode } from "./types.ts";

interface Acc {
  node: TreeNode;
  packedSum: number;
  nullPacked: number;
  explicit: boolean;
}

/** dir path ("" is the root) -> children keyed by kind and name, so a file and a folder
 * may share a name. */
export interface ArchiveIndex {
  dirs: Map<string, Map<string, Acc>>;
}

function normalize(p: string): string {
  const s = p.replace(/\\/g, "/");
  let a = 0;
  // Index loops: regex trimming is quadratic on long slash runs.
  for (;;) {
    if (s.startsWith("./", a)) a += 2;
    else if (s.charCodeAt(a) === 47) a++;
    else break;
  }
  let b = s.length;
  while (b > a && s.charCodeAt(b - 1) === 47) b--;
  return s.slice(a, b);
}

/** Deeper entries (hostile names) become one root-level node named by the whole path. */
const MAX_SEGMENTS = 512;

function isDeep(path: string): boolean {
  let n = 0;
  for (let i = 0; i < path.length; i++) {
    if (path.charCodeAt(i) === 47 && ++n >= MAX_SEGMENTS) return true;
  }
  return false;
}

function locate(path: string): [string, string, boolean] {
  if (isDeep(path)) return ["", path, true];
  const [parent, name] = split(path);
  return [parent, name, false];
}

function slot(index: ArchiveIndex, dir: string): Map<string, Acc> {
  let m = index.dirs.get(dir);
  if (!m) {
    m = new Map();
    index.dirs.set(dir, m);
  }
  return m;
}

function folderAcc(index: ArchiveIndex, parent: string, name: string, path: string): Acc {
  const m = slot(index, parent);
  const key = "d:" + name;
  let a = m.get(key);
  if (!a) {
    a = {
      node: { name, path, isDir: true, size: 0, packed: 0, modified: null, encrypted: false, crc: null },
      packedSum: 0,
      nullPacked: 0,
      explicit: false,
    };
    m.set(key, a);
  }
  return a;
}

function split(path: string): [string, string] {
  const i = path.lastIndexOf("/");
  return i < 0 ? ["", path] : [path.slice(0, i), path.slice(i + 1)];
}

/** Adds `sign * file` to every ancestor folder of `path` (creating them). */
function applyToAncestors(index: ArchiveIndex, path: string, size: number, packed: number | null, sign: 1 | -1) {
  let parent = "";
  const parts = path.split("/");
  for (let i = 0; i < parts.length - 1; i++) {
    const p = parent ? parent + "/" + parts[i] : parts[i];
    const a = folderAcc(index, parent, parts[i], p);
    a.node.size += sign * size;
    if (packed === null) a.nullPacked += sign;
    else a.packedSum += sign * packed;
    a.node.packed = a.nullPacked > 0 ? null : a.packedSum;
    parent = p;
  }
}

/** One pass over the entries. Later duplicates replace earlier ones; a folder entry that
 * comes after its children only contributes its metadata. */
export function buildIndex(entries: Entry[]): ArchiveIndex {
  const index: ArchiveIndex = { dirs: new Map() };
  for (const e of entries) {
    const path = normalize(e.path);
    if (!path) continue;
    const [parent, name, deep] = locate(path);
    if (e.isDir) {
      // Make sure ancestors exist, then the folder itself.
      if (!deep) applyToAncestors(index, path + "/x", 0, 0, 1);
      const a = folderAcc(index, parent, name, path);
      a.explicit = true;
      a.node.modified = e.modified;
      a.node.encrypted = e.encrypted;
      a.node.crc = e.crc;
      continue;
    }
    const m = slot(index, parent);
    const key = "f:" + name;
    const old = m.get(key);
    if (old && !deep) applyToAncestors(index, path, old.node.size, old.node.packed, -1);
    m.set(key, {
      node: {
        name, path, isDir: false, size: e.size, packed: e.packed,
        modified: e.modified, encrypted: e.encrypted, crc: e.crc,
      },
      packedSum: 0,
      nullPacked: 0,
      explicit: true,
    });
    if (!deep) applyToAncestors(index, path, e.size, e.packed, 1);
  }
  return index;
}

export function childrenOf(index: ArchiveIndex, dir: string): TreeNode[] {
  const m = index.dirs.get(normalize(dir));
  if (!m) return [];
  const out: TreeNode[] = [];
  for (const a of m.values()) out.push(a.node);
  return out;
}

/** Expands folders into the entry paths beneath them (files, and explicit empty
 * folders). A plain file path stays as it is. */
export function descendantPaths(index: ArchiveIndex, paths: string[]): string[] {
  const out = new Set<string>();
  const walk = (dir: string) => {
    const m = index.dirs.get(dir);
    if (!m) return;
    for (const a of m.values()) {
      if (!a.node.isDir) {
        out.add(a.node.path);
        continue;
      }
      const sub = index.dirs.get(a.node.path);
      if (!sub || sub.size === 0) {
        if (a.explicit) out.add(a.node.path);
      } else {
        walk(a.node.path);
      }
    }
  };
  for (const raw of paths) {
    const p = normalize(raw);
    const [parent, name] = locate(p);
    const m = index.dirs.get(parent);
    if (!m) continue;
    if (m.has("f:" + name)) out.add(p);
    if (m.has("d:" + name)) walk(p);
  }
  return [...out];
}
