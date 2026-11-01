<script lang="ts">
  import { safeName } from "../flows.ts";
  import { t } from "../i18n/index.ts";
  import Modal from "./Modal.svelte";

  let { name, onDone }: { name: string; onDone: (yes: boolean) => void } = $props();
</script>

<Modal
  title={t("open.riskyTitle")}
  icon="alert"
  tone="warn"
  onEscape={() => onDone(false)}
  onEnter={() => onDone(false)}
>
  <p class="text">{t("open.riskyMessage", { name: safeName(name) })}</p>
  {#snippet footer()}
    <button class="btn" onclick={() => onDone(true)}>{t("open.openAnyway")}</button>
    <button class="btn primary" data-autofocus onclick={() => onDone(false)}>{t("common.cancel")}</button>
  {/snippet}
</Modal>

<style>
  .text {
    margin: 0;
    max-height: 160px;
    overflow-y: auto;
    overflow-wrap: anywhere;
  }
</style>
