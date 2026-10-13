// Bridge to the Rust backend. Works in Tauri (invoke + asset protocol) and in
// a plain browser against the Vite dev server (invoke -> /api shim is NOT
// needed for dev: we call the same endpoints through a small fetch shim that
// hits the Rust CLI's one-shot JSON mode).

const IS_Tauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

let _invoke = null;
let _listen = null;

async function tauriCore() {
  if (!_invoke) {
    const core = await import('@tauri-apps/api/core');
    _invoke = core.invoke;
  }
  return _invoke;
}

async function tauriEvent() {
  if (!_listen) {
    const ev = await import('@tauri-apps/api/event');
    _listen = ev.listen;
  }
  return _listen;
}

export async function catalogItems() {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('catalog_items');
  }
  const r = await fetch('/api/catalog');
  if (!r.ok) throw new Error(`catalog ${r.status}`);
  return r.json();
}

export async function itemDetail(id) {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('item_detail', { id });
  }
  const r = await fetch(`/api/item/${encodeURIComponent(id)}`);
  if (!r.ok) throw new Error(`detail ${r.status}`);
  return r.json();
}

export async function stats() {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('stats');
  }
  const r = await fetch('/api/stats');
  if (!r.ok) throw new Error(`stats ${r.status}`);
  return r.json();
}

// M6: filtered query. `filter` is a plain object; empty/absent = no filter.
// The whole filter (incl. tags, album, rating, notes...) is sent as one JSON
// object so both paths deserialize into the same Rust `Filter`.
export async function queryItems(filter = {}) {
  const f = cleanFilter(filter);
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('query', { filter: f });
  }
  const r = await fetch(`/api/catalog?f=${encodeURIComponent(JSON.stringify(f))}`);
  if (!r.ok) throw new Error(`query ${r.status}`);
  return r.json();
}

// Library management: tags, notes, ratings, albums, saved searches, bulk
// moves, maintenance. See src-tauri/src/library.rs `call` for every command.
export async function call(cmd, args = {}) {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('call', { cmd, args });
  }
  const r = await fetch(`/api/call/${encodeURIComponent(cmd)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(args),
  });
  let data;
  try { data = await r.json(); } catch { data = { error: `HTTP ${r.status}` }; }
  if (!r.ok) throw new Error(data.error || `${cmd} failed (${r.status})`);
  return data;
}

/** Drop empty values; coerce numeric fields. */
export function cleanFilter(f = {}) {
  const out = {};
  for (const [k, v] of Object.entries(f)) {
    if (v === undefined || v === null || v === '') continue;
    if (v === false && k !== 'aiReviewed') continue; // aiReviewed:false = "not reviewed yet"
    if (v === 'all' && k !== 'presence') continue; // presence:'all' is meaningful
    if (Array.isArray(v) && v.length === 0) continue;
    out[k] = v;
  }
  for (const k of ['minWidth', 'minHeight', 'limit', 'minRating', 'album', 'category']) {
    if (out[k] !== undefined) out[k] = Number(out[k]);
  }
  return out;
}

// M6: facet counts for the filter UI.
export async function facets() {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('facets');
  }
  const r = await fetch('/api/facets');
  if (!r.ok) throw new Error(`facets ${r.status}`);
  return r.json();
}

// M7: duplicate groups (largest content first). `limit` optional.
export async function dupGroups(limit) {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('dup_groups', { limit: limit ?? null });
  }
  const qs = limit ? `?limit=${encodeURIComponent(limit)}` : '';
  const r = await fetch(`/api/dups${qs}`);
  if (!r.ok) throw new Error(`dups ${r.status}`);
  return r.json();
}

// M7: dup summary (groups, duplicate count, wasted bytes, flagged).
export async function dupSummary() {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('dup_summary');
  }
  const r = await fetch('/api/dups/summary');
  if (!r.ok) throw new Error(`dup summary ${r.status}`);
  return r.json();
}

// M7: staging actions (non-destructive; every one writes a report).
async function stagingAction(mediaId, action) {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke(`staging_${action}`, { mediaId });
  }
  const r = await fetch(`/api/dups/${encodeURIComponent(mediaId)}/${action}`, { method: 'POST' });
  let body;
  try { body = await r.json(); } catch { body = { error: `HTTP ${r.status}` }; }
  if (!r.ok) throw new Error(body.error || `staging ${action} ${r.status}`);
  return body;
}

export const stagingDryRun = (mediaId) => stagingAction(mediaId, 'dry-run');
export const stagingApply = (mediaId) => stagingAction(mediaId, 'apply');
export const stagingUndo = (mediaId) => stagingAction(mediaId, 'undo');

// M8: rename / move (relocate) the canonical file.
export async function moveTarget(mediaId, layout = 'year') {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    return invoke('move_target', { mediaId, layout });
  }
  const r = await fetch(`/api/relocate/${encodeURIComponent(mediaId)}/move-target?layout=${encodeURIComponent(layout)}`);
  if (!r.ok) throw new Error(`move-target ${r.status}`);
  return (await r.json()).target;
}

async function relocateAction(mediaId, action, body) {
  if (IS_Tauri) {
    const invoke = await tauriCore();
    const cmd = action === 'dry-run' ? 'relocate_dry_run' : action === 'apply' ? 'relocate_apply' : 'relocate_undo';
    return invoke(cmd, body);
  }
  const url = action === 'undo'
    ? `/api/relocate/${encodeURIComponent(mediaId)}/undo`
    : `/api/relocate/${encodeURIComponent(mediaId)}/${action}`;
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  let data;
  try { data = await r.json(); } catch { data = { error: `HTTP ${r.status}` }; }
  if (!r.ok) throw new Error(data.error || `relocate ${action} ${r.status}`);
  return data;
}

export const relocateDryRun = (mediaId, op, targetPath) =>
  relocateAction(mediaId, 'dry-run', { mediaId, op, targetPath });
export const relocateApply = (mediaId, op, targetPath) =>
  relocateAction(mediaId, 'apply', { mediaId, op, targetPath });
export const relocateUndo = (mediaId) => relocateAction(mediaId, 'undo', { mediaId });

export async function onCatalogChanged(cb) {
  if (IS_Tauri) {
    const listen = await tauriEvent();
    return listen('catalog-changed', (e) => cb(e.payload));
  }
  // Browser-dev fallback: poll every 15 s.
  const t = setInterval(() => cb({ note: 'poll' }), 15000);
  return () => clearInterval(t);
}

// Absolute media path -> URL the frontend can load. Synchronous and matches
// Tauri's convertFileSrc exactly (both use encodeURIComponent on the path).
//   Tauri:   http://asset.localhost/<encoded>
//   browser: /localfile/<encoded>   (Vite middleware resolves to disk)
export function mediaUrl(absPath) {
  if (!absPath) return null;
  if (IS_Tauri) {
    return `http://asset.localhost/${encodeURIComponent(absPath)}`;
  }
  return `/localfile/${encodeURIComponent(absPath)}`;
}

export function isTauri() {
  return IS_Tauri;
}
