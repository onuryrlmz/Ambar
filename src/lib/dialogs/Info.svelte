<script lang="ts">
  import { safeName, safeText } from "../flows.ts";
  import { formatBytes } from "../format.ts";
  import { t } from "../i18n/index.ts";
  import type { ArchiveInfo } from "../types.ts";
  import Modal from "./Modal.svelte";

  let { path, info, onDone }: { path: string; info: ArchiveInfo; onDone: () => void } = $props();

  const yesNo = (v: boolean) => t(v ? "common.yes" : "common.no");

  const rows = $derived<[string, string][]>([
    [t("info.format"), t(`format.${info.format}`)],
    [t("info.fileSize"), formatBytes(info.fileSize)],
    [t("info.solid"), yesNo(info.solid)],
    [t("info.encryptedHeaders"), yesNo(info.encryptedHeaders)],
    [t("info.multiVolume"), yesNo(info.multiVolume)],
    [t("info.recovery"), yesNo(info.hasRecovery)],
    [t("info.locked"), yesNo(info.locked)],
  ]);
</script>

<Modal title={t("info.title")} icon="info" width={460} onEscape={onDone} onEnter={onDone}>
  <div class="path">{safeName(path)}</div>
  <dl>
    {#each rows as [label, value] (label)}
      <dt>{label}</dt>
      <dd>{value}</dd>
    {/each}
  </dl>
  {#if info.comment}
    <div class="caption">{t("info.comment")}</div>
    <div class="comment">{safeText(info.comment)}</div>
  {/if}
  {#snippet footer()}
    <button class="btn primary" data-autofocus onclick={onDone}>{t("common.ok")}</button>
  {/snippet}
</Modal>

<style>
  .path {
    font-size: 12px;
    color: var(--ink-2);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
    cursor: auto;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    margin: 0;
  }
  dt {
    color: var(--ink-2);
  }
  dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  .caption {
    margin-bottom: -5px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .comment {
    max-height: 140px;
    padding: 6px 8px;
    border-radius: 6px;
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line-soft);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    overflow-y: auto;
    user-select: text;
    -webkit-user-select: text;
    cursor: auto;
  }
</style>
