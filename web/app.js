let providers = [];
let authSettings = {enabled: true, api_keys: []};
const $ = selector => document.querySelector(selector);
const $$ = selector => [...document.querySelectorAll(selector)];
const providerDialog = $('#provider-dialog');
const providerForm = $('#provider-form');
let providerStep = 1;
let providerIdEdited = false;
let selectedProviderId = null;

function slugify(value) {
  return value.normalize('NFKD').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}

function showView(name) {
  $('#providers-view').hidden = name !== 'providers';
  $('#models-view').hidden = name !== 'models';
  $('#access-view').hidden = name !== 'access';
  $$('.nav[data-view]').forEach(item => item.classList.toggle('active', item.dataset.view === name));
  const active = $(`.nav[data-view="${name}"]`);
  const indicator = $('.nav-indicator');
  indicator.style.transform = `translateY(${active.offsetTop}px)`;
  selectedProviderId = name === 'providers' ? selectedProviderId : null;
  if (name === 'providers') renderProviderPage();
}
$$('.nav[data-view]').forEach(item => item.addEventListener('click', () => showView(item.dataset.view)));
requestAnimationFrame(() => showView('providers'));

function setProviderStep(step) {
  providerStep = step;
  $$('.form-step').forEach(element => { element.hidden = Number(element.dataset.step) !== step; });
  $$('.step').forEach(element => element.classList.toggle('active', Number(element.dataset.stepDot) <= step));
  $('#next-step').hidden = step === 2;
  $('#create-provider').hidden = step !== 2;
  $('#step-description').textContent = step === 1 ? 'Choose the provider name and ID.' : 'Connect the first API endpoint.';
  (step === 1 ? $('#display-name') : $('#base-url')).focus();
}

function openProviderDialog() {
  providerForm.reset();
  providerIdEdited = false;
  $('#provider-error').textContent = '';
  toggleKeyRequirement();
  setProviderStep(1);
  providerDialog.showModal();
}
$('#open-provider').addEventListener('click', openProviderDialog);
$('#empty-add').addEventListener('click', openProviderDialog);
$$('.close-dialog').forEach(button => button.addEventListener('click', () => providerDialog.close()));
$('#next-step').addEventListener('click', () => {
  const fields = $$('[data-step="1"] input');
  if (fields.every(field => field.reportValidity())) setProviderStep(2);
});
$('#display-name').addEventListener('input', event => {
  if (!providerIdEdited) $('#provider-id').value = slugify(event.target.value);
});
$('#provider-id').addEventListener('input', event => {
  event.target.value = slugify(event.target.value);
  providerIdEdited = event.target.value !== slugify($('#display-name').value);
});

function setApiType(type) {
  const input = $(`input[name="api_type"][value="${type}"]`);
  input.checked = true;
  $$('#api-type-choices .choice').forEach(choice => choice.classList.toggle('selected', choice.contains(input)));
}
$$('input[name="api_type"]').forEach(input => input.addEventListener('change', () => setApiType(input.value)));
$('#base-url').addEventListener('input', event => {
  const hostname = new URL(event.target.value, 'http://invalid').hostname;
  if (hostname === 'api.anthropic.com' || hostname.endsWith('.anthropic.com')) setApiType('anthropic');
  if (hostname.includes('openai.com') || hostname.includes('azure.com')) setApiType('openai_compatible');
});

function toggleKeyRequirement() {
  const required = $('#requires-key').checked;
  $('#initial-key-section').classList.toggle('collapsed', !required);
  $('#api-key').required = required;
}
$('#requires-key').addEventListener('change', toggleKeyRequirement);
function bindSecretToggles(root = document) {
  root.querySelectorAll('.toggle-key').forEach(button => {
    if (button.dataset.bound) return;
    button.dataset.bound = 'true';
    button.addEventListener('click', () => {
      const input = button.parentElement.querySelector('input');
      input.type = input.type === 'password' ? 'text' : 'password';
      button.textContent = input.type === 'password' ? 'Show' : 'Hide';
    });
  });
}
bindSecretToggles();

