<script lang="ts" module>
  export type ToolAction = "extract" | "test" | "show" | "info";
</script>

<script lang="ts">
  import { t } from "./i18n/index.ts";
  import Icon from "./Icon.svelte";

  let {
    can,
    onAction,
  }: {
    /** Which actions make sense right now. */
    can: Record<ToolAction, boolean>;
    onAction: (a: ToolAction) => void;
  } = $props();

  type Tool = { id: "add" | "delete" | "repair" | ToolAction; icon: string; key: string; gapBefore?: boolean };
  const tools: Tool[] = [
    { id: "add", icon: "add", key: "toolbar.add" },
    { id: "extract", icon: "extract", key: "toolbar.extract" },
    { id: "test", icon: "test", key: "toolbar.test", gapBefore: true },
    { id: "show", icon: "show", key: "toolbar.show" },
    { id: "delete", icon: "delete", key: "toolbar.delete", gapBefore: true },
    { id: "repair", icon: "repair", key: "toolbar.repair" },
    { id: "info", icon: "info", key: "toolbar.info", gapBefore: true },
  ];

  const isStage2 = (id: Tool["id"]) => id === "add" || id === "delete" || id === "repair";
</script>

<div class="toolbar" role="toolbar" aria-label={t("toolbar.label")}>
  {#each tools as x (x.id)}
    {#if x.gapBefore}<span class="gap"></span>{/if}
    {@const stage2 = isStage2(x.id)}
    {@const off = stage2 || !can[x.id as ToolAction]}
    <!-- aria-disabled rather than disabled: a disabled button shows no tooltip in WebKit. -->
    <button
      class="tool"
      class:off
      aria-disabled={off}
      tabindex="-1"
      title={stage2 ? t("toolbar.stage2") : t(x.key)}
      onmousedown={(e) => e.preventDefault()}
      onclick={() => {
        if (!off) onAction(x.id as ToolAction);
      }}
    >
      <Icon name={x.icon} />
      <span class="caption">{t(x.key)}</span>
    </button>
  {/each}
</div>

<style>
  .toolbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
    height: 50px;
    padding: 0 10px;
    background: var(--chrome);
    border-bottom: 1px solid var(--line);
  }
  .tool {
    width: 58px;
    height: 42px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    border-radius: 6px;
    color: var(--ink);
  }
  .tool:not(.off):hover {
    background: var(--hover);
  }
  .tool:not(.off):active {
    background: var(--press);
  }
  .tool.off {
    color: var(--ink-3);
  }
  .tool.off :global(.icon) {
    opacity: 0.6;
  }
  .caption {
    font-size: 11px;
    white-space: nowrap;
  }
  .gap {
    width: 1px;
    height: 26px;
    margin: 0 6px;
    background: var(--line);
  }
</style>
