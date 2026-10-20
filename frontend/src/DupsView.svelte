<script>
  import { onMount } from 'svelte';
  import { call, mediaUrl } from './lib.js';
  import { toast, reloadLibrary } from './store.svelte.js';

  let { onchanged = () => {}, ongosettings = () => {} } = $props();

  let data = $state(null); // dups.scan result
  let runs = $state([]);
  let loading = $state(true);
  let error = $state(null);
  let busy = $state(false);

  let kindFilter = $state('all'); // all | exact | copies | similar
  let rootFilter = $state('');
  let q = $state('');

  const KIND = {
    exact: { label: 'Identical', long: 'Identical copies', cls: 'k-exact', help: 'Byte-for-byte the same file in more than one place. Any copy can go; the one kept is chosen by name and folder order.' },
    copies: { label: 'Lower quality', long: 'Same photo, lower-quality copies', cls: 'k-copies', help: 'The same picture saved smaller, more compressed, converted, or without its camera data. The best copy is kept.' },
    similar: { label: 'Look-alike', long: 'Look-alike photos — review', cls: 'k-similar', help: 'These look the same but none is clearly worse (same size and format, all with camera data). They may be burst shots or light edits: compare before removing, or mark them as not duplicates.' },
  };

  const groups = $derived.by(() => {
    let g = data?.groups || [];
    if (kindFilter !== 'all') g = g.filter((x) => x.kind === kindFilter);
    if (rootFilter) g = g.filter((x) => x.roots.includes(rootFilter));
    const s = q.trim().toLowerCase();
    if (s) g = g.filter((x) => x.members.some((m) => m.path.toLowerCase().includes(s) || (m.camera || '').toLowerCase().includes(s)));
    return g;
  });
  const allRoots = $derived([...new Set((data?.groups || []).flatMap((g) => g.roots))]);

  // ----- selection -----
  let selId = $state(null);
  const sel = $derived(groups.find((g) => g.id === selId) || null);
  let keepOcc = $state(null);
  let removeSet = $state(new Set());
  function select(g) {
    selId = g.id;
    keepOcc = g.keepOccId;
    removeSet = new Set(g.members.filter((m) => !m.keep && !m.protected).map((m) => m.occId));
    compare = false;
  }
  function chooseKeep(m) {
    keepOcc = m.occId;
    removeSet = new Set(sel.members.filter((x) => x.occId !== m.occId && !x.protected).map((x) => x.occId));
  }
  function toggleRemove(m) {
    const n = new Set(removeSet);
    if (n.has(m.occId)) n.delete(m.occId);
    else n.add(m.occId);
    removeSet = n;
  }
  const removeList = $derived(sel ? sel.members.filter((m) => removeSet.has(m.occId) && m.occId !== keepOcc) : []);
  const removeBytes = $derived(removeList.reduce((n, m) => n + m.bytes, 0));

  // ----- list virtualization -----
  const ROW_H = 84;
  let scroller = $state(null);
  let scrollTop = $state(0);
  let viewportH = $state(600);
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - 4));
  const last = $derived(Math.min(groups.length - 1, Math.ceil((scrollTop + viewportH) / ROW_H) + 4));
  const visible = $derived(Array.from({ length: Math.max(0, last - first + 1) }, (_, k) => first + k));
  $effect(() => {
    if (!scroller) return;
    viewportH = scroller.clientHeight;
    const ro = new ResizeObserver(() => scroller && (viewportH = scroller.clientHeight));
    ro.observe(scroller);
    return () => ro.disconnect();
  });

  async function load() {
    loading = true;
    error = null;
    try {
      [data, runs] = await Promise.all([call('dups.scan'), call('dups.runs', { limit: 15 })]);
      if (selId && !data.groups.some((g) => g.id === selId)) selId = null;
      else if (sel) select(sel);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }
  onMount(load);

  const actionWord = () => (data?.settings?.action === 'move' ? `move to ${data.settings.moveDir}` : 'send to the Recycle Bin');

  function afterClean(r) {
    const parts = [`${r.files.toLocaleString()} file${r.files === 1 ? '' : 's'} ${r.action === 'move' ? 'moved to the review folder' : 'sent to the Recycle Bin'} — ${fmtBytes(r.bytes)} freed`];
    if (r.merged) parts.push(`notes/tags merged into ${r.merged} kept photo${r.merged === 1 ? '' : 's'}`);
    if (r.failed.length) parts.push(`${r.failed.length} failed`);
    if (r.skipped.length) parts.push(`${r.skipped.length} skipped`);
    toast(parts.join(' · '), r.failed.length ? 'warn' : 'ok', 12000, r.runId ? { label: 'Undo', run: () => undo(r.runId) } : null);
    if (r.failed.length || r.skipped.length) console.warn('dedupe', r.failed, r.skipped);
    onchanged();
    reloadLibrary();
  }

  async function cleanSelected() {
    if (!sel || !removeList.length) return;
    const look = sel.kind === 'similar' ? '\n\nThese are look-alikes: make sure they are really the same photo.' : '';
    if (!window.confirm(`Keep 1 copy and ${actionWord()} ${removeList.length} other${removeList.length === 1 ? '' : 's'} (${fmtBytes(removeBytes)})?${look}\n\nYou can undo this.`)) return;
    busy = true;
    try {
      const r = await call('dups.resolve', { groups: [{ id: sel.id, keepOccId: keepOcc, removeOccIds: removeList.map((m) => m.occId) }] });
      afterClean(r);
      const idx = groups.findIndex((g) => g.id === sel.id);
      await load();
      const next = groups[Math.min(idx, groups.length - 1)];
      if (next) select(next);
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 10000);
    } finally {
      busy = false;
    }
  }

  async function cleanAll() {
    const s = data.summary;
    const files = s.exact.files + s.copies.files;
    const bytes = s.exact.bytes + s.copies.bytes;
    if (!files) return;
    if (!window.confirm(
      `Clean up ${(s.exact.groups + s.copies.groups).toLocaleString()} groups?\n\n` +
      `Keeps the best copy of each and will ${actionWord()} ${files.toLocaleString()} file(s), ${fmtBytes(bytes)}:\n` +
      `  • ${s.exact.files.toLocaleString()} identical copies\n  • ${s.copies.files.toLocaleString()} lower-quality copies\n\n` +
      `Look-alikes (${s.similar.groups}) are left for you to review. Protected folders are not touched.\nYou can undo the whole clean-up.`)) return;
    busy = true;
    try {
      afterClean(await call('dups.resolveAll', { kinds: ['exact', 'copies'] }));
      selId = null;
      await load();
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 10000);
    } finally {
      busy = false;
    }
  }

  async function ignore() {
    if (!sel) return;
    busy = true;
    try {
      await call('dups.ignore', { id: sel.id });
      toast('Marked as not duplicates — this group won’t be shown again');
      const idx = groups.findIndex((g) => g.id === sel.id);
      selId = null;
      await load();
      const next = groups[Math.min(idx, groups.length - 1)];
      if (next) select(next);
    } finally {
      busy = false;
    }
  }
  async function unignoreAll() {
    const r = await call('dups.unignoreAll');
    toast(`${r.restored} ignored group(s) will be shown again`);
    load();
  }

  async function undo(runId) {
    busy = true;
    try {
      const r = await call('dups.undo', { runId });
      toast(`Undone: ${r.restored} file(s) back${r.problems.length ? ` — ${r.problems.length} could not be restored` : ''}`, r.problems.length ? 'warn' : 'ok', 9000);
      if (r.problems.length) console.warn('undo problems', r.problems);
      onchanged();
      reloadLibrary();
      await load();
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 9000);
    } finally {
      busy = false;
    }
  }

  // ----- compare / big view -----
  let compare = $state(false);
  let big = $state(null);

  function fmtBytes(b) {
    if (!b) return '0 B';
    if (b >= 1e9) return (b / 1e9).toFixed(2) + ' GB';
    if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
    if (b >= 1e3) return Math.round(b / 1e3) + ' KB';
    return b + ' B';
  }
  const dirOf = (p) => p.split(/[\\/]/).slice(0, -1).join('\\');
  const dims = (m) => (m.width ? `${m.width}×${m.height}` : '—');
  const mp = (m) => (m.width ? ((m.width * m.height) / 1e6).toFixed(1) + ' MP' : '');
  const REL = { keeper: 'keep', identical: 'identical', lower: 'lower quality', lookalike: 'look-alike' };
  const when = (s) => new Date(s * 1000).toLocaleString();