providerForm.addEventListener('submit', async event => {
  event.preventDefault();
  const data = new FormData(providerForm);
  const payload = {
    id: data.get('id'),
    name: data.get('name'),
    endpoint: {
      api_type: data.get('api_type'),
      base_url: data.get('base_url'),
      requires_api_key: data.get('requires_api_key') === 'on',
      api_key: data.get('requires_api_key') === 'on' ? data.get('api_key') : null
    }
  };
  const response = await fetch('/admin/providers', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#provider-error'));
  providerDialog.close();
  await loadProviders();
  [1000, 3000, 8000].forEach(delay => setTimeout(loadProviders, delay));
});

function keyCount(provider) {
  return provider.endpoints.reduce((count, endpoint) => count + endpoint.api_keys.length, 0);
}

function renderProviders() {
  $('#provider-count').textContent = `${providers.length} provider${providers.length === 1 ? '' : 's'}`;
  $('#empty').hidden = providers.length > 0;
  $('#providers').hidden = providers.length === 0;
  $('#providers').replaceChildren(...providers.map(provider => {
    const card = document.createElement('button');
    card.className = 'provider-list-item';
    card.dataset.provider = provider.id;
    const modelStatus = provider.model_discovery_error ? 'Model discovery failed' : provider.models_discovered_at ? `${provider.discovered_models.length} models` : 'Discovering models…';
    card.innerHTML = `<span class="provider-avatar">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</span><span class="provider-list-main"><strong>${escapeHtml(provider.name)}</strong><code>${escapeHtml(provider.id)}/model-id</code></span><span class="provider-list-meta">${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} · ${keyCount(provider)} key${keyCount(provider) === 1 ? '' : 's'}<small class="${provider.model_discovery_error ? 'error-text' : ''}">${escapeHtml(modelStatus)}</small></span><span class="chevron">›</span>`;
    card.addEventListener('click', () => { selectedProviderId = provider.id; renderProviderPage(); });
    return card;
  }));
  renderProviderPage();
  renderRoutes();
}

function renderProviderPage() {
  const provider = providers.find(item => item.id === selectedProviderId);
  $('#provider-list-page').hidden = Boolean(provider);
  $('#provider-detail-page').hidden = !provider;
  if (!provider) return;
  const endpointHtml = provider.endpoints.map(endpoint => `<section class="card"><div class="card-head"><div><h2>${escapeHtml(endpoint.id)} <span class="kind">${formatType(endpoint.api_type)}</span></h2><p><code>${escapeHtml(endpoint.base_url)}</code></p></div><button class="button secondary add-key" data-provider="${provider.id}">＋ Add key</button></div><div class="traffic-help"><strong>Traffic distribution</strong><span>Requests are divided by relative weight. Equal values split traffic evenly.</span></div><div class="key-list">${endpoint.api_keys.length ? endpoint.api_keys.map(key => `<div class="key-row"><span class="status ${key.enabled ? 'enabled' : ''}"></span><span class="key-name">${escapeHtml(key.name)}</span><label class="inline-weight"><small>Traffic weight</small><input class="weight" type="range" min="1" max="300" value="${key.weight}" data-provider="${provider.id}" data-key="${key.id}"><output>${key.weight}</output></label><button class="key-toggle text-link" data-provider="${provider.id}" data-key="${key.id}" data-enabled="${key.enabled}">${key.enabled ? 'Disable' : 'Enable'}</button></div>`).join('') : '<div class="empty compact-empty">No API keys configured</div>'}</div></section>`).join('');
  const discovery = provider.model_discovery_error ? `<span class="error-text">${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `${provider.discovered_models.length} models discovered` : 'Models have not been discovered yet';
  $('#provider-detail').innerHTML = `<div class="page-head"><div><h1>${escapeHtml(provider.name)}</h1><p><code>${escapeHtml(provider.id)}/model-id</code></p></div><button class="delete-provider button danger" data-provider="${provider.id}">Delete provider</button></div><section class="card"><div class="card-head"><div><h2>Models</h2><p>${discovery}</p></div><button class="button secondary refresh-models" data-provider="${provider.id}">Refresh models</button></div>${provider.discovered_models.length ? `<div class="model-chips">${provider.discovered_models.map(model => `<code>${escapeHtml(model)}</code>`).join('')}</div>` : '<div class="empty compact-empty">No models reported by this provider.</div>'}</section><h2 class="section-title">API endpoints and keys</h2>${endpointHtml}`;
  bindProviderActions();
}

$('#back-to-providers').addEventListener('click', () => { selectedProviderId = null; renderProviderPage(); });

function bindProviderActions() {
  $$('.delete-provider').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    if (confirm(`Delete ${provider.name}?`)) { await fetch(`/admin/providers/${provider.id}`, {method: 'DELETE'}); selectedProviderId = null; await loadProviders(); }
  }));
  $$('.add-key').forEach(button => button.addEventListener('click', () => openKeyDialog(button.dataset.provider)));
  $$('.key-toggle').forEach(button => button.addEventListener('click', async () => {
    await patchKey(button.dataset.provider, button.dataset.key, {enabled: button.dataset.enabled !== 'true'});
  }));
  $$('.weight').forEach(input => {
    input.addEventListener('input', () => { const output = input.parentElement.querySelector('output'); if (output) output.textContent = input.value; });
    input.addEventListener('change', async () => {
      if (input.reportValidity()) await patchKey(input.dataset.provider, input.dataset.key, {weight: Number(input.value)});
    });
  });
  $$('.refresh-models').forEach(button => button.addEventListener('click', () => refreshModels(button.dataset.provider, button)));
}

async function patchKey(providerId, keyId, update) {
  await fetch(`/admin/providers/${providerId}/keys/${keyId}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(update)});
  await loadProviders();
}

