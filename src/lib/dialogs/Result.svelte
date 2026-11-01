<script lang="ts">
  import { safeName, type JobSummary } from "../flows.ts";
  import { formatCount } from "../format.ts";
  import { errorMessage, errorText, t } from "../i18n/index.ts";
  import Icon from "../Icon.svelte";
  import type { FileFailure } from "../types.ts";
  import VirtualList from "../VirtualList.svelte";
  import Modal from "./Modal.svelte";
  import type { ResultOp } from "./store.svelte.ts";

  let {
    op,
    name,
    summary,
    onDone,
  }: {
    op: ResultOp;
    name: string;
    summary: JobSummary;
    onDone: () => void;
  } = $props();

  let details = $state(false);

  const title = $derived.by(() => {
    if (op === "open") return t("result.openFailed", { name: safeName(name) });
    const k = summary.kind;
    if (op === "test") {
      if (k === "ok") return t("done.testOk");
      if (k === "partial") return t("result.testPartial");
      return t(k === "cancelled" ? "result.testCancelled" : "result.testFailed");
    }
    if (k === "ok") return t("result.extractOk");
    if (k === "partial") return t("result.extractPartial");
    return t(k === "cancelled" ? "result.extractCancelled" : "result.extractFailed");
  });

  const icon = $derived(summary.kind === "ok" ? "test" : "alert");
  const tone = $derived(summary.kind === "ok" ? "good" : "warn");

  const lines = $derived.by(() => {
    const out: string[] = [];
    if (summary.kind === "cancelled") {
      out.push(errorText("cancelled"));
      if (op === "extract" && summary.mayHaveWritten) out.push(t("result.partialNote"));
      return out;
    }
    if (summary.error) {
      out.push(errorMessage(summary.error));
      return out;
    }
    if (summary.ok > 0) {
      out.push(t(op === "test" ? "result.tested" : "result.extracted", { n: formatCount(summary.ok) }));
    }
    if (summary.skipped > 0) out.push(t("result.skipped", { n: formatCount(summary.skipped) }));
    if (summary.failures.length > 0) out.push(t("result.failedCount", { n: formatCount(summary.failures.length) }));
    if (out.length === 0) out.push(t("result.nothing"));
    return out;
  });

  const hasDetails = $derived(
    summary.kind !== "cancelled" &&
      (!!summary.error?.detail || summary.failures.some((f) => !!f.error.detail)),
  );

  const ROW = 24;
  const ROW_DETAILED = 40;
  /** Enough to read, and a long list scrolls inside instead of growing the dialog. */
  const VISIBLE_ROWS = 8;
  const rowH = $derived(details ? ROW_DETAILED : ROW);
  const listH = $derived(Math.min(summary.failures.length, VISIBLE_ROWS) * rowH);
</script>

<Modal {title} {icon} {tone} width={summary.failures.length > 0 ? 560 : 420} onEscape={onDone} onEnter={onDone}>
  <div class="lines">
    {#each lines as line (line)}
      <p>{line}</p>
    {/each}
  </div>

  {#if summary.failures.length > 0}
    <div class="caption">{t("result.failures")}</div>
    <div class="failures" style:height="{listH}px">
      {#key details}
        <VirtualList items={summary.failures} rowHeight={rowH} overscan={4}>
          {#snippet row(f: FileFailure, _i: number)}
            <div class="failure" class:detailed={details}>
              <div class="top">
                <span class="path" title={safeName(f.path)}>{safeName(f.path)}</span>
                <span class="code">{errorMessage(f.error)}</span>
              </div>
              {#if details}
                <div class="detail">{f.error.detail ? safeName(f.error.detail) : t("result.noDetails")}</div>
              {/if}
            </div>
          {/snippet}
        </VirtualList>
      {/key}
    </div>
  {/if}

  {#if hasDetails}
    <div class="more">
      <button class="toggle" aria-expanded={details} onclick={() => (details = !details)}>
        <span class="chev" class:open={details}><Icon name="chevron" size={11} /></span>{t("result.details")}
      </button>
      {#if details && summary.error}
        <pre>{safeName(summary.error.detail)}</pre>
      {/if}
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
  .failures {
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
  .failure {
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding: 0 9px;
  }
  .top {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .path {
    flex: 1;
    min-width: 0;
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .code {
    flex: none;
    max-width: 55%;
    font-size: 11.5px;
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail {
    font-size: 11px;
    color: var(--ink-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 4px 0 2px;
    border-radius: 4px;
    font-size: 12px;
    color: var(--ink-2);
    outline: none;
  }
  .toggle:hover {
    color: var(--ink);
  }
  .toggle:focus-visible {
    box-shadow: 0 0 0 2px var(--accent);
  }
  .chev {
    display: block;
    transition: transform 0.1s;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  pre {
    max-height: 120px;
    margin: 4px 0 0;
    padding: 6px 8px;
    border-radius: 6px;
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line-soft);
    font: 11.5px/1.35 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    overflow-y: auto;
    user-select: text;
    -webkit-user-select: text;
    cursor: auto;
  }
</style>
