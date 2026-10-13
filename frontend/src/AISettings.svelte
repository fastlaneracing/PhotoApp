<script>
  import { onMount } from 'svelte';
  import { call } from './lib.js';
  import { toast, refreshAi } from './store.svelte.js';

  let { onsaved = () => {} } = $props();

  const PRESETS = [
    { label: 'LM Studio (this PC)', provider: 'openai', baseUrl: 'http://localhost:1234/v1' },
    { label: 'Ollama (this PC)', provider: 'openai', baseUrl: 'http://localhost:11434/v1' },
    { label: 'llama.cpp server', provider: 'openai', baseUrl: 'http://localhost:8080/v1' },
    { label: 'OpenAI', provider: 'openai', baseUrl: 'https://api.openai.com/v1' },
    { label: 'Anthropic', provider: 'anthropic', baseUrl: 'https://api.anthropic.com' },
  ];

  let f = $state(null);
  let apiKey = $state('');
  let models = $state([]);
  let busy = $state('');
  let test = $state(null);
  let testErr = $state('');

  async function load() {
    f = await call('ai.settings.get');
  }
  onMount(load);

  function preset(p) {
    f.provider = p.provider;
    f.baseUrl = p.baseUrl;
    models = [];
  }

  async function fetchModels() {
    busy = 'models';
    try {
      models = await call('ai.models', { provider: f.provider, baseUrl: f.baseUrl, apiKey });
      if (!models.length) toast('Server answered but lists no models — type the model name', 'warn');
      else if (!f.model) f.model = models[0];
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 8000);
    } finally {
      busy = '';
    }
  }

  async function save(quiet = false) {
    busy = 'save';
    try {
      const patch = { ...f };
      delete patch.hasKey;
      patch.apiKey = apiKey; // empty = keep the stored key
      f = await call('ai.settings.save', patch);
      apiKey = '';
      if (!quiet) toast('AI settings saved');
      refreshAi();
      onsaved();
      return true;
    } catch (e) {
      toast(String(e).replace(/^Error: /, ''), 'err', 8000);
      return false;
    } finally {
      busy = '';
    }
  }

  async function runTest() {
    if (!(await save(true))) return;
    busy = 'test';
    test = null;
    testErr = '';
    try {
      test = await call('ai.test');
    } catch (e) {
      testErr = String(e).replace(/^Error: /, '');
    } finally {
      busy = '';
    }
  }

  async function clearKey() {
    f = await call('ai.settings.save', { clearKey: true });
    toast('API key removed');
  }
</script>

