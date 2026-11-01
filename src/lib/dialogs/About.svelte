<script lang="ts">
  import { getVersion } from "@tauri-apps/api/app";
  import { onMount } from "svelte";
  import { t } from "../i18n/index.ts";
  import Icon from "../Icon.svelte";
  import Modal from "./Modal.svelte";

  let { onDone }: { onDone: () => void } = $props();

  let version = $state("");

  onMount(() => {
    getVersion().then(
      (v) => (version = v),
      () => {},
    );
  });
</script>

<Modal title={t("about.title")} width={340} onEscape={onDone} onEnter={onDone}>
  <div class="about">
    <span class="crate"><Icon name="archive" size={44} /></span>
    <div class="name">{t("app.name")}</div>
    <div class="line">{t("app.tagline")}</div>
    {#if version}
      <div class="line">{t("about.version", { v: version })}</div>
    {/if}
    <div class="by">{t("about.by")}</div>
    <div class="line">{t("about.copyright")}</div>
  </div>
  {#snippet footer()}
    <button class="btn primary" data-autofocus onclick={onDone}>{t("common.ok")}</button>
  {/snippet}
</Modal>

<style>
  .about {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: 6px 0 2px;
    text-align: center;
  }
  .crate {
    color: var(--crate);
    margin-bottom: 8px;
  }
  .name {
    font-size: 16px;
    font-weight: 600;
  }
  .by {
    margin-top: 10px;
  }
  .line {
    font-size: 12px;
    color: var(--ink-2);
  }
</style>
