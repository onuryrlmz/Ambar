<script lang="ts" module>
  // One hand-drawn set: 16 unit grid for list and navigation glyphs, 20 for the toolbar,
  // 1.3 stroke, round joins. Add and Extract share one crate, and their arrows point
  // along the real direction between the panes: local on the left, archive on the right.
  const crate = "M8.5 3.5h9.5v3H8.5zM9.5 6.5v10.5h7.5V6.5M12 9.5h2.5";
  const paths: Record<string, { box: 16 | 20; d: string; filled?: boolean }> = {
    folder: {
      box: 16,
      filled: true,
      d: "M1.5 4.25C1.5 3.56 2.06 3 2.75 3h3.1c.33 0 .65.13.88.37L7.9 4.5h5.35c.69 0 1.25.56 1.25 1.25v6.5c0 .69-.56 1.25-1.25 1.25H2.75c-.69 0-1.25-.56-1.25-1.25z",
    },
    file: { box: 16, d: "M4 1.75h5l3.25 3.5v9H4zM9 1.75v3.5h3.25" },
    archive: { box: 16, d: "M2 3h12v3H2zM3 6v7.25h10V6M6.25 8.5h3.5" },
    computer: { box: 16, d: "M2 3h12v8H2zM5.5 14h5M8 11v3" },
    link: { box: 16, d: "M6.5 9.5l4-4M6.5 5.5h4v4" },
    lock: { box: 16, d: "M4 7.25h8v6.25H4zM5.75 7.25V5a2.25 2.25 0 0 1 4.5 0v2.25" },
    up: { box: 16, d: "M8 13V3.5M3.75 7.75 8 3.5l4.25 4.25" },
    home: { box: 16, d: "M2.25 8 8 2.75 13.75 8M4 6.75v6.75h3V10h2v3.5h3V6.75" },
    chevron: { box: 16, d: "M6 3.5 10.5 8 6 12.5" },
    sortUp: { box: 16, d: "M4 10 8 6l4 4" },
    sortDown: { box: 16, d: "M4 6l4 4 4-4" },
    close: { box: 16, d: "M4.5 4.5l7 7M11.5 4.5l-7 7" },
    alert: { box: 16, d: "M8 2.25 14.5 13.5h-13zM8 6.5v3.5M8 11.75v.25" },
    add: { box: 20, d: crate + "M1.5 11.5h9M7.5 8.5l3 3-3 3" },
    extract: { box: 20, d: crate + "M11 11.5H1.5M4.5 8.5l-3 3 3 3" },
    test: { box: 20, d: "M10 2.75a7.75 7.75 0 1 0 0 15.5 7.75 7.75 0 0 0 0-15.5zM6.5 10.75l2.5 2.5 4.75-5.25" },
    show: { box: 20, d: "M1.75 10.5S4.75 5 10 5s8.25 5.5 8.25 5.5S15.25 16 10 16 1.75 10.5 1.75 10.5zM10 8a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z" },
    delete: { box: 20, d: "M3.5 5.5h13M8 5.5v-2h4v2M5 5.5l.75 12h8.5L15 5.5M8.5 8.5v6M11.5 8.5v6" },
    repair: { box: 20, d: "M13.3 2.9a4.6 4.6 0 0 0-5 6.3l-5.1 5.1a1.9 1.9 0 0 0 2.7 2.7L11 11.9a4.6 4.6 0 0 0 6.2-5.3l-2.9 2.9-2.8-.8-.7-2.8z" },
    info: { box: 20, d: "M10 2.75a7.75 7.75 0 1 0 0 15.5 7.75 7.75 0 0 0 0-15.5zM10 9.5v5M10 6.5v.25" },
  };
</script>

<script lang="ts">
  let { name, size }: { name: keyof typeof paths; size?: number } = $props();
  const p = $derived(paths[name]);
</script>

<svg
  class="icon"
  viewBox="0 0 {p.box} {p.box}"
  width={size ?? p.box}
  height={size ?? p.box}
  aria-hidden="true"
  fill={p.filled ? "currentColor" : "none"}
  stroke={p.filled ? "none" : "currentColor"}
  stroke-width="1.3"
  stroke-linecap="round"
  stroke-linejoin="round"
>
  <path d={p.d} />
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>
