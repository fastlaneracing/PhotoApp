<script>
  import { onMount } from 'svelte';
  import { call } from './lib.js';
  import { lib, reloadLibrary, toast, COLORS, colorHex } from './store.svelte.js';

  let { onchanged = () => {} } = $props();

  let sum = $state(null);
  let activity = $state([]);
  let busy = $state('');
  let lastSync = $state(null);
  let tagQ = $state('');

  async function load() {
    try {
      [sum, activity] = await Promise.all([call('maintenance.summary'), call('activity.list', { limit: 100 })]);
    } catch (e) {
      toast(String(e), 'err');
    }
  }
  onMount(() => {
    load();
    reloadLibrary();
  });

  async function op(label, cmd, args = {}, confirmText = null) {
    if (confirmText && !window.confirm(confirmText)) return null;
    busy = label;
    try {
      const r = await call(cmd, args);
      await Promise.all([load(), reloadLibrary()]);
      onchanged();
      return r;
    } catch (e) {
      toast(String(e), 'err', 8000);
      return null;
    } finally {
      busy = '';
    }
  }

  async function syncNow() {
    const r = await op('sync', 'maintenance.sync');
    if (r) {
      lastSync = r;
      toast(`Sync: ${r.added} new/changed · ${r.missing} gone · ${r.pruned} moved paths tidied`);
    }
  }
  async function pruneMoved() {
    const r = await op('prune', 'maintenance.pruneMoved');
    if (r) toast(`Removed ${r.removed} stale path rows`);
  }
  async function purgeGone() {
    const r = await op(
      'purge',
      'maintenance.purgeGone',
      {},
      `Remove ${sum.gone} items that are no longer anywhere on disk from the catalog?` +
        (sum.goneAnnotated ? `\n\n${sum.goneAnnotated} of them have notes, tags or ratings that will be removed with them.` : '') +
        '\n\nA backup of the catalog is taken first. Files on disk are not touched.'
    );
    if (r) toast(`Removed ${r.removed} items from the catalog (backup saved)`);
  }
  async function backupNow() {
    const r = await op('backup', 'maintenance.backup');
    if (r) toast('Backup saved: ' + r.path, 'ok', 6000);
  }

  // --- tags & categories ---
  async function renameTag(t) {
    const n = window.prompt('Rename tag', t.name);
    if (!n || n === t.name) return;
    await op('tag', 'tag.rename', { id: t.id, name: n });
  }
  async function mergeTag(t, intoId) {
    const into = lib.tags.find((x) => x.id === Number(intoId));
    if (!into) return;
    await op('tag', 'tag.merge', { fromId: t.id, intoId: into.id },
      `Merge “${t.name}” (${t.count}) into “${into.name}”? Every item tagged “${t.name}” gets “${into.name}”, and “${t.name}” is removed.`);
  }
  async function deleteTag(t) {
    await op('tag', 'tag.delete', { id: t.id }, `Delete tag “${t.name}”? It is removed from ${t.count} item(s). Photos are not affected.`);
  }
  async function setTagCategory(t, v) {
    await op('tag', 'tag.setCategory', { id: t.id, categoryId: v ? Number(v) : null });
  }
  async function newCategory() {
    const n = window.prompt('New category (e.g. Client, Job, Location, Equipment, Boat)');
    if (!n) return;
    await op('cat', 'category.create', { name: n });
  }
  async function renameCategory(c) {
    const n = window.prompt('Rename category', c.name);
    if (!n || n === c.name) return;
    await op('cat', 'category.update', { id: c.id, name: n });
  }
  async function colorCategory(c, color) {
    await op('cat', 'category.update', { id: c.id, color: color || null });
  }
  async function deleteCategory(c) {
    await op('cat', 'category.delete', { id: c.id }, `Delete category “${c.name}”? Its ${c.tagCount} tag(s) are kept as uncategorized.`);
  }

  // --- albums ---
  async function renameAlbum(a) {
    const n = window.prompt('Rename album', a.name);
    if (!n || n === a.name) return;
    await op('album', 'album.update', { id: a.id, name: n });
  }
  async function describeAlbum(a) {
    const d = window.prompt('Album description', a.description || '');
    if (d === null) return;
    await op('album', 'album.update', { id: a.id, description: d });
  }
  async function deleteAlbum(a) {
    await op('album', 'album.delete', { id: a.id }, `Delete album “${a.name}”? The ${a.count} photo(s) stay in your library.`);
  }

  const shownTags = $derived(lib.tags.filter((t) => !tagQ || t.name.toLowerCase().includes(tagQ.toLowerCase())));
  const when = (s) => new Date(s * 1000).toLocaleString();
</script>

