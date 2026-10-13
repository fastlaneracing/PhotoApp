<script>
  import { onMount } from 'svelte';
  import { mediaUrl, itemDetail, moveTarget, relocateDryRun, relocateApply, relocateUndo } from './lib.js';
  import Annotator from './Annotator.svelte';

  let { items = [], id, onclose, onprev, onnext, onchange = () => {} } = $props();

  let current = $derived(items.find((i) => i.id === id) || null);
  let detail = $state(null);
  let detailError = $state(null);
  let mediaFailed = $state(false);

  // --- M8: organize (rename / move) ---
  // mode: 'idle' | 'rename' | 'move' | 'applied'
  let orgMode = $state('idle');
  let renameDraft = $state('');
  let moveLayout = $state('year');
  let orgPlan = $state(null); // RelocatePlan preview
  let orgBusy = $state(false);
  let orgError = $state(null);
  let orgReport = $state(null);
  // Reset the organize panel whenever a different item is opened.
  $effect(() => {
    if (id != null) {
      orgMode = 'idle';
      orgPlan = null;
      orgError = null;
      orgReport = null;
    }
  });

  // Seed the rename draft with the current filename (minus extension) once the
  // detail arrives, so "Rename" is editable rather than blank.
  $effect(() => {
    const d = detail;
    if (d?.item?.path && orgMode === 'idle') {
      renameDraft = (d.item.path.split(/[\\/]/).pop() || '').replace(/\.[^.]+$/, '');
    }
  });

  const canonPath = $derived(src?.path || '');
  const canonName = $derived(canonPath ? canonPath.split(/[\\/]/).pop() : '');
  const canonDir = $derived(canonPath ? canonPath.slice(0, canonPath.length - (canonName?.length || 0)).replace(/[\\/]+$/, '') : '');
  const canonExt = $derived((canonName?.match(/\.[^.]+$/) || [''])[0]);
  const canonSep = $derived(canonPath.includes('\\') ? '\\' : '/');

  // Build the rename target from the current draft (keeps the extension, same folder).
  const renameTarget = $derived.by(() => {
    const raw = (renameDraft || '').trim();
    if (!raw) return null;
    const withExt = raw.includes('.') ? raw : raw + canonExt;
    return canonDir ? canonDir + canonSep + withExt : withExt;
  });

  // Suggested move target for the chosen layout (fetched lazily).
  let moveSuggested = $state(null);
  $effect(() => {
    if (orgMode !== 'move' || id == null) return;
    let cancelled = false;
    moveSuggested = null;
    moveTarget(id, moveLayout).then((t) => { if (!cancelled) moveSuggested = t; }).catch(() => {});
    return () => { cancelled = true; };
  });

  async function previewOrg() {
    if (id == null) return;
    orgBusy = true;
    orgError = null;
    try {
      if (orgMode === 'rename') {
        if (!renameTarget) throw new Error('enter a new filename first');
        orgPlan = await relocateDryRun(id, 'rename', renameTarget);
      } else if (orgMode === 'move') {
        if (!moveSuggested) throw new Error('loading suggested target…');
        orgPlan = await relocateDryRun(id, 'move', moveSuggested);
      }
    } catch (e) {
      orgError = String(e);
      orgPlan = null;
    } finally {
      orgBusy = false;
    }
  }

  async function applyOrg() {
    if (!orgPlan) return;
    const op = orgPlan.op;
    const ok = window.confirm(
      (op === 'rename' ? 'Rename' : 'Move') + ' to:\n' + orgPlan.targetPath + '\n\n' +
      'The file is relocated (never deleted). Undo is available after.'
    );
    if (!ok) return;
    orgBusy = true;
    orgError = null;
    try {
      const res = await relocateApply(id, op, orgPlan.targetPath);
      orgReport = res.reportPath;
      orgPlan = null;
      orgMode = 'applied';
      // Refresh the detail (path changed) so the panel shows the new location.
      const d = await itemDetail(id);
      detail = d;
    } catch (e) {
      orgError = String(e);
    } finally {
      orgBusy = false;
    }
  }

  async function undoOrg() {
    if (id == null) return;
    const ok = window.confirm('Undo the last rename/move — restore the file to its original path.');
    if (!ok) return;
    orgBusy = true;
    orgError = null;
    try {
      const res = await relocateUndo(id);
      orgReport = res.reportPath;
      if (res.warnings && res.warnings.length) orgError = 'Undo finished with warnings:\n' + res.warnings.join('\n');
      orgMode = 'idle';
      const d = await itemDetail(id);
      detail = d;
    } catch (e) {
      orgError = String(e);
    } finally {
      orgBusy = false;
    }
  }

  let mainSrc = $derived.by(() => {
    if (!current) return null;
    // Full-res source: the actual file for images and videos alike.
    return current.path ? mediaUrl(current.path) : null;
  });
  let isVideo = $derived(!!current && current.kind === 'video');

  // Load the detail (EXIF panel + occurrences) when the item changes.
  $effect(() => {
    if (id == null) return;
    let cancelled = false;
    detail = null;
    detailError = null;
    mediaFailed = false;
    itemDetail(id)
      .then((d) => { if (!cancelled) detail = d; })
      .catch((e) => { if (!cancelled) detailError = String(e); });
    return () => { cancelled = true; };
  });

  function fmtBytes(b) {
    if (!b) return '—';
    if (b >= 1e9) return (b / 1e9).toFixed(2) + ' GB';
    if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
    if (b >= 1e3) return Math.round(b / 1e3) + ' KB';
    return b + ' B';
  }

  // --- Zoom (mouse wheel, toward the cursor) + drag to pan --------------------
  let imgEl = $state(null);
  let stageEl = $state(null);
  // Non-passive so preventDefault() works (Svelte's onwheel is passive).
  $effect(() => {
    if (!stageEl) return;
    const h = (e) => onWheel(e);
    stageEl.addEventListener('wheel', h, { passive: false });
    return () => stageEl.removeEventListener('wheel', h);
  });
  let zoom = $state(1);
  let tx = $state(0);
  let ty = $state(0);
  let dragging = $state(false);
  let dragStart = null;
  let zoomHint = $state(false);
  let hintTimer = null;
  const MIN_Z = 1;
  const MAX_Z = 20;

  function resetZoom() {
    zoom = 1; tx = 0; ty = 0;
  }
  // New photo -> back to fit-to-screen.
  $effect(() => {
    id;
    resetZoom();
  });

  /** Zoom to `nz`, keeping the image point under (cx, cy) fixed on screen. */
  function zoomAt(nz, cx, cy) {
    if (!imgEl) return;
    nz = Math.min(MAX_Z, Math.max(MIN_Z, nz));
    if (nz === MIN_Z) { resetZoom(); return; }
    const r = imgEl.getBoundingClientRect(); // includes the current transform
    const left = r.left - tx;                 // untransformed layout position
    const top = r.top - ty;
    const ux = (cx - left - tx) / zoom;       // image point under the cursor
    const uy = (cy - top - ty) / zoom;
    tx = cx - left - nz * ux;
    ty = cy - top - nz * uy;
    zoom = nz;
    zoomHint = true;
    clearTimeout(hintTimer);
    hintTimer = setTimeout(() => (zoomHint = false), 1200);
  }

  function onWheel(e) {
    if (isVideo || !imgEl) return;
    e.preventDefault();
    // Smooth for trackpads, ~20% per notch for mouse wheels.
    const factor = Math.exp(-e.deltaY * (e.deltaMode === 1 ? 0.05 : 0.0018));
    zoomAt(zoom * factor, e.clientX, e.clientY);
  }

  function zoomCenter(f) {
    if (!imgEl) return;
    const r = imgEl.getBoundingClientRect();
    zoomAt(zoom * f, r.left + r.width / 2, r.top + r.height / 2);
  }

  function onDblClick(e) {
    if (isVideo) return;
    e.stopPropagation();
    if (zoom > 1) resetZoom();
    else zoomAt(2.5, e.clientX, e.clientY);
  }

  function onPointerDown(e) {
    if (zoom <= 1 || e.button !== 0) return;
    e.preventDefault();
    dragging = true;
    dragStart = { x: e.clientX, y: e.clientY, tx, ty };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }
  function onPointerMove(e) {
    if (!dragging || !dragStart) return;
    tx = dragStart.tx + (e.clientX - dragStart.x);
    ty = dragStart.ty + (e.clientY - dragStart.y);
  }
  function onPointerUp() {
    dragging = false;
    dragStart = null;
  }

  function onKey(e) {
    // Typing in the notes/tag/rename boxes must not flip photos or close.
    const t = e.target;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
    if (typing) {
      // Let the field handle it (Enter adds a tag, etc.); Esc just leaves the field.
      if (e.key === 'Escape') { e.preventDefault(); t.blur(); }
      return;
    }
    e.stopPropagation(); // the grid's shortcuts stay inactive while the viewer is open
    if (e.key === 'Escape') {
      e.preventDefault();
      if (zoom > 1) resetZoom(); // first Esc un-zooms, second closes
      else onclose();
    }
    else if (e.key === 'ArrowLeft') { e.preventDefault(); onprev(); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); onnext(); }
    else if (e.key === '+' || e.key === '=') { e.preventDefault(); zoomCenter(1.4); }
    else if (e.key === '-' || e.key === '_') { e.preventDefault(); zoomCenter(1 / 1.4); }
    else if (e.key === '0') { e.preventDefault(); resetZoom(); }
  }

  onMount(() => {
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  // EXIF rows — prefer the detail (authoritative), fall back to the grid item.
  const src = $derived(detail?.item || current);
  const rows = $derived.by(() => {
    if (!src) return [];
    const d = detail || {};
    const out = [];
    out.push(['Type', src.kind === 'video' ? 'video' : 'image']);
    out.push(['Format', src.ext ? '.' + src.ext : '—']);
    if (src.width && src.height) out.push(['Dimensions', `${src.width} × ${src.height}`]);
    out.push(['Size', fmtBytes(src.bytes)]);
    out.push(['Taken', src.takenAt || '—']);
    if (src.dateSource) out.push(['Date source', src.dateSource]);
    if (src.camera) out.push(['Camera', src.camera]);
    if (src.sha256) out.push(['SHA-256', src.sha256.slice(0, 24) + '…']);
    out.push(['Occurrences', src.occCount != null ? src.occCount : '—']);
    out.push(['Path', src.path || '—']);
    return out;
  });

  const occs = $derived(detail?.occurrences || []);
</script>

<div class="viewer" role="dialog" aria-label="media viewer" onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="stage" bind:this={stageEl}>
    <div class="media-wrap" onclick={(e) => e.stopPropagation()}>
      {#if mainSrc && !mediaFailed}
        {#if isVideo}
          <video
            class="media"
            src={mainSrc}
            controls
            autoplay
            onerror={() => (mediaFailed = true)}
          ></video>
        {:else}
          <img
            bind:this={imgEl}
            class="media zoomable"
            class:zoomed={zoom > 1}
            class:dragging
            src={mainSrc}
            alt=""
            draggable="false"
            style="transform: translate({tx}px, {ty}px) scale({zoom});"
            onerror={() => (mediaFailed = true)}
            ondblclick={onDblClick}
            onpointerdown={onPointerDown}
            onpointermove={onPointerMove}
            onpointerup={onPointerUp}
            onpointercancel={onPointerUp}
          />
        {/if}
      {:else}
        <div class="media-fail">
          {#if isVideo}
            ▶ No playable preview — this file is corrupted or unsupported.
          {:else}
            ▣ No preview available.
          {/if}
        </div>
      {/if}
    </div>
    {#if !isVideo && mainSrc && !mediaFailed}
      <div class="zoombar" class:show={zoomHint || zoom > 1}>
        <button onclick={() => zoomCenter(1 / 1.4)} title="Zoom out (−)">−</button>
        <button class="pct" onclick={resetZoom} title="Fit to screen (0)">{Math.round(zoom * 100)}%</button>
        <button onclick={() => zoomCenter(1.4)} title="Zoom in (+)">+</button>
      </div>
    {/if}
  </div>

  <aside class="panel" onclick={(e) => e.stopPropagation()}>
    <div class="panel-head">
      <span class="p-title">Details</span>
      <button class="x" onclick={onclose} title="Close (Esc)">✕</button>
    </div>

    <dl class="kv">
      {#each rows as [k, v] (k)}
        <dt>{k}</dt>
        <dd class:mono={k === 'SHA-256' || k === 'Path'} title={v}>{v}</dd>
      {/each}
    </dl>

    {#if current}
      <div class="sec">Notes & tags</div>
      <Annotator id={current.id} {onchange} compact />
    {/if}

    {#if occs.length > 1}
      <div class="sec">Occurrences ({occs.length})</div>
      <ul class="occ">
        {#each occs as o (o.id)}
          <li class:canonical={o.isCanonical} class:missing={o.missing}>
            <span class="dot" title={o.isCanonical ? 'canonical' : o.missing ? 'missing' : 'duplicate'}></span>
            <span class="occ-path" title={o.path}>{o.path}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if src?.path}
      <div class="sec organize">
        <div class="org-head">
          <span>Organize</span>
          <span class="org-sub">rename · move (never deletes)</span>
        </div>

        {#if orgMode === 'idle'}
          <div class="org-actions">
            <button class="orgbtn" onclick={() => { renameDraft = canonName ? canonName.replace(/\.[^.]+$/, '') : ''; orgMode = 'rename'; orgPlan = null; orgError = null; }}>Rename</button>
            <button class="orgbtn" onclick={() => { orgMode = 'move'; orgPlan = null; orgError = null; }}>Move</button>
          </div>
        {:else if orgMode === 'rename'}
          <label class="org-label">New filename (extension kept)</label>
          <input class="org-input" value={renameDraft} oninput={(e) => (renameDraft = e.target.value)}
            onkeydown={(e) => { if (e.key === 'Enter') previewOrg(); }} />
          {#if renameTarget}<div class="org-target" title={renameTarget}>→ {renameTarget}</div>{/if}
          <div class="org-row">
            <button class="orgbtn" disabled={orgBusy || !renameTarget} onclick={previewOrg}>{orgBusy ? '…' : 'Preview'}</button>
            <button class="orgbtn ghost" onclick={() => { orgMode = 'idle'; orgPlan = null; orgError = null; }}>cancel</button>
          </div>
        {:else if orgMode === 'move'}
          <label class="org-label">Layout</label>
          <select class="org-input" value={moveLayout} onchange={(e) => (moveLayout = e.target.value)}>
            <option value="year">by year  (…/2026/)</option>
            <option value="year_month">by year / month</option>
            <option value="root">root  (…/)</option>
          </select>
          {#if moveSuggested}<div class="org-target" title={moveSuggested}>→ {moveSuggested}</div>
          {:else}<div class="org-hint">computing target…</div>{/if}
          <div class="org-row">
            <button class="orgbtn" disabled={orgBusy || !moveSuggested} onclick={previewOrg}>{orgBusy ? '…' : 'Preview'}</button>
            <button class="orgbtn ghost" onclick={() => { orgMode = 'idle'; orgPlan = null; orgError = null; }}>cancel</button>
          </div>
        {:else if orgMode === 'applied'}
          <div class="org-ok">✓ Reorganized.</div>
          {#if orgReport}<div class="org-report" title={orgReport}>report: {orgReport}</div>{/if}
          <div class="org-row">
            <button class="orgbtn" disabled={orgBusy} onclick={undoOrg}>{orgBusy ? '…' : 'Undo'}</button>
            <button class="orgbtn ghost" onclick={() => (orgMode = 'idle')}>done</button>
          </div>
        {/if}

        {#if orgPlan}
          <div class="org-plan">
            <div class="org-planline">
              <span class="op-src" title={orgPlan.originalPath}>{orgPlan.originalPath}</span>
            </div>
            <div class="org-planline">→</div>
            <div class="org-planline">
              <span class="op-dst" title={orgPlan.targetPath}>{orgPlan.targetPath}</span>
            </div>
            {#if orgPlan.crossDrive}<div class="org-warn">cross-drive: copy-then-remove</div>{/if}
            {#if orgPlan.warnings.length}
              {#each orgPlan.warnings as w (w)}<div class="org-warn">⚠ {w}</div>{/each}
            {/if}
            <div class="org-row" style="margin-top:10px">
              <button class="orgbtn" disabled={orgBusy} onclick={() => { orgPlan = null; }}>back</button>
              <button class="orgbtn primary" disabled={orgBusy} onclick={applyOrg}>{orgBusy ? 'working…' : 'Apply'}</button>
            </div>
          </div>
        {/if}

        {#if orgError}<div class="org-err">{orgError}</div>{/if}
        <div class="org-charter">dry-run report first · undo restores the original path · never overwrites</div>
      </div>
    {/if}

    {#if detailError}
      <div class="err">Detail load failed: {detailError}</div>
    {/if}
  </aside>

  <div class="nav">
    <button class="navbtn" onclick={onprev} title="Previous (←)">‹</button>
    <span class="count">{items.findIndex((i) => i.id === id) + 1} / {items.length}</span>
    <button class="navbtn" onclick={onnext} title="Next (→)">›</button>
  </div>
</div>

<style>
  .viewer {
    position: fixed; inset: 0; z-index: 50; display: flex;
    background: rgba(5,7,10,0.96); backdrop-filter: blur(4px);
  }
  .stage {
    flex: 1; min-width: 0; display: flex; align-items: center; justify-content: center;
    padding: 24px; position: relative; overflow: hidden;
  }
  .zoomable { transform-origin: 0 0; cursor: zoom-in; user-select: none; will-change: transform; }
  .zoomable.zoomed { cursor: grab; }
  .zoomable.dragging { cursor: grabbing; }
  .zoombar {
    position: absolute; top: 16px; left: 50%; transform: translateX(-50%);
    display: flex; gap: 2px; background: var(--panel); border: 1px solid var(--border);
    border-radius: 18px; padding: 3px; opacity: 0; transition: opacity 0.2s; z-index: 2;
  }
  .stage:hover .zoombar, .zoombar.show { opacity: 1; }
  .zoombar button {
    background: none; border: 0; color: var(--text); min-width: 30px; height: 28px;
    border-radius: 14px; cursor: pointer; font-size: 15px;
  }
  .zoombar button:hover { background: var(--panel2); }
  .zoombar .pct { font-size: 12px; min-width: 54px; font-variant-numeric: tabular-nums; }
  .media-wrap { max-width: 100%; max-height: 100%; display: flex; }
  .media {
    max-width: 100%; max-height: 100vh; object-fit: contain;
    border-radius: 4px; background: #000;
  }
  .media-fail {
    color: var(--muted); font-size: 15px; padding: 40px; text-align: center;
    border: 1px dashed var(--border); border-radius: 10px; background: #0c0f14;
  }
  .panel {
    width: 360px; flex: 0 0 360px; background: var(--panel);
    border-left: 1px solid var(--border); overflow-y: auto; padding: 18px 20px 40px;
  }
  .panel-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 14px; }
  .p-title { font-weight: 600; font-size: 14px; letter-spacing: 0.3px; }
  .x { background: none; border: 1px solid var(--border); color: var(--muted); border-radius: 7px; width: 30px; height: 30px; cursor: pointer; font-size: 14px; }
  .x:hover { border-color: var(--accent); color: var(--text); }
  .kv { margin: 0; display: grid; grid-template-columns: 110px 1fr; gap: 7px 12px; }
  .kv dt { color: var(--muted); font-size: 12px; align-self: start; }
  .kv dd { margin: 0; font-size: 12.5px; color: var(--text); word-break: break-word; }
  .kv dd.mono { font-family: ui-monospace, Consolas, monospace; font-size: 11.5px; color: #c9d1d9; }
  .sec { margin: 20px 0 8px; font-size: 12px; font-weight: 600; color: var(--accent); letter-spacing: 0.3px; }
  .occ { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .occ li { display: flex; gap: 8px; align-items: flex-start; font-size: 11.5px; padding: 6px 8px; border-radius: 7px; background: #12151b; }
  .occ li.canonical { border: 1px solid #1f5c38; }
  .occ li.missing { opacity: 0.6; }
  .dot { width: 9px; height: 9px; border-radius: 50%; margin-top: 2px; flex: 0 0 auto; background: #5a6270; }
  .occ li.canonical .dot { background: #39c26d; }
  .occ li.missing .dot { background: #e0503c; }
  .occ-path { white-space: normal; word-break: break-all; color: #c9d1d9; font-family: ui-monospace, Consolas, monospace; font-size: 10.5px; }
  .err { margin-top: 16px; color: #f4a620; font-size: 12px; }

  /* --- M8: organize (rename / move) --- */
  .organize { padding-top: 16px; margin-top: 6px; }
  .org-head { display: flex; align-items: baseline; gap: 8px; }
  .org-sub { font-size: 10.5px; color: var(--muted); }
  .org-actions { display: flex; gap: 8px; margin-top: 10px; }
  .org-label { display: block; font-size: 11px; color: var(--muted); margin: 10px 0 4px; }
  .org-input {
    width: 100%; box-sizing: border-box; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 7px; padding: 7px 9px; font-size: 12.5px;
  }
  .org-input:focus { outline: none; border-color: var(--accent); }
  .org-target { margin-top: 8px; font-size: 11px; color: #9ff0b8; word-break: break-all; font-family: ui-monospace, Consolas, monospace; }
  .org-hint { margin-top: 8px; font-size: 11px; color: var(--muted); }
  .org-row { display: flex; gap: 8px; margin-top: 10px; }
  .orgbtn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    padding: 6px 13px; border-radius: 8px; cursor: pointer; font-size: 12.5px;
  }
  .orgbtn:hover { border-color: var(--accent); }
  .orgbtn:disabled { opacity: 0.5; cursor: default; }
  .orgbtn.ghost { background: none; color: var(--muted); }
  .orgbtn.primary { background: #173049; border-color: #2c567f; }
  .org-plan {
    margin-top: 12px; border: 1px solid #2c3a4d; border-radius: 9px; padding: 10px;
    background: #0f141c;
  }
  .org-planline { font-size: 11px; word-break: break-all; font-family: ui-monospace, Consolas, monospace; }
  .org-planline .op-src { color: #ffcf99; }
  .org-planline .op-dst { color: #9ff0b8; }
  .org-warn { margin-top: 6px; color: #f4a620; font-size: 11px; }
  .org-ok { color: #39c26d; font-size: 13px; font-weight: 600; }
  .org-report { margin-top: 6px; font-size: 10.5px; color: var(--muted); word-break: break-all; }
  .org-err { margin-top: 10px; color: #f4a620; font-size: 11.5px; white-space: pre-wrap; }
  .org-charter { margin-top: 12px; padding-top: 10px; border-top: 1px solid var(--border); font-size: 10px; color: #556072; line-height: 1.5; }
  .nav {
    position: absolute; bottom: 18px; left: 50%; transform: translateX(-50%);
    display: flex; align-items: center; gap: 14px; background: var(--panel);
    border: 1px solid var(--border); border-radius: 22px; padding: 5px 10px;
  }
  .navbtn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    width: 34px; height: 34px; border-radius: 50%; cursor: pointer; font-size: 18px; line-height: 1;
  }
  .navbtn:hover { border-color: var(--accent); }
  .count { font-size: 12.5px; color: var(--muted); font-variant-numeric: tabular-nums; min-width: 70px; text-align: center; }
</style>
