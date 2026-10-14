<script>
  import { queryItems, stats, onCatalogChanged, isTauri, call, cleanFilter } from './lib.js';
  import { reloadLibrary, toast, toasts, deleteItems, restoreItems, copyItemsTo, ai, refreshAi, onAiProgress } from './store.svelte.js';
  import ContextMenu from './ContextMenu.svelte';
  import FilterBar from './FilterBar.svelte';
  import Grid from './Grid.svelte';
  import Viewer from './Viewer.svelte';
  import DupsView from './DupsView.svelte';
  import Sidebar from './Sidebar.svelte';
  import Inspector from './Inspector.svelte';
  import ManageView from './ManageView.svelte';
  import AIView from './AIView.svelte';
  import AskBar from './AskBar.svelte';
  import { onMount } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';

  let items = $state([]);
  let statsInfo = $state(null);
  let filter = $state({});
  let loading = $state(true);
  let error = $state(null);
  let viewerId = $state(null);
  let lastChanged = $state(null);
  let view = $state('library'); // 'library' | 'dups' | 'manage'
  let syncing = $state(null); // { done, total } while the launch sync adds files
  let size = $state(176);
  try { size = Number(localStorage.getItem('pa.size')) || 176; } catch {}

  const selected = new SvelteSet();
  let anchor = $state(-1);
  let cursor = $state(null); // index to keep in view

  const selIds = $derived([...selected]);
  const resultPhotos = $derived(items.reduce((n, i) => n + (i.kind === 'image' ? 1 : 0), 0));
  const selItem = $derived(selIds.length === 1 ? items.find((i) => i.id === selIds[0]) || null : null);

  let seq = 0;
  async function refresh() {
    const my = ++seq;
    loading = true;
    error = null;
    try {
      const [itemsRes, statsRes] = await Promise.all([queryItems(filter), stats()]);
      if (my !== seq) return; // a newer refresh superseded this one
      items = itemsRes;
      statsInfo = statsRes;
      // Keep the selection for items still in view.
      const present = new Set(itemsRes.map((i) => i.id));
      for (const id of [...selected]) if (!present.has(id)) selected.delete(id);
    } catch (e) {
      if (my === seq) error = String(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  function handleFilter(f) {
    filter = f || {};
    selected.clear();
    anchor = -1;
    view = 'library';
    refresh();
  }

  function onselect(id, idx, e) {
    if (id == null) {
      selected.clear();
      anchor = -1;
      return;
    }
    if (e?.shiftKey && anchor >= 0) {
      if (!(e.ctrlKey || e.metaKey)) selected.clear();
      const [a, b] = anchor < idx ? [anchor, idx] : [idx, anchor];
      for (let i = a; i <= b; i++) selected.add(items[i].id);
    } else if (e?.ctrlKey || e?.metaKey) {
      if (selected.has(id)) selected.delete(id);
      else selected.add(id);
      anchor = idx;
    } else {
      selected.clear();
      selected.add(id);
      anchor = idx;
    }
  }

  // Right-click: act on the selection; a tile outside it becomes the selection
  // (same as Explorer).
  let menu = $state(null); // { x, y }
  function oncontext(id, idx, e) {
    if (!selected.has(id)) onselect(id, idx, null);
    menu = { x: e.clientX, y: e.clientY };
  }
  const menuItems = $derived.by(() => {
    if (!menu) return [];
    const sel = items.filter((i) => selected.has(i.id));
    const onDisk = sel.filter((i) => !i.missing && !i.deleted);
    const one = sel.length === 1 ? sel[0] : null;
    const n = onDisk.length;
    const out = [
      { label: n > 1 ? `Copy ${n.toLocaleString()} items to…` : 'Copy to…', run: () => copyItemsTo(onDisk), disabled: !n },
      { sep: true },
      { label: 'View', hint: 'Enter', run: () => openItem(sel[0].id), disabled: !sel.length },
    ];
    if (isTauri()) {
      out.push(
        { label: 'Show in Explorer', run: () => call('reveal', { path: one.path }).catch((e) => toast(String(e), 'err')), disabled: !one || one.missing },
        { label: 'Open file', run: () => call('openFile', { path: one.path }).catch((e) => toast(String(e), 'err')), disabled: !one || one.missing },
      );
    }
    out.push({ sep: true });
    if (sel.length && sel.every((i) => i.deleted)) {
      out.push({ label: 'Restore', run: () => restoreItems(sel.map((i) => i.id), refresh) });
    } else {
      out.push({ label: n > 1 ? `Delete ${n.toLocaleString()} items` : 'Delete', hint: 'Del', danger: true, disabled: !n, run: () => deleteItems(onDisk, refresh) });
    }
    return out;
  });

  // After an edit: notes only patch the tile; everything else re-queries so
  // filtered views (e.g. Untagged) stay truthful.
  function onAnnotChange(kind, id, text) {
    if (kind === 'notes') {
      const it = items.find((i) => i.id === id);
      if (it) it.hasNotes = !!(text && text.trim());
      return;
    }
    refresh();
    // AI approvals and undo can create tags/albums or change their counts:
    // keep the sidebar's Categories & tags and Albums lists in step.
    if (kind === 'ai' || kind === 'undo' || kind === 'tags' || kind === 'albums') reloadLibrary();
  }

  /** After AI-tab approvals / rejections / undo. */
  function onAiChanged() {
    refresh();
    reloadLibrary();
  }

  function openItem(id) {
    viewerId = id;
  }
  function stepItem(delta) {
    const idx = items.findIndex((i) => i.id === viewerId);
    if (idx < 0) return;
    const next = idx + delta;
    if (next >= 0 && next < items.length) viewerId = items[next].id;
  }

  async function exportCsv() {
    try {
      const r = await call('export.csv', { filter: cleanFilter(filter) });
      toast(`Exported ${r.rows.toLocaleString()} rows → ${r.path}`, 'ok', 8000);
      if (isTauri()) call('reveal', { path: r.path }).catch(() => {});
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  async function quick(cmd, args, msg) {
    try {
      await call(cmd, args);
      toast(msg);
      refresh();
    } catch (e) {
      toast(String(e), 'err');
    }
  }

  function onKey(e) {
    if (view !== 'library' || viewerId !== null) return;
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
    const ids = [...selected];
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      for (const it of items) selected.add(it.id);
      return;
    }
    if (e.key === 'Escape') { selected.clear(); anchor = -1; return; }
    if (e.key === 'Enter' && ids.length) { e.preventDefault(); openItem(ids[0]); return; }
    if ((e.key === 'ArrowRight' || e.key === 'ArrowLeft') && items.length) {
      e.preventDefault();
      const cur = anchor >= 0 ? anchor : -1;
      const next = Math.max(0, Math.min(items.length - 1, cur + (e.key === 'ArrowRight' ? 1 : -1)));
      onselect(items[next].id, next, e.shiftKey ? { shiftKey: true } : null);
      if (!e.shiftKey) anchor = next;
      cursor = next;
      return;
    }
    if (!ids.length || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Delete') {
      e.preventDefault();
      const sel = items.filter((i) => selected.has(i.id) && !i.deleted && !i.missing);
      if (sel.length) deleteItems(sel, refresh);
      return;
    }
    if (/^[0-5]$/.test(e.key)) {
      const r = Number(e.key);
      quick('rating.set', { ids, rating: r }, r ? `${ids.length} → ${'★'.repeat(r)}` : `Rating cleared on ${ids.length}`);
    } else if (e.key.toLowerCase() === 'f') {
      const allFav = ids.every((id) => items.find((i) => i.id === id)?.favorite);
      quick('favorite.set', { ids, value: !allFav }, allFav ? `${ids.length} unfavorited` : `♥ ${ids.length} favorited`);
    }
  }

  $effect(() => {
    try { localStorage.setItem('pa.size', String(size)); } catch {}
  });

  onMount(() => {
    refresh();
    reloadLibrary();
    window.addEventListener('keydown', onKey);
    refreshAi();
    // While an AI run is going, refresh the grid as suggestions arrive.
    const offAi = onAiProgress(() => { if (view === 'library') refresh(); });
    let stop = null;
    let changeTimer = null;
    onCatalogChanged((payload) => {
      const pr = payload?.progress;
      if (pr) {
        syncing = pr.total ? pr : null;
        if (pr.phase === 'adding') return; // just the counter; no re-query per batch
      } else if (payload?.sync) {
        syncing = null;
      }
      lastChanged = payload;
      if (payload?.sync) {
        const s = payload.sync;
        if (s.added || s.missing) toast(`Library synced: ${s.added} new/changed, ${s.missing} gone`);
        if (s.unreachableRoots?.length) toast(`Offline: ${s.unreachableRoots.join(', ')} — nothing marked missing`, 'warn', 8000);
      }
      // Coalesce bursts (e.g. 300 files dropped into the library at once).
      clearTimeout(changeTimer);
      changeTimer = setTimeout(() => {
        if (payload?.note !== 'poll' || !viewerId) refresh();
        reloadLibrary();
      }, 400);
    }).then((u) => (stop = u));
    return () => {
      window.removeEventListener('keydown', onKey);
      offAi();
      if (typeof stop === 'function') stop();
    };
  });
</script>

<div class="app">
  <header class="bar">
    <div class="brand">
      <span class="logo">▣</span>
      <span class="name">photoApp</span>
    </div>
    <nav class="tabs">
      <button class="tab {view === 'library' ? 'on' : ''}" onclick={() => (view = 'library')}>Library</button>
      <button class="tab {view === 'dups' ? 'on' : ''}" onclick={() => (view = 'dups')}>
        Duplicates{statsInfo ? ` (${statsInfo.dupGroups})` : ''}
      </button>
      <button class="tab {view === 'ai' ? 'on' : ''}" onclick={() => (view = 'ai')}>
        ✨ AI{ai.stats?.pending ? ` (${ai.stats.pending.toLocaleString()})` : ''}
      </button>
      <button class="tab {view === 'manage' ? 'on' : ''}" onclick={() => (view = 'manage')}>Manage</button>
    </nav>
    <div class="stats" class:empty={!statsInfo}>
      {#if statsInfo}
        <span class="cnt" title="Photos on disk"><b>{statsInfo.images.toLocaleString()}</b> photos</span>
        <span class="cnt" title="Videos on disk"><b>{statsInfo.videos.toLocaleString()}</b> videos</span>
        <span class="cnt total" title="Photos + videos on disk"><b>{(statsInfo.present ?? statsInfo.total).toLocaleString()}</b> total</span>
        <span><b>{(statsInfo.tagged ?? 0).toLocaleString()}</b> tagged</span>
        {#if statsInfo.missing}
          <button class="warnlink" onclick={() => handleFilter({ presence: 'missing' })}><b>{statsInfo.missing}</b> not on disk</button>
        {/if}
      {/if}
    </div>
    <div class="right">
      {#if view === 'library'}
        <label class="zoom" title="Thumbnail size">
          <span>▫</span><input type="range" min="120" max="320" step="8" bind:value={size} /><span>▣</span>
        </label>
      {/if}
      {#if syncing}
        <span class="sync" title="Cataloging files found on disk (hash, date, thumbnail)">
          syncing {syncing.done.toLocaleString()} / {syncing.total.toLocaleString()}
        </span>
      {/if}
      {#if lastChanged && lastChanged.note !== 'poll'}
        <span class="live" title={lastChanged.note}>live</span>
      {/if}
      {#if !isTauri()}
        <span class="mode">browser dev</span>
      {/if}
      <button class="refresh" onclick={refresh} disabled={loading}>
        {loading ? 'loading…' : 'refresh'}
      </button>
    </div>
  </header>

  {#if view === 'dups'}
    <DupsView />
  {:else if view === 'ai'}
    <AIView onchanged={onAiChanged} onshow={(ids) => handleFilter({ ids, presence: 'all' })} />
  {:else if view === 'manage'}
    <ManageView onchanged={refresh} />
  {:else}
    <div class="body">
      <Sidebar {filter} onfilter={handleFilter} stats={statsInfo} />
      <div class="center">
        <AskBar {filter} resultIds={items.map((i) => i.id)} selectedIds={selIds} onfilter={handleFilter} ongoai={() => (view = 'ai')} />
        <FilterBar filter={filter} onfilter={handleFilter} resultCount={items.length} resultPhotos={resultPhotos} loading={loading} onexport={exportCsv} stats={statsInfo} />
        <main class="content">
          {#if error}
            <div class="errorbox">
              <p>Failed to load catalog</p>
              <code>{error}</code>
              <p class="hint">Is the API running? <code>photoapp.exe api</code> (port 8787)</p>
              <button class="refresh" onclick={refresh}>retry</button>
            </div>
          {:else if loading && items.length === 0}
            <div class="loading">Loading catalog…</div>
          {:else if items.length === 0}
            <div class="empty">No items match the current filters.</div>
          {:else}
            <Grid {items} onopen={openItem} {onselect} {oncontext} {selected} {size} scrollToIndex={cursor} />
          {/if}
        </main>
      </div>
      <Inspector ids={selIds} item={selItem} selItems={items.filter((i) => selected.has(i.id))} onchange={onAnnotChange} onopen={openItem} stats={statsInfo} ondone={refresh} />
    </div>
  {/if}

  {#if viewerId !== null}
    <Viewer
      items={items}
      id={viewerId}
      onclose={() => (viewerId = null)}
      onprev={() => stepItem(-1)}
      onnext={() => stepItem(1)}
      onchange={onAnnotChange}
    />
  {/if}

  {#if menu}
    <ContextMenu x={menu.x} y={menu.y} items={menuItems}
      title={selected.size > 1 ? `${selected.size.toLocaleString()} selected` : ''}
      onclose={() => (menu = null)} />
  {/if}

  <div class="toasts">
    {#each toasts.list as t (t.id)}
      <div class="toast {t.kind}">
        <span>{t.msg}</span>
        {#if t.action}
          <button class="tact" onclick={() => { toasts.list = toasts.list.filter((x) => x.id !== t.id); t.action.run(); }}>{t.action.label}</button>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .app { height: 100vh; display: flex; flex-direction: column; background: var(--bg); }
  .bar {
    display: flex; align-items: center; gap: 16px;
    padding: 10px 16px; background: var(--panel); border-bottom: 1px solid var(--border);
  }
  .brand { display: flex; align-items: center; gap: 8px; }
  .logo { color: var(--accent); font-size: 20px; }
  .name { font-weight: 600; font-size: 15px; letter-spacing: 0.2px; }
  .stats { display: flex; gap: 14px; color: var(--muted); font-size: 13px; align-items: center; }
  .stats .total { padding-left: 14px; border-left: 1px solid var(--border); }
  .stats b { color: var(--text); font-variant-numeric: tabular-nums; }
  .warnlink { background: none; border: 0; color: var(--muted); font-size: 13px; cursor: pointer; padding: 0; }
  .warnlink b { color: #f4a620; }
  .warnlink:hover { text-decoration: underline; }
  .tabs { display: flex; gap: 4px; }
  .tab {
    background: none; border: 1px solid transparent; color: var(--muted);
    padding: 6px 14px; border-radius: 8px; cursor: pointer; font-size: 13.5px;
  }
  .tab:hover { color: var(--text); border-color: var(--border); }
  .tab.on { color: var(--text); background: var(--panel2); border-color: var(--border); box-shadow: inset 0 -2px 0 var(--accent); }
  .right { margin-left: auto; display: flex; align-items: center; gap: 10px; }
  .zoom { display: flex; align-items: center; gap: 4px; color: var(--muted); font-size: 12px; }
  .zoom input { width: 90px; accent-color: var(--accent); }
  .live {
    color: #39c26d; font-size: 12px; border: 1px solid #1f5c38; padding: 2px 8px;
    border-radius: 10px; background: #0f2417;
  }
  .sync {
    color: #8ab4f8; font-size: 12px; border: 1px solid #2c3a4d; padding: 2px 8px;
    border-radius: 10px; background: #111a28; font-variant-numeric: tabular-nums;
  }
  .mode { color: var(--muted); font-size: 11px; border: 1px solid var(--border); padding: 2px 8px; border-radius: 10px; }
  .refresh {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    padding: 6px 12px; border-radius: 8px; cursor: pointer; font-size: 13px;
  }
  .refresh:hover { border-color: var(--accent); }
  .refresh:disabled { opacity: 0.6; cursor: default; }
  .body { flex: 1; min-height: 0; display: flex; }
  .center { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .content { flex: 1; min-height: 0; overflow: hidden; }
  .loading { padding: 40px; color: var(--muted); }
  .empty { padding: 40px; color: var(--muted); text-align: center; }
  .errorbox { padding: 24px; margin: 24px; border: 1px solid #6b2a2a; background: #241212; border-radius: 10px; }
  .errorbox code { display: block; margin: 12px 0; padding: 10px; background: #150b0b; border-radius: 6px; color: #f0a; word-break: break-all; }
  .hint { color: var(--muted); font-size: 13px; }
  .toasts { position: fixed; right: 16px; bottom: 16px; z-index: 100; display: flex; flex-direction: column; gap: 8px; max-width: 460px; }
  .toast {
    background: #12261a; border: 1px solid #1f5c38; color: #d6f5df; padding: 9px 13px; border-radius: 9px;
    font-size: 12.5px; box-shadow: 0 8px 24px rgba(0,0,0,0.45); word-break: break-word;
  }
  .toast { display: flex; align-items: center; gap: 12px; }
  .tact {
    margin-left: auto; background: rgba(255,255,255,0.08); color: inherit; border: 1px solid rgba(255,255,255,0.25);
    border-radius: 6px; padding: 3px 10px; font-size: 12px; cursor: pointer; font-weight: 600; white-space: nowrap;
  }
  .tact:hover { background: rgba(255,255,255,0.18); }
  .toast.err { background: #2a1414; border-color: #6b2a2a; color: #ffd0c8; }
  .toast.warn { background: #2a2210; border-color: #6b5520; color: #ffe7b0; }
</style>
