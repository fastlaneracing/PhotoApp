<script>
  import { onMount } from 'svelte';
  import { facets as loadFacets } from './lib.js';
  import { lib, COLORS } from './store.svelte.js';

  let { filter = {}, onfilter = () => {}, resultCount = null, resultPhotos = null, loading = false, onexport = null, stats = null } = $props();

  let facetData = $state({ kinds: [], years: [], cameras: [], folders: [] });
  let searchDraft = $state(filter.search || '');
  let searchTimer = null;

  const years = $derived(facetData.years.map((y) => y.value));
  const cameras = $derived(facetData.cameras.map((c) => c.value));
  const folders = $derived(facetData.folders.map((f) => f.value));

  const activeChips = $derived.by(() => {
    const chips = [];
    const push = (key, label) => {
      const v = filter[key];
      if (v !== undefined && v !== null && v !== '' && v !== 'all') chips.push({ key, label: `${label}: ${v}` });
    };
    if (filter.kind && filter.kind !== 'all') chips.push({ key: 'kind', label: filter.kind === 'video' ? 'videos only' : 'photos only' });
    push('year', 'year'); push('decade', 'decade');
    push('camera', 'camera'); push('folder', 'folder');
    if (filter.minWidth) chips.push({ key: 'minWidth', label: `≥${filter.minWidth}px` });
    if (filter.minHeight) chips.push({ key: 'minHeight', label: `≥${filter.minHeight}px` });
    if (filter.search) chips.push({ key: 'search', label: `“${filter.search}”` });
    for (const id of filter.tags || []) {
      const t = lib.tags.find((x) => x.id === id);
      chips.push({ key: 'tag:' + id, label: `# ${t ? (t.category ? t.category + ' › ' : '') + t.name : id}` });
    }
    if ((filter.tags || []).length > 1) chips.push({ key: 'tagMode', label: filter.tagMode === 'any' ? 'any tag' : 'all tags' });
    for (const id of filter.excludeTags || []) {
      const t = lib.tags.find((x) => x.id === id);
      chips.push({ key: 'xtag:' + id, label: `hiding # ${t ? t.name : id}`, hide: true });
    }
    for (const id of filter.excludeCategories || []) {
      const c = lib.categories.find((x) => x.id === id);
      chips.push({ key: 'xcat:' + id, label: `hiding ${c ? c.name : id} tags`, hide: true });
    }
    if (filter.category) chips.push({ key: 'category', label: `category: ${lib.categories.find((c) => c.id === filter.category)?.name ?? filter.category}` });
    if (filter.album) chips.push({ key: 'album', label: `album: ${lib.albums.find((a) => a.id === filter.album)?.name ?? filter.album}` });
    if (filter.favorite) chips.push({ key: 'favorite', label: '♥ favorites' });
    if (filter.hasNotes) chips.push({ key: 'hasNotes', label: '✎ has notes' });
    if (filter.untagged) chips.push({ key: 'untagged', label: 'untagged' });
    if (filter.hideTagged) chips.push({ key: 'hideTagged', label: 'hiding all tagged', hide: true });
    if (filter.minRating) chips.push({ key: 'minRating', label: `${'★'.repeat(filter.minRating)}+` });
    if (filter.color) chips.push({ key: 'color', label: `label: ${filter.color}` });
    if (filter.dateFrom) chips.push({ key: 'dateFrom', label: `from ${filter.dateFrom}` });
    if (filter.dateTo) chips.push({ key: 'dateTo', label: `to ${filter.dateTo}` });
    if (filter.presence === 'missing') chips.push({ key: 'presence', label: 'not on disk' });
    if (filter.presence === 'deleted') chips.push({ key: 'presence', label: 'recently deleted' });
    if (filter.presence === 'all') chips.push({ key: 'presence', label: 'incl. not on disk' });
    if (filter.dupsOnly) chips.push({ key: 'dupsOnly', label: 'duplicates' });
    if (filter.aiPending) chips.push({ key: 'aiPending', label: '✨ waiting approval' });
    if (filter.aiReviewed === false) chips.push({ key: 'aiReviewed', label: 'not AI-reviewed' });
    if (filter.aiReviewed === true) chips.push({ key: 'aiReviewed', label: 'AI-reviewed' });
    if (filter.ids) chips.push({ key: 'ids', label: `${filter.ids.length} specific item(s)` });
    if (filter.dir) chips.push({ key: 'dir', label: `in ${filter.dir}` });
    return chips;
  });

  function removeChip(key) {
    for (const [prefix, field] of [['xtag:', 'excludeTags'], ['xcat:', 'excludeCategories']]) {
      if (key.startsWith(prefix)) {
        const id = Number(key.slice(prefix.length));
        const next = { ...filter, [field]: (filter[field] || []).filter((x) => x !== id) };
        if (!next[field].length) delete next[field];
        onfilter(next);
        return;
      }
    }
    if (key.startsWith('tag:')) {
      const id = Number(key.slice(4));
      const next = { ...filter, tags: (filter.tags || []).filter((t) => t !== id) };
      if (!next.tags.length) delete next.tags;
      onfilter(next);
    } else set(key, '');
  }

  // Hide every tagged item. Showing only certain tags/categories can't work at
  // the same time, so those filters are dropped when hiding is turned on.
  function toggleHideTagged() {
    const next = { ...filter };
    if (next.hideTagged) delete next.hideTagged;
    else {
      next.hideTagged = true;
      delete next.tags;
      delete next.tagMode;
      delete next.category;
    }
    onfilter(next);
  }

  function set(key, value) {
    const next = { ...filter };
    if (value === '' || value === 'all' || value == null) delete next[key];
    else next[key] = value;
    onfilter(next);
  }

  function onSearchInput() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      const next = { ...filter };
      if (searchDraft.trim()) next.search = searchDraft.trim();
      else delete next.search;
      onfilter(next);
    }, 250);
  }

  function clearAll() {
    searchDraft = '';
    onfilter({});
  }

  onMount(async () => {
    try {
      facetData = await loadFacets();
    } catch (e) {
      console.warn('facets failed', e);
    }
  });
