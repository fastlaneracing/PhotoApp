<script>
  import { onMount } from 'svelte';
  import Tile from './Tile.svelte';

  let {
    items = [],
    onopen = () => {},
    onselect = () => {},
    oncontext = () => {},
    selected = new Set(),
    size = 176,
    scrollToIndex = null,
  } = $props();

  // Fixed cell geometry -> virtualize by row. Only the visible rows (plus
  // overscan) become DOM nodes, so 10,000+ items stay cheap to scroll.
  const GAP = 12;
  const CAPTION = 42;
  const OVERSCAN = 3;
  let CELL_W = $derived(size);
  let THUMB_H = $derived(Math.round(size * 0.91));
  let ROW_H = $derived(THUMB_H + CAPTION + GAP);

  let scroller = $state(null);
  let scrollTop = $state(0);
  let viewportH = $state(720);
  let viewportW = $state(1280);

  let cols = $derived(Math.max(1, Math.floor((viewportW + GAP) / (CELL_W + GAP))));
  let totalRows = $derived(Math.ceil(items.length / cols));
  let firstRow = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  let lastRow = $derived(Math.min(totalRows - 1, Math.ceil((scrollTop + viewportH) / ROW_H) + OVERSCAN));
  let visible = $derived.by(() => {
    const out = [];
    for (let r = firstRow; r <= lastRow; r++) {
      if (r < 0 || r >= totalRows) continue;
      const rowItems = items.slice(r * cols, r * cols + cols);
      out.push({ r, rowItems, top: r * ROW_H });
    }
    return out;
  });
  let totalH = $derived(totalRows * ROW_H);
  
  function measure() {
    if (!scroller) return;
    viewportH = scroller.clientHeight;
    viewportW = scroller.clientWidth;
  }

  onMount(() => {
    measure();
    const ro = new ResizeObserver(measure);
    if (scroller) ro.observe(scroller);
    return () => ro.disconnect();
  });

  function onScroll() {
    if (scroller) scrollTop = scroller.scrollTop;
  }

  // Keep a keyboard-moved selection in view.
  $effect(() => {
    if (scrollToIndex == null || !scroller) return;
    const row = Math.floor(scrollToIndex / cols);
    const top = row * ROW_H;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + ROW_H > scroller.scrollTop + viewportH) scroller.scrollTop = top + ROW_H - viewportH;
  });
</script>

<div class="scroller" bind:this={scroller} onscroll={onScroll} onclick={(e) => { if (e.target === e.currentTarget || e.target.classList.contains('space')) onselect(null, -1, e); }}>
  <div class="space" style="height: {totalH}px; position: relative;">
    {#each visible as v (v.r)}
      <div class="row" style="position: absolute; top: {v.top}px; left: 0; right: 0; display: flex; gap: {GAP}px; padding: 0 16px;">
        {#each v.rowItems as it, j (it.id)}
          <Tile
            item={it}
            {onopen}
            onselect={(e) => onselect(it.id, v.r * cols + j, e)}
            oncontext={(e) => oncontext(it.id, v.r * cols + j, e)}
            selected={selected.has(it.id)}
            thumbH={THUMB_H}
            cellW={CELL_W}
          />
        {/each}
      </div>
    {/each}
  </div>
</div>

<div class="foot">
  {items.length.toLocaleString()} items{selected.size ? ` · ${selected.size.toLocaleString()} selected` : ''}
</div>

<style>
  .scroller {
    height: calc(100% - 25px); overflow-y: auto; overflow-x: hidden; background: var(--bg);
    scroll-behavior: auto;
  }
  .space { will-change: height; }
  .row { box-sizing: border-box; }
  .foot {
    padding: 6px 16px; font-size: 11px; color: var(--muted);
    background: var(--panel); border-top: 1px solid var(--border);
    font-variant-numeric: tabular-nums;
  }
</style>
