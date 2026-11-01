<script lang="ts">
  import { tick } from "svelte";
  import Crumbs from "./Crumbs.svelte";
  import type { OpenFailureText } from "./flows.ts";
  import FileList, { type Column } from "./FileList.svelte";
  import { formatBytes, formatCrc, formatDate, formatRatio } from "./format.ts";
  import { errorText, t } from "./i18n/index.ts";
  import Icon from "./Icon.svelte";
  import { safeName } from "./names.ts";
  import { activeModel, activeTab, allTabs, archiveError, closeTab, dismissArchiveError, focusTab, updateActive } from "./tabs.svelte.ts";
  import { archiveCrumbs, navigate, parentOf, tabLabel, toggleSort, type PaneView } from "./tabs-core.ts";
  import type { TreeNode } from "./types.ts";

  let {
    active,
    onActivate,
    onOpenEntry,
    onQuickLook,
    onDragStart,
  }: {
    active: boolean;
    onActivate: () => void;
    /** Enter or double-click on a file inside the archive. */
    onOpenEntry?: (row: TreeNode) => void;
    onQuickLook?: (row: TreeNode) => void;
    onDragStart?: (x: number, y: number) => void;
  } = $props();

  const columns: Column<TreeNode>[] = [
    { key: "size", label: t("col.size"), width: 76, priority: 1, text: (r) => formatBytes(r.size) },
    {
      key: "packed", label: t("col.packed"), width: 76, priority: 3,
      text: (r) => (r.packed === null ? "—" : formatBytes(r.packed)),
    },
    { key: "ratio", label: t("col.ratio"), width: 50, priority: 4, text: (r) => formatRatio(r.packed, r.size) },
    {
      key: "modified", label: t("col.modified"), width: 124, priority: 2,
      text: (r) => formatDate(r.modified === null ? null : r.modified * 1000),
    },
    { key: "crc", label: t("col.crc"), width: 76, priority: 5, text: (r) => formatCrc(r.crc) },
  ];

  let list = $state<FileList<TreeNode>>();
  let strip = $state<HTMLDivElement>();

  const tab = $derived(activeTab());
  const openFailure = $derived(archiveError());

  // Keep the current tab in view when the strip overflows.
  $effect(() => {
    if (!tab) return;
    void tab.id;
    tick().then(() => strip?.querySelector(".tab.current")?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });

  function failureText(f: OpenFailureText): string {
    if (f.kind === "one") return t("archive.openFailed", { name: f.name, message: errorText(f.code) });
    if (f.code === null) return t("drop.mixed", { n: f.n, names: f.names });
    if (f.code === "not_archive") return t("drop.notArchives", { n: f.n, names: f.names });
    return t("drop.notArchive", { n: f.n, names: f.names, message: errorText(f.code) });
  }

  function goToFolder(dir: string, cameFrom?: string) {
    updateActive((x) => navigate(x, dir, cameFrom));
  }

  function up() {
    if (!tab) return;
    const parent = parentOf(tab.cwd);
    if (parent !== null) goToFolder(parent, tab.cwd);
  }

  function jump(dir: string) {
    onActivate();
    // The child of `dir` on the way down to here, if we are going up.
    const here = tab?.cwd ?? "";
    if (dir === "" || here.startsWith(dir + "/")) {
      const end = here.indexOf("/", dir.length + 1);
      goToFolder(dir, end < 0 ? here : here.slice(0, end));
    }
  }

  function open(row: TreeNode) {
    if (row.isDir) goToFolder(row.path);
    else onOpenEntry?.(row);
  }

  export const handleKey = (e: KeyboardEvent) => list?.handleKey(e) ?? false;
  export const selectEverything = () => list?.selectEverything();
  export const focus = () => list?.focus();
</script>

<section class="pane" aria-label={t("pane.archive")}>
  <div class="strip" role="tablist" aria-label={t("archive.tabs")} bind:this={strip}>
    {#each allTabs() as x (x.id)}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <div
        class="tab"
        class:current={x.id === tab?.id}
        role="tab"
        tabindex="-1"
        aria-selected={x.id === tab?.id}
        title={safeName(x.path)}
        onmousedown={(e) => {
          if (e.button === 0) {
            onActivate();
            focusTab(x.id);
          }
        }}
        onauxclick={(e) => e.button === 1 && closeTab(x.id)}
      >
        {#if x.loading}
          <span class="spinner"></span>
        {:else}
          <span class="glyph"><Icon name={x.locked ? "lock" : "archive"} size={14} /></span>
        {/if}
        <span class="label">{tabLabel(x.path)}</span>
        <button
          class="nav-btn tab-close"
          title={t("archive.closeTab")}
          aria-label={t("archive.closeTab")}
          tabindex="-1"
          onmousedown={(e) => e.stopPropagation()}
          onclick={() => closeTab(x.id)}
        >
          <Icon name="close" size={11} />
        </button>
      </div>
    {/each}
  </div>

  {#if tab}
    <div class="nav">
      <button class="nav-btn" title={t("toolbar.up")} tabindex="-1" disabled={tab.cwd === ""} onmousedown={(e) => e.preventDefault()} onclick={() => { onActivate(); up(); }}>
        <Icon name="up" size={14} />
      </button>
      <Crumbs crumbs={archiveCrumbs(tab.cwd)} onGo={jump}>
        {#snippet lead()}
          <button
            class="seg root"
            class:here={tab.cwd === ""}
            title={safeName(tab.path)}
            tabindex="-1"
            onclick={() => jump("")}
          >
            <span class="glyph"><Icon name="archive" size={13} /></span>
            <span class="root-name">{tabLabel(tab.path)}</span>
          </button>
        {/snippet}
      </Crumbs>
    </div>
  {/if}

  {#if openFailure}
    <div class="banner" role="alert">
      <Icon name="alert" size={14} />
      <span class="message">{failureText(openFailure)}</span>
      <button class="nav-btn" title={t("common.close")} tabindex="-1" onclick={dismissArchiveError}><Icon name="close" size={12} /></button>
    </div>
  {/if}

  {#if tab && !tab.loading}
    <FileList
      bind:this={list}
      model={activeModel()}
      view={tab}
      {columns}
      sortKey={tab.sortKey}
      sortDir={tab.sortDir}
      {active}
      folderKey="{tab.id}:{tab.cwd}"
      emptyText={t("pane.empty")}
      onView={(v: PaneView) => updateActive((x) => ({ ...x, ...v }))}
      onSort={(k) => updateActive((x) => toggleSort(x, k))}
      {onActivate}
      onOpen={open}
      onUp={up}
      onSpace={(r) => onQuickLook?.(r)}
      {onDragStart}
    >
      {#snippet nameAfter(r: TreeNode)}
        {#if r.encrypted}
          <span class="after" title={t("archive.encrypted")}><Icon name="lock" size={12} /></span>
        {/if}
      {/snippet}
    </FileList>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="idle" class:active onmousedown={onActivate}>
      {#if tab}
        <div class="spinner big"></div>
        <p class="line">{t("archive.loading")}</p>
      {:else}
        <div class="zone">
          <span class="crate"><Icon name="archive" size={44} /></span>
          <p class="line">{t("archive.dropHint")}</p>
          <p class="sub">{t("archive.dropSub")}</p>
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .after {
    flex: none;
    opacity: 0.7;
  }
  .tab-close {
    width: 17px;
    height: 17px;
    visibility: hidden;
  }
  :global(.tab.current) .tab-close,
  :global(.tab:hover) .tab-close {
    visibility: visible;
  }
  .root {
    flex: 0 1 auto;
    min-width: 28px;
    display: flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 4px;
    border-radius: 4px;
    overflow: hidden;
  }
  .root:hover {
    background: var(--hover);
    color: var(--ink);
  }
  .root.here {
    flex-shrink: 0;
    max-width: 100%;
    color: var(--ink);
    font-weight: 500;
  }
  .root .glyph {
    color: var(--crate);
  }
  .root-name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .idle {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 14px;
    background: var(--surface);
    color: var(--ink-2);
  }
  .zone {
    flex: 1;
    align-self: stretch;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 12px;
    text-align: center;
    border: 1.5px dashed var(--zone-line);
    border-radius: 10px;
  }
  .crate {
    color: var(--crate);
    margin-bottom: 12px;
  }
  .line {
    margin: 0;
    font-size: 13px;
    font-weight: 500;
    color: var(--ink);
  }
  .sub {
    margin: 3px 0 0;
    font-size: 12px;
    color: var(--ink-2);
  }
  .big {
    width: 18px;
    height: 18px;
    border-width: 2px;
  }
</style>
