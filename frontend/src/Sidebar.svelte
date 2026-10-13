<script>
  import { call, cleanFilter } from './lib.js';
  import { lib, reloadLibrary, toast, colorHex, ai } from './store.svelte.js';

  let { filter = {}, onfilter = () => {}, stats = null } = $props();

  const SCOPE = ['favorite', 'hasNotes', 'untagged', 'minRating', 'dupsOnly', 'presence', 'album', 'category', 'tags', 'tagMode', 'aiPending', 'aiReviewed', 'ids'];

  const VIEWS = [
    { key: 'all', label: 'All photos & videos', f: {}, count: () => stats?.present },
    { key: 'fav', label: 'Favorites', f: { favorite: true }, count: () => stats?.favorites, icon: '♥' },
    { key: 'notes', label: 'Has notes', f: { hasNotes: true }, count: () => stats?.withNotes, icon: '✎' },
    { key: 'untagged', label: 'Untagged', f: { untagged: true }, count: () => (stats ? stats.present - stats.tagged : null), icon: '◌' },
    { key: 'top', label: 'Rated 4★ and up', f: { minRating: 4 }, icon: '★' },
    { key: 'dups', label: 'Duplicates on disk', f: { dupsOnly: true }, count: () => stats?.dupGroups, icon: '⧉' },
    { key: 'missing', label: 'Not on disk', f: { presence: 'missing' }, count: () => stats?.missing, icon: '⚠' },
    { key: 'deleted', label: 'Recently deleted', f: { presence: 'deleted' }, count: () => stats?.deleted, icon: '🗑' },
    { key: 'aipending', label: 'AI: waiting approval', f: { aiPending: true }, count: () => ai.stats?.pendingPhotos, icon: '✨' },
    { key: 'ainot', label: 'AI: not reviewed yet', f: { aiReviewed: false }, icon: '◌' },
  ];

  function scoped(extra) {
    const next = { ...filter };
    for (const k of SCOPE) delete next[k];
    return { ...next, ...extra };
  }
  function sameScope(f) {
    const a = cleanFilter(Object.fromEntries(SCOPE.filter((k) => filter[k] !== undefined).map((k) => [k, filter[k]])));
    const b = cleanFilter(f);
    return JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());
  }

  // Hide / unhide a tag: items carrying it drop out of every view (handy for
  // "tag everything that isn't tagged DOG yet"). Not part of SCOPE, so it
  // stays on while you switch views, albums or searches.
  function toggleHideTag(t) {
    const cur = new Set(filter.excludeTags || []);
    if (cur.has(t.id)) cur.delete(t.id);
    else cur.add(t.id);
    const next = { ...filter, excludeTags: [...cur] };
    if (!next.excludeTags.length) delete next.excludeTags;
    if (next.tags) {
      next.tags = next.tags.filter((x) => x !== t.id); // can't show-only and hide at once
      if (!next.tags.length) delete next.tags;
    }
    onfilter(next);
  }
  function toggleHideCategory(c) {
    const cur = new Set(filter.excludeCategories || []);
    if (cur.has(c.id)) cur.delete(c.id);
    else cur.add(c.id);
    const next = { ...filter, excludeCategories: [...cur] };
    if (!next.excludeCategories.length) delete next.excludeCategories;
    if (next.category === c.id) delete next.category;
    onfilter(next);
  }

  function toggleTag(t) {
    const cur = new Set(filter.tags || []);
    if (cur.has(t.id)) cur.delete(t.id);
    else cur.add(t.id);
    const next = { ...filter, tags: [...cur] };
    if (!next.tags.length) delete next.tags;
    delete next.category;
    if (next.excludeTags?.includes(t.id)) {
      next.excludeTags = next.excludeTags.filter((x) => x !== t.id);
      if (!next.excludeTags.length) delete next.excludeTags;
    }
    onfilter(next);
  }

  // Group tags by category for display.
  const groups = $derived.by(() => {
    const byCat = new Map();
    for (const c of lib.categories) byCat.set(c.id, { cat: c, tags: [] });
    const none = { cat: null, tags: [] };
    for (const t of lib.tags) {
      if (t.categoryId && byCat.has(t.categoryId)) byCat.get(t.categoryId).tags.push(t);
      else none.tags.push(t);
    }
    const out = [...byCat.values()];
    if (none.tags.length) out.push(none);
    return out;
  });

  let collapsed = $state({});
  let tagFilter = $state('');

  async function newAlbum() {
    const name = window.prompt('New album name');
    if (!name) return;
    try {
      const r = await call('album.create', { name });
      await reloadLibrary();
      onfilter(scoped({ album: r.id }));
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  async function saveSearch() {
    const name = window.prompt('Save the current search & filters as:');
    if (!name) return;
    try {
      await call('search.save', { name, filter: cleanFilter(filter) });
      await reloadLibrary();
      toast(`Saved search “${name}”`);
    } catch (e) {
      toast(String(e), 'err');
    }
  }
  async function deleteSearch(s) {
    if (!window.confirm(`Delete saved search “${s.name}”?`)) return;
    await call('search.delete', { id: s.id });
    reloadLibrary();
  }
</script>

<nav class="side">
  <div class="h">Library</div>
  {#each VIEWS as v (v.key)}
    <button class="item" class:on={sameScope(v.f)} onclick={() => onfilter(scoped(v.f))}>
      <span class="ic">{v.icon || '▣'}</span><span class="lb">{v.label}</span>
      {#if v.count && v.count() != null}<span class="n">{Number(v.count()).toLocaleString()}</span>{/if}
    </button>
  {/each}

  <div class="h">Albums <button class="add" onclick={newAlbum} title="New album">＋</button></div>
  {#if !lib.albums.length}<div class="hint">Group photos for a job, customer or event.</div>{/if}
  {#each lib.albums as a (a.id)}
    <button class="item" class:on={filter.album === a.id} onclick={() => onfilter(scoped({ album: a.id }))} title={a.description || a.name}>
      <span class="ic">▤</span><span class="lb">{a.name}</span><span class="n">{a.count}</span>
    </button>
  {/each}

  <div class="h">Categories & tags</div>
  {#if lib.tags.length}<div class="hint">Click a tag to show only it · <b>hide</b> to take it out of every view</div>{/if}
  {#if lib.tags.length > 12}
    <input class="tf" placeholder="Filter tags…" bind:value={tagFilter} />
  {/if}
  {#if !lib.tags.length}<div class="hint">Select photos and add tags in the right panel. <code>Category: tag</code> files a tag under a category.</div>{/if}
  {#each groups as g (g.cat?.id ?? 'none')}
    {@const key = g.cat?.id ?? 'none'}
    {@const shown = g.tags.filter((t) => !tagFilter || t.name.toLowerCase().includes(tagFilter.toLowerCase()))}
    {#if shown.length || !tagFilter}
      <div class="cat">
        <button class="caret" onclick={() => (collapsed[key] = !collapsed[key])}>{collapsed[key] ? '▸' : '▾'}</button>
        {#if g.cat}
          {@const catHidden = (filter.excludeCategories || []).includes(g.cat.id)}
          <button class="catname" class:on={filter.category === g.cat.id} class:hidden={catHidden} onclick={() => onfilter(scoped({ category: g.cat.id }))} title="Show only items tagged in {g.cat.name}">
            <span class="dot" style="background: {g.cat.color ? colorHex(g.cat.color) : '#5a6270'}"></span>{g.cat.name}
          </button>
          <button class="hide" class:active={catHidden} onclick={() => toggleHideCategory(g.cat)}
            title={catHidden ? `Show items tagged in ${g.cat.name} again` : `Hide every item tagged in ${g.cat.name}`}>{catHidden ? 'hidden' : 'hide'}</button>
        {:else}
          <span class="catname plain">Other tags</span>
        {/if}
      </div>
      {#if !collapsed[key]}
        {#each shown as t (t.id)}
          {@const hidden = (filter.excludeTags || []).includes(t.id)}
          <div class="trow">
            <button class="item tag" class:on={(filter.tags || []).includes(t.id)} class:hidden onclick={() => toggleTag(t)} title="Show only items tagged {t.name}">
              <span class="lb">{t.name}</span><span class="n">{t.count}</span>
            </button>
            <button class="hide" class:active={hidden} onclick={() => toggleHideTag(t)}
              title={hidden ? `Show items tagged ${t.name} again` : `Hide items tagged ${t.name} from every view`}>{hidden ? 'hidden' : 'hide'}</button>
          </div>
        {/each}
      {/if}
    {/if}
  {/each}
  {#if (filter.tags || []).length > 1}
    <div class="mode">
      match
      <button class:on={filter.tagMode !== 'any'} onclick={() => onfilter({ ...filter, tagMode: undefined })}>all tags</button>
      <button class:on={filter.tagMode === 'any'} onclick={() => onfilter({ ...filter, tagMode: 'any' })}>any tag</button>
    </div>
  {/if}

  <div class="h">Saved searches <button class="add" onclick={saveSearch} title="Save current search">＋</button></div>
  {#if !lib.searches.length}<div class="hint">Save any search + filter combination with ＋.</div>{/if}
  {#each lib.searches as s (s.id)}
    <div class="srow">
      <button class="item" onclick={() => onfilter({ ...s.filter })} title={JSON.stringify(s.filter)}>
        <span class="ic">⌕</span><span class="lb">{s.name}</span>
      </button>
      <button class="del" onclick={() => deleteSearch(s)} title="Delete">✕</button>
    </div>
  {/each}
</nav>

<style>
  .side {
    width: 230px; flex: 0 0 230px; background: var(--panel); border-right: 1px solid var(--border);
    overflow-y: auto; padding: 8px 8px 40px; box-sizing: border-box;
  }
  .h {
    display: flex; align-items: center; margin: 14px 6px 4px; font-size: 10.5px; color: var(--muted);
    text-transform: uppercase; letter-spacing: 0.6px; font-weight: 600;
  }
  .add { margin-left: auto; background: none; border: 0; color: var(--muted); cursor: pointer; font-size: 14px; }
  .add:hover { color: var(--accent); }
  .item {
    width: 100%; display: flex; align-items: center; gap: 8px; text-align: left; background: none; border: 0;
    color: var(--text); padding: 5px 8px; border-radius: 6px; cursor: pointer; font-size: 12.5px;
  }
  .item:hover { background: var(--panel2); }
  .item.on { background: #263041; box-shadow: inset 2px 0 0 var(--accent); }
  .ic { width: 14px; text-align: center; color: var(--muted); font-size: 12px; }
  .lb { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .n { color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
  .tag { padding-left: 30px; }
  .trow { display: flex; align-items: center; }
  .trow .item { flex: 1; min-width: 0; }
  .item.hidden .lb, .catname.hidden { text-decoration: line-through; color: var(--muted); }
  .hide {
    flex: 0 0 auto; background: none; border: 1px solid transparent; color: #6b7383; font-size: 10.5px;
    border-radius: 9px; padding: 0 7px; height: 18px; cursor: pointer; visibility: hidden; margin-left: 2px;
  }
  .trow:hover .hide, .cat:hover .hide { visibility: visible; }
  .hide:hover { color: var(--text); border-color: var(--border); }
  .hide.active { visibility: visible; color: #f4a620; border-color: #6b5520; background: #2a2210; }
  .hint b { color: #9aa0ab; font-weight: 600; }
  .cat { display: flex; align-items: center; gap: 2px; margin-top: 2px; }
  .caret { background: none; border: 0; color: var(--muted); cursor: pointer; width: 18px; font-size: 10px; }
  .catname {
    flex: 1; display: flex; align-items: center; gap: 7px; background: none; border: 0; color: var(--text);
    font-size: 12.5px; font-weight: 600; padding: 4px 6px; border-radius: 6px; cursor: pointer; text-align: left;
  }
  .catname:hover { background: var(--panel2); }
  .catname.on { background: #263041; }
  .catname.plain { cursor: default; color: var(--muted); font-weight: 500; }
  .dot { width: 8px; height: 8px; border-radius: 50%; }
  .hint { margin: 2px 8px 4px; font-size: 11.5px; color: #6b7383; line-height: 1.4; }
  code { font-size: 10.5px; color: #cfe0ff; }
  .tf {
    width: calc(100% - 12px); margin: 2px 6px 6px; box-sizing: border-box; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 6px; padding: 5px 8px; font-size: 12px;
  }
  .mode { display: flex; gap: 4px; align-items: center; margin: 6px 8px; font-size: 11px; color: var(--muted); }
  .mode button { background: none; border: 1px solid var(--border); color: var(--muted); border-radius: 10px; padding: 1px 8px; font-size: 11px; cursor: pointer; }
  .mode button.on { color: var(--text); border-color: var(--accent); }
  .srow { display: flex; align-items: center; }
  .srow .item { flex: 1; }
  .del { background: none; border: 0; color: #556072; cursor: pointer; font-size: 10.5px; visibility: hidden; }
  .srow:hover .del { visibility: visible; }
  .del:hover { color: #e5484d; }
</style>
