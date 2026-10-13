<script>
  import { mediaUrl } from './lib.js';
  import { colorHex } from './store.svelte.js';

  let { item, onopen, onselect = () => {}, selected = false, thumbH = 160, cellW = 176 } = $props();

  let thumbSrc = $derived(item.thumbPath ? mediaUrl(item.thumbPath) : null);
  let broken = $state(false);

  const name = $derived(item.path.split(/[\\/]/).pop() || item.path);
  const date = $derived((item.takenAt || '').replace(':', '-'));
  const year = $derived((item.takenAt || '').slice(0, 4));
  const isVideo = $derived(item.kind === 'video');

  function fmtBytes(b) {
    if (!b) return '';
    if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
    if (b >= 1e3) return Math.round(b / 1e3) + ' KB';
    return b + ' B';
  }
</script>

<button
  class="tile"
  class:missing={item.missing}
  class:sel={selected}
  style="width: {cellW}px;"
  onclick={(e) => onselect(e)}
  ondblclick={() => onopen(item.id)}
  title={item.path + (item.tags ? '\nTags: ' + item.tags : '')}
>
  <div class="thumb" style="height: {thumbH}px;">
    {#if thumbSrc && !broken && (!item.missing || item.deleted)}
      <img src={thumbSrc} loading="lazy" alt="" onerror={() => (broken = true)} draggable="false" />
    {:else}
      <div class="placeholder">
        {#if item.deleted}
          <span class="ph-icon">🗑</span>
          <span class="ph-label">in Recycle Bin</span>
        {:else if item.missing}
          <span class="ph-icon">⚠</span>
          <span class="ph-label">missing</span>
        {:else if isVideo}
          <span class="ph-icon">▶</span>
          <span class="ph-label">video</span>
        {:else}
          <span class="ph-icon">▣</span>
          <span class="ph-label">no preview</span>
        {/if}
      </div>
    {/if}

    {#if isVideo && !item.missing}
      <span class="badge kind">▶</span>
    {/if}
    {#if item.inDupGroup && item.copies > 1}
      <span class="badge dup" title="{item.copies} identical copies on disk">×{item.copies}</span>
    {/if}
    {#if item.deleted}
      <span class="badge miss">deleted</span>
    {:else if item.missing}
      <span class="badge miss">missing</span>
    {/if}
    <span class="check" aria-hidden="true">{selected ? '✓' : ''}</span>
    <div class="marks">
      {#if item.aiPending}<span class="aip" title="{item.aiPending} AI suggestion(s) waiting for approval">✨{item.aiPending}</span>{/if}
      {#if item.colorLabel}<span class="cl" style="background: {colorHex(item.colorLabel)}"></span>{/if}
      {#if item.favorite}<span class="fv">♥</span>{/if}
      {#if item.hasNotes}<span class="nt" title="has notes">✎</span>{/if}
      {#if item.tags}<span class="tg" title={item.tags}>#</span>{/if}
      {#if item.rating}<span class="rt">{'★'.repeat(item.rating)}</span>{/if}
    </div>
  </div>

  <div class="caption">
    <div class="name" title={name}>{name}</div>
    <div class="meta">
      <span>{date}</span>
      {#if item.width}<span>{item.width}×{item.height}</span>{/if}
      <span>{fmtBytes(item.bytes)}</span>
    </div>
  </div>
</button>

<style>
  .tile {
    appearance: none; border: 0; background: var(--panel); color: inherit;
    padding: 6px; border-radius: 10px; cursor: pointer; text-align: left;
    display: flex; flex-direction: column; gap: 6px; flex: 0 0 auto;
    border: 1px solid var(--border);
  }
  .tile:hover { border-color: #4a5260; background: var(--panel2); }
  .tile.sel { border-color: var(--accent); background: #2a2016; box-shadow: 0 0 0 1px var(--accent); }
  .check {
    position: absolute; left: 6px; bottom: 6px; width: 18px; height: 18px; border-radius: 50%;
    border: 1.5px solid rgba(255,255,255,0.7); background: rgba(10,12,16,0.45); color: #1a1206;
    font-size: 12px; font-weight: 700; display: flex; align-items: center; justify-content: center;
    opacity: 0; transition: opacity 0.1s;
  }
  .tile:hover .check { opacity: 1; }
  .tile.sel .check { opacity: 1; background: var(--accent); border-color: var(--accent); }
  .marks {
    position: absolute; right: 6px; bottom: 6px; display: flex; gap: 4px; align-items: center;
    background: rgba(10,12,16,0.72); border-radius: 9px; padding: 1px 6px; font-size: 11px;
  }
  .marks:empty { display: none; }
  .cl { width: 9px; height: 9px; border-radius: 50%; }
  .fv { color: #e5484d; }
  .aip { color: #ffe7b0; font-size: 10.5px; }
  .nt { color: #cfe0ff; }
  .tg { color: #8ab4f8; font-weight: 700; }
  .rt { color: #f5c518; letter-spacing: -1px; }
  .tile:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .tile.missing { opacity: 0.65; }
  .tile.missing img { filter: grayscale(0.7); }
  .thumb {
    position: relative; width: 100%; border-radius: 7px; overflow: hidden;
    background: #0a0c10;
  }
  .thumb img {
    width: 100%; height: 100%; object-fit: cover; display: block;
    pointer-events: none; user-select: none;
  }
  .placeholder {
    width: 100%; height: 100%; display: flex; flex-direction: column;
    align-items: center; justify-content: center; gap: 6px;
    background: repeating-linear-gradient(45deg, #12151b, #12151b 10px, #0e1116 10px, #0e1116 20px);
    color: var(--muted);
  }
  .ph-icon { font-size: 30px; line-height: 1; color: #5a6270; }
  .ph-label { font-size: 11px; letter-spacing: 0.4px; }
  .badge {
    position: absolute; top: 6px; font-size: 11px; font-weight: 600;
    padding: 2px 7px; border-radius: 9px; backdrop-filter: blur(3px);
  }
  .badge.kind { right: 6px; background: rgba(10,12,16,0.7); color: #fff; }
  .badge.dup { left: 6px; background: rgba(244,123,32,0.9); color: #1a1206; }
  .badge.miss { right: 6px; background: rgba(240,80,60,0.85); color: #fff; }
  .caption { padding: 0 2px; }
  .name {
    font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
    color: var(--text);
  }
  .meta {
    display: flex; gap: 8px; margin-top: 2px; font-size: 11px; color: var(--muted);
    white-space: nowrap; overflow: hidden; font-variant-numeric: tabular-nums;
  }
</style>
