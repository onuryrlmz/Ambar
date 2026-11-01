<script lang="ts">
  import { formatBytes, formatCount } from "./format.ts";
  import { t } from "./i18n/index.ts";
  import Icon from "./Icon.svelte";
  import { localModel, localView } from "./local.svelte.ts";
  import { activeModel, activeTab } from "./tabs.svelte.ts";
  import { selectionSummary, type ListModel, type PaneView } from "./tabs-core.ts";

  // Two halves that line up with the panes above them.
  let { split, busy = false }: { split: number; busy?: boolean } = $props();

  function describe(model: ListModel<{ size: number }>, view: PaneView): string {
    const s = selectionSummary(model, view.selection);
    if (s.count > 0) return t("pane.selectedSize", { n: formatCount(s.count), size: formatBytes(s.size) });
    const n = model.rows.length;
    return t(n === 1 ? "pane.item" : "pane.items", { n: formatCount(n) });
  }

  const left = $derived(describe(localModel(), localView()));
  const tab = $derived(activeTab());
  const right = $derived(tab && !tab.loading ? describe(activeModel(), tab) : "");
</script>

<footer class="status" style:grid-template-columns="{split * 100}% 1fr">
  <div class="half"><span class="text">{left}</span></div>
  <div class="half">
    <span class="text">{busy ? t("status.opening") : right}</span>
    {#if tab?.info}
      <span class="facts">
        {#if tab.locked}
          <span class="fact"><Icon name="lock" size={12} />{t("archive.encrypted")}</span>
        {/if}
        <span class="fact">{t(`format.${tab.info.format}`)}</span>
      </span>
    {/if}
  </div>
</footer>

<style>
  .status {
    flex: none;
    display: grid;
    height: 24px;
    background: var(--chrome);
    border-top: 1px solid var(--line);
    font-size: 11.5px;
    color: var(--ink-2);
  }
  .half {
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 0 12px;
  }
  .half + .half {
    border-left: 1px solid var(--line);
  }
  .text {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-variant-numeric: tabular-nums;
  }
  .facts {
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .fact {
    display: flex;
    align-items: center;
    gap: 3px;
  }
</style>
