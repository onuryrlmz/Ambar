/** Rows to render for a scroll position: [start, end), clamped, with overscan. */
export function visibleRange(
  scrollTop: number,
  viewportH: number,
  rowH: number,
  total: number,
  overscan: number,
): { start: number; end: number } {
  if (total <= 0 || rowH <= 0) return { start: 0, end: 0 };
  const maxTop = Math.max(0, total * rowH - viewportH);
  const top = Math.min(Math.max(0, scrollTop), maxTop);
  const start = Math.max(0, Math.floor(top / rowH) - overscan);
  const end = Math.min(total, Math.ceil((top + viewportH) / rowH) + overscan);
  return { start, end };
}
