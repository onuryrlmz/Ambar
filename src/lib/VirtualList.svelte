<script lang="ts" generics="T">
  import { untrack, type Snippet } from "svelte";
  import { visibleRange } from "./virtual.ts";

  let {
    items,
    rowHeight,
    row,
    onScroll,
    overscan = 8,
    keepScroll = false,
  }: {
    items: T[];
    rowHeight: number;
    row: Snippet<[T, number]>;
    onScroll?: (scrollTop: number) => void;
    overscan?: number;
    /** Keep the scroll position when `items` changes (re-sort of the same folder). */
    keepScroll?: boolean;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportH = $state(0);

  const range = $derived(visibleRange(scrollTop, viewportH, rowHeight, items.length, overscan));
  const slice = $derived(items.slice(range.start, range.end));

  let prevItems: T[] | undefined;
  $effect(() => {
    const cur = items;
    if (prevItems !== undefined && cur !== prevItems && !untrack(() => keepScroll)) {
      if (el) el.scrollTop = 0;
      scrollTop = 0;
    }
    prevItems = cur;
  });

  function handleScroll() {
    if (!el) return;
    scrollTop = el.scrollTop;
    onScroll?.(scrollTop);
  }

  /** Scrolls the minimum needed to make row `i` fully visible (keyboard navigation). */
  export function scrollToIndex(i: number) {
    if (!el || items.length === 0) return;
    const idx = Math.min(Math.max(0, i), items.length - 1);
    const top = idx * rowHeight;
    const bottom = top + rowHeight;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }

  /** Whole rows that fit in the viewport (for PageUp/PageDown). */
  export function pageSize(): number {
    return Math.max(1, Math.floor(viewportH / rowHeight));
  }
</script>

<div class="vl" bind:this={el} bind:clientHeight={viewportH} onscroll={handleScroll}>
  <div class="spacer" style:height="{items.length * rowHeight}px">
    <div class="win" style:transform="translateY({range.start * rowHeight}px)">
      {#each slice as item, k (range.start + k)}
        <div class="r" style:height="{rowHeight}px">
          {@render row(item, range.start + k)}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .vl {
    overflow-y: auto;
    overflow-x: hidden;
    height: 100%;
    position: relative;
  }
  .spacer {
    position: relative;
  }
  .win {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
  }
  .r {
    box-sizing: border-box;
    overflow: hidden;
  }
</style>