<div class="manage">
  <section>
    <h2>Catalog health</h2>
    {#if sum}
      <div class="roots">
        {#each sum.roots as r (r.path)}
          <span class="root" class:off={!r.online}>{r.online ? '●' : '○'} {r.name} — {r.path} {r.online ? '' : '(offline — nothing is marked missing while offline)'}</span>
        {/each}
      </div>
      <div class="cards">
        <div class="card"><b>{sum.present.toLocaleString()}</b>on disk</div>
        <div class="card"><b>{sum.tagged.toLocaleString()}</b>tagged</div>
        <div class="card"><b>{sum.withNotes.toLocaleString()}</b>with notes</div>
        <div class="card"><b>{sum.tags}</b>tags</div>
        <div class="card"><b>{sum.albums}</b>albums</div>
        <div class="card"><b>{sum.dupGroups}</b>duplicate groups</div>
      </div>

      <div class="ops">
        <div class="op">
          <div><b>Sync with disk</b><p>Picks up files added, edited, moved or deleted outside the app (also runs automatically at launch and live while the app is open).</p></div>
          <button class="btn primary" disabled={!!busy} onclick={syncNow}>{busy === 'sync' ? 'Syncing…' : 'Sync now'}</button>
        </div>
        {#if lastSync}
          <div class="res">
            {lastSync.added} new/changed · {lastSync.missing} gone · {lastSync.pruned} moved paths tidied · {lastSync.seconds.toFixed(1)}s
            {#if lastSync.unreachableRoots.length}<span class="warn"> · offline: {lastSync.unreachableRoots.join(', ')}</span>{/if}
            {#if lastSync.failedCount}<span class="warn"> · {lastSync.failedCount} could not be read</span>{/if}
          </div>
        {/if}
        <div class="op">
          <div><b>Tidy moved paths</b><p>{sum.movedPaths.toLocaleString()} old path record(s) for files that still exist elsewhere. Safe — notes and tags stay with the photo.</p></div>
          <button class="btn" disabled={!!busy || !sum.movedPaths} onclick={pruneMoved}>Tidy</button>
        </div>
        <div class="op">
          <div><b>Remove items no longer on disk</b><p>{sum.gone.toLocaleString()} item(s) have no copy left on any drive{sum.goneAnnotated ? `; ${sum.goneAnnotated} carry notes/tags` : ''}. Browse them first under <i>Not on disk</i>. A backup is taken automatically.</p></div>
          <button class="btn danger" disabled={!!busy || !sum.gone} onclick={purgeGone}>Remove {sum.gone.toLocaleString()}</button>
        </div>
        <div class="op">
          <div><b>Back up catalog</b><p>Snapshot of all notes, tags, albums and ratings ({sum.lastBackup ? 'last: ' + when(Number(sum.lastBackup)) : 'none yet'}). One is taken automatically each day at launch; the newest 20 are kept in <code>data\backups</code>.</p></div>
          <button class="btn" disabled={!!busy} onclick={backupNow}>{busy === 'backup' ? 'Saving…' : 'Back up now'}</button>
        </div>
      </div>
    {:else}
      <div class="muted">loading…</div>
    {/if}
  </section>

  <section>
    <h2>Categories <button class="btn sm" onclick={newCategory}>＋ New category</button></h2>
    <p class="muted">Categories group tags — e.g. <i>Client</i>, <i>Job</i>, <i>Location</i>, <i>Boat</i>, <i>Equipment</i>. Typing <code>Client: Smith Marine</code> in any tag box files the tag automatically.</p>
    {#if lib.categories.length}
      <table>
        <thead><tr><th>Category</th><th>Color</th><th>Tags</th><th></th></tr></thead>
        <tbody>
          {#each lib.categories as c (c.id)}
            <tr>
              <td><span class="dot" style="background: {c.color ? colorHex(c.color) : '#5a6270'}"></span>{c.name}</td>
              <td>
                <select value={c.color || ''} onchange={(e) => colorCategory(c, e.target.value)}>
                  <option value="">none</option>
                  {#each COLORS as col (col.key)}<option value={col.key}>{col.key}</option>{/each}
                </select>
              </td>
              <td class="num">{c.tagCount}</td>
              <td class="acts"><button class="lnk" onclick={() => renameCategory(c)}>rename</button><button class="lnk bad" onclick={() => deleteCategory(c)}>delete</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <section>
    <h2>Tags <input class="q" placeholder="filter…" bind:value={tagQ} /></h2>
    {#if shownTags.length}
      <table>
        <thead><tr><th>Tag</th><th>Category</th><th>Items</th><th>Merge into</th><th></th></tr></thead>
        <tbody>
          {#each shownTags as t (t.id)}
            <tr>
              <td>{t.name}</td>
              <td>
                <select value={t.categoryId ?? ''} onchange={(e) => setTagCategory(t, e.target.value)}>
                  <option value="">—</option>
                  {#each lib.categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
                </select>
              </td>
              <td class="num">{t.count}</td>
              <td>
                <select value="" onchange={(e) => { mergeTag(t, e.target.value); e.target.value = ''; }}>
                  <option value="">…</option>
                  {#each lib.tags.filter((x) => x.id !== t.id) as x (x.id)}<option value={x.id}>{x.category ? x.category + ' › ' : ''}{x.name}</option>{/each}
                </select>
              </td>
              <td class="acts"><button class="lnk" onclick={() => renameTag(t)}>rename</button><button class="lnk bad" onclick={() => deleteTag(t)}>delete</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p class="muted">No tags yet.</p>
    {/if}
  </section>

  <section>
    <h2>Albums</h2>
    {#if lib.albums.length}
      <table>
        <thead><tr><th>Album</th><th>Description</th><th>Items</th><th></th></tr></thead>
        <tbody>
          {#each lib.albums as a (a.id)}
            <tr>
              <td>{a.name}</td>
              <td class="desc">{a.description}</td>
              <td class="num">{a.count}</td>
              <td class="acts">
                <button class="lnk" onclick={() => renameAlbum(a)}>rename</button>
                <button class="lnk" onclick={() => describeAlbum(a)}>describe</button>
                <button class="lnk bad" onclick={() => deleteAlbum(a)}>delete</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p class="muted">No albums yet — select photos in the Library and use “Add to album”.</p>
    {/if}
  </section>

  <section>
    <h2>Activity</h2>
    {#if activity.length}
      <table class="act">
        <tbody>
          {#each activity as a, i (i)}
            <tr><td class="when">{when(a.at)}</td><td>{a.action}</td><td class="num">{a.count || ''}</td><td class="desc">{a.detail}</td></tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p class="muted">Nothing yet.</p>
    {/if}
  </section>
</div>

<style>
  .manage { height: 100%; overflow-y: auto; padding: 18px 26px 60px; box-sizing: border-box; }
  section { max-width: 1050px; margin-bottom: 30px; }
  h2 { font-size: 15px; display: flex; align-items: center; gap: 12px; margin: 0 0 10px; }
  .muted { color: var(--muted); font-size: 12.5px; }
  .roots { display: flex; flex-direction: column; gap: 3px; margin-bottom: 10px; font-size: 12px; }
  .root { color: #3fb950; }
  .root.off { color: #f4a620; }
  .cards { display: grid; grid-template-columns: repeat(6, 1fr); gap: 10px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 9px; padding: 10px 12px; font-size: 12px; color: var(--muted); }
  .card b { display: block; color: var(--text); font-size: 20px; font-variant-numeric: tabular-nums; }
  .ops { margin-top: 14px; display: flex; flex-direction: column; gap: 8px; }
  .op { display: flex; align-items: center; gap: 16px; background: var(--panel); border: 1px solid var(--border); border-radius: 9px; padding: 10px 14px; }
  .op > div { flex: 1; }
  .op b { font-size: 13px; }
  .op p { margin: 3px 0 0; font-size: 12px; color: var(--muted); line-height: 1.4; }
  .res { font-size: 12px; color: #9ff0b8; padding: 0 14px; }
  .warn { color: #f4a620; }
  .btn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border); white-space: nowrap;
    padding: 6px 13px; border-radius: 8px; cursor: pointer; font-size: 12.5px;
  }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.45; cursor: default; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn.danger { border-color: #6b2a2a; color: #ffb4a8; }
  .btn.sm { padding: 3px 9px; font-size: 11.5px; font-weight: 400; }
  table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
  th { text-align: left; color: var(--muted); font-weight: 500; font-size: 11px; text-transform: uppercase; letter-spacing: 0.4px; padding: 6px 8px; border-bottom: 1px solid var(--border); }
  td { padding: 5px 8px; border-bottom: 1px solid #1d2129; vertical-align: middle; }
  td.num { font-variant-numeric: tabular-nums; color: var(--muted); }
  td.desc { color: var(--muted); max-width: 420px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  td.when { color: var(--muted); white-space: nowrap; width: 170px; }
  .acts { text-align: right; white-space: nowrap; }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 12px; padding: 0 5px; }
  .lnk.bad { color: #ff8f80; }
  select, .q {
    background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 6px;
    padding: 3px 6px; font-size: 12px;
  }
  .q { font-weight: 400; width: 180px; }
  .dot { display: inline-block; width: 9px; height: 9px; border-radius: 50%; margin-right: 8px; }
  code { background: #0f141c; border: 1px solid var(--border); border-radius: 4px; padding: 0 4px; font-size: 11.5px; color: #cfe0ff; }
</style>
