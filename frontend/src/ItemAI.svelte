<script>
  // Per-photo AI + history: pending suggestions (approve / reject each), the
  // AI description, "Review with AI", and every change ever made to this
  // photo (by you or the AI) with undo.
  import { call } from './lib.js';
  import { ai, toast, reviewWithAi, describeChange, SUGG_LABEL, onAiProgress, refreshAi } from './store.svelte.js';

  let { id, onchange = () => {} } = $props();

  let pending = $state([]);
  let history = $state([]);
  let desc = $state('');
  let showHist = $state(false);
  let busy = $state(false);

  async function load(mid) {
    try {
      const [p, h, a] = await Promise.all([
        call('ai.forMedia', { id: mid }),
        call('history.media', { id: mid, limit: 100 }),
        call('annotation.get', { id: mid }),
      ]);
      if (mid !== id) return;
      pending = p;
      history = h;
      desc = a.aiDescription || '';
    } catch (e) {
      console.warn(e);
    }
  }
  $effect(() => {
    load(id);
  });
  $effect(() => onAiProgress(() => load(id)));

  async function decide(s, approve) {
    busy = true;
    try {
      if (approve) {
        const r = await call('ai.approve', { suggestionIds: [s.id] });
        if (r.failed.length) toast(r.failed[0], 'err');
      } else await call('ai.reject', { suggestionIds: [s.id] });
      onchange('ai', id);
      refreshAi();
    } finally {
      busy = false;
      load(id);
    }
  }
  async function decideAll(approve) {
    busy = true;
    try {
      if (approve) await call('ai.approve', { mediaIds: [id] });
      else await call('ai.reject', { mediaIds: [id] });
      onchange('ai', id);
      refreshAi();
    } finally {
      busy = false;
      load(id);
    }
  }
  async function undo(h) {
    try {
      await call('history.undo', { changeId: h.id });
      onchange('undo', id);
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 7000);
    }
    load(id);
  }
  async function undoAllAi() {
    if (!window.confirm('Undo every AI change on this photo?')) return;
    const r = await call('history.undoMedia', { id, aiOnly: true });
    toast(`Undid ${r.undone} AI change(s)` + (r.conflicts.length ? ` — ${r.conflicts.length} skipped (edited since)` : ''), r.conflicts.length ? 'warn' : 'ok');
    onchange('undo', id);
    load(id);
  }
  const when = (t) => new Date(t * 1000).toLocaleString();
  const liveAi = $derived(history.filter((h) => h.source === 'ai' && !h.undoneAt).length);
</script>