</script>

<div class="filterbar">
  <div class="row1">
    <div class="search">
      <span class="icn">🔍</span>
      <input
        type="text"
        placeholder="Search notes, tags, albums, filenames, folders, camera…   &quot;exact phrase&quot;  -exclude"
        bind:value={searchDraft}
        oninput={onSearchInput}
        onkeydown={(e) => { if (e.key === 'Enter') { clearTimeout(searchTimer); onfilter({ ...filter, search: searchDraft.trim() || undefined }); } }}
      />
      {#if filter.search}
        <button class="clear" title="Clear search" onclick={() => { searchDraft = ''; set('search', ''); }}>✕</button>
      {/if}
    </div>

    <div class="kind" role="group" aria-label="Show photos or videos">
      {#each [['all', 'All', null], ['image', 'Photos', stats?.images], ['video', 'Videos', stats?.videos]] as [k, label, n] (k)}
        <button class:on={(filter.kind || 'all') === k} onclick={() => set('kind', k)} title={k === 'all' ? 'Photos and videos' : `Only ${label.toLowerCase()}`}>
          {label}{#if n != null}<span class="kn">{n.toLocaleString()}</span>{/if}
        </button>
      {/each}
    </div>

    <button class="hidetag" class:on={filter.hideTagged} onclick={toggleHideTagged}
      title={filter.hideTagged ? 'Show tagged items again' : 'Hide every item that already has a tag (stays on while you switch views)'}>
      {filter.hideTagged ? '◌ Tagged hidden' : '◌ Hide tagged'}{#if stats?.tagged}<span class="kn">{stats.tagged.toLocaleString()}</span>{/if}
    </button>

    <label class="sel">
      <span>year</span>
      <select value={filter.year || ''} onchange={(e) => set('year', e.target.value)}>
        <option value="">any</option>
        {#each years as y (y)}<option value={y}>{y}</option>{/each}
      </select>
    </label>

    <label class="sel grow">
      <span>camera</span>
      <select value={filter.camera || ''} onchange={(e) => set('camera', e.target.value)}>
        <option value="">any</option>
        {#each cameras as c (c)}<option value={c}>{c}</option>{/each}
      </select>
    </label>

    <label class="sel grow">
      <span>folder</span>
      <select value={filter.folder || ''} onchange={(e) => set('folder', e.target.value)}>
        <option value="">any</option>
        {#each folders as f (f)}<option value={f}>{f}</option>{/each}
      </select>
    </label>

    <label class="num">
      <span>≥ width</span>
      <input type="number" min="0" step="100" placeholder="px"
        value={filter.minWidth ?? ''} oninput={(e) => set('minWidth', e.target.value ? +e.target.value : '')} />
    </label>

    <label class="sel">
      <span>rating</span>
      <select value={filter.minRating || ''} onchange={(e) => set('minRating', e.target.value ? +e.target.value : '')}>
        <option value="">any</option>
        {#each [1, 2, 3, 4, 5] as r (r)}<option value={r}>{'★'.repeat(r)}{r < 5 ? '+' : ''}</option>{/each}
      </select>
    </label>

    <label class="sel">
      <span>label</span>
      <select value={filter.color || ''} onchange={(e) => set('color', e.target.value)}>
        <option value="">any</option>
        {#each COLORS as c (c.key)}<option value={c.key}>{c.key}</option>{/each}
        <option value="none">no label</option>
      </select>
    </label>

    <label class="num date">
      <span>from</span>
      <input type="date" value={filter.dateFrom ?? ''} onchange={(e) => set('dateFrom', e.target.value)} />
    </label>
    <label class="num date">
      <span>to</span>
      <input type="date" value={filter.dateTo ?? ''} onchange={(e) => set('dateTo', e.target.value)} />
    </label>

    <label class="sel">
      <span>sort</span>
      <select value={filter.sort || 'takenDesc'} onchange={(e) => set('sort', e.target.value)}>
        <option value="takenDesc">newest</option>
        <option value="takenAsc">oldest</option>
        <option value="nameAsc">name A–Z</option>
        <option value="sizeDesc">largest</option>
        <option value="ratingDesc">top rated</option>
        <option value="editedDesc">recently edited</option>
      </select>
    </label>
  </div>

  <div class="row2">
    {#if activeChips.length}
      <div class="chips">
        {#each activeChips as chip (chip.key)}
          <span class="chip" class:hidechip={chip.hide} title={chip.label}>
            {chip.label}
            <button class="x" onclick={() => removeChip(chip.key)}>✕</button>
          </span>
        {/each}
        <button class="clearall" onclick={clearAll}>clear all</button>
      </div>
    {/if}
    {#if onexport}
      <button class="clearall" onclick={onexport} title="Export these results (with notes, tags, albums) to a CSV file">export CSV</button>
    {/if}
    <span class="count" class:loading={loading}>
      {#if resultCount == null}…{:else if loading}updating…{:else}{#if resultPhotos != null}<b>{resultPhotos.toLocaleString()}</b> photos{' · '}<b>{(resultCount - resultPhotos).toLocaleString()}</b> videos{' · '}{/if}<b>{resultCount.toLocaleString()}</b> total{/if}
    </span>
  </div>
</div>

<style>
  .filterbar { display: flex; flex-direction: column; gap: 8px; padding: 10px 16px; background: var(--panel); border-bottom: 1px solid var(--border); }
  .row1 { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .search { position: relative; display: flex; align-items: center; flex: 1 1 320px; min-width: 220px; }
  .search .icn { position: absolute; left: 10px; opacity: 0.6; font-size: 13px; pointer-events: none; }
  .search input { width: 100%; padding: 8px 30px 8px 30px; background: var(--bg); border: 1px solid var(--border); border-radius: 8px; color: var(--text); font-size: 13px; }
  .search input:focus { outline: none; border-color: var(--accent); }
  .search .clear { position: absolute; right: 6px; background: none; border: 0; color: var(--muted); cursor: pointer; font-size: 13px; }
  .kind { display: flex; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; align-self: flex-end; }
  .kind button {
    background: var(--bg); color: var(--muted); border: 0; border-right: 1px solid var(--border);
    padding: 7px 12px; font-size: 12.5px; cursor: pointer; display: flex; gap: 6px; align-items: center;
  }
  .kind button:last-child { border-right: 0; }
  .kind button:hover { color: var(--text); }
  .kind button.on { background: #263041; color: var(--text); box-shadow: inset 0 -2px 0 var(--accent); }
  .hidetag {
    align-self: flex-end; display: flex; gap: 6px; align-items: center; white-space: nowrap;
    background: var(--bg); color: var(--muted); border: 1px solid var(--border); border-radius: 8px;
    padding: 7px 12px; font-size: 12.5px; cursor: pointer;
  }
  .hidetag:hover { color: var(--text); }
  .hidetag.on { color: #f4a620; border-color: #6b5520; background: #2a2210; }
  .kn { font-size: 10.5px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .sel { display: flex; flex-direction: column; gap: 2px; }
  .sel > span { font-size: 10px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.4px; }
  .sel.grow, .num { flex: 1 1 140px; min-width: 120px; }
  select, .num input {
    background: var(--bg); border: 1px solid var(--border); color: var(--text);
    padding: 6px 8px; border-radius: 7px; font-size: 12.5px;
  }
  select:focus, .num input:focus { outline: none; border-color: var(--accent); }
  .num > span { font-size: 10px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.4px; }
  .num { display: flex; flex-direction: column; gap: 2px; }
  .num input { width: 100%; box-sizing: border-box; }
  .num.date { flex: 0 0 auto; min-width: 0; }
  .num:not(.date) { flex: 0 0 110px; min-width: 0; }
  .num.date input { color-scheme: dark; }
  .row2 { display: flex; align-items: center; gap: 10px; min-height: 14px; }
  .chips { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; flex: 1; }
  .chip { display: inline-flex; align-items: center; gap: 6px; background: #1a2230; border: 1px solid #2c3a4d; color: #cfe0ff; font-size: 11.5px; padding: 3px 6px 3px 9px; border-radius: 12px; }
  .chip.hidechip { background: #2a2210; border-color: #6b5520; color: #ffe7b0; }
  .chip .x { background: none; border: 0; color: #7f95b8; cursor: pointer; font-size: 11px; padding: 0 2px; }
  .chip .x:hover { color: #fff; }
  .clearall { background: none; border: 1px solid var(--border); color: var(--muted); font-size: 11px; padding: 3px 9px; border-radius: 12px; cursor: pointer; }
  .clearall:hover { border-color: var(--accent); color: var(--text); }
  .count { margin-left: auto; font-size: 12.5px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .count.loading { opacity: 0.6; }
  .count b { color: var(--text); font-weight: 600; }
</style>
