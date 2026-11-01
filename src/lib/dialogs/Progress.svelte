<script lang="ts">
  import { safeName, progressView } from "../flows.ts";
  import { formatBytes, formatCount } from "../format.ts";
  import { t } from "../i18n/index.ts";
  import Modal from "./Modal.svelte";
  import type { ProgressState } from "./store.svelte.ts";

  let { state: job, onCancel }: { state: ProgressState; onCancel: () => void } = $props();

  const view = $derived(progressView(job.ev));
</script>

<!-- Nothing has the focus at the start: Enter must not cancel by accident. Esc is İptal. -->
<Modal
  title={job.batch
    ? t(job.op === "extract" ? "progress.extracting" : "progress.testing")
    : `${t(job.op === "extract" ? "progress.extracting" : "progress.testing")}: ${safeName(job.name)}`}
  icon={job.op === "extract" ? "extract" : "test"}
  width={460}
  focusPanel
  onEscape={onCancel}
>
  {#if job.batch}
    <div class="overall">
      <span class="count">{t("progress.batchCount", { i: formatCount(job.batch.index), n: formatCount(job.batch.total) })}</span>
      <span class="archive" title={safeName(job.name)}>{safeName(job.name)}</span>
    </div>
  {/if}
  <div class="current" title={safeName(view.current)}>
    {view.current ? safeName(view.current) : t("progress.waiting")}
  </div>
  <div
    class="rail"
    class:indeterminate={view.percent === null}
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={view.percent ?? undefined}
  >
    <div class="fill" style:transform={view.percent === null ? undefined : `scaleX(${view.percent / 100})`}></div>
  </div>
  <div class="numbers">
    <span>{view.total > 0 ? t("progress.bytes", { done: formatBytes(view.done), total: formatBytes(view.total) }) : ""}</span>
    <span>{view.percent === null ? "" : t("progress.percent", { n: view.percent })}</span>
  </div>
  {#snippet footer()}
    <button class="btn" disabled={job.cancelling} onclick={onCancel}>
      {job.cancelling ? t("progress.cancelling") : t("progress.cancel")}
    </button>
  {/snippet}
</Modal>

<style>
  .overall {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
    font-size: 13px;
  }
  .count {
    flex: none;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .archive {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .current {
    height: 18px;
    font-size: 12.5px;
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rail {
    position: relative;
    height: 6px;
    border-radius: 3px;
    background: var(--rail);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transform: scaleX(0);
  }
  .indeterminate .fill {
    width: 34%;
    border-radius: 3px;
    transform: none;
    animation: slide 1.3s ease-in-out infinite alternate;
  }
  @keyframes slide {
    from {
      margin-left: 0;
    }
    to {
      margin-left: 66%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .indeterminate .fill {
      width: 100%;
      opacity: 0.45;
      animation: none;
    }
  }
  .numbers {
    display: flex;
    justify-content: space-between;
    height: 16px;
    font-size: 11.5px;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
  }
</style>