<div class="iai">
  <div class="hd">
    <span>✨ AI</span>
    {#if ai.stats?.configured}
      <button class="lnk" disabled={busy} onclick={() => reviewWithAi([id], { rereview: true, label: 'Review 1 photo' })}>Review with AI</button>
    {/if}
  </div>
  {#if desc}<div class="desc" title="AI description (searchable)">{desc}</div>{/if}

  {#if pending.length}
    <div class="pend">
      {#each pending as s (s.id)}
        <div class="ps">
          <span class="k">{SUGG_LABEL[s.kind] || s.kind}</span>
          <span class="v">{s.kind === 'add_tag' ? `${s.payload.category ? s.payload.category + ': ' : ''}${s.payload.name}` : describeChange(s.kind, s.payload).replace(/^Description: |^Note: |^Rating → /, '')}{#if s.payload.isNew}<em> new</em>{/if}</span>
          <button class="ok" disabled={busy} onclick={() => decide(s, true)} title="Approve">✓</button>
          <button class="no" disabled={busy} onclick={() => decide(s, false)} title="Reject">✕</button>
        </div>
      {/each}
      <div class="row">
        <button class="btn sm primary" disabled={busy} onclick={() => decideAll(true)}>Approve all {pending.length}</button>
        <button class="btn sm" disabled={busy} onclick={() => decideAll(false)}>Reject all</button>
      </div>
    </div>
  {/if}

  <button class="hist-t" onclick={() => (showHist = !showHist)}>
    {showHist ? '▾' : '▸'} History ({history.length}){liveAi ? ` · ${liveAi} by AI` : ''}
  </button>
  {#if showHist}
    {#if liveAi}<button class="lnk bad" onclick={undoAllAi}>undo all AI changes on this photo</button>{/if}
    {#each history as h (h.id)}
      <div class="h" class:undone={h.undoneAt}>
        <span class="src {h.source}" title={h.source === 'dedupe' ? 'Merged from a duplicate that was removed' : ''}>{h.source === 'ai' ? 'AI' : h.source === 'dedupe' ? 'duplicate' : 'you'}</span>
        <span class="ht" title={JSON.stringify(h.payload)}>{describeChange(h.action, h.payload)}</span>
        {#if h.undoneAt}<span class="hw">undone</span>{:else}<button class="lnk bad" onclick={() => undo(h)}>undo</button>{/if}
        <span class="hw full">{when(h.at)}{h.jobId ? ` · AI run #${h.jobId}` : ''}</span>
      </div>
    {/each}
    {#if !history.length}<div class="hw">No changes recorded yet.</div>{/if}
  {/if}
</div>

<style>
  .iai { display: flex; flex-direction: column; gap: 6px; }
  .hd { display: flex; align-items: center; gap: 8px; font-size: 11px; color: var(--accent); text-transform: uppercase; letter-spacing: 0.4px; font-weight: 600; }
  .hd .lnk { margin-left: auto; text-transform: none; letter-spacing: 0; font-weight: 400; }
  .desc { font-size: 12px; color: #cfe0ff; background: #111a28; border: 1px solid #2c3a4d; border-radius: 7px; padding: 6px 8px; line-height: 1.45; }
  .pend { border: 1px solid #6b5520; background: #1d1810; border-radius: 8px; padding: 6px 8px; display: flex; flex-direction: column; gap: 4px; }
  .ps { display: flex; align-items: flex-start; gap: 6px; font-size: 12px; }
  .k { flex: 0 0 74px; color: #c9a86b; font-size: 11px; padding-top: 1px; }
  .v { flex: 1; min-width: 0; word-break: break-word; }
  .v em { font-size: 10px; color: #f4a620; font-style: normal; margin-left: 4px; }
  .ok, .no { background: none; border: 1px solid var(--border); border-radius: 5px; width: 22px; height: 20px; cursor: pointer; font-size: 11px; color: var(--muted); padding: 0; }
  .ok:hover { color: #3fb950; border-color: #3fb950; }
  .no:hover { color: #e5484d; border-color: #e5484d; }
  .row { display: flex; gap: 6px; margin-top: 2px; }
  .btn { background: var(--panel2); color: var(--text); border: 1px solid var(--border); border-radius: 7px; padding: 4px 10px; font-size: 12px; cursor: pointer; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn:disabled { opacity: 0.5; }
  .hist-t { background: none; border: 0; color: var(--muted); font-size: 11.5px; cursor: pointer; text-align: left; padding: 4px 0 0; }
  .h { display: grid; grid-template-columns: auto 1fr auto; gap: 0 6px; font-size: 11.5px; padding: 3px 0; border-bottom: 1px solid #1d2129; align-items: center; }
  .h.undone .ht { text-decoration: line-through; color: var(--muted); }
  .ht { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .src { font-size: 9.5px; border-radius: 6px; padding: 0 5px; background: #263041; color: #8ab4f8; }
  .src.user { background: #1d2a1d; color: #9ff0b8; }
  .hw { color: #6b7383; font-size: 10.5px; }
  .hw.full { grid-column: 2 / span 2; }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 11.5px; padding: 0; text-align: left; }
  .lnk.bad { color: #ff8f80; }
  .lnk:disabled { opacity: 0.5; }
</style>
