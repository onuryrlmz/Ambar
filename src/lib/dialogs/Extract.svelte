<script lang="ts">
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { listLocal } from "../api.ts";
  import { safeName, toArchiveError, type ExtractScope } from "../flows.ts";
  import { formatCount } from "../format.ts";
  import { errorText, t } from "../i18n/index.ts";
  import { baseName } from "../tabs-core.ts";
  import type { OverwritePolicy } from "../types.ts";
  import Modal from "./Modal.svelte";
  import type { ExtractChoice } from "./store.svelte.ts";

  let {
    cwd,
    selectedCount,
    dest: initialDest,
    batchCount = null,
    onDone,
  }: {
    cwd: string;
    selectedCount: number;
    dest: string;
    /** How many archives a batch holds. Null extracts from the open archive. */
    batchCount?: number | null;
    onDone: (choice: ExtractChoice | null) => void;
  } = $props();

  // The dialog opens once per request: its fields start from the props and go their own way.
  const start = () => ({ dest: initialDest, scope: (selectedCount > 0 ? "selected" : "folder") as ExtractScope });
  let dest = $state(start().dest);
  let scope = $state<ExtractScope>(start().scope);
  let keepPaths = $state(true);
  let keepBroken = $state(false);
  let overwrite = $state<OverwritePolicy>("ask");
  let placement = $state<"together" | "separate">("together");
  let destError = $state<string | null>(null);
  let checking = $state(false);

  const folderName = $derived(safeName(baseName(cwd)));
  const policies: { value: OverwritePolicy; key: string }[] = [
    { value: "ask", key: "extract.ask" },
    { value: "overwrite", key: "extract.overwriteAll" },
    { value: "skip", key: "extract.skip" },
    { value: "rename", key: "extract.rename" },
  ];

  async function choose() {
    try {
      const picked = await openDialog({ directory: true, multiple: false, defaultPath: dest.trim() || undefined });
      if (typeof picked === "string") {
        dest = picked;
        destError = null;
      }
    } catch (e) {
      destError = t("extract.destBad", { message: errorText(toArchiveError(e).code) });
    }
  }

  async function submit() {
    const path = dest.trim();
    if (!path || checking) return;
    checking = true;
    try {
      await listLocal(path); // a typo stays in the dialog
      onDone({ dest: path, scope, keepPaths, keepBroken, overwrite, separate: placement === "separate" });
    } catch (e) {
      destError = t("extract.destBad", { message: errorText(toArchiveError(e).code) });
    } finally {
      checking = false;
    }
  }
</script>

<Modal title={t("extract.title")} icon="extract" width={480} onEscape={() => onDone(null)} onEnter={submit}>
  <label class="block" for="extract-dest">{t("extract.dest")}</label>
  <div class="row">
    <input
      id="extract-dest"
      class="field grow"
      spellcheck="false"
      autocapitalize="off"
      autocomplete="off"
      data-autofocus
      bind:value={dest}
      oninput={() => (destError = null)}
    />
    <button class="btn" onclick={choose}>{t("extract.choose")}</button>
  </div>
  {#if destError}
    <p class="bad" role="alert">{destError}</p>
  {/if}

  {#if batchCount !== null}
    <p class="note">{t("extract.batchCount", { n: formatCount(batchCount) })}</p>
    <fieldset>
      <legend>{t("extract.placement")}</legend>
      <label class="check">
        <input type="radio" name="placement" value="together" bind:group={placement} />
        {t("extract.placeTogether")}
      </label>
      <label class="check">
        <input type="radio" name="placement" value="separate" bind:group={placement} />
        {t("extract.placeSeparate")}
      </label>
    </fieldset>
  {:else if selectedCount > 0}
    <fieldset>
      <legend>{t("extract.scope")}</legend>
      <label class="check">
        <input type="radio" name="scope" value="selected" bind:group={scope} />
        {t("extract.scopeSelected", { n: formatCount(selectedCount) })}
      </label>
      <label class="check">
        <input type="radio" name="scope" value="folder" bind:group={scope} />
        <span class="wrap">{cwd === "" ? t("extract.scopeArchive") : t("extract.scopeFolder", { name: folderName })}</span>
      </label>
    </fieldset>
  {:else}
    <p class="note">{cwd === "" ? t("extract.noneInArchive") : t("extract.noneInFolder", { name: folderName })}</p>
  {/if}

  <div class="row">
    <label class="side" for="extract-overwrite">{t("extract.overwrite")}</label>
    <select id="extract-overwrite" class="field grow" bind:value={overwrite}>
      {#each policies as p (p.value)}
        <option value={p.value}>{t(p.key)}</option>
      {/each}
    </select>
  </div>

  <div class="checks">
    <label class="check"><input type="checkbox" bind:checked={keepPaths} />{t("extract.keepPaths")}</label>
    <label class="check"><input type="checkbox" bind:checked={keepBroken} />{t("extract.keepBroken")}</label>
  </div>

  {#snippet footer()}
    <button class="btn" onclick={() => onDone(null)}>{t("common.cancel")}</button>
    <button class="btn primary" disabled={!dest.trim() || checking} onclick={submit}>{t("extract.start")}</button>
  {/snippet}
</Modal>

<style>
  .block {
    font-size: 12px;
    color: var(--ink-2);
    margin-bottom: -6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .side {
    flex: none;
    font-size: 12.5px;
  }
  .bad {
    margin: 0;
    padding: 5px 8px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-ink);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .note {
    margin: 0;
    color: var(--ink-2);
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 5px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .wrap {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
  }
</style>
