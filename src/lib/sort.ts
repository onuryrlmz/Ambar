import type { SortDir, SortKey, TreeNode } from "./types.ts";

const collator = new Intl.Collator("tr", { numeric: true });

function cmpNullable(a: number | null, b: number | null): number {
  return (a ?? 0) - (b ?? 0);
}

/** Folders always come first; null values sort last in either direction. */
export function sortNodes(nodes: TreeNode[], key: SortKey, dir: SortDir): TreeNode[] {
  const sign = dir === "asc" ? 1 : -1;
  return [...nodes].sort((a, b) => {
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    let c = 0;
    if (key === "name") {
      c = sign * collator.compare(a.name, b.name);
    } else if (key === "size") {
      c = sign * (a.size - b.size);
    } else {
      const x = a[key];
      const y = b[key];
      if (x === null || y === null) {
        if (x !== y) return x === null ? 1 : -1;
      } else {
        c = sign * cmpNullable(x, y);
      }
    }
    return c !== 0 ? c : collator.compare(a.name, b.name);
  });
}
