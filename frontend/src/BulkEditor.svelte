<script>
  // Edit many items at once: tags, rating, favorite, color label, append a
  // note, album membership, move to a folder (dry-run -> apply -> undo).
  import { call } from './lib.js';
  import { lib, reloadLibrary, toast, tagLabel, COLORS, deleteItems, restoreItems, ai, reviewWithAi } from './store.svelte.js';
  import TagInput from './TagInput.svelte';

  let { ids = [], onchange = () => {}, items = [], ondone = () => {} } = $props();
  const deletable = $derived(items.filter((i) => !i.deleted && !i.missing));
  const restorable = $derived(items.filter((i) => i.deleted));

  let selTags = $state([]);
  let noteText = $state('');
  let albumPick = $state('');
  let busy = $state(false);

  // move
  let folders = $state([]);
  let moveDir = $state('');
  let movePlan = $state(null);
  let lastMoved = $state(null); // ids from the last applied bulk move (for undo)

  const n = $derived(ids.length);

  async function loadTags() {
    try {
      selTags = await call('tags.forItems', { ids });
    } catch {
      selTags = [];
    }
  }
  $effect(() => {
    ids;
    loadTags();
    movePlan = null;
  });

  async function run(cmd, args, kind, msg) {
    busy = true;
    try {
      await call(cmd, { ids, ...args });
      if (msg) toast(msg);
      await loadTags();
      onchange(kind);
    } catch (e) {
      toast(String(e), 'err', 6000);
    } finally {
      busy = false;
    }
  }

  async function addTags(names) {
    await run('tags.add', { names }, 'tags', `Tagged ${n} item${n === 1 ? '' : 's'}: ${names.join(', ')}`);
    reloadLibrary();
  }
  async function removeTag(t) {
    await run('tags.remove', { tagIds: [t.id] }, 'tags', `Removed “${t.name}” from ${t.count}`);
    reloadLibrary();
  }
  const rate = (r) => run('rating.set', { rating: r }, 'rating', r ? `Rated ${n} → ${'★'.repeat(r)}` : `Cleared rating on ${n}`);
  const fav = (v) => run('favorite.set', { value: v }, 'favorite', v ? `${n} marked favorite` : `${n} unfavorited`);
  const color = (c) => run('color.set', { color: c }, 'color', c ? `Labeled ${n} ${c}` : `Cleared label on ${n}`);

  async function appendNote() {
    if (!noteText.trim()) return;
    await run('notes.append', { text: noteText }, 'notes', `Note added to ${n} item${n === 1 ? '' : 's'}`);
    noteText = '';
  }

  async function addAlbum() {
    const v = albumPick;
    albumPick = '';
    if (!v) return;
    if (v === '__new') {
      const name = window.prompt(`New album name (adds the ${n} selected items)`);
      if (!name) return;
      busy = true;
      try {
        await call('album.create', { name, ids });
        toast(`Album “${name}” created with ${n} items`);
        onchange('albums');
      } catch (e) {
        toast(String(e), 'err');
      } finally {
        busy = false;
      }
    } else {
      const a = lib.albums.find((x) => x.id === Number(v));
      await run('album.add', { albumId: Number(v) }, 'albums', `Added ${n} to “${a?.name}”`);
    }
    reloadLibrary();
  }

  async function loadFolders() {
    if (folders.length) return;
    try {
      folders = await call('folders.list');
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  async function planMove() {
    if (!moveDir.trim()) return;
    busy = true;
    try {
      movePlan = await call('bulk.movePlan', { ids, dir: moveDir.trim() });
    } catch (e) {
      movePlan = null;
      toast(String(e), 'err', 6000);
    } finally {
      busy = false;
    }
  }

  async function applyMove() {
    if (!movePlan) return;
    const ok = window.confirm(
      `Move ${movePlan.ok} file${movePlan.ok === 1 ? '' : 's'} on disk to:\n${movePlan.dir}\n\n` +
      (movePlan.failed ? `${movePlan.failed} will be skipped (collisions).\n` : '') +
      'Nothing is deleted or overwritten. Each move is journaled and can be undone.'
    );
    if (!ok) return;
    busy = true;
    try {
      const res = await call('bulk.moveApply', { ids, dir: movePlan.dir });
      lastMoved = res.results.filter((r) => r.ok && !r.skipped).map((r) => r.id);
      toast(`Moved ${res.ok} file${res.ok === 1 ? '' : 's'}${res.failed ? `, ${res.failed} failed` : ''}`, res.failed ? 'warn' : 'ok', 6000);
      movePlan = null;
      folders = [];
      onchange('move');
    } catch (e) {
      toast(String(e), 'err', 8000);
    } finally {
      busy = false;
    }
  }

  async function undoMove() {
    if (!lastMoved?.length) return;
    if (!window.confirm(`Undo: move ${lastMoved.length} file(s) back to where they were?`)) return;
    busy = true;
    try {
      const res = await call('bulk.undo', { ids: lastMoved });
      toast(`Restored ${res.undone} file(s)` + (res.errors.length ? ` — ${res.errors.length} warning(s)` : ''), res.errors.length ? 'warn' : 'ok', 6000);
      lastMoved = null;
      folders = [];
      onchange('move');
    } catch (e) {
      toast(String(e), 'err', 8000);
    } finally {
      busy = false;
    }
  }
</script>

<div class="bulk" class:busy>
  <div class="head"><b>{n.toLocaleString()}</b> items selected</div>

  {#if ai.stats?.configured}
    <div class="row">
      <button class="btn ai" onclick={() => reviewWithAi(ids)}>✨ Review {n.toLocaleString()} with AI</button>
      <button class="btn ghost" onclick={() => reviewWithAi(ids, { rereview: true })} title="Also re-review items the AI has already looked at">re-review</button>
    </div>
  {/if}

  <div class="sec">Tags on selection</div>
  {#if selTags.length}
    <div class="chips">
      {#each selTags as t (t.id)}
        <span class="chip" title="{tagLabel(t)} — on {t.count} of {n}">
          {#if t.category}<span class="cat">{t.category}</span>{/if}{t.name}
          <span class="cnt">{t.count === n ? 'all' : t.count}</span>
          <button class="x" onclick={() => removeTag(t)} title="Remove from all selected">✕</button>
        </span>
      {/each}
    </div>
  {:else}
    <div class="muted">No tags yet.</div>
  {/if}
  <TagInput onadd={addTags} placeholder="Tag all selected…  (Category: tag)" />

  <div class="sec">Rating · favorite · label</div>
  <div class="row">
    {#each [1, 2, 3, 4, 5] as r (r)}
      <button class="btn sm" onclick={() => rate(r)} title="Set {r} star">{'★'.repeat(r)}</button>
    {/each}
    <button class="btn sm ghost" onclick={() => rate(0)}>clear</button>
  </div>
  <div class="row">
    <button class="btn sm" onclick={() => fav(true)}>♥ Favorite</button>
    <button class="btn sm ghost" onclick={() => fav(false)}>♡ Unfavorite</button>
  </div>
  <div class="row">
    {#each COLORS as c (c.key)}
      <button class="sw" style="--c: {c.hex}" title={c.key} onclick={() => color(c.key)}></button>
    {/each}
    <button class="btn sm ghost" onclick={() => color(null)}>no label</button>
  </div>

  <div class="sec">Add a note to all</div>
  <textarea rows="3" bind:value={noteText} placeholder="Appended to each item's notes"></textarea>
  <div class="row"><button class="btn sm" disabled={!noteText.trim()} onclick={appendNote}>Append note</button></div>

  <div class="sec">Albums</div>
  <select class="pick" bind:value={albumPick} onchange={addAlbum}>
    <option value="">Add selected to album…</option>
    {#each lib.albums as a (a.id)}<option value={a.id}>{a.name} ({a.count})</option>{/each}
    <option value="__new">+ New album from selection…</option>
  </select>

  <div class="sec">Move files on disk</div>
  <input
    class="pick"
    list="pa-folders"
    placeholder="Destination folder inside your library, e.g. D:\Pictures\Clients\Smith"
    bind:value={moveDir}
    onfocus={loadFolders}
    oninput={() => (movePlan = null)}
  />
  <datalist id="pa-folders">
    {#each folders as f (f.path)}<option value={f.path}>{f.count} items</option>{/each}
  </datalist>
  <div class="row">
    <button class="btn sm" disabled={!moveDir.trim()} onclick={planMove}>Preview move</button>
    {#if lastMoved?.length}
      <button class="btn sm ghost" onclick={undoMove}>Undo last move ({lastMoved.length})</button>
    {/if}
  </div>
  {#if movePlan}
    <div class="plan">
      <div><b>{movePlan.ok}</b> will move · <b>{movePlan.skipped}</b> already there · <b class:bad={movePlan.failed}>{movePlan.failed}</b> blocked</div>
      {#each movePlan.results.filter((r) => !r.ok).slice(0, 6) as r (r.id)}
        <div class="bad small" title={r.from}>✕ {(r.from || '').split(/[\\/]/).pop()}: {r.error}</div>
      {/each}
      {#if movePlan.ok}
        <button class="btn sm primary" onclick={applyMove}>Move {movePlan.ok} file{movePlan.ok === 1 ? '' : 's'}</button>
      {/if}
    </div>
  {/if}
  <div class="charter">New folders are created as needed · never overwrites · undoable</div>

  <div class="sec">Delete</div>
  <div class="row">
    {#if deletable.length}
      <button class="btn danger" onclick={() => deleteItems(deletable, ondone)}>Delete {deletable.length.toLocaleString()} → Recycle Bin</button>
    {/if}
    {#if restorable.length}
      <button class="btn" onclick={() => restoreItems(restorable.map((i) => i.id), ondone)}>Restore {restorable.length.toLocaleString()}</button>
    {/if}
  </div>
  <div class="charter">Undo or restore later from “Recently deleted” · notes and tags are kept</div>
</div>

<style>
  .bulk { display: flex; flex-direction: column; gap: 6px; }
  .bulk.busy { opacity: 0.7; pointer-events: none; }
  .head { font-size: 14px; }
  .sec { margin-top: 12px; font-size: 11px; color: var(--accent); text-transform: uppercase; letter-spacing: 0.4px; font-weight: 600; }
  .row { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; }
  .btn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    padding: 5px 10px; border-radius: 7px; cursor: pointer; font-size: 12px;
  }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn.ghost { background: none; color: var(--muted); }
  .btn.ai { background: #111a28; border-color: #2c3a4d; color: #cfe0ff; }
  .btn.danger { color: #ffb4a8; border-color: #5a2a2a; }
  .btn.danger:hover { border-color: #e5484d; background: #2a1414; }
  .btn.primary { background: #173049; border-color: #2c567f; margin-top: 8px; }
  .sw { width: 18px; height: 18px; border-radius: 50%; background: var(--c); border: 0; cursor: pointer; padding: 0; }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .chip {
    display: inline-flex; align-items: center; gap: 5px; background: #1a2230; border: 1px solid #2c3a4d;
    color: #cfe0ff; font-size: 12px; padding: 2px 4px 2px 8px; border-radius: 12px;
  }
  .chip .cat { font-size: 10px; color: #8aa0c0; }
  .chip .cat::after { content: ' ›'; }
  .cnt { font-size: 10px; color: #8aa0c0; background: #0f141c; border-radius: 8px; padding: 0 5px; }
  .x { background: none; border: 0; color: #7f95b8; cursor: pointer; font-size: 10.5px; padding: 0 3px; }
  .x:hover { color: #fff; }
  textarea, .pick {
    width: 100%; box-sizing: border-box; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 7px; padding: 7px 9px; font: inherit; font-size: 12.5px;
  }
  textarea:focus, .pick:focus { outline: none; border-color: var(--accent); }
  .muted { color: var(--muted); font-size: 12px; }
  .plan { margin-top: 6px; border: 1px solid #2c3a4d; border-radius: 8px; padding: 8px 10px; background: #0f141c; font-size: 12px; }
  .bad { color: #f4a620; }
  .small { font-size: 11px; margin-top: 3px; word-break: break-all; }
  .charter { margin-top: 4px; font-size: 10.5px; color: #556072; }
</style>
