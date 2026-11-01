<script lang="ts">
  import { onMount, tick, type Snippet } from "svelte";
  import Icon from "../Icon.svelte";

  let {
    title,
    icon,
    tone,
    width = 420,
    labelId = "modal-title",
    focusPanel = false,
    onEscape,
    onEnter,
    children,
    footer,
  }: {
    title: string;
    icon?: string;
    tone?: "warn" | "good" | "plain";
    width?: number;
    labelId?: string;
    /** Start on the panel, not a control: Enter must not press anything by accident. */
    focusPanel?: boolean;
    onEscape?: () => void;
    /** Enter anywhere but on a button, which Enter simply presses. */
    onEnter?: () => void;
    children: Snippet;
    footer?: Snippet;
  } = $props();

  const FOCUSABLE =
    'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, [tabindex="0"]';

  let panel = $state<HTMLDivElement>();

  const focusables = () => [...(panel?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])];

  onMount(() => {
    tick().then(() => {
      const start = focusPanel ? null : (panel?.querySelector<HTMLElement>("[data-autofocus]") ?? focusables()[0]);
      (start ?? panel)?.focus({ preventScroll: true });
    });
  });

  function trapTab(e: KeyboardEvent) {
    e.preventDefault();
    const all = focusables();
    if (all.length === 0) {
      panel?.focus();
      return;
    }
    const at = all.indexOf(document.activeElement as HTMLElement);
    const next = at < 0 ? (e.shiftKey ? all.length - 1 : 0) : (at + (e.shiftKey ? -1 : 1) + all.length) % all.length;
    all[next].focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === "Tab") {
      trapTab(e);
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (!e.repeat) onEscape?.();
    } else if (e.key === "Enter") {
      e.preventDefault();
      // A held key that opened this dialog must not also answer it.
      if (e.repeat) return;
      const target = e.target as HTMLElement;
      if (target instanceof HTMLButtonElement && panel?.contains(target)) target.click();
      else if (!(target instanceof HTMLTextAreaElement)) onEnter?.();
    }
  }
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={(e) => e.target === e.currentTarget && e.preventDefault()}>
  <div
    class="panel"
    bind:this={panel}
    role="dialog"
    aria-modal="true"
    aria-labelledby={labelId}
    tabindex="-1"
    style:width="{width}px"
  >
    <header>
      {#if icon}
        <span class="glyph {tone ?? 'plain'}"><Icon name={icon} size={16} /></span>
      {/if}
      <h2 id={labelId}>{title}</h2>
    </header>
    <div class="content">{@render children()}</div>
    {#if footer}
      <footer>{@render footer()}</footer>
    {/if}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: grid;
    place-items: center;
    padding: 16px;
    background: var(--scrim);
  }
  .panel {
    display: flex;
    flex-direction: column;
    max-width: 100%;
    max-height: 100%;
    background: var(--chrome-raised);
    border-radius: 10px;
    box-shadow: var(--panel-shadow), 0 0 0 0.5px var(--line);
    outline: none;
    overflow: hidden;
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 13px 16px 0;
  }
  h2 {
    margin: 0;
    min-width: 0;
    font-size: 13.5px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .glyph.plain {
    color: var(--crate);
  }
  .glyph.warn {
    color: var(--warn-icon);
  }
  .glyph.good {
    color: var(--good);
  }
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 16px 14px;
    overflow-y: auto;
  }
  footer {
    flex: none;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 16px;
    background: var(--chrome);
    border-top: 1px solid var(--line-soft);
  }
</style>