{#if f}
  <div class="set">
    <div class="presets">
      {#each PRESETS as p (p.label)}
        <button class="chip" class:on={f.baseUrl === p.baseUrl} onclick={() => preset(p)}>{p.label}</button>
      {/each}
    </div>

    <div class="grid">
      <label>API type
        <select bind:value={f.provider}>
          <option value="openai">OpenAI-compatible (LM Studio, Ollama, llama.cpp, vLLM, OpenAI)</option>
          <option value="anthropic">Anthropic-compatible</option>
        </select>
      </label>
      <label>Base URL
        <input bind:value={f.baseUrl} placeholder="http://localhost:1234/v1" />
      </label>
      <label>API key <span class="hint">{f.hasKey ? 'saved — leave blank to keep' : 'optional for local servers'}</span>
        <div class="row">
          <input type="password" bind:value={apiKey} placeholder={f.hasKey ? '••••••••' : 'none'} autocomplete="off" />
          {#if f.hasKey}<button class="btn ghost" onclick={clearKey}>remove</button>{/if}
        </div>
      </label>
      <label>Model <span class="hint">needs a vision model to look at photos (e.g. Qwen2.5-VL, Gemma 3, LLaVA, Llama 3.2 Vision)</span>
        <div class="row">
          <input list="ai-models" bind:value={f.model} placeholder="model id" />
          <datalist id="ai-models">{#each models as m (m)}<option value={m}></option>{/each}</datalist>
          <button class="btn" onclick={fetchModels} disabled={!!busy}>{busy === 'models' ? '…' : 'List models'}</button>
        </div>
      </label>
    </div>

    <div class="checks">
      <label><input type="checkbox" bind:checked={f.vision} /> Send the picture to the model (vision)</label>
      <label><input type="checkbox" bind:checked={f.suggestNotes} /> Let it suggest notes (visible text, numbers, places)</label>
      <label title="Asks the server for strict JSON. Turn on only if your server supports response_format."><input type="checkbox" bind:checked={f.jsonMode} /> Strict JSON mode</label>
    </div>

    <label class="wide">Your instructions to the model <span class="hint">what matters in your library — used in every review</span>
      <textarea rows="3" bind:value={f.instructions} placeholder="e.g. Many photos are APBA offshore race boats — read hull numbers and boat names. Trucks are hotshot rigs for LP Logistics."></textarea>
    </label>
    <label class="wide">Preferred tag categories <span class="hint">comma separated</span>
      <input bind:value={f.categories} />
    </label>

    <div class="nums">
      <label>Max tags per photo <input type="number" min="1" max="30" bind:value={f.maxTags} /></label>
      <label>Image size sent (px) <input type="number" min="256" max="4096" step="64" bind:value={f.imageMaxPx} /></label>
      <label>Temperature <input type="number" min="0" max="2" step="0.1" bind:value={f.temperature} /></label>
      <label>Max reply tokens <input type="number" min="128" max="16384" step="128" bind:value={f.maxTokens} /></label>
      <label>Timeout (s) <input type="number" min="10" max="3600" bind:value={f.timeoutSecs} /></label>
    </div>

    <div class="row actions">
      <button class="btn primary" onclick={() => save()} disabled={!!busy}>{busy === 'save' ? 'Saving…' : 'Save'}</button>
      <button class="btn" onclick={runTest} disabled={!!busy || !f.model}>{busy === 'test' ? 'Testing… (a local model can take a while)' : 'Save & test connection'}</button>
    </div>
    {#if test}
      <div class="ok">
        ✓ Connected — replied “{test.reply}” in {(test.ms / 1000).toFixed(1)}s{test.models ? ` · ${test.models} models available` : ''}
        {#if test.vision}<div>✓ Vision: “{test.vision}”</div>{/if}
        {#if test.visionError}<div class="warn">⚠ Vision failed: {test.visionError} — pick a vision model or turn vision off</div>{/if}
      </div>
    {/if}
    {#if testErr}<div class="err">✕ {testErr}</div>{/if}
  </div>
{/if}

<style>
  .set { display: flex; flex-direction: column; gap: 12px; }
  .presets { display: flex; flex-wrap: wrap; gap: 6px; }
  .chip { background: var(--panel2); border: 1px solid var(--border); color: var(--muted); border-radius: 14px; padding: 4px 11px; font-size: 12px; cursor: pointer; }
  .chip:hover { color: var(--text); }
  .chip.on { color: var(--text); border-color: var(--accent); }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px 16px; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--muted); }
  .hint { font-size: 11px; color: #6b7383; }
  .wide { width: 100%; }
  input, select, textarea {
    background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 7px;
    padding: 7px 9px; font: inherit; font-size: 12.5px; box-sizing: border-box; width: 100%;
  }
  input:focus, select:focus, textarea:focus { outline: none; border-color: var(--accent); }
  input[type='checkbox'] { width: auto; accent-color: var(--accent); }
  .checks { display: flex; flex-wrap: wrap; gap: 6px 22px; }
  .checks label { flex-direction: row; align-items: center; gap: 7px; color: var(--text); font-size: 12.5px; }
  .nums { display: grid; grid-template-columns: repeat(5, 1fr); gap: 10px; }
  .row { display: flex; gap: 6px; align-items: center; }
  .row input { flex: 1; }
  .btn {
    background: var(--panel2); color: var(--text); border: 1px solid var(--border); white-space: nowrap;
    padding: 6px 13px; border-radius: 8px; cursor: pointer; font-size: 12.5px;
  }
  .btn:hover { border-color: var(--accent); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn.primary { background: #173049; border-color: #2c567f; }
  .btn.ghost { background: none; color: var(--muted); }
  .ok { color: #9ff0b8; font-size: 12.5px; line-height: 1.6; }
  .err { color: #ff9a8a; font-size: 12.5px; }
  .warn { color: #f4a620; }
</style>
