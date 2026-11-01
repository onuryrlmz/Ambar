<script lang="ts" module>
  import type { SortKey as ColumnKey } from "./types.ts";

  export interface Column<Row> {
    key: ColumnKey;
    label: string;
    width: number;
    /** Lower stays longer when the pane narrows. */
    priority: number;
    text: (row: Row) => string;
  }

  export const isMac = () => /Mac/i.test(globalThis.navigator?.platform ?? "");
</script>

<script lang="ts" generics="R extends import('./types.ts').TreeNode">
  import { tick, untrack, type Snippet } from "svelte";
  import { t } from "./i18n/index.ts";
  import Icon from "./Icon.svelte";
  import { safeName } from "./names.ts";
  import * as core from "./tabs-core.ts";
  import type { SortDir, SortKey } from "./types.ts";
  import VirtualList from "./VirtualList.svelte";

  const ROW_H = 24;
  const MIN_NAME = 260;
  const INSET = 12;
  const OVERLAY_BAR = 16;

  let {
    model,
    view,
    columns,
    sortKey,
    sortDir,
    active,
    folderKey,
    emptyText,
    onView,
    onSort,
    onActivate,
    onOpen,
    onUp,
    onSpace,
    onDragStart,
    dim,
    nameAfter,
  }: {
    model: core.ListModel<R>;
    view: core.PaneView;
    columns: Column<R>[];
    sortKey: SortKey;
    sortDir: SortDir;
    active: boolean;
    /** Changes with the folder: a new folder starts a new scroll position. */
    folderKey: string;
    emptyText: string;
    onView: (v: core.PaneView) => void;
    onSort: (key: SortKey) => void;
    onActivate: () => void;
    onOpen: (row: R) => void;
    onUp: () => void;
    /** Space on the Mac (Quick Look). */
    onSpace?: (row: R) => void;
    /** A press on a row became a drag, carrying the selection. */
    onDragStart?: (x: number, y: number) => void;
    dim?: (row: R) => boolean;
    nameAfter?: Snippet<[R]>;
  } = $props();

  let list = $state<{ scrollToIndex: (i: number) => void; pageSize: () => number }>();
  let body = $state<HTMLDivElement>();
  let width = $state(0);
  let bodyH = $state(0);
  let gutter = $state(0);

  const shown = $derived(core.fitColumns(columns, width - gutter - INSET * 2, MIN_NAME));
  const grid = $derived(["minmax(0, 1fr)", ...shown.map((c) => `${c.width}px`)].join(" "));

  // A classic scrollbar takes width from the rows; the header gives up the same amount
  // so that columns stay aligned.
  $effect(() => {
    void [folderKey, model.rows.length, bodyH, width];
    tick().then(() => {
      const el = body?.querySelector<HTMLElement>(".vl");
      gutter = el ? el.offsetWidth - el.clientWidth : 0;
    });
  });

  // Coming back to a folder shows the row we left on.
  $effect(() => {
    void folderKey;
    tick().then(() => untrack(() => reveal()));
  });

  function reveal() {
    const i = view.cursor === null ? undefined : model.indexOf.get(view.cursor);
    if (i !== undefined) list?.scrollToIndex(i);
  }

  const isSel = (i: number) => {
    const k = model.keys[i];
    return k !== undefined && view.selection.has(k);
  };

  /** From a press on a row until its release, or until the drag takes over. */
  let gesture: core.RowGesture | null = null;

  function press(e: MouseEvent, i: number) {
    if (e.button !== 0) return;
    onActivate();
    const pressed = core.pressRow(view, model, i, { toggle: e.metaKey || e.ctrlKey, range: e.shiftKey });
    gesture = core.beginGesture(e.clientX, e.clientY, pressed.collapseOnRelease ? i : null);
    onView(pressed.view);
  }

  function release(i: number) {
    if (gesture && core.releaseGesture(gesture, i) !== null) onView(core.selectClick(view, model, i, {}));
    gesture = null;
  }

  function moved(e: MouseEvent) {
    if (!gesture) return;
    // The button came up where we could not see it.
    if (e.buttons === 0) {
      gesture = null;
      return;
    }
    const next = core.moveGesture(gesture, e.clientX, e.clientY);
    gesture = next.gesture;
    if (next.started) onDragStart?.(e.clientX, e.clientY);
  }

  /** Clicking the blank area clears the selection; a scrollbar is not the blank area. */
  function onScrollbar(e: MouseEvent, target: Element, vl: HTMLElement): boolean {
    const box = vl.getBoundingClientRect();
    // A classic scrollbar lands beyond the client width.
    if (e.clientX - box.left >= vl.clientWidth) return true;
    // A Mac overlay scrollbar takes no width, and a press on it lands on the scroller itself.
    return target === vl && vl.scrollHeight > vl.clientHeight && box.right - e.clientX <= OVERLAY_BAR;
  }

  function pressBlank(e: MouseEvent) {
    onActivate();
    const target = e.target as Element;
    if (target.closest(".row")) return;
    const vl = body?.querySelector<HTMLElement>(".vl");
    if (vl && onScrollbar(e, target, vl)) return;
    onView(core.clearSelection(view));
  }

  function moveBy(step: number, extend: boolean) {
    const cur = view.cursor === null ? undefined : model.indexOf.get(view.cursor);
    const target = cur === undefined ? (step > 0 ? 0 : model.keys.length - 1) : cur + step;
    moveAbs(target, extend);
  }

  function moveAbs(i: number, extend: boolean) {
    if (model.keys.length === 0) return;
    onView(core.moveTo(view, model, i, extend));
    list?.scrollToIndex(Math.min(Math.max(0, i), model.keys.length - 1));
  }

  const cursorRow = (): R | undefined => {
    const i = view.cursor === null ? undefined : model.indexOf.get(view.cursor);
    return i === undefined ? undefined : model.rows[i];
  };

  export function selectEverything() {
    onView(core.selectAll(view, model.keys));
  }

  export function focus() {
    body?.focus({ preventScroll: true });
  }

  /** Returns whether the key was used. The page sends keys to the active pane. */
  export function handleKey(e: KeyboardEvent): boolean {
    const cmd = core.keyCommand(e);
    if (!cmd) return false;
    switch (cmd.kind) {
      case "move":
        moveBy(cmd.step, cmd.extend);
        break;
      case "page":
        moveBy(cmd.step * (list?.pageSize() ?? 10), cmd.extend);
        break;
      case "edge":
        moveAbs(cmd.to === "start" ? 0 : model.keys.length - 1, cmd.extend);
        break;
      case "open": {
        const r = cursorRow();
        if (r) onOpen(r);
        break;
      }
      case "up":
        onUp();
        break;
      case "space":
        if (isMac()) {
          const r = cursorRow();
          if (r) onSpace?.(r);
        } else if (view.cursor === null) moveBy(1, false);
        else onView(core.toggleCursor(view));
        break;
      case "all":
        selectEverything();
        break;
    }
    return true;
  }

  const sortIcon = $derived(sortDir === "asc" ? "sortUp" : "sortDown");
