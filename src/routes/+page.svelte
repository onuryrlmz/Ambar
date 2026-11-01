<script lang="ts">
  import "$lib/theme.css";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { onMount, tick } from "svelte";
  import { initJobEvents, setMenuLanguage } from "$lib/api.ts";
  import ArchivePane from "$lib/ArchivePane.svelte";
  import Host from "$lib/dialogs/Host.svelte";
  import { modalOpen, showAbout, showInfo } from "$lib/dialogs/store.svelte.ts";
  import { isMac } from "$lib/FileList.svelte";
  import { safeName } from "$lib/flows.ts";
  import { getLang, setLang, t, type Lang } from "$lib/i18n/index.ts";
  import Icon from "$lib/Icon.svelte";
  import { localModel, localShowHidden, localView, toggleShowHidden } from "$lib/local.svelte.ts";
  import LocalPane from "$lib/LocalPane.svelte";
  import { extractDragged, extractWithDialog, openLocalFile, openPaths, previewLocalFile, showEntry, testArchive } from "$lib/operations.ts";
  import StatusBar from "$lib/StatusBar.svelte";
  import { activeModel, activeTab, allTabs, closeTab } from "$lib/tabs.svelte.ts";
  import {
    baseName, clampSplit, fileToShow, isCloseTabKey, isReloadKey, parseSplit, selectedPaths, type Tab,
  } from "$lib/tabs-core.ts";
  import Toolbar, { type ToolAction } from "$lib/Toolbar.svelte";
  import type { TreeNode } from "$lib/types.ts";

  const SPLIT_KEY = "ambar.split";

  function loadSplit(): number {
    try {
      return parseSplit(localStorage.getItem(SPLIT_KEY));
    } catch {
      return 0.5;
    }
  }

  function saveSplit() {
    try {
      localStorage.setItem(SPLIT_KEY, String(splitWanted));
    } catch {
      // no storage: the ratio just is not remembered
    }
  }

  let lang = $state<Lang>(getLang());
  let activePane = $state<"local" | "archive">("local");
  let splitWanted = $state(loadSplit());
  let panesWidth = $state(0);
  let panesEl = $state<HTMLDivElement>();
  let dragging = $state(false);
  let localPane = $state<LocalPane>();
  let archivePane = $state<ArchivePane>();

  const split = $derived(clampSplit(splitWanted, panesWidth));
  const current = () => (activePane === "local" ? localPane : archivePane);
  const hasArchive = $derived(!!activeTab() && !activeTab()?.loading);
  const toShow = $derived.by(() => {
    const tab = activeTab();
    return tab && !tab.loading ? fileToShow(tab, activeModel()) : null;
  });
  const can = $derived<Record<ToolAction, boolean>>({
    extract: hasArchive,
    test: hasArchive,
    show: toShow !== null,
    info: !!activeTab()?.info,
  });

  /** Rows being carried with the mouse, between the panes. */
  type ItemDrag = { source: "local" | "archive"; x: number; y: number; label: string; over: "local" | "archive" | null };
  let itemDrag = $state.raw<ItemDrag | null>(null);
  let leftEl = $state<HTMLDivElement>();
  let rightEl = $state<HTMLDivElement>();
  let osDropping = $state(false);
  let opening = $state(0);

  function activate(pane: "local" | "archive") {
    activePane = pane;
    tick().then(() => {
      if (!modalOpen()) current()?.focus();
    });
  }

  // A closed modal gives the keyboard back to the pane that had it.
  let modalWasOpen = false;
  $effect(() => {
    const open = modalOpen();
    if (modalWasOpen && !open) tick().then(() => current()?.focus());
    modalWasOpen = open;
  });

  // The last tab went away: there is nothing left to act on over there.
  $effect(() => {
    if (allTabs().length === 0 && activePane === "archive") activate("local");
  });

  async function openFromDialog() {
    const picked = await openDialog({ multiple: true, directory: false });
    if (picked && (await openPaths(picked))) activate("archive");
  }

  async function openFromLeft(path: string) {
    if (await openLocalFile(path)) activate("archive");
  }

  async function openEntryNow(tab: Tab, row: TreeNode, quickLook: boolean) {
    opening++;
    try {
      await showEntry(tab, row, quickLook);
    } finally {
      opening--;
    }
  }

  function onOpenEntry(row: TreeNode) {
    const tab = activeTab();
    if (tab) void openEntryNow(tab, row, false);
  }

  function onQuickLook(row: TreeNode) {
    const tab = activeTab();
    if (tab && !row.isDir) void openEntryNow(tab, row, true);
  }

  function sideAt(x: number, y: number): "local" | "archive" | null {
    const inside = (el?: HTMLElement) => {
      const b = el?.getBoundingClientRect();
      return !!b && x >= b.left && x < b.right && y >= b.top && y < b.bottom;
    };
    return inside(leftEl) ? "local" : inside(rightEl) ? "archive" : null;
  }

  function startItemDrag(source: ItemDrag["source"], x: number, y: number) {
    const selection = source === "local" ? localView().selection : activeTab()?.selection;
    if (!selection || selection.size === 0) return;
    const paths = selectedPaths(selection);
    const label = paths.length === 1 ? safeName(baseName(paths[0])) : t("drag.count", { n: paths.length });
    itemDrag = { source, x, y, label, over: null };
  }

  function moveItemDrag(e: MouseEvent) {
    if (!itemDrag) return;
    if (e.buttons === 0 || modalOpen()) {
      itemDrag = null;
      return;
    }
    const over = sideAt(e.clientX, e.clientY);
    itemDrag = { ...itemDrag, x: e.clientX, y: e.clientY, over: over === itemDrag.source ? null : over };
  }

  function dropItemDrag(e: MouseEvent) {
    const drag = itemDrag;
    if (!drag) return;
    itemDrag = null;
    const side = sideAt(e.clientX, e.clientY);
    if (drag.source === "archive" && side === "local") {
      const tab = activeTab();
      const dest = localView().cwd;
      if (tab && dest) void extractDragged(tab, dest);
    } else if (drag.source === "local" && side === "archive") {
      // Only files that turn out to be archives open. The rest do nothing in this stage.
      const files = localModel().rows.filter((r) => !r.isDir && localView().selection.has("f:" + r.path));
      void openPaths(files.map((r) => r.path), true).then((showing) => {
        if (showing) activate("archive");
      });
    }
  }

  /** From the menu on the Mac, which owns Cmd+W there; from a key press elsewhere. */
  function closeCurrentTab() {
    const tab = activeTab();
    if (tab) closeTab(tab.id);
    else if (isMac()) void getCurrentWindow().close();
  }

  function changeLang(l: Lang) {
    setLang(l);
    lang = l;
    document.documentElement.lang = l;
    void setMenuLanguage(l, localShowHidden());
  }

  function runAction(action: ToolAction) {
    const tab = activeTab();
    if (!tab || !can[action]) return;
    switch (action) {
      case "extract":
        void extractWithDialog(tab);
        break;
      case "test":
        void testArchive(tab);
        break;
      case "show":
        if (toShow) void openEntryNow(tab, toShow, false);
        break;
      case "info":
        if (tab.info) void showInfo(tab.path, tab.info);
        break;
    }
  }

  function onMenu(id: string) {
    // The menu bar stays live behind a modal, and Cmd+W and Cmd+O come through it on the Mac.
    if (modalOpen()) {
      if (id === "close-window") void getCurrentWindow().close();
      else if (id === "select-all" && document.activeElement instanceof HTMLInputElement) document.activeElement.select();
      // The click already flipped the native check mark: put it back.
      else if (id === "show-hidden") void setMenuLanguage(lang, localShowHidden());
      return;
    }
    switch (id) {
      case "open":
        void openFromDialog();
        break;
      case "close-tab":
        closeCurrentTab();
        break;
      case "close-window":
        void getCurrentWindow().close();
        break;
      case "extract":
      case "test":
      case "show":
      case "info":
        runAction(id);
        break;
      case "about":
        void showAbout();
        break;
      case "select-all":
        if (document.activeElement instanceof HTMLInputElement) document.activeElement.select();
        else current()?.selectEverything();
        break;
      case "refresh":
        void localPane?.refresh();
        break;
      case "show-hidden":
        toggleShowHidden();
        void setMenuLanguage(lang, localShowHidden());
        break;
      case "lang-tr":
        changeLang("tr");
        break;
      case "lang-en":
        changeLang("en");
        break;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    // A reload would wipe every open tab. Cmd+R is our own Yenile, so it still refreshes.
    if (!import.meta.env.DEV && isReloadKey(e)) {
      e.preventDefault();
      if (!modalOpen()) void localPane?.refresh();
      return;
    }
    // An open modal has the keyboard to itself.
    if (modalOpen()) return;
    if (itemDrag) {
      e.preventDefault();
      if (e.key === "Escape") itemDrag = null;
      return;
    }
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    const mod = e.metaKey || e.ctrlKey;
    const key = e.key.toLowerCase();
    // A held key must not launch the same thing again and again.
    if (e.repeat && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      if (activePane === "local" && allTabs().length > 0) activate("archive");
      else if (activePane === "archive") activate("local");
    } else if (isCloseTabKey(e, isMac())) {
      e.preventDefault();
      closeCurrentTab();
    } else if (mod && key === "l") {
      e.preventDefault();
      void localPane?.editPath();
    } else if (current()?.handleKey(e)) {
      e.preventDefault();
    }
  }

  function startDrag(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    dragging = true;
  }

  function drag(e: PointerEvent) {
    if (!dragging || !panesEl) return;
    const box = panesEl.getBoundingClientRect();
    splitWanted = clampSplit((e.clientX - box.left) / box.width, box.width);
  }

  function endDrag() {
    if (!dragging) return;
    dragging = false;
    saveSplit();
  }

  function resetSplit() {
    splitWanted = 0.5;
    saveSplit();
  }

  onMount(() => {
    document.documentElement.lang = lang;
    if (lang !== "tr" || localShowHidden()) void setMenuLanguage(lang, localShowHidden());
    void initJobEvents().catch(() => {});
    const unlisten = listen<string>("menu", (e) => onMenu(e.payload));
    const undrop = getCurrentWebview().onDragDropEvent(({ payload }) => {
      // A drop behind a modal is ignored, and the ring must not promise otherwise.
      if (modalOpen()) {
        osDropping = false;
      } else if (payload.type === "drop") {
        osDropping = false;
        void openPaths(payload.paths).then((showing) => {
          if (showing) activate("archive");
        });
      } else {
        osDropping = payload.type !== "leave";
      }
    });
    return () => {
      void unlisten.then((u) => u());
      void undrop.then((u) => u());
    };
  });
</script>

<svelte:window
  onkeydown={onKeydown}
  onmousemove={moveItemDrag}
  onmouseup={dropItemDrag}
  onblur={() => (itemDrag = null)}
  oncontextmenu={(e) => {
    // WebKit's default menu offers Reload, which would wipe every open tab.
    if (!(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)) e.preventDefault();
  }}
/>

<!-- t() is not reactive: a language change rebuilds everything. Folder and tab state live in modules. -->
{#key lang}
  <div class="app" class:dragging class:carrying={itemDrag !== null}>
    <Toolbar {can} onAction={runAction} />
    <div class="panes" bind:this={panesEl} bind:clientWidth={panesWidth}>
      <div class="side" class:drop-on={itemDrag?.over === "local"} bind:this={leftEl} style:width="{split * 100}%">
        <LocalPane
          bind:this={localPane}
          active={activePane === "local"}
          onActivate={() => (activePane = "local")}
          onOpenFile={(p) => void openFromLeft(p)}
          onQuickLook={(r) => {
            if (!r.isDir) void previewLocalFile(r.path);
          }}
          onDragStart={(x, y) => startItemDrag("local", x, y)}
        />
      </div>
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        class="splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label={t("pane.splitter")}
        style:left="{split * 100}%"
        onpointerdown={startDrag}
        onpointermove={drag}
        onpointerup={endDrag}
        onpointercancel={endDrag}
        ondblclick={resetSplit}
      ></div>
      <div class="side rest" class:drop-on={itemDrag?.over === "archive" || osDropping} bind:this={rightEl}>
        <ArchivePane
          bind:this={archivePane}
          active={activePane === "archive"}
          onActivate={() => (activePane = "archive")}
          {onOpenEntry}
          {onQuickLook}
          onDragStart={(x, y) => startItemDrag("archive", x, y)}
        />
      </div>
    </div>
    <StatusBar {split} busy={opening > 0} />
    {#if itemDrag}
      <div class="ghost" style:transform="translate({itemDrag.x + 12}px, {itemDrag.y + 10}px)">
        <Icon name={itemDrag.source === "archive" ? "file" : "archive"} size={13} />
        <span>{itemDrag.label}</span>
      </div>
    {/if}
  </div>
{/key}

<Host />

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .app.dragging {
    cursor: col-resize;
  }
  .app.carrying {
    cursor: grabbing;
  }
  .ghost {
    position: fixed;
    top: 0;
    left: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    gap: 5px;
    max-width: 260px;
    height: 24px;
    padding: 0 9px;
    border-radius: 6px;
    background: var(--sel);
    color: var(--sel-ink);
    font-size: 12px;
    pointer-events: none;
    box-shadow: var(--panel-shadow);
  }
  .ghost span {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .side.drop-on {
    position: relative;
  }
  .side.drop-on::after {
    content: "";
    position: absolute;
    inset: 0;
    z-index: 3;
    pointer-events: none;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .panes {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .side {
    flex: none;
    min-width: 0;
    height: 100%;
  }
  .side.rest {
    flex: 1;
    border-left: 1px solid var(--line);
  }
  .splitter {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 7px;
    margin-left: -3px;
    z-index: 2;
    cursor: col-resize;
    touch-action: none;
  }
</style>
