<script>
  // Right-click menu for grid tiles. `items` = [{ label, run, disabled?, danger?, sep? }]
  import { onMount } from 'svelte';
  let { x = 0, y = 0, title = '', items = [], onclose = () => {} } = $props();

  let el = $state(null);
  let pos = $state({ left: x, top: y });

  onMount(() => {
    // Keep the menu inside the window.
    const r = el.getBoundingClientRect();
    pos = {
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    };
    el.querySelector('button:not(:disabled)')?.focus();
    const away = (e) => { if (!el.contains(e.target)) onclose(); };
    const key = (e) => { if (e.key === 'Escape') { e.stopPropagation(); onclose(); } };
    window.addEventListener('mousedown', away, true);
    window.addEventListener('keydown', key, true);
    window.addEventListener('blur', onclose);
    window.addEventListener('resize', onclose);
    return () => {
      window.removeEventListener('mousedown', away, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('blur', onclose);
      window.removeEventListener('resize', onclose);
    };
  });

  function pick(it) {
    if (it.disabled) return;
    onclose();
    it.run();
  }
</script>

<div class="cm" bind:this={el} style="left: {pos.left}px; top: {pos.top}px;" role="menu" oncontextmenu={(e) => e.preventDefault()}>
  {#if title}<div class="ttl">{title}</div>{/if}
  {#each items as it}
    {#if it.sep}
      <div class="sep"></div>
    {:else}
      <button role="menuitem" class:danger={it.danger} disabled={it.disabled} onclick={() => pick(it)}>
        <span>{it.label}</span>{#if it.hint}<kbd>{it.hint}</kbd>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .cm {
    position: fixed; z-index: 200; min-width: 190px; padding: 4px;
    background: var(--panel2); border: 1px solid var(--border); border-radius: 8px;
    box-shadow: 0 10px 28px rgba(0,0,0,0.5); font-size: 13px;
  }
  .ttl { padding: 5px 10px 6px; color: var(--muted); font-size: 11.5px; border-bottom: 1px solid var(--border); margin-bottom: 4px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 280px; }
  button {
    display: flex; width: 100%; align-items: center; gap: 16px; text-align: left;
    background: none; border: 0; color: var(--text); padding: 6px 10px; border-radius: 5px; cursor: pointer; font-size: 13px;
  }
  button:hover:not(:disabled), button:focus-visible { background: var(--accent); color: #1a1206; outline: none; }
  button:disabled { opacity: 0.45; cursor: default; }
  button.danger { color: #ff9a8a; }
  button.danger:hover:not(:disabled) { background: #b8352a; color: #fff; }
  kbd { margin-left: auto; font: inherit; font-size: 11px; color: var(--muted); }
  button:hover kbd { color: inherit; }
  .sep { height: 1px; background: var(--border); margin: 4px 2px; }
</style>