</script>

<div class="dups">
  <div class="listwrap">
    <div class="listhead">
      <div class="title">
        <span class="icn">⧉</span> Duplicates
        {#if data}
          <span class="sub">
            {data.summary.groups.toLocaleString()} groups across {allRoots.length} folder{allRoots.length === 1 ? '' : 's'} ·
            <b>{fmtBytes(data.summary.bytes)}</b> reclaimable
          </span>
        {/if}
        <span class="spacer"></span>
        <button class="btn sm" onclick={ongosettings} title="Duplicate settings">Settings…</button>
        <button class="btn sm" disabled={loading || busy} onclick={load}>{loading ? 'Scanning…' : 'Rescan'}</button>
      </div>
      {#if data}
        <div class="kinds">
          <button class="kind" class:on={kindFilter === 'all'} onclick={() => (kindFilter = 'all')}>All <b>{data.summary.groups}</b></button>
          {#each ['exact', 'copies', 'similar'] as k (k)}
            <button class="kind {KIND[k].cls}" class:on={kindFilter === k} onclick={() => (kindFilter = k)} title={KIND[k].help}>
              {KIND[k].label} <b>{data.summary[k].groups}</b><span class="kb">{fmtBytes(data.summary[k].bytes)}</span>
            </button>
          {/each}
        </div>
        <div class="controls">
          <input type="text" placeholder="Filter by name, folder, camera…" bind:value={q} />
          {#if allRoots.length > 1}
            <select bind:value={rootFilter}>
              <option value="">all folders</option>
              {#each allRoots as r (r)}<option value={r}>{r}</option>{/each}
            </select>
          {/if}
          <span class="count">{groups.length.toLocaleString()}</span>
        </div>
        {#if data.summary.exact.files + data.summary.copies.files > 0}
          <div class="cleanall">
            <button class="btn danger" disabled={busy} onclick={cleanAll}>
              Clean up {(data.summary.exact.files + data.summary.copies.files).toLocaleString()} identical &amp; lower-quality copies ({fmtBytes(data.summary.exact.bytes + data.summary.copies.bytes)})
            </button>
            <span class="hint">keeps the best copy of each · look-alikes wait for review · undoable</span>
          </div>
        {/if}
      {/if}
    </div>

    {#if error}
      <div class="errorbox"><p>Couldn’t scan for duplicates</p><code>{error}</code><button class="btn" onclick={load}>retry</button></div>
    {:else if loading && !data}
      <div class="loading">Looking for duplicates across your library folders…<br /><span class="hint">The first scan fingerprints every photo and can take a minute or two; later scans only look at new ones.</span></div>
    {:else if !groups.length}
      <div class="empty">
        {data?.summary.groups ? 'No groups match the filter.' : 'No duplicates found. 🎉'}
        {#if data?.ignored}<div class="hint">{data.ignored} group(s) marked “not duplicates” are hidden — <button class="lnk" onclick={unignoreAll}>show them again</button></div>{/if}
      </div>
    {:else}
      <div class="scroller" bind:this={scroller} onscroll={() => (scrollTop = scroller.scrollTop)}>
        <div style="height: {groups.length * ROW_H}px; position: relative;">
          {#each visible as i (groups[i].id)}
            {@const g = groups[i]}
            <button class="grouprow" class:sel={selId === g.id} style="top: {i * ROW_H}px;" onclick={() => select(g)}>
              <div class="thumb">
                {#if g.thumbPath}<img src={mediaUrl(g.thumbPath)} alt="" loading="lazy" draggable="false" />{:else}<span class="ph">{g.mediaKind === 'video' ? '▶' : '▣'}</span>{/if}
                <span class="cnt">×{g.members.length}</span>
              </div>
              <div class="meta">
                <div class="gname"><span class="badge {KIND[g.kind].cls}">{KIND[g.kind].label}</span>{g.title}</div>
                <div class="gmeta">
                  <span>{g.roots.join(' · ')}</span>
                  {#if g.takenAt}<span>{g.takenAt.slice(0, 10)}</span>{/if}
                  <span class="rec">{fmtBytes(g.reclaimBytes)}</span>
                </div>
                <div class="greason">keep: {g.reason}</div>
              </div>
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>

  <aside class="panel">
    {#if !sel}
      <div class="panelfill">
        <span class="big">⧉</span>
        <p>Select a group to see every copy, which one is kept and why.</p>
        {#if data?.ignored}<p class="hint">{data.ignored} group(s) marked “not duplicates” are hidden. <button class="lnk" onclick={unignoreAll}>Show again</button></p>{/if}
        {#if runs.length}
          <div class="runs">
            <div class="sectitle">Recent clean-ups</div>
            {#each runs as r (r.id)}
              <div class="run" class:undone={r.status === 'undone'}>
                <span>{when(r.at)}{r.auto ? ' · auto' : ''}</span>
                <span>{r.files.toLocaleString()} file(s) · {fmtBytes(r.bytes)} · {r.action === 'move' ? 'moved' : 'Recycle Bin'}</span>
                {#if r.status === 'undone'}<span class="muted">undone</span>{:else}<button class="lnk" disabled={busy} onclick={() => undo(r.id)}>undo</button>{/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {:else}
      <div class="panelscroll">
        <div class="phead">
          <div>
            <div class="pkind {KIND[sel.kind].cls}">{KIND[sel.kind].long}</div>
            <div class="pname" title={sel.title}>{sel.title}</div>
            <div class="phelp">{KIND[sel.kind].help}</div>
          </div>
          <button class="btn sm" onclick={() => (compare = !compare)}>{compare ? 'List' : 'Compare side by side'}</button>
        </div>

        {#if compare}
          <div class="compare">
            {#each sel.members as m (m.occId)}
              <figure class:keep={m.occId === keepOcc}>
                <button class="cimg" onclick={() => (big = m)} title="View full size">
                  <img src={mediaUrl(m.kind === 'video' ? m.thumbPath : m.path)} alt="" />
                </button>
                <figcaption><b>{m.occId === keepOcc ? 'KEEP' : removeSet.has(m.occId) ? 'remove' : 'leave'}</b> · {dims(m)} · {m.format} · {fmtBytes(m.bytes)}<br />{m.root}</figcaption>
              </figure>
            {/each}
          </div>
        {/if}

        <div class="members">
          {#each sel.members as m (m.occId)}
            {@const keep = m.occId === keepOcc}
            <div class="mem" class:keep class:rm={!keep && removeSet.has(m.occId)}>
              <button class="mthumb" onclick={() => (big = m)} title="View full size">
                {#if m.thumbPath}<img src={mediaUrl(m.thumbPath)} alt="" loading="lazy" />{:else}<span class="ph">▣</span>{/if}
              </button>
              <div class="minfo">
                <div class="mname" title={m.path}>{m.name}{#if m.copyName}<span class="tagc">copy name</span>{/if}</div>
                <div class="mdir" title={dirOf(m.path)}>{dirOf(m.path)}</div>
                <div class="mspec">
                  <b>{dims(m)}</b>{#if mp(m)}<span>{mp(m)}</span>{/if}<span>{m.format}</span><span>{fmtBytes(m.bytes)}</span>
                  <span class={m.hasCamera ? 'ok' : 'dim'}>{m.hasCamera ? 'camera data' : 'no camera data'}</span>
                </div>
                <div class="mfolder">
                  <span class="pill">#{m.rootPriority} {m.root}</span>
                  {#if m.protected}<span class="pill blue">🔒 protected</span>{/if}
                  {#if !m.keep && m.relation !== 'identical'}<span class="pill rel-{m.relation}">{REL[m.relation]}</span>{/if}
                  {#if m.distance != null && sel.kind !== 'exact'}<span class="dist" title="Picture fingerprint difference (0 = identical look)">Δ{m.distance}</span>{/if}
                </div>
              </div>
              <div class="mact">
                <label class="keepbtn" class:on={keep}><input type="radio" name="keep" checked={keep} onchange={() => chooseKeep(m)} /> keep</label>
                {#if !keep}
                  {#if m.protected}
                    <span class="muted small">kept (protected)</span>
                  {:else}
                    <label class="rmbtn"><input type="checkbox" checked={removeSet.has(m.occId)} onchange={() => toggleRemove(m)} /> remove</label>
                  {/if}
                {/if}
              </div>
            </div>
          {/each}
        </div>

        <div class="why">Suggested keep: <b>{sel.reason}</b>{#if keepOcc !== sel.keepOccId}<span class="warn"> · you picked a different copy</span>{/if}</div>

        <div class="actions">
          <button class="btn danger" disabled={busy || !removeList.length} onclick={cleanSelected}>
            {busy ? 'Working…' : removeList.length ? `Keep 1, ${data.settings.action === 'move' ? 'move' : 'recycle'} ${removeList.length} (${fmtBytes(removeBytes)})` : 'Nothing selected to remove'}
          </button>
          <button class="btn" disabled={busy} onclick={ignore} title="Hide this group from now on">Not duplicates</button>
        </div>
        <p class="hint">
          {data.settings.action === 'move' ? `Removed copies go to ${data.settings.moveDir}.` : 'Removed copies go to the Recycle Bin.'}
          {data.settings.mergeAnnotations ? ' Their notes, tags, rating and albums are added to the kept copy.' : ''}
          Undo from the toast, or from “Recent clean-ups”.
        </p>
      </div>
    {/if}
  </aside>

  {#if big}
    <div class="lb" role="button" tabindex="0" onclick={() => (big = null)} onkeydown={(e) => e.key === 'Escape' && (big = null)}>
      <img src={mediaUrl(big.kind === 'video' ? big.thumbPath : big.path)} alt="" />
      <div class="lbcap">{big.path} · {dims(big)} · {big.format} · {fmtBytes(big.bytes)}</div>
    </div>
  {/if}
</div>

<style>
  .dups { display: flex; flex: 1; min-height: 0; position: relative; }
  .listwrap { flex: 1 1 58%; min-width: 0; display: flex; flex-direction: column; border-right: 1px solid var(--border); }
  .listhead { padding: 10px 14px; border-bottom: 1px solid var(--border); background: var(--panel); display: flex; flex-direction: column; gap: 8px; }
  .title { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .title .icn { color: var(--accent); font-size: 18px; }
  .title .sub { color: var(--muted); font-size: 12.5px; }
  .title .sub b { color: #f4a620; }
  .spacer { flex: 1; }
  .kinds { display: flex; gap: 6px; flex-wrap: wrap; }
  .kind { background: var(--bg); border: 1px solid var(--border); color: var(--muted); border-radius: 8px; padding: 4px 10px; font-size: 12px; cursor: pointer; }
  .kind b { color: var(--text); margin-left: 3px; }
  .kind .kb { margin-left: 6px; font-size: 11px; color: #6b7688; }
  .kind.on { border-color: var(--accent); color: var(--text); background: var(--panel2); }
  .controls { display: flex; gap: 8px; align-items: center; }
  .controls input { flex: 1 1 220px; background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 6px 10px; border-radius: 7px; font-size: 13px; }
  .controls select { background: var(--bg); border: 1px solid var(--border); color: var(--text); padding: 6px 8px; border-radius: 7px; font-size: 12.5px; }
  .count { font-size: 12px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .cleanall { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .scroller { flex: 1; overflow-y: auto; min-height: 0; }
  .grouprow {
    position: absolute; left: 0; right: 0; height: 84px; box-sizing: border-box; width: 100%; text-align: left;
    display: flex; gap: 12px; padding: 8px 14px; cursor: pointer; background: none; border: 0;
    border-bottom: 1px solid #161a22; align-items: center; color: inherit; font: inherit;
  }
  .grouprow:hover { background: var(--panel); }
  .grouprow.sel { background: #14202e; box-shadow: inset 3px 0 0 var(--accent); }
  .thumb { position: relative; width: 66px; height: 66px; flex: 0 0 auto; border-radius: 8px; overflow: hidden; background: #0a0c10; }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; pointer-events: none; }
  .thumb .ph, .mthumb .ph { position: absolute; inset: 0; display: grid; place-items: center; color: #5a6270; font-size: 22px; }
  .thumb .cnt { position: absolute; right: 3px; bottom: 3px; background: rgba(0,0,0,0.75); color: #fff; font-size: 10.5px; font-weight: 700; padding: 0 5px; border-radius: 6px; }
  .meta { min-width: 0; flex: 1; }
  .gname { font-size: 13px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--text); }
  .gmeta { display: flex; gap: 10px; margin-top: 3px; font-size: 11.5px; color: var(--muted); white-space: nowrap; overflow: hidden; }
  .gmeta .rec { color: #f4a620; margin-left: auto; font-variant-numeric: tabular-nums; }
  .greason { margin-top: 3px; font-size: 11px; color: #6b7688; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .badge { font-size: 9.5px; font-weight: 700; padding: 1px 6px; border-radius: 7px; margin-right: 7px; text-transform: uppercase; letter-spacing: 0.3px; vertical-align: 1px; }
  .k-exact.badge, .kind.k-exact.on { background: #1d2a3d; color: #8ab4f8; }
  .k-copies.badge, .kind.k-copies.on { background: #3a2a10; color: #ffcf7a; }
  .k-similar.badge, .kind.k-similar.on { background: #33203d; color: #e0b0ff; }
  .loading, .empty { padding: 40px; color: var(--muted); line-height: 1.6; }

  .panel { flex: 1 1 42%; min-width: 360px; max-width: 640px; background: var(--panel); display: flex; flex-direction: column; }
  .panelfill { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: var(--muted); padding: 30px; text-align: center; }
  .panelfill .big { font-size: 42px; color: #39404e; }
  .runs { width: 100%; max-width: 480px; margin-top: 16px; text-align: left; }
  .run { display: flex; gap: 10px; font-size: 12px; padding: 5px 0; border-bottom: 1px solid #1d2129; align-items: center; }
  .run span:nth-child(2) { flex: 1; color: var(--text); }
  .run.undone { opacity: 0.55; }
  .sectitle { font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; color: var(--muted); margin-bottom: 6px; }
  .panelscroll { flex: 1; overflow-y: auto; padding: 14px; min-height: 0; }
  .phead { display: flex; gap: 10px; align-items: flex-start; justify-content: space-between; margin-bottom: 12px; }
  .pkind { display: inline-block; font-size: 10.5px; font-weight: 700; padding: 2px 8px; border-radius: 8px; text-transform: uppercase; letter-spacing: 0.4px; }
  .pkind.k-exact { background: #1d2a3d; color: #8ab4f8; }
  .pkind.k-copies { background: #3a2a10; color: #ffcf7a; }
  .pkind.k-similar { background: #33203d; color: #e0b0ff; }
  .pname { font-size: 15px; font-weight: 600; margin-top: 5px; word-break: break-all; }
  .phelp { font-size: 12px; color: var(--muted); margin-top: 4px; line-height: 1.45; }
  .compare { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 8px; margin-bottom: 12px; }
  .compare figure { margin: 0; border: 1px solid #222b38; border-radius: 8px; overflow: hidden; background: #0a0c10; }
  .compare figure.keep { border-color: #1f5c38; }
  .cimg { display: block; width: 100%; padding: 0; border: 0; background: none; cursor: zoom-in; }
  .cimg img { width: 100%; height: 160px; object-fit: contain; display: block; }
  .compare figcaption { font-size: 11px; color: var(--muted); padding: 5px 7px; line-height: 1.4; }
  .compare figure.keep figcaption b { color: #6fdc95; }
  .members { display: flex; flex-direction: column; gap: 8px; }
  .mem { display: flex; gap: 10px; padding: 8px; border: 1px solid #222b38; border-radius: 9px; background: #10141b; align-items: center; }
  .mem.keep { border-color: #1f5c38; background: #0f2016; }
  .mem.rm { border-color: #5a3418; }
  .mthumb { position: relative; width: 64px; height: 64px; flex: 0 0 auto; border-radius: 7px; overflow: hidden; background: #0a0c10; border: 0; padding: 0; cursor: zoom-in; }
  .mthumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .minfo { flex: 1; min-width: 0; }
  .mname { font-size: 12.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .tagc { font-size: 9.5px; margin-left: 6px; color: #ffcf7a; border: 1px solid #5a4415; border-radius: 6px; padding: 0 5px; }
  .mdir { font-size: 10.5px; color: #6b7688; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin-top: 1px; }
  .mspec { display: flex; gap: 8px; flex-wrap: wrap; font-size: 11.5px; color: var(--muted); margin-top: 3px; font-variant-numeric: tabular-nums; }
  .mspec b { color: var(--text); }
  .mspec .ok { color: #6fdc95; }
  .mspec .dim { color: #7d6a5a; }
  .mfolder { display: flex; gap: 5px; flex-wrap: wrap; margin-top: 4px; align-items: center; }
  .pill { font-size: 10px; padding: 1px 7px; border-radius: 8px; background: #1a1e26; border: 1px solid #2c3340; color: #b8c2d0; }
  .pill.blue { color: #8ab4f8; border-color: #2c3a4d; background: #111a28; }
  .pill.rel-lower { color: #ffcf7a; border-color: #5a4415; background: #2a2010; }
  .pill.rel-lookalike { color: #e0b0ff; border-color: #4a2d5a; background: #241830; }
  .dist { font-size: 10px; color: #6b7688; }
  .mact { display: flex; flex-direction: column; gap: 6px; flex: 0 0 auto; font-size: 12px; }
  .mact input { accent-color: var(--accent); }
  .keepbtn.on { color: #6fdc95; font-weight: 600; }
  .rmbtn { color: #ffb48a; }
  .why { margin: 12px 0 10px; font-size: 12.5px; color: var(--muted); }
  .why b { color: var(--text); font-weight: 600; }
  .warn { color: #f4a620; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .hint { font-size: 11.5px; color: #7d8796; line-height: 1.45; }
  .muted { color: var(--muted); }
  .small { font-size: 11px; }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 12px; padding: 0 4px; }
  .btn { background: var(--panel2); color: var(--text); border: 1px solid var(--border); padding: 7px 14px; border-radius: 8px; cursor: pointer; font-size: 13px; }
  .btn.sm { padding: 4px 10px; font-size: 12px; }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn.danger { background: #3a2210; border-color: #7a4a1a; color: #ffcf99; }
  .btn.danger:hover:not(:disabled) { border-color: #f47b20; }
  .errorbox { padding: 16px; margin: 16px; border: 1px solid #6b2a2a; background: #241212; border-radius: 10px; }
  .errorbox code { display: block; margin: 8px 0; padding: 8px; background: #150b0b; border-radius: 6px; color: #f8a; font-size: 11.5px; white-space: pre-wrap; word-break: break-all; }
  .lb { position: fixed; inset: 0; background: rgba(0,0,0,0.88); z-index: 90; display: flex; flex-direction: column; align-items: center; justify-content: center; cursor: zoom-out; }
  .lb img { max-width: 94vw; max-height: 88vh; object-fit: contain; }
  .lbcap { color: #cfd6e0; font-size: 12px; margin-top: 8px; }
</style>
