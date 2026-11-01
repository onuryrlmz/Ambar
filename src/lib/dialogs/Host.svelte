<script lang="ts">
  import About from "./About.svelte";
  import Conflict from "./Conflict.svelte";
  import Extract from "./Extract.svelte";
  import Info from "./Info.svelte";
  import Password from "./Password.svelte";
  import Progress from "./Progress.svelte";
  import Result from "./Result.svelte";
  import Risky from "./Risky.svelte";
  import { cancelProgress, currentDialog, currentProgress } from "./store.svelte.ts";

  const dialog = $derived(currentDialog());
  const progress = $derived(currentProgress());
</script>

<!-- Keyed on the request, so a repeated prompt starts empty. -->
{#if dialog}
  {#key dialog}
    {#if dialog.kind === "password"}
      <Password name={dialog.name} wrong={dialog.wrong} onDone={dialog.done} />
    {:else if dialog.kind === "extract"}
      <Extract cwd={dialog.cwd} selectedCount={dialog.selectedCount} dest={dialog.dest} onDone={dialog.done} />
    {:else if dialog.kind === "conflict"}
      <Conflict path={dialog.path} onDone={dialog.done} />
    {:else if dialog.kind === "risky"}
      <Risky name={dialog.name} onDone={dialog.done} />
    {:else if dialog.kind === "result"}
      <Result op={dialog.op} name={dialog.name} summary={dialog.summary} onDone={dialog.done} />
    {:else if dialog.kind === "info"}
      <Info path={dialog.path} info={dialog.info} onDone={dialog.done} />
    {:else if dialog.kind === "about"}
      <About onDone={dialog.done} />
    {/if}
  {/key}
{:else if progress}
  <Progress state={progress} onCancel={cancelProgress} />
{/if}
