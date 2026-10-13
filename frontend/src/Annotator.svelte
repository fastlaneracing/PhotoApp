<script>
  // Notes, rating, favorite, color label, tags and albums for ONE item.
  // Notes autosave (debounced) and are flushed before switching items.
  import { call } from './lib.js';
  import { lib, reloadLibrary, tagLabel, toast, COLORS } from './store.svelte.js';
  import TagInput from './TagInput.svelte';

  let { id, onchange = () => {}, compact = false } = $props();

  let ann = $state(null);
  let notes = $state('');
  let saveState = $state(''); // '' | 'saving' | 'saved' | 'error'
  let timer = null;
  let pending = null; // { id, text }
  let albumPick = $state('');

  async function load(mid) {
    try {
      const a = await call('annotation.get', { id: mid });
      if (mid !== id) return;
      ann = a;
      notes = a.notes || '';
      saveState = '';
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  async function flush() {
    clearTimeout(timer);
    if (!pending) return;
    const p = pending;
    pending = null;
    saveState = 'saving';
    try {
      await call('notes.set', { id: p.id, notes: p.text });
      if (p.id === id) saveState = 'saved';
      onchange('notes', p.id, p.text);
    } catch (e) {
      saveState = 'error';
      toast('Note not saved: ' + e, 'err', 6000);
    }
  }

  $effect(() => {
    const mid = id;
    ann = null;
    load(mid);
    return () => { flush(); };
  });

  function onNotesInput(e) {
    notes = e.target.value;
    pending = { id, text: notes };
    saveState = 'saving';
    clearTimeout(timer);
    timer = setTimeout(flush, 700);
  }

  async function act(cmd, args, kind) {
    try {
      await call(cmd, args);
      await load(id);
      onchange(kind, id);
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  const setRating = (r) => act('rating.set', { ids: [id], rating: ann?.rating === r ? 0 : r }, 'rating');
  const toggleFav = () => act('favorite.set', { ids: [id], value: !ann?.favorite }, 'favorite');
  const setColor = (c) => act('color.set', { ids: [id], color: ann?.colorLabel === c ? null : c }, 'color');
  async function addTags(names) {
    await act('tags.add', { ids: [id], names }, 'tags');
    reloadLibrary();
  }
  async function removeTag(t) {
    await act('tags.remove', { ids: [id], tagIds: [t.id] }, 'tags');
    reloadLibrary();
  }
  async function addAlbum() {
    const v = albumPick;
    albumPick = '';
    if (!v) return;
    if (v === '__new') {
      const name = window.prompt('New album name');
      if (!name) return;
      try {
        await call('album.create', { name, ids: [id] });
      } catch (e) { toast(String(e), 'err'); return; }
      await load(id);
      onchange('albums', id);
    } else {
      await act('album.add', { albumId: Number(v), ids: [id] }, 'albums');
    }
    reloadLibrary();
  }
  async function removeAlbum(a) {
    await act('album.remove', { albumId: a.id, ids: [id] }, 'albums');
    reloadLibrary();
  }
</script>

{#if ann}
  <div class="ann" class:compact>
    <div class="row">
      <div class="stars" title="Rating (keys 0–5)">
        {#each [1, 2, 3, 4, 5] as r (r)}
          <button class="star" class:on={ann.rating >= r} onclick={() => setRating(r)}>★</button>
        {/each}
      </div>
      <button class="fav" class:on={ann.favorite} onclick={toggleFav} title="Favorite (F)">{ann.favorite ? '♥' : '♡'}</button>
      <div class="colors">
        {#each COLORS as c (c.key)}
          <button
            class="sw"
            class:on={ann.colorLabel === c.key}
            style="--c: {c.hex}"
            title={c.key}
            onclick={() => setColor(c.key)}
          ></button>
        {/each}
      </div>
    </div>

    <div class="lbl">
      Notes
      <span class="save {saveState}">
        {saveState === 'saving' ? 'saving…' : saveState === 'saved' ? '✓ saved' : saveState === 'error' ? 'not saved' : ''}
      </span>
    </div>
    <textarea
      rows={compact ? 4 : 6}
      value={notes}
      oninput={onNotesInput}
      onblur={flush}
      placeholder="Job, customer, location, invoice #, who's in it… (searchable)"
    ></textarea>

    <div class="lbl">Tags</div>
    {#if ann.tags.length}
      <div class="chips">
        {#each ann.tags as t (t.id)}
          <span class="chip" title={tagLabel(t)}>
            {#if t.category}<span class="cat">{t.category}</span>{/if}{t.name}
            <button class="x" onclick={() => removeTag(t)} title="Remove tag">✕</button>
          </span>
        {/each}
      </div>
    {/if}
    <TagInput onadd={addTags} exclude={ann.tags.map((t) => t.id)} />

    <div class="lbl">Albums</div>
    {#if ann.albums.length}
      <div class="chips">
        {#each ann.albums as a (a.id)}
          <span class="chip alb">{a.name}<button class="x" onclick={() => removeAlbum(a)} title="Remove from album">✕</button></span>
        {/each}
      </div>
    {/if}
    <select class="pick" bind:value={albumPick} onchange={addAlbum}>
      <option value="">Add to album…</option>
      {#each lib.albums.filter((a) => !ann.albums.some((x) => x.id === a.id)) as a (a.id)}
        <option value={a.id}>{a.name} ({a.count})</option>
      {/each}
      <option value="__new">+ New album…</option>
    </select>
  </div>
{:else}
  <div class="loading">loading…</div>
{/if}

<style>
  .ann { display: flex; flex-direction: column; gap: 6px; }
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .stars { display: flex; }
  .star { background: none; border: 0; color: #3a414e; font-size: 19px; cursor: pointer; padding: 0 1px; line-height: 1; }
  .star.on { color: #f5c518; }
  .star:hover { color: #ffd84d; }
  .fav { background: none; border: 0; color: #6b7280; font-size: 19px; cursor: pointer; line-height: 1; }
  .fav.on { color: #e5484d; }
  .colors { display: flex; gap: 5px; margin-left: auto; }
  .sw { width: 15px; height: 15px; border-radius: 50%; background: var(--c); border: 2px solid transparent; cursor: pointer; opacity: 0.45; padding: 0; }
  .sw.on { opacity: 1; border-color: #fff; }
  .sw:hover { opacity: 1; }
  .lbl { margin-top: 8px; font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.4px; display: flex; }
  .save { margin-left: auto; text-transform: none; letter-spacing: 0; }
  .save.saved { color: #3fb950; }
  .save.error { color: #e5484d; }
  textarea {
    width: 100%; box-sizing: border-box; resize: vertical; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 7px; padding: 8px 9px; font: inherit; font-size: 13px; line-height: 1.45;
  }
  textarea:focus { outline: none; border-color: var(--accent); }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .chip {
    display: inline-flex; align-items: center; gap: 5px; background: #1a2230; border: 1px solid #2c3a4d;
    color: #cfe0ff; font-size: 12px; padding: 2px 4px 2px 8px; border-radius: 12px;
  }
  .chip.alb { background: #221a30; border-color: #3d2c4d; color: #e3cfff; }
  .chip .cat { font-size: 10px; color: #8aa0c0; margin-right: 2px; }
  .chip .cat::after { content: ' ›'; }
  .x { background: none; border: 0; color: #7f95b8; cursor: pointer; font-size: 10.5px; padding: 0 3px; }
  .x:hover { color: #fff; }
  .pick {
    background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 7px;
    padding: 6px 8px; font-size: 12.5px;
  }
  .loading { color: var(--muted); font-size: 12px; padding: 8px 0; }
</style>
