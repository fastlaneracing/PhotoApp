<script>
  import { onMount } from 'svelte';
  import { call, pickFolder, isTauri } from './lib.js';
  import { toast, reloadLibrary } from './store.svelte.js';

  let { onchanged = () => {} } = $props();

  let s = $state(null); // settings.get
  let busy = $state('');
  let exclDraft = $state('');

  async function load() {
    try {
      s = await call('settings.get');
    } catch (e) {
      toast(String(e), 'err', 8000);
    }
  }
  onMount(load);

  async function run(label, cmd, args, okMsg) {
    busy = label;
    try {
      const r = await call(cmd, args);
      await load();
      reloadLibrary();
      onchanged();
      if (okMsg) toast(typeof okMsg === 'function' ? okMsg(r) : okMsg, 'ok', 6000);
      return r;
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 9000);
      return null;
    } finally {
      busy = '';
    }
  }

  // ----- folders -----
  async function addFolder() {
    let path;
    try {
      path = await pickFolder('', 'Add a library folder');
    } catch (e) {
      toast(String(e), 'err');
      return;
    }
    if (!path) return;
    await run('add', 'folders.add', { path }, (r) =>
      `Added “${r.name}”. ${isTauri() ? 'Cataloging its photos in the background — new files appear as they are read.' : 'Cataloged.'}`);
  }
  async function addTyped() {
    const path = window.prompt('Folder path to add (e.g. D:/Photos or \\\\NAS\\Photos)');
    if (path) await run('add', 'folders.add', { path }, (r) => `Added “${r.name}”.`);
  }
  async function rename(f) {
    const n = window.prompt('Name for this library folder', f.name);
    if (!n || n === f.name) return;
    await run('f', 'folders.update', { name: f.name, newName: n }, `Renamed to “${n}”`);
  }
  const update = (f, args, msg) => run('f', 'folders.update', { name: f.name, ...args }, msg);
  async function togglePause(f) {
    await update(f, { enabled: !f.enabled }, f.enabled
      ? `Paused “${f.name}”: not scanned or watched; its photos stay in the catalog.`
      : `Resumed “${f.name}”: syncing it now.`);
  }
  async function toggleProtect(f) {
    if (!f.protected) {
      await update(f, { protected: true }, `“${f.name}” is protected: photoApp will never delete or move files there.`);
    } else if (window.confirm(`Unprotect “${f.name}”?\n\nphotoApp may then delete or move files in ${f.path} when you ask it to (including duplicate clean-up).`)) {
      await update(f, { protected: false }, `“${f.name}” is no longer protected`);
    }
  }
  async function remove(f) {
    let p;
    try {
      p = await call('folders.remove', { name: f.name, preview: true });
    } catch (e) {
      toast(String(e), 'err');
      return;
    }
    const msg =
      `Remove “${f.name}” (${f.path}) from the library?\n\n` +
      `No files are deleted from disk. ${p.items.toLocaleString()} item(s) have a copy there.\n` +
      (p.onlyHere
        ? `${p.onlyHere.toLocaleString()} of them exist only in this folder and will move to “Not on disk”` +
          (p.onlyHereAnnotated ? ` (${p.onlyHereAnnotated.toLocaleString()} carry notes, tags, ratings or albums — kept in the catalog)` : '') +
          '.\nAdd the folder again later and they come straight back with their notes.'
        : 'All of them also exist in another library folder, so nothing drops out.');
    if (!window.confirm(msg)) return;
    await run('f', 'folders.remove', { name: f.name }, `Removed “${f.name}” from the library (files untouched)`);
  }

  // ----- exclusions -----
  async function saveExclusions(list) {
    await run('ex', 'exclusions.set', { list }, 'Skipped folders saved — re-syncing');
  }
  async function addExclusion() {
    let path = exclDraft.trim();
    if (!path) {
      try { path = await pickFolder(s?.folders?.[0]?.path || '', 'Folder to skip while scanning'); } catch { return; }
    }
    if (!path) return;
    exclDraft = '';
    await saveExclusions([...(s.exclusions || []), path]);
  }

  // ----- duplicates -----
  async function setDup(patch) {
    busy = 'dup';
    try {
      const d = await call('dups.settings.set', patch);
      s.dups = d;
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 9000);
      await load();
    } finally {
      busy = '';
    }
  }
  async function pickMoveDir() {
    let dir;
    try { dir = await pickFolder(s.dups.moveDir || '', 'Folder for removed duplicate copies'); } catch (e) { toast(String(e), 'err'); return; }
    if (dir) await setDup({ action: 'move', moveDir: dir });
  }
  async function setAuto(v) {
    if (v !== 'off') {
      const what = v === 'exact' ? 'identical copies' : 'identical copies and clearly lower-quality copies';
      const where = s.dups.action === 'move' ? `moved to ${s.dups.moveDir}` : 'sent to the Recycle Bin';
      if (!window.confirm(`Clean up automatically?\n\nAfter every sync, ${what} will be ${where}, keeping the best copy. Look-alike photos are never removed automatically, and protected folders are never touched.\n\nEvery clean-up can be undone from the Duplicates tab.`)) {
        await load();
        return;
      }
    }
    await setDup({ auto: v });
  }

  function fmtBytes(b) {
    if (!b) return '0 B';
    if (b >= 1e12) return (b / 1e12).toFixed(2) + ' TB';
    if (b >= 1e9) return (b / 1e9).toFixed(1) + ' GB';
    if (b >= 1e6) return (b / 1e6).toFixed(0) + ' MB';
    return Math.max(1, Math.round(b / 1e3)) + ' KB';
  }