const keyDialog = $('#key-dialog');
function openKeyDialog(providerId) {
  const provider = providers.find(item => item.id === providerId);
  $('#key-form').reset();
  $('#key-form [name="provider_id"]').value = providerId;
  $('#key-form [name="secret"]').type = 'password';
  $('#key-form .toggle-key').textContent = 'Show';
  setNewKeyWeight(100);
  $('#key-endpoint').replaceChildren(...provider.endpoints.map(endpoint => new Option(`${endpoint.id} · ${formatType(endpoint.api_type)}`, endpoint.id)));
  keyDialog.showModal();
}
$$('.close-key').forEach(button => button.addEventListener('click', () => keyDialog.close()));
$('#key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const providerId = data.get('provider_id');
  const payload = {endpoint_id: data.get('endpoint_id'), name: data.get('name'), secret: data.get('secret'), weight: Number(data.get('weight'))};
  const response = await fetch(`/admin/providers/${providerId}/keys`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (response.ok) { keyDialog.close(); await loadProviders(); }
});

function setNewKeyWeight(weight) {
  $('#new-key-weight').value = weight; $('#new-key-weight-output').textContent = weight;
  $$('.weight-presets button').forEach(button => button.classList.toggle('selected', Number(button.dataset.weight) === Number(weight)));
}
$('#new-key-weight').addEventListener('input', event => setNewKeyWeight(event.target.value));
$$('.weight-presets button').forEach(button => button.addEventListener('click', () => setNewKeyWeight(button.dataset.weight)));

const routeDialog = $('#route-dialog');
function populateRouteTargets() {
  const provider = providers.find(item => item.id === $('#route-provider').value);
  const options = (provider?.endpoints || []).flatMap(endpoint => endpoint.api_keys.filter(key => key.enabled).map(key => new Option(`${endpoint.id} · ${key.name}`, `${endpoint.id}\n${key.id}`)));
  $('#route-target').replaceChildren(...options);
}
$('#open-route').addEventListener('click', () => {
  $('#route-form').reset(); $('#route-error').textContent = '';
  $('#route-provider').replaceChildren(...providers.map(provider => new Option(provider.name, provider.id)));
  populateRouteTargets(); routeDialog.showModal();
});
$('#route-provider').addEventListener('change', populateRouteTargets);
$$('.close-route').forEach(button => button.addEventListener('click', () => routeDialog.close()));
$('#route-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const [endpointId, apiKeyId] = data.get('route_target').split('\n');
  const response = await fetch(`/admin/providers/${data.get('provider_id')}/model-routes`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({pattern: data.get('pattern'), endpoint_id: endpointId, api_key_id: apiKeyId})});
  if (!response.ok) return showApiError(response, $('#route-error'));
  routeDialog.close(); await loadProviders();
});

async function refreshModels(providerId, button) {
  const original = button.textContent; button.disabled = true; button.textContent = 'Refreshing…';
  const response = await fetch(`/admin/providers/${providerId}/models/refresh`, {method: 'POST'});
  await loadProviders();
  if (!response.ok) { const body = await response.json(); alert(body.error?.message || 'Model discovery failed'); }
  button.disabled = false; button.textContent = original;
}
$('#refresh-all-models').addEventListener('click', async event => {
  const button = event.currentTarget; button.disabled = true; button.textContent = 'Refreshing…';
  const responses = await Promise.all(providers.map(provider => fetch(`/admin/providers/${provider.id}/models/refresh`, {method: 'POST'})));
  await loadProviders(); button.disabled = false; button.textContent = 'Refresh models';
  const failures = responses.filter(response => !response.ok).length;
  if (failures) alert(`${failures} provider${failures === 1 ? '' : 's'} could not refresh models. See the status below.`);
});

