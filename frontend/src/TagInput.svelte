<script>
  // Tag entry with autocomplete. Enter / comma adds. "Category: tag" creates the
  // tag inside that category (category is created if new).
  import { lib, tagLabel } from './store.svelte.js';

  let { onadd = () => {}, placeholder = 'Add tag…  (e.g. Client: Smith Marine)', exclude = [] } = $props();

  let text = $state('');
  let open = $state(false);
  let hi = $state(0);

  const suggestions = $derived.by(() => {
    const q = text.trim().toLowerCase();
    const ex = new Set(exclude);
    const all = lib.tags.filter((t) => !ex.has(t.id));
    if (!q) return all.slice(0, 8);
    return all
      .filter((t) => tagLabel(t).toLowerCase().includes(q))
      .sort((a, b) => (a.name.toLowerCase().startsWith(q) ? -1 : 0) - (b.name.toLowerCase().startsWith(q) ? -1 : 0))
      .slice(0, 8);
  });

  function commit(value) {
    const names = String(value)
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    if (names.length) onadd(names);
    text = '';
    hi = 0;
  }

  function onKey(e) {
    if (e.key === 'Enter') {
      e.preventDefault();
      // A highlighted suggestion wins only when the user moved the highlight;
      // plain Enter adds exactly what was typed.
      if (open && hi > 0 && suggestions[hi - 1]) commit(suggestions[hi - 1].name);
      else if (text.trim()) commit(text);
    } else if (e.key === ',') {
      e.preventDefault();
      if (text.trim()) commit(text);
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      open = true;
      hi = Math.min(suggestions.length, hi + 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      hi = Math.max(0, hi - 1);
    } else if (e.key === 'Escape') {
      open = false;
      hi = 0;
      e.stopPropagation();
    }
  }
</script>

<div class="ti">
  <input
    type="text"
    bind:value={text}
    {placeholder}
    onfocus={() => (open = true)}
    onblur={() => setTimeout(() => (open = false), 150)}
    oninput={() => { open = true; hi = 0; }}
    onkeydown={onKey}
  />
  {#if open && suggestions.length}
    <ul class="sugg">
      {#each suggestions as s, i (s.id)}
        <li>
          <button class:hi={hi === i + 1} onmousedown={(e) => { e.preventDefault(); commit(s.name); }}>
            {#if s.category}<span class="cat">{s.category}</span>{/if}
            <span>{s.name}</span>
            <span class="n">{s.count}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .ti { position: relative; }
  input {
    width: 100%; box-sizing: border-box; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 7px; padding: 7px 9px; font-size: 12.5px;
  }
  input:focus { outline: none; border-color: var(--accent); }
  .sugg {
    position: absolute; z-index: 30; left: 0; right: 0; top: calc(100% + 3px);
    list-style: none; margin: 0; padding: 4px; background: var(--panel2);
    border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 8px 24px rgba(0,0,0,0.45);
    max-height: 260px; overflow-y: auto;
  }
  .sugg button {
    width: 100%; display: flex; align-items: center; gap: 8px; text-align: left;
    background: none; border: 0; color: var(--text); padding: 6px 8px; border-radius: 6px;
    cursor: pointer; font-size: 12.5px;
  }
  .sugg button:hover, .sugg button.hi { background: #263041; }
  .cat { font-size: 10.5px; color: var(--muted); border: 1px solid var(--border); border-radius: 8px; padding: 0 6px; }
  .n { margin-left: auto; color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
</style>
