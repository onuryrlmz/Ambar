<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "./Icon.svelte";
  import type { Crumb } from "./tabs-core.ts";

  let {
    crumbs,
    onGo,
    onBlank,
    lead,
  }: {
    crumbs: Crumb[];
    onGo: (path: string) => void;
    /** A click on the bar itself, beside the segments. */
    onBlank?: () => void;
    lead?: Snippet;
  } = $props();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="crumbs" onclick={(e) => e.target === e.currentTarget && onBlank?.()}>
  {@render lead?.()}
  {#each crumbs as c, i}
    {#if i > 0 || lead}
      <span class="sep"><Icon name="chevron" size={10} /></span>
    {/if}
    {#if c.path === null}
      <span class="more">{c.label}</span>
    {:else}
      {@const path = c.path}
      <button
        class="seg"
        class:last={i === crumbs.length - 1}
        title={c.label}
        tabindex="-1"
        onclick={() => onGo(path)}><span>{c.label}</span></button>
    {/if}
  {/each}
</div>

<style>
  .crumbs {
    flex: 1;
    min-width: 0;
    height: 22px;
    display: flex;
    align-items: center;
    padding-left: 2px;
    font-size: 12px;
    color: var(--ink-2);
    overflow: hidden;
  }
  .sep {
    flex: none;
    color: var(--ink-3);
    margin: 0 1px;
  }
  .more {
    flex: none;
    padding: 0 3px;
  }
  /* Every segment can shrink, the current folder last. */
  .seg {
    flex: 0 1 auto;
    min-width: 18px;
    height: 20px;
    padding: 0 4px;
    border-radius: 4px;
    overflow: hidden;
  }
  .seg span {
    display: block;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .seg:hover {
    background: var(--hover);
    color: var(--ink);
  }
  .seg:active {
    background: var(--press);
  }
  .seg.last {
    flex-shrink: 0;
    max-width: 60%;
    color: var(--ink);
    font-weight: 500;
  }
</style>