</script>

<div class="settings">
  {#if !s}
    <div class="muted">loading…</div>
  {:else}
    <section>
      <h2>Library folders
        <button class="btn primary sm" disabled={!!busy} onclick={addFolder}>＋ Add folder…</button>
        <button class="lnk" disabled={!!busy} onclick={addTyped} title="Type a path, e.g. a network share">type a path</button>
      </h2>
      <p class="muted">photoApp catalogs, watches and searches every active folder as one library, and finds duplicates across all of them.
        Order is priority: when copies are equal, the one in the higher folder is kept, and new year folders go into the first one.</p>
      <table>
        <thead><tr><th>#</th><th>Name</th><th>Folder</th><th>Items</th><th>Status</th><th>Options</th><th></th></tr></thead>
        <tbody>
          {#each s.folders as f, i (f.name)}
            <tr class:paused={!f.enabled}>
              <td class="num prio">
                <button class="arrow" disabled={!!busy || i === 0} onclick={() => update(f, { move: -1 })} title="Higher priority">▲</button>
                {f.priority}
                <button class="arrow" disabled={!!busy || i === s.folders.length - 1} onclick={() => update(f, { move: 1 })} title="Lower priority">▼</button>
              </td>
              <td><button class="namebtn" onclick={() => rename(f)} title="Rename">{f.name}</button></td>
              <td class="path" title={f.path}>{f.path}</td>
              <td class="num">{f.items.toLocaleString()}<span class="sub">{fmtBytes(f.bytes)}</span></td>
              <td>
                {#if !f.enabled}<span class="pill gray">paused</span>
                {:else if f.online}<span class="pill green">online</span>
                {:else}<span class="pill amber" title="Drive not connected — nothing is marked missing while offline">offline</span>{/if}
                {#if f.protected}<span class="pill blue" title="photoApp never deletes or moves files here">🔒 protected</span>{/if}
              </td>
              <td class="acts">
                <button class="lnk" disabled={!!busy} onclick={() => togglePause(f)}>{f.enabled ? 'pause' : 'resume'}</button>
                <button class="lnk" disabled={!!busy} onclick={() => toggleProtect(f)}>{f.protected ? 'unprotect' : 'protect'}</button>
              </td>
              <td class="acts"><button class="lnk bad" disabled={!!busy || s.folders.length < 2} onclick={() => remove(f)}>remove</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
      <ul class="explain">
        <li><b>Pause</b> — stop scanning and watching a folder (a slow network share, a drive you rarely plug in). Its photos, notes and tags stay in the catalog.</li>
        <li><b>Protect</b> — photoApp never deletes, moves or replaces files in this folder (a backup drive, a client archive). Duplicates there are always kept.</li>
        <li><b>Remove</b> — takes the folder out of the library. Files on disk are not touched; notes and tags stay in the catalog in case you add it back.</li>
      </ul>

      <h3>Skip these folders while scanning</h3>
      <p class="muted">Files inside a skipped folder leave the library (they show under <i>Not on disk</i>; notes and tags are kept).</p>
      <div class="excl">
        {#each s.exclusions as e (e)}
          <span class="chip">{e}<button onclick={() => saveExclusions(s.exclusions.filter((x) => x !== e))} title="Stop skipping">✕</button></span>
        {/each}
        {#if !s.exclusions.length}<span class="muted">None.</span>{/if}
      </div>
      <div class="row">
        <input class="txt" placeholder="Folder path (or leave empty to browse)…" bind:value={exclDraft} onkeydown={(e) => e.key === 'Enter' && addExclusion()} />
        <button class="btn sm" disabled={!!busy} onclick={addExclusion}>Skip folder</button>
      </div>
      <p class="muted small">Saved in <code>{s.configPath}</code> (previous version kept as <code>config.toml.bak</code>).</p>
    </section>

    <section>
      <h2>Duplicates</h2>
      <div class="grid">
        <div class="opt">
          <div class="lbl">What counts as a duplicate</div>
          <label><input type="checkbox" checked disabled /> Identical copies (same file, byte for byte)</label>
          <label><input type="checkbox" checked={s.dups.similar} onchange={(e) => setDup({ similar: e.target.checked })} /> The same photo saved at a different size or quality
            <span class="hint">resized, re-compressed, converted (PNG/JPEG/WebP), or stripped of camera data by a phone app or website</span></label>
          {#if s.dups.similar}
            <div class="sub-opt">
              <span>Match</span>
              {#each [['strict', 'strict'], ['normal', 'normal'], ['loose', 'loose']] as [v, l] (v)}
                <label class="radio"><input type="radio" name="sens" checked={s.dups.sensitivity === v} onchange={() => setDup({ sensitivity: v })} /> {l}</label>
              {/each}
              <span class="hint">loose finds more, but review its matches</span>
            </div>
            <label class="sub-opt"><input type="checkbox" checked={s.dups.requireSameDate} onchange={(e) => setDup({ requireSameDate: e.target.checked })} /> Must have the same date taken (when both have one)
              <span class="hint">keeps burst shots and look-alikes from different moments apart</span></label>
          {/if}
          <label><input type="checkbox" checked={s.dups.includeVideos} onchange={(e) => setDup({ includeVideos: e.target.checked })} /> Include videos (identical copies only)</label>
          <div class="sub-opt">
            <span>Where</span>
            <label class="radio"><input type="radio" name="scope" checked={s.dups.scope === 'all'} onchange={() => setDup({ scope: 'all' })} /> anywhere</label>
            <label class="radio"><input type="radio" name="scope" checked={s.dups.scope === 'acrossFolders'} onchange={() => setDup({ scope: 'acrossFolders' })} /> only between different library folders</label>
          </div>
        </div>

        <div class="opt">
          <div class="lbl">Which copy to keep</div>
          <label class="radio"><input type="radio" name="keep" checked={s.dups.keepRule === 'quality'} onchange={() => setDup({ keepRule: 'quality' })} /> <b>The highest quality one</b>, wherever it is</label>
          <span class="hint indent">most pixels → has camera data → lossless over lossy (TIFF/PNG over JPEG) → least compressed. Folder order breaks ties.</span>
          <label class="radio"><input type="radio" name="keep" checked={s.dups.keepRule === 'folder'} onchange={() => setDup({ keepRule: 'folder' })} /> The one in the highest-priority folder</label>
          <span class="hint indent">quality only breaks ties inside the same folder</span>
          <label><input type="checkbox" checked={s.dups.preferOriginalNames} onchange={(e) => setDup({ preferOriginalNames: e.target.checked })} /> Prefer original names over “IMG_1234 (2).jpg” / “Copy of …”</label>
          <label><input type="checkbox" checked={s.dups.mergeAnnotations} onchange={(e) => setDup({ mergeAnnotations: e.target.checked })} /> Give the kept copy the notes, tags, rating, label and albums of the removed ones</label>
        </div>

        <div class="opt">
          <div class="lbl">What happens to the other copies</div>
          <label class="radio"><input type="radio" name="act" checked={s.dups.action === 'recycle'} onchange={() => setDup({ action: 'recycle' })} /> Send to the Recycle Bin</label>
          <label class="radio"><input type="radio" name="act" checked={s.dups.action === 'move'} onchange={() => (s.dups.moveDir ? setDup({ action: 'move' }) : pickMoveDir())} /> Move to a review folder outside the library</label>
          {#if s.dups.action === 'move'}
            <div class="sub-opt"><code>{s.dups.moveDir || '(none chosen)'}</code><button class="btn sm" onclick={pickMoveDir}>Change…</button></div>
            <span class="hint indent">kept as &lt;review folder&gt;\&lt;library folder name&gt;\&lt;original subfolders&gt;</span>
          {/if}
          <span class="hint">Files in protected folders are never removed. Every clean-up can be undone from the Duplicates tab.</span>
        </div>

        <div class="opt">
          <div class="lbl">Automatic clean-up after each sync</div>
          <select value={s.dups.auto} onchange={(e) => setAuto(e.target.value)}>
            <option value="off">Off — I review in the Duplicates tab</option>
            <option value="exact">Identical copies only</option>
            <option value="copies">Identical + clearly lower-quality copies</option>
          </select>
          <span class="hint">Look-alikes (same size and format, both with camera data — e.g. burst shots) always wait for your review.</span>
        </div>
      </div>
    </section>
  {/if}
</div>

<style>
  .settings { height: 100%; overflow-y: auto; padding: 18px 26px 60px; box-sizing: border-box; }
  section { max-width: 1100px; margin-bottom: 34px; }
  h2 { font-size: 15px; display: flex; align-items: center; gap: 12px; margin: 0 0 8px; }
  h3 { font-size: 13px; margin: 18px 0 6px; }
  .muted { color: var(--muted); font-size: 12.5px; margin: 0 0 10px; line-height: 1.45; }
  .muted.small { font-size: 11.5px; margin-top: 8px; }
  table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
  th { text-align: left; color: var(--muted); font-weight: 500; font-size: 11px; text-transform: uppercase; letter-spacing: 0.4px; padding: 6px 8px; border-bottom: 1px solid var(--border); }
  td { padding: 7px 8px; border-bottom: 1px solid #1d2129; vertical-align: middle; }
  tr.paused td { opacity: 0.6; }
  td.num { font-variant-numeric: tabular-nums; color: var(--muted); white-space: nowrap; }
  td.num .sub { display: block; font-size: 10.5px; color: #6b7688; }
  td.prio { width: 64px; }
  td.path { color: #cfe0ff; font-family: ui-monospace, Consolas, monospace; font-size: 11.5px; max-width: 360px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .arrow { background: none; border: 0; color: var(--muted); cursor: pointer; font-size: 9px; padding: 0 3px; }
  .arrow:hover:not(:disabled) { color: var(--accent); }
  .arrow:disabled { opacity: 0.25; cursor: default; }
  .namebtn { background: none; border: 0; color: var(--text); cursor: text; font-size: 13px; font-weight: 600; padding: 0; }
  .namebtn:hover { text-decoration: underline dotted; }
  .pill { font-size: 10.5px; font-weight: 600; padding: 2px 8px; border-radius: 9px; margin-right: 4px; white-space: nowrap; }
  .pill.green { background: #0f2417; color: #6fdc95; border: 1px solid #1f5c38; }
  .pill.amber { background: #2a2210; color: #ffd28a; border: 1px solid #6b5520; }
  .pill.gray { background: #1a1e26; color: #9aa4b2; border: 1px solid #2c3340; }
  .pill.blue { background: #111a28; color: #8ab4f8; border: 1px solid #2c3a4d; }
  .acts { text-align: right; white-space: nowrap; }
  .lnk { background: none; border: 0; color: #8ab4f8; cursor: pointer; font-size: 12px; padding: 0 5px; font-weight: 400; }
  .lnk.bad { color: #ff8f80; }
  .lnk:disabled { opacity: 0.4; cursor: default; }
  .explain { margin: 10px 0 0; padding-left: 18px; color: var(--muted); font-size: 12px; line-height: 1.55; }
  .explain b { color: var(--text); font-weight: 600; }
  .excl { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 8px; }
  .chip { background: var(--panel2); border: 1px solid var(--border); border-radius: 12px; padding: 2px 4px 2px 10px; font-size: 12px; font-family: ui-monospace, Consolas, monospace; display: inline-flex; align-items: center; gap: 4px; }
  .chip button { background: none; border: 0; color: var(--muted); cursor: pointer; font-size: 11px; }
  .chip button:hover { color: #ff8f80; }
  .row { display: flex; gap: 8px; align-items: center; }
  .txt { flex: 0 1 420px; background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 7px; padding: 5px 9px; font-size: 12.5px; }
  .grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  .opt { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 12px 14px; display: flex; flex-direction: column; gap: 7px; font-size: 12.5px; }
  .opt label { display: block; line-height: 1.4; cursor: pointer; }
  .opt .lbl { font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; color: var(--muted); margin-bottom: 2px; }
  .opt input[type='checkbox'], .opt input[type='radio'] { accent-color: var(--accent); margin-right: 4px; vertical-align: -1px; }
  .opt select { background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 7px; padding: 5px 8px; font-size: 12.5px; }
  .hint { display: block; color: #7d8796; font-size: 11.5px; margin-left: 22px; line-height: 1.4; }
  .hint.indent { margin-top: -4px; }
  .sub-opt { display: flex; flex-wrap: wrap; gap: 6px 12px; align-items: center; margin-left: 22px; color: var(--muted); }
  .sub-opt .hint { margin-left: 0; display: inline; }
  label.radio { display: inline-block; }
  .opt > label.radio { display: block; }
  .btn { background: var(--panel2); color: var(--text); border: 1px solid var(--border); white-space: nowrap; padding: 6px 13px; border-radius: 8px; cursor: pointer; font-size: 12.5px; font-weight: 400; }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.45; cursor: default; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn.sm { padding: 3px 10px; font-size: 12px; }
  code { background: #0f141c; border: 1px solid var(--border); border-radius: 4px; padding: 0 4px; font-size: 11.5px; color: #cfe0ff; }
</style>
