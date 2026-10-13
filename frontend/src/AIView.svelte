<script>
  import { onMount } from 'svelte';
  import { call, mediaUrl } from './lib.js';
  import { ai, refreshAi, onAiProgress, toast, describeChange, SUGG_LABEL } from './store.svelte.js';
  import AISettings from './AISettings.svelte';

  let { onchanged = () => {}, onshow = () => {} } = $props();

  let showSettings = $state(false);
  let jobFilter = $state(null); // job id or null
  let cards = $state([]);
  let totalPhotos = $state(0);
  let totalSuggestions = $state(0);
  let loading = $state(false);
  let busy = $state(false);
  let recent = $state([]);
  let big = $state(null); // lightbox item
  const PAGE = 24;

  // Per-suggestion UI state: checked + edited payload.
  let decide = $state({}); // sid -> { on: bool, payload: {...}, edited: bool }

  async function loadQueue(append = false) {
    loading = true;
    try {
      const r = await call('ai.queue', { limit: PAGE, offset: append ? cards.length : 0, jobId: jobFilter ?? undefined });
      for (const c of r.cards) {
        for (const s of c.suggestions) {
          if (!decide[s.id]) decide[s.id] = { on: true, payload: { ...s.payload }, edited: false };
        }
      }
      cards = append ? [...cards, ...r.cards] : r.cards;
      totalPhotos = r.totalPhotos;
      totalSuggestions = r.totalSuggestions;
    } catch (e) {
      toast(String(e), 'err');
    } finally {
      loading = false;
    }
  }

  async function loadRecent() {
    try {
      recent = await call('history.recent', { source: 'ai', limit: 60 });
    } catch {}
  }

  async function reloadAll() {
    await Promise.all([loadQueue(), loadRecent(), refreshAi()]);
  }

  onMount(() => {
    reloadAll();
    if (ai.stats && !ai.stats.configured) showSettings = true;
    const off = onAiProgress(() => {
      if (!busy) loadQueue();
    });
    return off;
  });

  $effect(() => {
    if (ai.stats && !ai.stats.configured) showSettings = true;
  });

  function edit(sid, key, value) {
    decide[sid].payload[key] = value;
    decide[sid].edited = true;
  }

  /** Approve the checked suggestions and reject the unchecked ones of these cards. */
  async function decideCards(list) {
    const approveIds = [];
    const rejectIds = [];
    const edits = {};
    for (const c of list) {
      for (const s of c.suggestions) {
        const d = decide[s.id];
        if (d?.on) {
          approveIds.push(s.id);
          if (d.edited) edits[s.id] = d.payload;
        } else rejectIds.push(s.id);
      }
    }
    busy = true;
    try {
      let applied = 0;
      let failed = [];
      if (approveIds.length) {
        const r = await call('ai.approve', { suggestionIds: approveIds, edits });
        applied = r.applied;
        failed = r.failed;
      }
      if (rejectIds.length) await call('ai.reject', { suggestionIds: rejectIds });
      toast(`Applied ${applied} change${applied === 1 ? '' : 's'}${rejectIds.length ? `, rejected ${rejectIds.length}` : ''}` + (failed.length ? ` — ${failed.length} failed` : ''), failed.length ? 'warn' : 'ok');
      if (failed.length) console.warn(failed);
      onchanged();
    } catch (e) {
      toast(String(e), 'err');
    } finally {
      busy = false;
      await reloadAll();
    }
  }

  async function rejectCard(c) {
    busy = true;
    try {
      await call('ai.reject', { mediaIds: [c.mediaId], jobId: jobFilter ?? undefined });
    } finally {
      busy = false;
      reloadAll();
    }
  }

  async function jobAction(j, what) {
    const msgs = {
      approve: `Approve all ${j.pending} pending suggestion(s) from “${j.label}” without reviewing them one by one?`,
      reject: `Reject all ${j.pending} pending suggestion(s) from “${j.label}”?`,
      undo: `Undo the ${j.liveChanges} change(s) this run applied? Notes, tags and descriptions go back to how they were before.`,
    };
    if (msgs[what] && !window.confirm(msgs[what])) return;
    busy = true;
    try {
      if (what === 'approve') {
        const r = await call('ai.approve', { jobId: j.id });
        toast(`Applied ${r.applied} change(s)`);
      } else if (what === 'reject') {
        const r = await call('ai.reject', { jobId: j.id });
        toast(`Rejected ${r.rejected}`);
      } else if (what === 'undo') {
        const r = await call('history.undoJob', { jobId: j.id });
        toast(`Undid ${r.undone} change(s)` + (r.conflicts.length ? ` — ${r.conflicts.length} skipped (edited since)` : ''), r.conflicts.length ? 'warn' : 'ok', 7000);
      } else if (what === 'cancel') {
        await call('ai.cancel', { jobId: j.id });
        toast('Stopping after the current photo');
      }
      onchanged();
    } catch (e) {
      toast(String(e), 'err');
    } finally {
      busy = false;
      reloadAll();
    }
  }

  async function undoChange(h) {
    try {
      await call('history.undo', { changeId: h.id });
      toast('Undone');
      onchanged();
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 7000);
    }
    reloadAll();
  }

  function filterJob(j) {
    jobFilter = jobFilter === j.id ? null : j.id;
    loadQueue();
  }

  const checkedOnPage = $derived(cards.reduce((n, c) => n + c.suggestions.filter((s) => decide[s.id]?.on).length, 0));
  const when = (t) => new Date(t * 1000).toLocaleString();
  const fname = (p) => (p || '').split(/[\\/]/).pop();
  const running = $derived(ai.jobs.filter((j) => j.status === 'running' || j.status === 'queued'));
