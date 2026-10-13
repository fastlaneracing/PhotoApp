<script>
  import { onMount } from 'svelte';
  import { dupGroups, dupSummary, stagingDryRun, stagingApply, stagingUndo, mediaUrl } from './lib.js';

  let { onopenviewer = null } = $props();

  // ----- list data (client-side filter + sort; 9,921 groups is small) -----
  let groups = $state([]);
  let summary = $state(null);
  let loading = $state(true);
  let error = $state(null);

  let searchDraft = $state('');
  let flaggedOnly = $state('all'); // 'all' | 'flagged' | 'unflagged'
  let sortMode = $state('sizeDesc'); // 'sizeDesc' | 'nameAsc'

  // Virtualization: fixed 96px rows, only the visible window (+overscan) in DOM.
  const ROW_H = 96;
  const OVERSCAN = 4;
  let scroller = $state(null);
  let scrollTop = $state(0);
  let viewportH = $state(600);
  let firstRow = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  let lastRow = $derived(Math.min(filtered.length - 1, Math.ceil((scrollTop + viewportH) / ROW_H) + OVERSCAN));
  const visibleRows = $derived(
    Array.from({ length: Math.max(0, lastRow - firstRow + 1) }, (_, k) => firstRow + k)
  );
  let rendered = $derived(visibleRows.length);

  const filtered = $derived.by(() => {
    let out = groups;
    const q = searchDraft.trim().toLowerCase();
    if (q) {
      out = out.filter((g) =>
        g.filename.toLowerCase().includes(q) ||
        (g.camera || '').toLowerCase().includes(q) ||
        g.occurrences.some((o) => o.path.toLowerCase().includes(q))
      );
    }
    if (flaggedOnly === 'flagged') out = out.filter((g) => g.hasFlagged);
    if (flaggedOnly === 'unflagged') out = out.filter((g) => !g.hasFlagged);
    if (sortMode === 'nameAsc') {
      out = [...out].sort((a, b) => a.filename.localeCompare(b.filename));
    }
    // sizeDesc is the server order — keep as-is
    return out;
  });

  // ----- selection + staging workflow (right panel) -----
  let selectedId = $state(null);
  let selected = $derived(groups.find((g) => g.mediaId === selectedId) || null);
  // 'idle' -> [Preview plan] ; 'preview' -> plan shown, [Apply] ; 'applied' -> [Undo]
  let stageState = $state('idle');
  let plan = $state(null);
  let busy = $state(false);
  let actionError = $state(null);
  let lastReportPath = $state(null);

  function select(g) {
    selectedId = g.mediaId;
    stageState = g.hasFlagged ? 'applied' : 'idle';
    plan = null;
    actionError = null;
    lastReportPath = null;
  }

  async function refreshGroupState() {
    // Re-fetch just the one group's state after an action (flags changed).
    if (!selectedId) return;
    try {
      const all = await dupGroups();
      const fresh = all.find((g) => g.mediaId === selectedId);
      if (fresh) {
        const i = groups.findIndex((g) => g.mediaId === selectedId);
        if (i >= 0) groups[i] = fresh;
        groups = [...groups]; // trigger $derived
        if (fresh.hasFlagged) stageState = 'applied';
      }
      summary = await dupSummary();
    } catch (e) {
      console.warn('refresh failed', e);
    }
  }

  async function previewPlan() {
    if (!selected) return;
    busy = true;
    actionError = null;
    try {
      plan = await stagingDryRun(selected.mediaId);
      stageState = 'preview';
      lastReportPath = plan.reportPath;
    } catch (e) {
      actionError = String(e);
    } finally {
      busy = false;
    }
  }

  async function applyStaging() {
    if (!selected || !plan) return;
    const n = plan.moves.filter((m) => m.onDisk).length;
    const ok = window.confirm(
      `Stage ${n} duplicate file${n === 1 ? '' : 's'} (~${fmtBytes(selected.bytes)} each)\n` +
      `into: ${plan.stagingDir}\n` +
      `The original is kept in place. Nothing is deleted.\n\nProceed?`
    );
    if (!ok) return;
    busy = true;
    actionError = null;
    try {
      const res = await stagingApply(selected.mediaId);
      lastReportPath = res.reportPath;
      stageState = 'applied';
      plan = null;
      await refreshGroupState();
    } catch (e) {
      actionError = String(e);
    } finally {
      busy = false;
    }
  }

  async function undoStaging() {
    if (!selected) return;
    const ok = window.confirm(
      `Undo staging for ${selected.filename}\n` +
      `— moves staged copies back to their original folders and clears the flags.`
    );
    if (!ok) return;
    busy = true;
    actionError = null;
    try {
      const res = await stagingUndo(selected.mediaId);
      lastReportPath = res.reportPath;
      if (res.warnings && res.warnings.length) {
        actionError = 'Undo finished with warnings:\n' + res.warnings.join('\n');
      }
      stageState = 'idle';
      plan = null;
      await refreshGroupState();
    } catch (e) {
      actionError = String(e);
    } finally {
      busy = false;
    }
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const [gs, s] = await Promise.all([dupGroups(), dupSummary()]);
      groups = gs;
      summary = s;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => load());

  // The scroller node appears asynchronously (after the list loads) — attach
  // the observer reactively so it is guaranteed to exist.
  $effect(() => {
    if (!scroller) return;
    viewportH = scroller.clientHeight;
    const ro = new ResizeObserver(() => {
      if (scroller) viewportH = scroller.clientHeight;
    });
    ro.observe(scroller);
    return () => ro.disconnect();
  });

  function fmtBytes(b) {
    if (!b) return '0 B';
    if (b >= 1e9) return (b / 1e9).toFixed(2) + ' GB';
    if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
    if (b >= 1e3) return Math.round(b / 1e3) + ' KB';
    return b + ' B';
  }
  function base(p) {
    return p.split(/[\\/]/).pop() || p;
  }
  function dirOf(p) {
    return p.split(/[\\/]/).slice(0, -1).join('\\');
  }
  function fmtDate(s) {
    return (s || '').replace('T', ' ');
  }
