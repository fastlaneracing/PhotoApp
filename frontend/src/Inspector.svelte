<script>
  import { mediaUrl, call, isTauri } from './lib.js';
  import { toast, deleteItems, restoreItems, copyItemsTo } from './store.svelte.js';
  import Annotator from './Annotator.svelte';
  import BulkEditor from './BulkEditor.svelte';
  import ItemAI from './ItemAI.svelte';

  let { ids = [], item = null, selItems = [], onchange = () => {}, onopen = () => {}, stats = null, ondone = () => {} } = $props();

  const name = $derived(item ? item.path.split(/[\\/]/).pop() : '');
  const folder = $derived(item ? item.path.slice(0, item.path.length - name.length).replace(/[\\/]+$/, '') : '');

  async function reveal(open) {
    try {
      await call(open ? 'openFile' : 'reveal', { path: item.path });
    } catch (e) {
      toast(String(e), 'err');
    }
  }
  function fmtBytes(b) {
    if (!b) return '—';
    if (b >= 1e9) return (b / 1e9).toFixed(2) + ' GB';
    if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
    return Math.round(b / 1e3) + ' KB';
  }
</script>

<aside class="insp">
  {#if ids.length === 1 && item}
    <button class="preview" onclick={() => onopen(item.id)} title="Open viewer (Enter)">
      {#if item.thumbPath}<img src={mediaUrl(item.thumbPath)} alt="" />{:else}<div class="noimg">no preview</div>{/if}
    </button>
    <div class="fname" title={item.path}>{name}</div>
    <div class="folder" title={folder}>{folder}</div>
    <div class="meta">
      <span>{item.takenAt || '—'}</span>
      {#if item.width}<span>{item.width}×{item.height}</span>{/if}
      <span>{fmtBytes(item.bytes)}</span>
      {#if item.camera}<span>{item.camera}</span>{/if}
      {#if item.deleted}<span class="warn">in Recycle Bin</span>{:else if item.missing}<span class="warn">not on disk</span>{/if}
    </div>
    <div class="acts">
      <button class="btn" onclick={() => onopen(item.id)}>View</button>
      {#if isTauri()}
        <button class="btn" onclick={() => reveal(false)} disabled={item.missing}>Show in Explorer</button>
        <button class="btn" onclick={() => reveal(true)} disabled={item.missing}>Open file</button>
      {/if}
      <button class="btn" onclick={() => copyItemsTo([item])} disabled={item.missing || item.deleted} title="Copy the file to another folder">Copy to…</button>
      {#if item.deleted}
        <button class="btn restore" onclick={() => restoreItems([item.id], ondone)}>Restore</button>
      {:else if !item.missing}
        <button class="btn del" onclick={() => deleteItems([item], ondone)} title="Move to Recycle Bin (Del)">Delete</button>
      {/if}
    </div>
    <div class="divider"></div>
    <Annotator id={item.id} {onchange} />
    <div class="divider"></div>
    <ItemAI id={item.id} {onchange} />
  {:else if ids.length > 1}
    <BulkEditor {ids} {onchange} items={selItems} {ondone} />
  {:else}
    <div class="empty">
      <div class="big">Select photos to organize</div>
      <ul>
        <li><b>Click</b> to select · <b>Ctrl</b>-click to add · <b>Shift</b>-click for a range</li>
        <li><b>Double-click</b> or <b>Enter</b> opens the viewer</li>
        <li><b>Ctrl+A</b> selects every result · <b>Esc</b> clears</li>
        <li><b>1–5</b> rate · <b>0</b> clears · <b>F</b> favorite · <b>Del</b> deletes</li>
        <li>Notes and tags are searchable from the search box</li>
        <li>Type <code>Client: Smith</code> in a tag box to file a tag under a category</li>
      </ul>
      {#if stats}
        <div class="st">
          <div><b>{(stats.present ?? stats.total).toLocaleString()}</b> on disk</div>
          <div><b>{(stats.tagged ?? 0).toLocaleString()}</b> tagged</div>
          <div><b>{(stats.withNotes ?? 0).toLocaleString()}</b> with notes</div>
          <div><b>{(stats.favorites ?? 0).toLocaleString()}</b> favorites</div>
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .insp {
    width: 330px; flex: 0 0 330px; background: var(--panel); border-left: 1px solid var(--border);
    overflow-y: auto; padding: 14px 16px 40px; box-sizing: border-box;
  }
  .preview { display: block; width: 100%; padding: 0; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; background: #0a0c10; cursor: zoom-in; }
  .preview img { width: 100%; max-height: 240px; object-fit: contain; display: block; }
  .noimg { height: 140px; display: flex; align-items: center; justify-content: center; color: var(--muted); font-size: 12px; }
  .fname { margin-top: 10px; font-weight: 600; font-size: 13.5px; word-break: break-all; }
  .folder { font-size: 11px; color: var(--muted); word-break: break-all; margin-top: 2px; font-family: ui-monospace, Consolas, monospace; }
  .meta { display: flex; flex-wrap: wrap; gap: 4px 10px; margin-top: 6px; font-size: 11.5px; color: var(--muted); }
  .warn { color: #f4a620; }
  .acts { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }
  .btn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    padding: 5px 10px; border-radius: 7px; cursor: pointer; font-size: 12px;
  }
  .btn:hover { border-color: var(--accent); }
  .btn.del { margin-left: auto; color: #ffb4a8; border-color: #5a2a2a; }
  .btn.del:hover { border-color: #e5484d; background: #2a1414; }
  .btn.restore { margin-left: auto; color: #9ff0b8; border-color: #1f5c38; }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .divider { height: 1px; background: var(--border); margin: 14px 0 6px; }
  .empty { color: var(--muted); font-size: 12.5px; }
  .big { color: var(--text); font-size: 14px; font-weight: 600; margin: 6px 0 10px; }
  .empty ul { padding-left: 16px; margin: 0; display: flex; flex-direction: column; gap: 7px; line-height: 1.4; }
  .empty b { color: var(--text); font-weight: 600; }
  code { background: #0f141c; border: 1px solid var(--border); border-radius: 4px; padding: 0 4px; font-size: 11.5px; color: #cfe0ff; }
  .st { margin-top: 18px; display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
  .st div { background: var(--panel2); border: 1px solid var(--border); border-radius: 8px; padding: 8px 10px; }
  .st b { display: block; color: var(--text); font-size: 16px; font-variant-numeric: tabular-nums; }
</style>