</script>

<div class="aiv">
  <div class="top">
    <div class="sum">
      {#if ai.stats}
        <span><b>{ai.stats.pending.toLocaleString()}</b> suggestions waiting on <b>{ai.stats.pendingPhotos.toLocaleString()}</b> photos</span>
        <span><b>{ai.stats.reviewed.toLocaleString()}</b> reviewed by AI</span>
        {#each running as j (j.id)}
          <span class="run">
            ⟳ {j.label}: {j.done}/{j.total}{j.failed ? ` · ${j.failed} failed` : ''}
            <span class="bar"><span style="width: {j.total ? (100 * j.done) / j.total : 0}%"></span></span>
            <button class="lnk" onclick={() => jobAction(j, 'cancel')}>stop</button>
          </span>
        {/each}
      {/if}
    </div>
    <button class="btn" class:on={showSettings} onclick={() => (showSettings = !showSettings)}>⚙ AI settings</button>
  </div>

  {#if showSettings}
    <section class="panel">
      <h3>Connect your model</h3>
      <p class="muted">Use any OpenAI-compatible server (LM Studio, Ollama, llama.cpp, vLLM) or an Anthropic-compatible API. The model only ever <b>suggests</b> — nothing changes until you approve it, and every applied change can be undone.</p>
      <AISettings onsaved={() => { refreshAi(); }} />
    </section>
  {/if}

  <div class="cols">
    <aside class="side">
      <h4>AI runs</h4>
      {#if !ai.jobs.length}<p class="muted">None yet. Select photos in the Library and choose <b>Review with AI</b>, or ask in the ✨ box.</p>{/if}
      {#each ai.jobs as j (j.id)}
        <div class="job" class:sel={jobFilter === j.id}>
          <button class="jt" onclick={() => filterJob(j)} title="Show only this run's suggestions">
            <span class="st {j.status}">{j.status}</span>
            <span class="jl">{j.label || j.kind}</span>
          </button>
          <div class="jm">
            #{j.id} · {when(j.createdAt)}{j.model ? ` · ${j.model}` : ''}<br />
            {#if j.kind === 'review'}{j.done}/{j.total} reviewed{j.failed ? `, ${j.failed} failed` : ''} · {/if}{j.pending} pending · {j.applied} applied · {j.rejected} rejected
            {#if j.error && j.status !== 'done'}<div class="jerr" title={j.error}>{j.error}</div>{/if}
          </div>
          <div class="ja">
            {#if j.pending}<button class="lnk" onclick={() => jobAction(j, 'approve')}>approve all</button><button class="lnk" onclick={() => jobAction(j, 'reject')}>reject all</button>{/if}
            {#if j.liveChanges}<button class="lnk bad" onclick={() => jobAction(j, 'undo')}>undo {j.liveChanges} applied</button>{/if}
            {#if j.status === 'running' || j.status === 'queued'}<button class="lnk bad" onclick={() => jobAction(j, 'cancel')}>stop</button>{/if}
          </div>
        </div>
      {/each}

      <h4>Recent AI changes</h4>
      {#if !recent.length}<p class="muted">Nothing applied yet.</p>{/if}
      {#each recent as h (h.id)}
        <div class="hist" class:undone={h.undoneAt}>
          <span class="ht">{describeChange(h.action, h.payload)}</span>
          <span class="hm">#{h.mediaId} · run {h.jobId} · {when(h.at)}</span>
          {#if !h.undoneAt}<button class="lnk bad" onclick={() => undoChange(h)}>undo</button>{:else}<span class="hm">undone</span>{/if}
        </div>
      {/each}
    </aside>

    <main class="queue">
      <div class="qhead">
        <h3>Waiting for approval {jobFilter ? `· run #${jobFilter}` : ''}</h3>
        <span class="muted">{totalSuggestions.toLocaleString()} suggestions on {totalPhotos.toLocaleString()} photos</span>
        {#if jobFilter}<button class="lnk" onclick={() => { jobFilter = null; loadQueue(); }}>show all runs</button>{/if}
        {#if cards.length}
          <button class="btn primary" disabled={busy} onclick={() => decideCards(cards)}>
            Apply page: approve {checkedOnPage} checked, reject the rest
          </button>
        {/if}
      </div>
      {#if !cards.length && !loading}
        <div class="empty">
          Nothing waiting. In the <b>Library</b>, select photos and click <b>Review with AI</b> (right panel), or type a request in the ✨ box above the grid.
        </div>
      {/if}
      <div class="cards">
        {#each cards as c (c.mediaId)}
          <div class="card">
            <button class="thumb" onclick={() => (big = c.item)} title="Open larger">
              {#if c.item?.thumbPath}<img src={mediaUrl(c.item.thumbPath)} alt="" loading="lazy" />{:else}<span class="muted">no preview</span>{/if}
            </button>
            <div class="cbody">
              <div class="cname" title={c.item?.path}>{fname(c.item?.path)}</div>
              <div class="cmeta">
                {c.item?.takenAt || ''} {c.item?.kind === 'video' ? '· video' : ''}
                <button class="lnk" onclick={() => onshow([c.mediaId])}>show in library</button>
              </div>
              {#if c.current.tags.length}
                <div class="have">has: {c.current.tags.map((t) => (t.category ? t.category + ': ' : '') + t.name).join(', ')}</div>
              {/if}
              <div class="suggs">
                {#each c.suggestions as s (s.id)}
                  {@const d = decide[s.id]}
                  {#if d}
                    <div class="sg" class:off={!d.on}>
                      <input type="checkbox" bind:checked={d.on} />
                      <span class="sk">{SUGG_LABEL[s.kind] || s.kind}</span>
                      {#if s.kind === 'description' || s.kind === 'append_note'}
                        <textarea rows={s.kind === 'description' ? 3 : 2} value={d.payload.text} oninput={(e) => edit(s.id, 'text', e.target.value)}></textarea>
                      {:else if s.kind === 'add_tag'}
                        <input class="cat" value={d.payload.category || ''} placeholder="category" oninput={(e) => edit(s.id, 'category', e.target.value)} />
                        <input class="tag" value={d.payload.name} oninput={(e) => edit(s.id, 'name', e.target.value)} />
                        {#if s.payload.isNew}<span class="new" title="This tag doesn't exist yet — approving creates it">new</span>{/if}
                      {:else}
                        <span class="sv">{describeChange(s.kind, d.payload)}</span>
                      {/if}
                    </div>
                  {/if}
                {/each}
              </div>
              <div class="cact">
                <button class="btn sm primary" disabled={busy} onclick={() => decideCards([c])}>Approve checked</button>
                <button class="btn sm" disabled={busy} onclick={() => rejectCard(c)}>Reject all</button>
              </div>
            </div>
          </div>
        {/each}
      </div>
      {#if cards.length < totalPhotos}
        <button class="btn more" disabled={loading} onclick={() => loadQueue(true)}>{loading ? 'Loading…' : `Load more (${totalPhotos - cards.length} photos left)`}</button>
      {/if}
    </main>
  </div>

  {#if big}
    <div class="lb" onclick={() => (big = null)} role="dialog">
      <img src={mediaUrl(big.kind === 'video' ? big.thumbPath : big.path)} alt="" />
    </div>
  {/if}
</div>

<style>
  .aiv { height: 100%; display: flex; flex-direction: column; overflow: hidden; }
  .top { display: flex; align-items: center; gap: 14px; padding: 10px 18px; border-bottom: 1px solid var(--border); background: var(--panel); }
  .sum { display: flex; flex-wrap: wrap; gap: 6px 18px; font-size: 13px; color: var(--muted); flex: 1; align-items: center; }
  .sum b { color: var(--text); }
  .run { display: inline-flex; align-items: center; gap: 8px; color: #8ab4f8; }
  .bar { width: 120px; height: 5px; background: #1d2433; border-radius: 3px; overflow: hidden; display: inline-block; }
  .bar span { display: block; height: 100%; background: #3b82f6; transition: width 0.4s; }
  .panel { padding: 14px 18px 18px; border-bottom: 1px solid var(--border); background: #12151b; max-height: 60vh; overflow-y: auto; }
  .panel h3 { margin: 0 0 4px; font-size: 14px; }
  .panel p { margin: 0 0 12px; }
  .cols { flex: 1; min-height: 0; display: flex; }
  .side { width: 330px; flex: 0 0 330px; overflow-y: auto; border-right: 1px solid var(--border); padding: 10px 14px 40px; background: var(--panel); box-sizing: border-box; }
  h4 { margin: 14px 0 8px; font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }
  .job { border: 1px solid var(--border); border-radius: 8px; padding: 7px 9px; margin-bottom: 7px; background: var(--bg); }
  .job.sel { border-color: var(--accent); }
  .jt { display: flex; gap: 8px; align-items: center; background: none; border: 0; color: var(--text); cursor: pointer; padding: 0; font-size: 12.5px; text-align: left; width: 100%; }
  .jl { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
  .st { font-size: 10px; border-radius: 8px; padding: 1px 6px; background: #263041; color: #8ab4f8; flex: 0 0 auto; }
  .st.done { background: #12261a; color: #9ff0b8; }
  .st.error, .st.cancelled { background: #2a1414; color: #ff9a8a; }
  .jm { font-size: 11px; color: var(--muted); margin-top: 4px; line-height: 1.45; }
  .jerr { color: #ff9a8a; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ja { display: flex; flex-wrap: wrap; gap: 2px 8px; margin-top: 4px; }
  .hist { display: grid; grid-template-columns: 1fr auto; gap: 0 6px; font-size: 11.5px; padding: 5px 0; border-bottom: 1px solid #1d2129; }
  .hist .ht { grid-column: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hist .hm { grid-column: 1; color: #6b7383; font-size: 10.5px; }
  .hist .lnk, .hist > .hm:last-child { grid-column: 2; grid-row: 1 / span 2; align-self: center; }
  .hist.undone .ht { text-decoration: line-through; color: var(--muted); }
  .queue { flex: 1; min-width: 0; overflow-y: auto; padding: 12px 18px 60px; }
  .qhead { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-bottom: 12px; }
  .qhead h3 { margin: 0; font-size: 14px; }
  .qhead .btn.primary { margin-left: auto; }
  .empty { color: var(--muted); padding: 30px 0; font-size: 13px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(460px, 1fr)); gap: 12px; }
  .card { display: flex; gap: 12px; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 10px; }
  .thumb { flex: 0 0 150px; height: 150px; border-radius: 7px; overflow: hidden; background: #0a0c10; border: 0; padding: 0; cursor: zoom-in; display: flex; align-items: center; justify-content: center; }
  .thumb img { width: 100%; height: 100%; object-fit: cover; }
  .cbody { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 5px; }
  .cname { font-weight: 600; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cmeta, .have { font-size: 11px; color: var(--muted); }
  .suggs { display: flex; flex-direction: column; gap: 4px; }
  .sg { display: flex; align-items: flex-start; gap: 6px; font-size: 12px; }
  .sg.off { opacity: 0.45; }
  .sg input[type='checkbox'] { margin-top: 5px; accent-color: var(--accent); }
  .sk { flex: 0 0 82px; color: var(--muted); font-size: 11px; padding-top: 5px; }
  .sv { padding-top: 4px; }
  .sg textarea, .sg input:not([type]) {
    background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 6px;
    padding: 4px 7px; font: inherit; font-size: 12px; box-sizing: border-box;
  }
  .sg textarea { flex: 1; resize: vertical; }
  .sg .cat { width: 92px; color: #8aa0c0; }
  .sg .tag { flex: 1; min-width: 0; }
  .new { font-size: 10px; background: #2a2210; border: 1px solid #6b5520; color: #ffe7b0; border-radius: 8px; padding: 1px 6px; margin-top: 4px; }
  .cact { display: flex; gap: 6px; margin-top: 4px; }
  .btn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border); white-space: nowrap;
    padding: 6px 12px; border-radius: 8px; cursor: pointer; font-size: 12.5px;
  }
  .btn:hover { border-color: var(--accent); }
  .btn.on { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn.sm { padding: 4px 10px; font-size: 12px; }
  .btn.more { margin: 16px auto 0; display: block; }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 11.5px; padding: 0; }
  .lnk.bad { color: #ff8f80; }
  .muted { color: var(--muted); font-size: 12px; }
  .lb { position: fixed; inset: 0; z-index: 80; background: rgba(5,7,10,0.94); display: flex; align-items: center; justify-content: center; cursor: zoom-out; }
  .lb img { max-width: 94vw; max-height: 94vh; object-fit: contain; }
</style>
