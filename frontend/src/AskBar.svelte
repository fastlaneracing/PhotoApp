<script>
  // Natural-language requests to the AI: find photos (becomes a filter), have
  // the vision model review photos, or propose tags/notes/ratings/albums.
  // Proposals always wait in the AI tab for approval.
  import { call, cleanFilter } from './lib.js';
  import { ai, refreshAi, toast } from './store.svelte.js';

  let { filter = {}, resultIds = [], selectedIds = [], onfilter = () => {}, ongoai = () => {} } = $props();

  let q = $state('');
  let busy = $state(false);
  let answer = $state(null); // { reply, action, ... }

  async function ask() {
    const question = q.trim();
    if (!question || busy) return;
    busy = true;
    answer = null;
    try {
      const r = await call('ai.ask', {
        question,
        selectedIds,
        resultIds: resultIds.slice(0, 20000),
        filter: cleanFilter(filter),
      });
      answer = r;
      if (r.action === 'filter') {
        onfilter({ ...r.filter });
      } else if (r.action === 'review' || r.action === 'propose') {
        refreshAi();
      }
    } catch (e) {
      answer = { reply: String(e).replace(/^Error: /, ''), error: true };
    } finally {
      busy = false;
    }
  }
</script>

<div class="ask">
  <span class="spark">✨</span>
  {#if ai.stats && !ai.stats.configured}
    <span class="setup">Connect a local or cloud AI model to describe, tag and find photos. <button class="lnk" onclick={ongoai}>Set up AI →</button></span>
  {:else}
    <input
      bind:value={q}
      placeholder={selectedIds.length
        ? `Ask about the ${selectedIds.length} selected… e.g. "review these", "tag these Key West", "rate these 5 stars"`
        : 'Ask your library… e.g. "boat photos from 2019 at night", "review everything untagged", "videos of trucks"'}
      onkeydown={(e) => { if (e.key === 'Enter') ask(); if (e.key === 'Escape') { answer = null; e.currentTarget.blur(); } }}
      disabled={busy}
    />
    <button class="go" onclick={ask} disabled={busy || !q.trim()}>{busy ? 'Thinking…' : 'Ask'}</button>
    {#if ai.stats?.pending}
      <button class="pend" onclick={ongoai} title="AI suggestions waiting for your approval">{ai.stats.pending.toLocaleString()} to approve</button>
    {/if}
  {/if}
</div>
{#if answer}
  <div class="ans" class:err={answer.error}>
    <span>{answer.reply || '(no reply)'}</span>
    {#if answer.action === 'filter' && answer.unknown?.length}
      <span class="muted"> — not in your tags: {answer.unknown.join(', ')} (searched as words)</span>
    {/if}
    {#if answer.action === 'review'}
      <span class="muted"> — reviewing {answer.count} item(s) in the background.</span>
      <button class="lnk" onclick={ongoai}>Open AI tab</button>
    {/if}
    {#if answer.action === 'propose'}
      <span class="muted"> — {answer.count} change(s) for {answer.photos} photo(s) are waiting for your approval.</span>
      <button class="lnk" onclick={ongoai}>Review them</button>
    {/if}
    <button class="x" onclick={() => (answer = null)}>✕</button>
  </div>
{/if}

<style>
  .ask { display: flex; align-items: center; gap: 8px; padding: 8px 16px 0; background: var(--panel); }
  .spark { font-size: 15px; }
  input {
    flex: 1; background: #10141c; color: var(--text); border: 1px solid #2c3a4d; border-radius: 8px;
    padding: 8px 11px; font-size: 13px;
  }
  input:focus { outline: none; border-color: #8ab4f8; }
  .go, .pend {
    background: #173049; color: var(--text); border: 1px solid #2c567f; border-radius: 8px;
    padding: 7px 14px; font-size: 12.5px; cursor: pointer; white-space: nowrap;
  }
  .go:disabled { opacity: 0.5; cursor: default; }
  .pend { background: #2a2210; border-color: #6b5520; color: #ffe7b0; }
  .setup { font-size: 12.5px; color: var(--muted); }
  .ans {
    margin: 6px 16px 0; padding: 7px 11px; border-radius: 8px; background: #111a28; border: 1px solid #2c3a4d;
    font-size: 12.5px; color: #cfe0ff; display: flex; flex-wrap: wrap; gap: 4px; align-items: center;
  }
  .ans.err { background: #2a1414; border-color: #6b2a2a; color: #ffd0c8; }
  .muted { color: var(--muted); }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 12.5px; padding: 0 4px; }
  .x { margin-left: auto; background: none; border: 0; color: var(--muted); cursor: pointer; }
</style>