</script>

<svelte:window onmouseup={() => (gesture = null)} onmousemove={moved} />

<div class="list" class:active bind:clientWidth={width}>
  <div
    class="head"
    role="row"
    style:grid-template-columns={grid}
    style:padding-right="{INSET + gutter}px"
  >
    {@render headCell("name", t("col.name"), false)}
    {#each shown as c (c.key)}
      {@render headCell(c.key, c.label, true)}
    {/each}
  </div>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="body"
    role="listbox"
    aria-multiselectable="true"
    tabindex="0"
    bind:this={body}
    bind:clientHeight={bodyH}
    onmousedown={pressBlank}
  >
    {#key folderKey}
      <VirtualList bind:this={list} items={model.rows} rowHeight={ROW_H} keepScroll={true}>
        {#snippet row(r: R, i: number)}
          {@const sel = isSel(i)}
          <div
            class="row"
            class:sel
            class:joined-up={sel && isSel(i - 1)}
            class:joined-down={sel && isSel(i + 1)}
            class:cursor={active && !sel && view.cursor === model.keys[i]}
            class:dim={dim?.(r)}
            role="option"
            aria-selected={sel}
            tabindex="-1"
            style:grid-template-columns={grid}
            onmousedown={(e) => press(e, i)}
            onmouseup={(e) => e.button === 0 && release(i)}
            ondblclick={(e) => e.button === 0 && onOpen(r)}
          >
            <div class="cell name" title={safeName(r.name)}>
              <span class="kind" class:folder={r.isDir} class:crate={!r.isDir && core.isArchiveName(r.name)}>
                <Icon name={r.isDir ? "folder" : core.isArchiveName(r.name) ? "archive" : "file"} />
              </span>
              <span class="text">{safeName(r.name)}</span>
              {@render nameAfter?.(r)}
            </div>
            {#each shown as c (c.key)}
              <div class="cell num">{c.text(r)}</div>
            {/each}
          </div>
        {/snippet}
      </VirtualList>
    {/key}
    {#if model.rows.length === 0}
      <div class="empty">{emptyText}</div>
    {/if}
  </div>
  <div class="ring" aria-hidden="true"></div>
</div>

{#snippet headCell(key: SortKey, label: string, num: boolean)}
  <div class="hwrap" role="columnheader" aria-sort={sortKey !== key ? "none" : sortDir === "asc" ? "ascending" : "descending"}>
  <button
    class="hcell"
    class:num
    class:on={sortKey === key}
    tabindex="-1"
    title={label}
    onmousedown={(e) => e.preventDefault()}
    onclick={() => {
      onActivate();
      onSort(key);
    }}
  >
    {#if num && sortKey === key}<Icon name={sortIcon} size={10} />{/if}
    <span>{label}</span>
    {#if !num && sortKey === key}<Icon name={sortIcon} size={10} />{/if}
  </button>
  </div>
{/snippet}

<style>
  .list {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface);
  }
  .head {
    flex: none;
    display: grid;
    height: 23px;
    padding-left: 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--line-soft);
  }
  .hwrap {
    min-width: 0;
    padding: 3px 0;
  }
  .hcell {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    padding: 0 6px;
    border-right: 1px solid var(--line-soft);
    color: var(--ink-2);
    font-size: 11.5px;
    white-space: nowrap;
    overflow: hidden;
  }
  .hwrap:last-child .hcell {
    border-right: 0;
  }
  .hcell span {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hcell.num {
    justify-content: flex-end;
  }
  .hcell:hover {
    color: var(--ink);
  }
  .hcell.on {
    color: var(--ink);
    font-weight: 600;
  }
  .hwrap:first-child .hcell {
    padding-left: 1px;
  }
  .body {
    position: relative;
    flex: 1;
    min-height: 0;
    outline: none;
  }
  .row {
    display: grid;
    height: 24px;
    align-items: center;
    position: relative;
    isolation: isolate;
    padding: 0 12px;
    color: var(--ink);
    outline: none;
  }
  /* The highlight is inset, but the row is full width: its margins are still the row. */
  .row::before {
    content: "";
    position: absolute;
    inset: 0 6px;
    z-index: -1;
    border-radius: 5px;
  }
  .row.sel::before {
    background: var(--sel-idle);
  }
  .row.joined-up::before {
    border-top-left-radius: 0;
    border-top-right-radius: 0;
  }
  .row.joined-down::before {
    border-bottom-left-radius: 0;
    border-bottom-right-radius: 0;
  }
  .active .row.sel {
    color: var(--sel-ink);
  }
  .active .row.sel::before {
    background: var(--sel);
  }
  .row.cursor::before {
    box-shadow: inset 0 0 0 1px var(--cursor-line);
  }
  .cell {
    min-width: 0;
    padding: 0 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cell.num {
    text-align: right;
    font-size: 12px;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
  }
  .active .row.sel .cell.num {
    color: var(--sel-ink-2);
  }
  .cell.name {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-left: 0;
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .kind {
    flex: none;
    color: var(--ink-3);
  }
  .kind.folder {
    color: var(--folder);
  }
  .kind.crate {
    color: var(--crate);
  }
  .active .row.sel .kind {
    color: var(--sel-ink);
  }
  .row.dim .text,
  .row.dim .kind,
  .row.dim .cell.num {
    opacity: 0.5;
  }
  .active .row.sel.dim .text,
  .active .row.sel.dim .kind,
  .active .row.sel.dim .cell.num {
    opacity: 0.75;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--ink-3);
    pointer-events: none;
  }
  .ring {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .active .ring {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
</style>
