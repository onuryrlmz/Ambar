<script lang="ts">
  import { onDestroy } from "svelte";
  import { safeName } from "../flows.ts";
  import { t } from "../i18n/index.ts";
  import Icon from "../Icon.svelte";
  import Modal from "./Modal.svelte";

  let {
    name,
    wrong,
    onDone,
  }: {
    name: string;
    wrong: boolean;
    /** The only way out for the password. Null on cancel. */
    onDone: (password: string | null) => void;
  } = $props();

  let value = $state("");

  function submit() {
    if (!value) return;
    const given = value;
    value = "";
    onDone(given);
  }

  function cancel() {
    value = "";
    onDone(null);
  }

  onDestroy(() => {
    value = "";
  });
</script>

<Modal title={t("password.title")} icon="lock" onEscape={cancel} onEnter={submit}>
  <p class="text">{t("password.prompt", { name: safeName(name) })}</p>
  {#if wrong}
    <p class="wrong" role="alert"><Icon name="alert" size={13} />{t("password.wrong")}</p>
  {/if}
  <input
    class="field"
    type="password"
    aria-label={t("password.label")}
    placeholder={t("password.label")}
    autocomplete="off"
    autocapitalize="off"
    spellcheck="false"
    data-autofocus
    bind:value
  />
  {#snippet footer()}
    <button class="btn" onclick={cancel}>{t("common.cancel")}</button>
    <button class="btn primary" disabled={!value} onclick={submit}>{t("common.ok")}</button>
  {/snippet}
</Modal>

<style>
  .text {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .wrong {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 5px 8px;
    border-radius: 6px;
    background: var(--warn-bg);
    color: var(--warn-ink);
    font-size: 12px;
  }
  .wrong :global(.icon) {
    color: var(--warn-icon);
  }
</style>
