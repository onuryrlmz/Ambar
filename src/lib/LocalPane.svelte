<script lang="ts">
  import { onMount, tick } from "svelte";
  import { formatBytes, formatDate } from "./format.ts";
  import FileList, { type Column } from "./FileList.svelte";
  import { errorText, t } from "./i18n/index.ts";
  import Icon from "./Icon.svelte";
  import Crumbs from "./Crumbs.svelte";
  import {
    dismissLocalError, goHome, goTo, goUp, localError, localModel, localSort, localView,
    refreshLocal, setLocalView, startLocal, toggleLocalSort, type LocalRow,
  } from "./local.svelte.ts";
  import { localCrumbs } from "./tabs-core.ts";

  let {
    active,
    onActivate,
    onOpenFile,
    onQuickLook,
    onDragStart,
  }: {
    active: boolean;
    onActivate: () => void;
    /** Enter or double-click on a file: an archive opens in a tab, anything else in its app. */
    onOpenFile: (path: string) => void;
    onQuickLook?: (row: LocalRow) => void;
    onDragStart?: (x: number, y: number) => void;
  } = $props();

  const columns: Column<LocalRow>[] = [
    { key: "size", label: t("col.size"), width: 76, priority: 1, text: (r) => (r.isDir ? "—" : formatBytes(r.size)) },
    {
      key: "modified", label: t("col.modified"), width: 124, priority: 2,
      text: (r) => formatDate(r.modified === null ? null : r.modified * 1000),
    },
  ];

  let list = $state<FileList<LocalRow>>();
  let editing = $state(false);
  let draft = $state("");
  let input = $state<HTMLInputElement>();

  const view = $derived(localView());
  const crumbs = $derived(localCrumbs(view.cwd));
  const failure = $derived(localError());

  onMount(startLocal);

  function open(row: LocalRow) {
    // A linked folder has isDir set. A broken link does not, and fails like any bad file.
    if (row.isDir) void goTo(row.path);
    else onOpenFile(row.path);
  }

  export async function editPath() {
    onActivate();
    draft = view.cwd;
    editing = true;
    await tick();
    input?.select();
  }

  function stopEditing() {
    editing = false;
    list?.focus();
  }

  function commitPath() {
    const path = draft.trim();
    stopEditing();
    if (path && path !== view.cwd) void goTo(path);
  }

  export const handleKey = (e: KeyboardEvent) => list?.handleKey(e) ?? false;
  export const selectEverything = () => list?.selectEverything();
  export const focus = () => list?.focus();
  export const refresh = () => refreshLocal();
</script>

<section class="pane" aria-label={t("pane.local")}>
  <div class="strip">
    <div class="tab current">
      <span class="glyph computer"><Icon name="computer" size={14} /></span>
      <span class="label">{t("pane.local")}</span>
    </div>
  </div>
  <div class="nav">
    <button class="nav-btn" title={t("toolbar.up")} tabindex="-1" onmousedown={(e) => e.preventDefault()} onclick={() => { onActivate(); void goUp(); }}>
      <Icon name="up" size={14} />
    </button>
    <button class="nav-btn" title={t("toolbar.home")} tabindex="-1" onmousedown={(e) => e.preventDefault()} onclick={() => { onActivate(); void goHome(); }}>
      <Icon name="home" size={14} />
    </button>
    {#if editing}
      <input
        class="path-input"
        aria-label={t("pane.path")}
        spellcheck="false"
        autocapitalize="off"
        autocomplete="off"
        bind:this={input}
        bind:value={draft}
        onblur={() => (editing = false)}
        onkeydown={(e) => {
          if (e.key === "Enter") commitPath();
          else if (e.key === "Escape") stopEditing();
        }}
      />
    {:else}
      <Crumbs {crumbs} onGo={(p) => { onActivate(); void goTo(p); }} onBlank={editPath} />
    {/if}
  </div>
  {#if failure}
    <div class="banner" role="alert">
      <Icon name="alert" size={14} />
      <span class="message">{failure.path === null ? errorText(failure.code) : t("pane.listFailed", { name: failure.path, message: errorText(failure.code) })}</span>
      <button class="nav-btn" title={t("common.close")} tabindex="-1" onclick={dismissLocalError}><Icon name="close" size={12} /></button>
    </div>
  {/if}
  <FileList
    bind:this={list}
    model={localModel()}
    {view}
    {columns}
    sortKey={localSort().sortKey}
    sortDir={localSort().sortDir}
    {active}
    folderKey={view.cwd}
    emptyText={t("pane.empty")}
    onView={setLocalView}
    onSort={toggleLocalSort}
    {onActivate}
    onOpen={open}
    onUp={() => void goUp()}
    onSpace={(r) => onQuickLook?.(r)}
    {onDragStart}
    dim={(r) => r.hidden}
  >
    {#snippet nameAfter(r: LocalRow)}
      {#if r.isSymlink}
        <span class="after" title={t("pane.symlink")}><Icon name="link" size={13} /></span>
      {/if}
    {/snippet}
  </FileList>
</section>

<style>
  .after {
    flex: none;
    opacity: 0.6;
  }
</style>