function renderRoutes() {
  const routes = providers.flatMap(provider => provider.model_routes.map(route => ({provider, route})));
  $('#routes-empty').hidden = routes.length > 0; $('#routes-table').hidden = routes.length === 0;
  $('#routes').replaceChildren(...routes.map(({provider, route}) => {
    const endpoint = provider.endpoints.find(item => item.id === route.endpoint_id); const key = endpoint?.api_keys.find(item => item.id === route.api_key_id);
    const row = document.createElement('tr'); row.innerHTML = `<td>${escapeHtml(provider.name)}</td><td><code>${escapeHtml(route.pattern)}</code></td><td>${escapeHtml(endpoint?.id || 'Missing')}</td><td>${escapeHtml(key?.name || 'Missing')}</td><td><button class="delete-route text-link" data-provider="${provider.id}" data-pattern="${encodeURIComponent(route.pattern)}">Delete</button></td>`; return row;
  }));
  $$('.delete-route').forEach(button => button.addEventListener('click', async () => { await fetch(`/admin/providers/${button.dataset.provider}/model-routes/${button.dataset.pattern}`, {method: 'DELETE'}); await loadProviders(); }));
  $('#model-reference').replaceChildren(...providers.map(provider => {
    const item = document.createElement('article'); item.className = 'model-provider';
    const status = provider.model_discovery_error ? `<span class="error-text">Discovery failed: ${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `${provider.discovered_models.length} models` : 'Discovery has not run yet';
    item.innerHTML = `<header><div><strong>${escapeHtml(provider.name)}</strong><small>${status}</small></div><div class="model-actions"><button class="text-link refresh-models" data-provider="${provider.id}">Refresh</button>${provider.discovered_models.length ? '<button class="text-link toggle-models">Browse models</button>' : ''}</div></header>${provider.discovered_models.length ? `<div class="model-browser" hidden><div class="model-filter"><span>⌕</span><input type="search" placeholder="Filter ${provider.discovered_models.length} models"></div><div class="model-list"></div></div>` : ''}`;
    if (provider.discovered_models.length) {
      const list = item.querySelector('.model-list');
      const render = query => { const matches = provider.discovered_models.filter(model => model.toLowerCase().includes(query.toLowerCase())); list.replaceChildren(...matches.slice(0, 100).map(model => { const code = document.createElement('code'); code.textContent = model; return code; })); const summary = document.createElement('small'); summary.className = 'model-result-count'; summary.textContent = matches.length > 100 ? `Showing 100 of ${matches.length} matches` : `${matches.length} match${matches.length === 1 ? '' : 'es'}`; list.append(summary); };
      render(''); item.querySelector('.model-filter input').addEventListener('input', event => render(event.target.value));
      item.querySelector('.toggle-models').addEventListener('click', event => { const browser = item.querySelector('.model-browser'); browser.hidden = !browser.hidden; event.currentTarget.textContent = browser.hidden ? 'Browse models' : 'Hide models'; if (!browser.hidden) item.querySelector('.model-filter input').focus(); });
    }
    return item;
  }));
  $$('#model-reference .refresh-models').forEach(button => button.addEventListener('click', () => refreshModels(button.dataset.provider, button)));
}

const gatewayKeyDialog = $('#gateway-key-dialog');
$('#open-gateway-key').addEventListener('click', () => {
  $('#gateway-key-form').reset(); $('#gateway-key-error').textContent = '';
  $('#gateway-key-providers').replaceChildren(...providers.map(provider => { const label = document.createElement('label'); label.className = 'provider-check'; label.innerHTML = `<input type="checkbox" name="provider_ids" value="${escapeHtml(provider.id)}"><span class="custom-check">✓</span><span><strong>${escapeHtml(provider.name)}</strong><small>${escapeHtml(provider.id)}</small></span>`; return label; }));
  gatewayKeyDialog.showModal();
});
$$('.close-gateway-key').forEach(button => button.addEventListener('click', () => gatewayKeyDialog.close()));
$('#auth-enabled').addEventListener('change', async event => {
  const response = await fetch('/admin/auth', {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({enabled: event.target.checked})});
  if (!response.ok) { event.target.checked = !event.target.checked; alert('Could not update authentication setting.'); }
  await loadAuth();
});
$('#gateway-key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const expiry = data.get('expires_at');
  const payload = {note: data.get('note'), expires_at: expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null, provider_ids: data.getAll('provider_ids')};
  const response = await fetch('/admin/auth/keys', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#gateway-key-error'));
  const created = await response.json(); gatewayKeyDialog.close(); $('#generated-key').value = created.secret; $('#generated-key-dialog').showModal(); await loadAuth();
});
$('#copy-generated-key').addEventListener('click', async () => {
  const button = $('#copy-generated-key');
  try {
    await navigator.clipboard.writeText($('#generated-key').value);
    button.classList.add('copied'); $('.copy-key-label').textContent = 'Copied'; $('#copy-key-status').textContent = 'API key copied to clipboard.';
  } catch {
    $('#generated-key').select();
    $('#copy-key-status').textContent = 'Clipboard access was blocked. The key is selected; press Ctrl+C or Command+C.';
  }
});
$('#close-generated-key').addEventListener('click', () => {
  $('#generated-key').value = ''; $('#copy-generated-key').classList.remove('copied'); $('.copy-key-label').textContent = 'Copy API key';
  $('#copy-key-status').textContent = 'Store it somewhere secure before closing this window.'; $('#generated-key-dialog').close();
});

function renderAuth() {
  $('#auth-enabled').checked = authSettings.enabled;
  $('.switch-status').textContent = authSettings.enabled ? 'Enabled' : 'Disabled';
  $('#gateway-keys-empty').hidden = authSettings.api_keys.length > 0; $('#gateway-keys-table').hidden = authSettings.api_keys.length === 0;
  $('#gateway-keys').replaceChildren(...authSettings.api_keys.map(key => { const row = document.createElement('tr'); const expiry = key.expires_at ? new Date(key.expires_at * 1000).toLocaleString() : 'Never'; const access = key.provider_ids.length ? key.provider_ids.join(', ') : 'All providers'; row.innerHTML = `<td><code>${escapeHtml(key.prefix)}</code></td><td>${escapeHtml(key.note || '—')}</td><td>${escapeHtml(access)}</td><td>${escapeHtml(expiry)}</td><td><button class="delete-gateway-key text-link" data-key="${key.id}">Delete</button></td>`; return row; }));
  $$('.delete-gateway-key').forEach(button => button.addEventListener('click', async () => { if (confirm('Delete this API key?')) { await fetch(`/admin/auth/keys/${button.dataset.key}`, {method: 'DELETE'}); await loadAuth(); } }));
}
async function loadAuth() { const response = await fetch('/admin/auth'); authSettings = await response.json(); renderAuth(); }

const searchItems = [
  {label: 'Providers', description: 'Manage LLM providers', view: 'providers'},
  {label: 'Model routing', description: 'Route model IDs to API keys', view: 'models'},
  {label: 'Provider models', description: 'Discover models from providers', view: 'models'},
  {label: 'API access', description: 'Authentication and gateway keys', view: 'access'},
  {label: 'Generate API key', description: 'Create a gateway credential', view: 'access', action: () => $('#open-gateway-key').click()}
];
function closeSearch() { $('#search-results').hidden = true; }
$('#settings-search').addEventListener('input', event => {
  const query = event.target.value.trim().toLowerCase(); const results = $('#search-results');
  if (!query) return closeSearch();
  const matches = [...searchItems, ...providers.map(provider => ({label: provider.name, description: `Provider · ${provider.id}`, view: 'providers', action: () => { selectedProviderId = provider.id; renderProviderPage(); }}))].filter(item => `${item.label} ${item.description}`.toLowerCase().includes(query));
  results.replaceChildren(...matches.map(item => { const button = document.createElement('button'); button.innerHTML = `<strong>${escapeHtml(item.label)}</strong><small>${escapeHtml(item.description)}</small>`; button.addEventListener('click', () => { showView(item.view); item.action?.(); $('#settings-search').value = ''; closeSearch(); }); return button; }));
  if (!matches.length) { const empty = document.createElement('span'); empty.className = 'search-empty'; empty.textContent = 'No settings found'; results.replaceChildren(empty); }
  results.hidden = false;
});
$('#settings-search').addEventListener('keydown', event => { if (event.key === 'Escape') { event.target.value = ''; closeSearch(); } if (event.key === 'Enter') $('#search-results button')?.click(); });
document.addEventListener('click', event => { if (!event.target.closest('.search')) closeSearch(); });

async function showApiError(response, target) { const body = await response.json(); target.textContent = body.error?.message || `Request failed (${response.status})`; }
function formatType(type) { return type === 'anthropic' ? 'Anthropic' : 'OpenAI compatible'; }
function escapeHtml(value) { const node = document.createElement('span'); node.textContent = String(value); return node.innerHTML; }
async function loadProviders() { const response = await fetch('/admin/providers'); providers = await response.json(); renderProviders(); }
loadProviders();
loadAuth();