</script>

<div class="dups">
  <!-- ===== left: virtualized group list ===== -->
  <div class="listwrap">
    <div class="listhead">
      <div class="title">
        <span class="icn">⧉</span> Duplicates
        {#if summary}
          <span class="sub">
            {summary.groups} groups · {summary.duplicateOccurrences} extra copies ·
            <b>{fmtBytes(summary.wastedBytes)}</b> reclaimable
            {#if summary.flagged} · {summary.flagged} staged{/if}
          </span>
        {/if}
      </div>
      <div class="controls">
        <input type="text" placeholder="Filter by name, camera, folder…" bind:value={searchDraft} />
        <select value={flaggedOnly} onchange={(e) => (flaggedOnly = e.target.value)}>
          <option value="all">all</option>
          <option value="unflagged">not staged</option>
          <option value="flagged">staged</option>
        </select>
        <select value={sortMode} onchange={(e) => (sortMode = e.target.value)}>
          <option value="sizeDesc">largest</option>
          <option value="nameAsc">name A–Z</option>
        </select>
        <span class="count">{filtered.length}</span>
      </div>
    </div>

    {#if error}
      <div class="errorbox">
        <p>Failed to load duplicates</p>
        <code>{error}</code>
        <button class="btn" onclick={load}>retry</button>
      </div>
    {:else if loading}
      <div class="loading">Loading duplicate groups…</div>
    {:else if filtered.length === 0}
      <div class="empty">No groups match.</div>
    {:else}
      <div class="scroller" bind:this={scroller} onscroll={() => (scrollTop = scroller.scrollTop)}>
        <div class="space" style="height: {filtered.length * 96}px; position: relative;">
          {#each visibleRows as i (i)}
            {@const g = filtered[i]}
            <div
              class="grouprow {selectedId === g.mediaId ? 'sel' : ''}"
              style="top: {i * 96}px;"
              onclick={() => select(g)}
            >
              <div class="thumb">
                {#if g.thumbPath}
                  <img src={mediaUrl(g.thumbPath)} alt="" loading="lazy" draggable="false" />
                {:else}
                  <span class="ph">{g.kind === 'video' ? '▶' : '▣'}</span>
                {/if}
                {#if g.hasFlagged}
                  <span class="chipflag">{g.fullyFlagged ? 'staged' : 'partial'}</span>
                {/if}
              </div>
              <div class="meta">
                <div class="gname" title={g.filename}>{g.filename}</div>
                <div class="gmeta">
                  <span>×{g.occurrences.length}</span>
                  <span>{fmtBytes(g.bytes)}</span>
                  {#if g.takenAt}<span>{g.takenAt.slice(0, 10)}</span>{/if}
                  {#if g.camera}<span>{g.camera}</span>{/if}
                </div>
                <div class="gdir" title={g.occurrences[0]?.path || ''}>{base(g.occurrences[0]?.path || '')} …</div>
              </div>
              <div class="acthint">{selectedId === g.mediaId ? '→' : 'view'}</div>
            </div>
          {/each}
        </div>
      </div>
      <div class="listfoot">
        {filtered.length} groups · {rendered} DOM rows rendered · virtualized
      </div>
    {/if}
  </div>

  <!-- ===== right: detail + staging workflow ===== -->
  <aside class="panel" class:empty={!selected}>
    {#if !selected}
      <div class="panelfill">
        <span class="big">⧉</span>
        <p>Select a duplicate group to inspect its copies and stage them.</p>
        <p class="hint">
          Staging moves the extra copies into a <code>duplicates</code> folder in your first library root —
          the original stays put, nothing is ever deleted.
        </p>
      </div>
    {:else}
      <div class="panelscroll">
        <div class="phead">
          <div class="pthumb">
            {#if selected.thumbPath}
              <img src={mediaUrl(selected.thumbPath)} alt="" />
            {:else}
              <span class="ph">{selected.kind === 'video' ? '▶' : '▣'}</span>
            {/if}
          </div>
          <div class="pinfo">
            <div class="pname" title={selected.filename}>{selected.filename}</div>
            <div class="prow">
              <b>{fmtBytes(selected.bytes)}</b> · {selected.kind} · {selected.occurrences.length} copies
            </div>
            {#if selected.takenAt}<div class="prow">{fmtDate(selected.takenAt)} ({selected.dateSource})</div>{/if}
            {#if selected.camera}<div class="prow">{selected.camera}</div>{/if}
            {#if selected.width}<div class="prow">{selected.width}×{selected.height}</div>{/if}
            <div class="prow sha" title={selected.sha256}>{selected.sha256.slice(0, 18)}…</div>
          </div>
        </div>

        <div class="section">
          <div class="sectitle">Copies ({selected.occurrences.length})</div>
          <div class="occ">
            {#each selected.occurrences as o (o.id)}
              <div class="occrow {o.isCanonical ? 'canon' : ''} {o.missing ? 'miss' : ''}">
                <span class="tag">{o.isCanonical ? 'keep' : o.isFlagged ? 'staged' : 'dup'}</span>
                <div class="occp">
                  <div class="occname" title={o.path}>{base(o.path)}</div>
                  <div class="occdir">{dirOf(o.path)}</div>
                  {#if o.missing}<div class="warnline">missing on disk</div>{/if}
                  {#if o.isFlagged && o.existsOnDisk}<div class="okline">on disk (staged)</div>{/if}
                </div>
              </div>
            {/each}
          </div>
        </div>

        <div class="section actions">
          {#if stageState === 'idle'}
            <button class="btn primary" disabled={busy} onclick={previewPlan}>
              {busy ? 'working…' : 'Preview staging plan'}
            </button>
            <p class="hint">Writes a dry-run report to <code>data\reports</code>. Moves nothing.</p>
          {:else if stageState === 'preview' && plan}
            <div class="plan">
              <div class="planhead">
                Staging {plan.moves.filter((m) => m.onDisk).length} file(s)
                {#if plan.crossDrive}<span class="warnchip">cross-drive: copy-then-remove</span>{/if}
              </div>
              <div class="plantarget">into <code>{plan.stagingDir}</code></div>
              {#each plan.moves as m (m.occId)}
                <div class="planmove">
                  <div class="pl">
                    <span class="src" title={m.sourcePath}>{base(m.sourcePath)}</span>
                    <span class="arr">→</span>
                    <span class="dst" title={m.targetPath}>{base(m.targetPath)}</span>
                    {#if !m.onDisk}<span class="warnchip">missing — flag only</span>{/if}
                    {#if !m.targetFree}<span class="badchip">collision</span>{/if}
                  </div>
                </div>
              {/each}
              {#if plan.warnings.length}
                <div class="warns">
                  {#each plan.warnings as w (w)}<div>⚠ {w}</div>{/each}
                </div>
              {/if}
              {#if lastReportPath}<div class="report">report: <code>{lastReportPath}</code></div>{/if}
            </div>
            <div class="btnrow">
              <button class="btn" disabled={busy} onclick={() => { stageState = 'idle'; plan = null; }}>back</button>
              <button class="btn danger" disabled={busy} onclick={applyStaging}>
                {busy ? 'moving…' : `Apply — move ${plan.moves.filter((m) => m.onDisk).length} file(s)`}
              </button>
            </div>
          {:else if stageState === 'applied'}
            <div class="appliedbox">
              <span class="okchip">✓ staged</span>
              <p>Extra copies are flagged. Undo moves them back and clears the flags.</p>
              {#if lastReportPath}<div class="report">report: <code>{lastReportPath}</code></div>{/if}
            </div>
            <div class="btnrow">
              <button class="btn" disabled={busy} onclick={() => load()}>refresh</button>
              <button class="btn" disabled={busy} onclick={undoStaging}>{busy ? 'undoing…' : 'Undo staging'}</button>
            </div>
          {/if}

          {#if actionError}
            <div class="errorbox small">
              <pre>{actionError}</pre>
            </div>
          {/if}
        </div>

        <div class="charternote">
          Charter: dry-run report before any move · undo journal · never deletes ·
          collisions fail loudly · cross-drive = copy-then-rename.
        </div>
      </div>
    {/if}
  </aside>
</div>

<style>
  .dups { display: flex; flex: 1; min-height: 0; }

  /* ---------- left list ---------- */
  .listwrap { flex: 1 1 62%; min-width: 0; display: flex; flex-direction: column; border-right: 1px solid var(--border); }
  .listhead { padding: 10px 14px; border-bottom: 1px solid var(--border); background: var(--panel); }
  .title { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; margin-bottom: 8px; }
  .title .icn { color: var(--accent); font-size: 18px; }
  .title .sub { color: var(--muted); font-size: 12.5px; }
  .title .sub b { color: #f4a620; }
  .controls { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .controls input { flex: 1 1 220px; background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 6px 10px; border-radius: 7px; font-size: 13px; }
  .controls input:focus { outline: none; border-color: var(--accent); }
  .controls select { background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 6px 8px; border-radius: 7px; font-size: 12.5px; }
  .controls select:focus { outline: none; border-color: var(--accent); }
  .count { font-size: 12px; color: var(--muted); font-variant-numeric: tabular-nums; }

  .scroller { flex: 1; overflow-y: auto; min-height: 0; }
  .grouprow {
    position: absolute; left: 0; right: 0; height: 96px; box-sizing: border-box;
    display: flex; gap: 12px; padding: 10px 14px; cursor: pointer;
    border-bottom: 1px solid #161a22; align-items: center;
  }
  .grouprow:hover { background: var(--panel); }
  .grouprow.sel { background: #14202e; box-shadow: inset 3px 0 0 var(--accent); }
  .thumb { position: relative; width: 72px; height: 72px; flex: 0 0 auto; border-radius: 8px; overflow: hidden; background: #0a0c10; }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; pointer-events: none; }
  .thumb .ph { position: absolute; inset: 0; display: grid; place-items: center; color: #5a6270; font-size: 24px; }
  .chipflag { position: absolute; bottom: 4px; left: 4px; font-size: 9.5px; font-weight: 700; background: rgba(244,123,32,0.92); color: #1a1206; padding: 1px 6px; border-radius: 8px; letter-spacing: 0.3px; }
  .meta { min-width: 0; flex: 1; }
  .gname { font-size: 13.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--text); }
  .gmeta { display: flex; gap: 10px; margin-top: 3px; font-size: 11.5px; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; overflow: hidden; }
  .gdir { margin-top: 3px; font-size: 10.5px; color: #6b7688; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .acthint { flex: 0 0 auto; font-size: 11px; color: var(--muted); }
  .grouprow.sel .acthint { color: var(--accent); }

  .loading, .empty { padding: 40px; color: var(--muted); }
  .errorbox { padding: 16px; margin: 16px; border: 1px solid #6b2a2a; background: #241212; border-radius: 10px; }
  .errorbox.small { margin: 10px 0 0; padding: 10px; }
  .errorbox code, .errorbox pre { display: block; margin: 8px 0; padding: 8px; background: #150b0b; border-radius: 6px; color: #f8a; font-size: 11.5px; white-space: pre-wrap; word-break: break-all; }
  .btn { background: var(--panel2); color: var(--text); border: 1px solid var(--border); padding: 7px 14px; border-radius: 8px; cursor: pointer; font-size: 13px; }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.55; cursor: default; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn.danger { background: #3a2210; border-color: #7a4a1a; color: #ffcf99; }
  .btn.danger:hover { border-color: #f47b20; }

  /* ---------- right panel ---------- */
  .panel { flex: 1 1 38%; min-width: 320px; max-width: 560px; background: var(--panel); display: flex; flex-direction: column; }
  .panel.empty { background: transparent; }
  .panelfill { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: var(--muted); padding: 30px; text-align: center; }
  .panelfill .big { font-size: 42px; color: #39404e; }
  .panelfill code { background: #151a22; padding: 1px 6px; border-radius: 5px; font-size: 12px; }
  .hint { font-size: 12px; color: var(--muted); }

  .panelscroll { flex: 1; overflow-y: auto; padding: 14px; min-height: 0; }
  .phead { display: flex; gap: 12px; margin-bottom: 14px; }
  .pthumb { width: 92px; height: 92px; flex: 0 0 auto; border-radius: 9px; overflow: hidden; background: #0a0c10; position: relative; }
  .pthumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .pthumb .ph { position: absolute; inset: 0; display: grid; place-items: center; color: #5a6270; font-size: 28px; }
  .pinfo { min-width: 0; }
  .pname { font-size: 15px; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .prow { margin-top: 3px; font-size: 12.5px; color: var(--muted); }
  .prow b { color: var(--text); }
  .prow.sha { font-family: ui-monospace, Consolas, monospace; font-size: 11px; color: #5a6270; }

  .section { border-top: 1px solid var(--border); padding: 12px 0; }
  .section.actions { padding-top: 4px; }
  .sectitle { font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; color: var(--muted); margin-bottom: 8px; }
  .occ { display: flex; flex-direction: column; gap: 7px; }
  .occrow { display: flex; gap: 8px; align-items: flex-start; padding: 7px 9px; border: 1px solid #222b38; border-radius: 8px; background: #10141b; }
  .occrow.canon { border-color: #1f5c38; background: #0f2016; }
  .occrow.miss { opacity: 0.75; }
  .tag { flex: 0 0 auto; font-size: 10px; font-weight: 700; padding: 2px 8px; border-radius: 9px; background: #2c3a4d; color: #cfe0ff; margin-top: 1px; }
  .occrow.canon .tag { background: #1f5c38; color: #9ff0b8; }
  .occp { min-width: 0; flex: 1; }
  .occname { font-size: 12.5px; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .occdir { font-size: 10.5px; color: #6b7688; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin-top: 1px; }
  .warnline { color: #f4a620; font-size: 11px; margin-top: 2px; }
  .okline { color: #39c26d; font-size: 11px; margin-top: 2px; }

  .plan { border: 1px solid #2c3a4d; border-radius: 9px; padding: 10px; margin-bottom: 10px; background: #0f141c; }
  .planhead { font-size: 13px; margin-bottom: 4px; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .plantarget { font-size: 12px; color: var(--muted); margin-bottom: 8px; }
  .plantarget code { background: #151a22; padding: 1px 6px; border-radius: 5px; font-size: 11.5px; }
  .planmove { padding: 5px 0; border-top: 1px dashed #222b38; }
  .planmove .pl { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; font-size: 12px; }
  .planmove .src { color: #ffcf99; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 45%; }
  .planmove .arr { color: #5a6270; }
  .planmove .dst { color: #cfe0ff; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .warns { margin-top: 8px; }
  .warns div { color: #f4a620; font-size: 11.5px; margin-top: 3px; }
  .report { margin-top: 8px; font-size: 11px; color: var(--muted); }
  .report code { background: #151a22; padding: 1px 6px; border-radius: 5px; font-size: 10.5px; word-break: break-all; }
  .warnchip { font-size: 10px; font-weight: 700; background: #4a3413; color: #ffd28a; padding: 1px 7px; border-radius: 8px; }
  .badchip { font-size: 10px; font-weight: 700; background: #5c1f1f; color: #ff9c9c; padding: 1px 7px; border-radius: 8px; }
  .okchip { font-size: 11px; font-weight: 700; background: #1f5c38; color: #9ff0b8; padding: 2px 9px; border-radius: 9px; }
  .appliedbox { margin-bottom: 10px; }
  .appliedbox p { font-size: 12.5px; color: var(--muted); margin: 6px 0 0; }
  .btnrow { display: flex; gap: 8px; }
  .charternote { border-top: 1px solid var(--border); margin-top: 12px; padding: 10px 2px 4px; font-size: 10.5px; color: #556072; line-height: 1.5; }
  .listfoot { padding: 5px 14px; font-size: 11px; color: var(--muted); background: var(--panel); border-top: 1px solid var(--border); font-variant-numeric: tabular-nums; }
</style>
