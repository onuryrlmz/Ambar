<script lang="ts">
  import "$lib/theme.css";
  import { listen } from "@tauri-apps/api/event";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { onMount, tick } from "svelte";
  import { setMenuLanguage } from "$lib/api.ts";
  import ArchivePane from "$lib/ArchivePane.svelte";
  import { getLang, setLang, t, type Lang } from "$lib/i18n/index.ts";
  import LocalPane from "$lib/LocalPane.svelte";
  import StatusBar from "$lib/StatusBar.svelte";
  import { activeTab, allTabs, closeTab, openTab } from "$lib/tabs.svelte.ts";
  import { clampSplit, parseSplit } from "$lib/tabs-core.ts";
  import Toolbar, { type ToolAction } from "$lib/Toolbar.svelte";

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

  function activate(pane: "local" | "archive") {
    activePane = pane;
    tick().then(() => current()?.focus());
  }

  // The last tab went away: there is nothing left to act on over there.
  $effect(() => {
    if (allTabs().length === 0 && activePane === "archive") activate("local");
  });

  async function openArchiveFile(path: string) {
    if (await openTab(path)) activate("archive");
  }

  async function openFromDialog() {
    const picked = await openDialog({ multiple: true, directory: false });
    for (const path of picked ?? []) await openArchiveFile(path);
  }

  let lastClose = 0;
  function closeCurrentTab() {
    // Cmd+W can reach us twice: once as a key press and once through the menu.
    const now = performance.now();
    if (now - lastClose < 80) return;
    lastClose = now;
    const tab = activeTab();
    if (tab) closeTab(tab.id);
  }

  function changeLang(l: Lang) {
    setLang(l);
    lang = l;
    document.documentElement.lang = l;
    void setMenuLanguage(l);
  }

  /** Task 9 connects these to the extract, test, open_entry and info flows. */
  function runAction(_action: ToolAction) {}

  function onMenu(id: string) {
    switch (id) {
      case "open":
        void openFromDialog();
        break;
      case "close-tab":
        closeCurrentTab();
        break;
      case "extract":
      case "test":
      case "show":
      case "info":
        if (hasArchive) runAction(id);
        break;
      case "select-all":
        if (document.activeElement instanceof HTMLInputElement) document.activeElement.select();
        else current()?.selectEverything();
        break;
      case "refresh":
        void localPane?.refresh();
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
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    const mod = e.metaKey || e.ctrlKey;
    const key = e.key.toLowerCase();
    if (e.key === "Tab") {
      e.preventDefault();
      if (activePane === "local" && allTabs().length > 0) activate("archive");
      else if (activePane === "archive") activate("local");
    } else if (mod && key === "w") {
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
    if (lang !== "tr") void setMenuLanguage(lang);
    const unlisten = listen<string>("menu", (e) => onMenu(e.payload));
    return () => {
      void unlisten.then((u) => u());
    };
  });
</script>

<svelte:window onkeydown={onKeydown} />

<!-- t() is not reactive: a language change rebuilds everything. Folder and tab state live in modules. -->
{#key lang}
  <div class="app" class:dragging>
    <Toolbar {hasArchive} onAction={runAction} />
    <div class="panes" bind:this={panesEl} bind:clientWidth={panesWidth}>
      <div class="side" style:width="{split * 100}%">
        <LocalPane
          bind:this={localPane}
          active={activePane === "local"}
          onActivate={() => (activePane = "local")}
          onOpenFile={(p) => void openArchiveFile(p)}
        />
      </div>
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
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
      <div class="side rest">
        <ArchivePane
          bind:this={archivePane}
          active={activePane === "archive"}
          onActivate={() => (activePane = "archive")}
        />
      </div>
    </div>
    <StatusBar {split} />
  </div>
{/key}

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
