<script lang="ts">
  import type { BatchRow, BatchSummary } from "../batch.ts";
  import { safeName } from "../flows.ts";
  import { formatCount } from "../format.ts";
  import { errorMessage, t } from "../i18n/index.ts";
  import VirtualList from "../VirtualList.svelte";
  import Modal from "./Modal.svelte";

  let { summary, onDone }: { summary: BatchSummary; onDone: () => void } = $props();

  const TITLES = {
    ok: "batch.titleOk",
    partial: "batch.titlePartial",
    failed: "batch.titleFailed",
    cancelled: "batch.titleCancelled",
  };

  const lines = $derived.by(() => {
    const count = (n: number) => formatCount(n);
    const out = [t("batch.okArchives", { n: count(summary.okArchives), total: count(summary.total) })];
    if (summary.failedArchives > 0) out.push(t("batch.failedArchives", { n: count(summary.failedArchives) }));
    if (summary.dismissedArchives > 0) out.push(t("batch.dismissedArchives", { n: count(summary.dismissedArchives) }));
    if (summary.notFinished > 0) out.push(t("batch.notFinished", { n: count(summary.notFinished) }));
    if (summary.joinedParts > 0) out.push(t("batch.joined", { n: count(summary.joinedParts) }));
    if (summary.ok > 0) out.push(t("result.extracted", { n: count(summary.ok) }));
    if (summary.skipped > 0) out.push(t("result.skipped", { n: count(summary.skipped) }));
    if (summary.failed > 0) out.push(t("result.failedCount", { n: count(summary.failed) }));
    if (summary.kind === "cancelled" && summary.mayHaveWritten) out.push(t("result.partialNote"));
    return out;
  });

  const rowMessage = (r: BatchRow): string => {
    if (r.dismissed) return t("batch.dismissed");
    if (r.volumeOf !== undefined) return t("batch.joinedRow", { first: safeName(r.volumeOf) });
    if (r.more !== undefined) return t("batch.more", { n: formatCount(r.more) });
    return r.error ? errorMessage(r.error) : "";
  };

  const ROW = 40;
  /** Enough to read, and a long list scrolls inside instead of growing the dialog. */
  const VISIBLE_ROWS = 7;
  const listH = $derived(Math.min(summary.rows.length, VISIBLE_ROWS) * ROW);
</script>

<Modal
  title={t(TITLES[summary.kind])}
  icon={summary.kind === "ok" ? "test" : "alert"}
  tone={summary.kind === "ok" ? "good" : "warn"}
  width={summary.rows.length > 0 ? 600 : 420}
  onEscape={onDone}
  onEnter={onDone}
>
  <div class="lines">
    {#each lines as line (line)}
      <p>{line}</p>
    {/each}
  </div>

  {#if summary.rows.length > 0}
    <div class="caption">{t("result.details")}</div>
    <div class="rows" style:height="{listH}px">
      <VirtualList items={summary.rows} rowHeight={ROW} overscan={4}>
        {#snippet row(r: BatchRow, _i: number)}
          <div class="row">
            <div class="top">
              <span class="archive" title={safeName(r.archive)}>{safeName(r.archive)}</span>
              {#if r.path === null}<span class="message">{rowMessage(r)}</span>{/if}
            </div>
            {#if r.path !== null}
              <div class="entry">
                <span class="path" title={safeName(r.path)}>{safeName(r.path)}</span>
                <span class="message">{rowMessage(r)}</span>
              </div>
            {/if}
          </div>
        {/snippet}
      </VirtualList>
    </div>
  {/if}

  {#snippet footer()}
    <button class="btn primary" data-autofocus onclick={onDone}>{t("common.ok")}</button>
  {/snippet}
</Modal>

<style>
  .lines p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .lines p + p {
    margin-top: 3px;
  }
  .caption {
    margin-bottom: -5px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .rows {
    flex: none;
    min-height: 0;
    background: var(--surface);
    border-radius: 6px;
    box-shadow: inset 0 0 0 1px var(--line-soft);
    overflow: hidden;
    user-select: text;
    -webkit-user-select: text;
    cursor: auto;
  }
  .row {
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding: 0 9px;
  }
  .top,
  .entry {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .archive {
    flex: 1;
    min-width: 0;
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .path {
    flex: 1;
    min-width: 0;
    font-size: 11.5px;
    color: var(--ink-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .message {
    flex: none;
    max-width: 55%;
    font-size: 11.5px;
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
