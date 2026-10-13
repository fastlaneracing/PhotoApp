// Shared library state (tags, categories, albums, saved searches) used by the
// sidebar, inspector, filter chips and the Manage view. Reloaded after any
// edit that can change names or counts.
import { call } from './lib.js';

export const lib = $state({
  tags: [],
  categories: [],
  albums: [],
  searches: [],
  loaded: false,
});

export async function reloadLibrary() {
  try {
    const [tags, categories, albums, searches] = await Promise.all([
      call('tags.list'),
      call('categories.list'),
      call('albums.list'),
      call('searches.list'),
    ]);
    lib.tags = tags;
    lib.categories = categories;
    lib.albums = albums;
    lib.searches = searches;
    lib.loaded = true;
  } catch (e) {
    console.warn('library load failed', e);
  }
}

export function tagLabel(t) {
  return t.category ? `${t.category}: ${t.name}` : t.name;
}

// Toast notifications (bottom-right).
export const toasts = $state({ list: [] });
let toastSeq = 0;
export function toast(msg, kind = 'ok', ms = 3500, action = null) {
  const id = ++toastSeq;
  toasts.list = [...toasts.list, { id, msg, kind, action }];
  setTimeout(() => {
    toasts.list = toasts.list.filter((t) => t.id !== id);
  }, ms);
}

export const COLORS = [
  { key: 'red', hex: '#e5484d' },
  { key: 'orange', hex: '#f47b20' },
  { key: 'yellow', hex: '#f5c518' },
  { key: 'green', hex: '#3fb950' },
  { key: 'blue', hex: '#3b82f6' },
  { key: 'purple', hex: '#a371f7' },
];
export const colorHex = (k) => COLORS.find((c) => c.key === k)?.hex || 'transparent';

// Delete = send every copy on disk to the Windows Recycle Bin. Notes, tags and
// albums are kept, so Undo / Restore brings the photo back exactly as it was.
export async function deleteItems(items, onDone = () => {}) {
  const ids = items.map((i) => i.id);
  if (!ids.length) return;
  const photos = items.filter((i) => i.kind === 'image').length;
  const videos = ids.length - photos;
  const what = [photos && `${photos.toLocaleString()} photo${photos === 1 ? '' : 's'}`, videos && `${videos.toLocaleString()} video${videos === 1 ? '' : 's'}`]
    .filter(Boolean).join(' and ');
  const copies = items.reduce((n, i) => n + Math.max(1, i.copies || 1), 0);
  const ok = window.confirm(
    `Delete ${what}?\n\n` +
      `${copies.toLocaleString()} file${copies === 1 ? '' : 's'} will be moved to the Windows Recycle Bin` +
      (copies > ids.length ? ' (includes duplicate copies)' : '') + '.\n' +
      'You can undo, or restore later from "Recently deleted". Notes and tags are kept.'
  );
  if (!ok) return;
  try {
    const r = await call('items.delete', { ids });
    const msg = `Deleted ${r.items.toLocaleString()} item${r.items === 1 ? '' : 's'} (${r.files} file${r.files === 1 ? '' : 's'} to Recycle Bin)` +
      (r.failed.length ? ` — ${r.failed.length} failed` : '');
    toast(msg, r.failed.length ? 'warn' : 'ok', 10000, {
      label: 'Undo',
      run: () => restoreItems(ids, onDone),
    });
    if (r.failed.length) console.warn('delete failures', r.failed);
  } catch (e) {
    toast(String(e).replace(/^Error: /, ''), 'err', 10000);
  }
  onDone();
}

export async function restoreItems(ids, onDone = () => {}) {
  try {
    const r = await call('items.restore', { ids });
    toast(
      `Restored ${r.files} file${r.files === 1 ? '' : 's'}` +
        (r.skipped.length ? ` — ${r.skipped.length} skipped (name in use)` : '') +
        (r.notInBin ? ` — ${r.notInBin} no longer in the Recycle Bin` : ''),
      r.skipped.length || r.notInBin ? 'warn' : 'ok',
      7000
    );
  } catch (e) {
    toast(String(e).replace(/^Error: /, ''), 'err', 10000);
  }
  onDone();
}

// ---------------------------------------------------------------------------
// AI: shared status (pending approvals, running jobs). Polled while a job runs.
// ---------------------------------------------------------------------------
export const ai = $state({ stats: null, jobs: [] });
let aiTimer = null;
const aiListeners = new Set();
/** Called with no args whenever a poll sees progress (to refresh grids). */
export function onAiProgress(fn) {
  aiListeners.add(fn);
  return () => aiListeners.delete(fn);
}
export async function refreshAi() {
  try {
    const [s, jobs] = await Promise.all([call('ai.stats'), call('ai.jobs', { limit: 30 })]);
    const changed = JSON.stringify([ai.stats?.pending, ai.jobs.map((j) => [j.id, j.done, j.status])]) !==
      JSON.stringify([s.pending, jobs.map((j) => [j.id, j.done, j.status])]);
    ai.stats = s;
    ai.jobs = jobs;
    if (changed) aiListeners.forEach((f) => f());
    clearTimeout(aiTimer);
    if (s.running) aiTimer = setTimeout(refreshAi, 2000);
  } catch (e) {
    console.warn('ai status', e);
  }
}

export async function reviewWithAi(ids, { rereview = false, label = '' } = {}) {
  if (!ids.length) return null;
  try {
    const r = await call('ai.review', { ids, rereview, label: label || `Review ${ids.length} item${ids.length === 1 ? '' : 's'}` });
    toast(`AI is reviewing ${ids.length.toLocaleString()} item${ids.length === 1 ? '' : 's'} — suggestions appear in the AI tab for approval`, 'ok', 6000);
    refreshAi();
    return r;
  } catch (e) {
    toast(String(e).replace(/^Error: /, ''), 'err', 8000);
    return null;
  }
}

export const SUGG_LABEL = {
  add_tag: 'Add tag',
  remove_tag: 'Remove tag',
  description: 'Description',
  append_note: 'Add to notes',
  set_rating: 'Rating',
  set_favorite: 'Favorite',
  add_to_album: 'Add to album',
};

/** One-line human text for a suggestion or history payload. */
export function describeChange(kind, p) {
  switch (kind) {
    case 'add_tag': case 'tag_add': return `+ ${p.category ? p.category + ': ' : ''}${p.name}`;
    case 'remove_tag': case 'tag_remove': return `− ${p.category ? p.category + ': ' : ''}${p.name}`;
    case 'description': return `Description: ${(p.text ?? p.after ?? '').slice(0, 160)}`;
    case 'append_note': return `Note: ${p.text}`;
    case 'notes': return `Notes: “${String(p.after ?? '').slice(0, 120)}”`;
    case 'set_rating': return `Rating → ${'★'.repeat(p.value || 0) || 'none'}`;
    case 'rating': return `Rating ${'★'.repeat(p.before || 0) || '0'} → ${'★'.repeat(p.after || 0) || '0'}`;
    case 'set_favorite': case 'favorite': return (p.value ?? p.after) ? '♥ Favorite' : '♡ Not favorite';
    case 'color': return `Label ${p.before || 'none'} → ${p.after || 'none'}`;
    case 'add_to_album': case 'album_add': return `+ Album “${p.name}”`;
    case 'album_remove': return `− Album “${p.name}”`;
    default: return kind;
  }
}
