<script lang="ts">
  import type { ConflictAnswer } from "../flows.ts";
  import { safeName } from "../flows.ts";
  import { t } from "../i18n/index.ts";
  import type { ConflictChoice } from "../types.ts";
  import Modal from "./Modal.svelte";

  let { path, onDone }: { path: string; onDone: (a: ConflictAnswer) => void } = $props();

  let applyAll = $state(false);

  const answer = (choice: ConflictChoice) => onDone({ choice, applyAll });
</script>

<!-- The safe answer is the default: Enter never overwrites, and Esc stops the job. -->
<Modal
  title={t("conflict.title")}
  icon="alert"
  tone="warn"
  width={500}
  onEscape={() => answer("cancel")}
  onEnter={() => answer("skip")}
>
  <p class="text">{t("conflict.message", { path: safeName(path) })}</p>
  <label class="check"><input type="checkbox" bind:checked={applyAll} />{t("conflict.applyAll")}</label>
  {#snippet footer()}
    <button class="btn" onclick={() => answer("cancel")}>{t("progress.cancel")}</button>
    <span class="push"></span>
    <button class="btn" onclick={() => answer("rename")}>{t("extract.rename")}</button>
    <button class="btn" onclick={() => answer("overwrite")}>{t("extract.overwriteAll")}</button>
    <button class="btn primary" data-autofocus onclick={() => answer("skip")}>{t("extract.skip")}</button>
  {/snippet}
</Modal>

<style>
  .text {
    margin: 0;
    max-height: 120px;
    overflow-y: auto;
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
    cursor: auto;
  }
  .push {
    flex: 1;
  }
</style>
