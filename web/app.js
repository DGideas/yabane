let providers = [];
let authSettings = {enabled: true, api_keys: []};
let modelRoutes = [];
const $ = selector => document.querySelector(selector);
const $$ = selector => [...document.querySelectorAll(selector)];
const providerDialog = $('#provider-dialog');
const providerForm = $('#provider-form');
let providerStep = 1;
let providerIdEdited = false;
let selectedProviderId = null;
let adminSession = null;
let turnstileWidgetId = null;

async function renderTurnstile(action) {
  const widget = $('#turnstile-widget');
  $('#login-form [name="turnstile_token"]').value = '';
  if (action !== 'login') { widget.hidden = true; return; }
  const response = await fetch('/admin/turnstile-config');
  if (!response.ok) return showApiError(response, $('#login-error'));
  const {enabled, site_key: sitekey} = await response.json();
  widget.hidden = !enabled;
  if (!enabled) return;
  if (!window.turnstile && !document.querySelector('script[data-turnstile]')) {
    const script = document.createElement('script');
    script.src = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit';
    script.async = true; script.defer = true; script.dataset.turnstile = '';
    document.head.append(script);
  }
  const attempt = () => {
    if (!window.turnstile) return setTimeout(attempt, 100);
    if (turnstileWidgetId !== null) turnstile.remove(turnstileWidgetId);
    turnstileWidgetId = turnstile.render(widget, {sitekey, action, callback: token => { $('#login-form [name="turnstile_token"]').value = token; }, 'expired-callback': () => { $('#login-form [name="turnstile_token"]').value = ''; }});
  };
  attempt();
}

async function initializeAdmin() {
  const response = await fetch('/admin/session'); adminSession = await response.json();
  $('#login-screen').hidden = adminSession.authenticated; $('#admin-app').hidden = !adminSession.authenticated;
  if (!adminSession.authenticated) {
    const setup = !adminSession.configured;
    $('#login-title').textContent = setup ? 'Set up Yabane' : 'Welcome back';
    $('#login-description').textContent = setup ? 'Create the first administrator account.' : 'Sign in to manage your Yabane gateway.';
    $('.setup-only').hidden = !setup; $('#login-form [name="email"]').required = setup;
    $('#login-form .identity-label').textContent = setup ? 'Username' : 'Username or email';
    $('.login-submit').textContent = setup ? 'Create administrator' : 'Sign in';
    renderTurnstile(setup ? 'setup' : 'login');
    bindSecretToggles($('#login-screen'));
    return;
  }
  const initial = (adminSession.username || 'A').slice(0, 1).toUpperCase();
  $('#account-menu').textContent = initial; $('.account-avatar-large').textContent = initial;
  $('#account-name').textContent = adminSession.username || 'Administrator'; $('#account-email').textContent = adminSession.email || '';
  await Promise.all([loadProviders(), loadAuth(), loadRoutes()]);
}

$('#login-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const setup = !adminSession.configured;
  const payload = {username: data.get('username'), email: setup ? data.get('email') : null, password: data.get('password'), turnstile_token: data.get('turnstile_token') || ''};
  const response = await fetch(setup ? '/admin/setup' : '/admin/login', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) { if (window.turnstile) turnstile.reset(); return showApiError(response, $('#login-error')); }
  await initializeAdmin();
});
const accountMenu = $('#account-menu');
const accountPopover = $('#account-popover');
function closeAccountMenu() { accountPopover.hidden = true; accountMenu.setAttribute('aria-expanded', 'false'); }
accountMenu.addEventListener('click', event => { event.stopPropagation(); const opening = accountPopover.hidden; accountPopover.hidden = !opening; accountMenu.setAttribute('aria-expanded', String(opening)); });
document.addEventListener('click', event => { if (!event.target.closest('.account-control')) closeAccountMenu(); });
document.addEventListener('keydown', event => { if (event.key === 'Escape') closeAccountMenu(); });
$('#logout').addEventListener('click', async () => { await fetch('/admin/logout', {method: 'POST'}); location.href = '/login'; });
const profileDialog = $('#profile-dialog');
$('#open-profile').addEventListener('click', () => { closeAccountMenu(); const form = $('#profile-form'); form.reset(); form.elements.username.value = adminSession.username || ''; form.elements.email.value = adminSession.email || ''; $('#profile-error').textContent = ''; bindSecretToggles(profileDialog); profileDialog.showModal(); });
$$('.close-profile').forEach(button => button.addEventListener('click', () => profileDialog.close()));
$('#profile-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target);
  const response = await fetch('/admin/profile', {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({username: data.get('username'), email: data.get('email'), current_password: data.get('current_password'), new_password: data.get('new_password')})});
  if (!response.ok) return showApiError(response, $('#profile-error'));
  profileDialog.close(); await initializeAdmin();
});

function slugify(value) {
  return value.normalize('NFKD').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}

const viewPaths = {home: '/home', providers: '/providers', models: '/model-routing', access: '/api-access', activity: '/activity', management: '/management-api'};
function showView(name, updateHistory = true) {
  $('#home-view').hidden = name !== 'home';
  $('#providers-view').hidden = name !== 'providers';
  $('#models-view').hidden = name !== 'models';
  $('#access-view').hidden = name !== 'access';
  $('#activity-view').hidden = name !== 'activity';
  $('#management-view').hidden = name !== 'management';
  $$('.nav[data-view]').forEach(item => item.classList.toggle('active', item.dataset.view === name));
  const active = $(`.nav[data-view="${name}"]`);
  const indicator = $('.nav-indicator');
  indicator.style.transform = `translateY(${active.offsetTop}px)`;
  selectedProviderId = name === 'providers' ? selectedProviderId : null;
  if (name === 'providers') renderProviderPage();
  if (name === 'home') loadDashboard();
  if (name === 'activity') loadActivity();
  if (name === 'management') loadManagementKeys();
  if (updateHistory) history.pushState({}, '', selectedProviderId && name === 'providers' ? `/providers/${encodeURIComponent(selectedProviderId)}` : viewPaths[name]);
  document.title = `${active.textContent.trim()} · Yabane`;
}
$$('.nav[data-view]').forEach(item => item.addEventListener('click', () => showView(item.dataset.view)));
function routeFromLocation() {
  const path = location.pathname;
  if (path.startsWith('/providers/')) { selectedProviderId = decodeURIComponent(path.slice('/providers/'.length)); showView('providers', false); }
  else if (path === '/home' || path === '/') showView('home', false);
  else if (path === '/model-routing') showView('models', false);
  else if (path === '/api-access') showView('access', false);
  else if (path === '/activity') showView('activity', false);
  else if (path === '/management-api') showView('management', false);
  else showView('home', path !== '/home');
}
window.addEventListener('popstate', routeFromLocation);
requestAnimationFrame(routeFromLocation);

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
      socks5_proxy: data.get('socks5_proxy') || null,
      extra_headers: {},
      extra_body: {},
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
    card.addEventListener('click', () => { selectedProviderId = provider.id; history.pushState({}, '', `/providers/${encodeURIComponent(provider.id)}`); renderProviderPage(); });
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
  const endpointHtml = provider.endpoints.map((endpoint, index) => {
    const endpointModels = Object.values(provider.model_endpoints).filter(ids => ids.includes(endpoint.id)).length;
    const enabledKeys = endpoint.api_keys.filter(key => key.enabled).length;
    return `<article class="endpoint-card"><header class="endpoint-head"><span class="endpoint-index">${index + 1}</span><div class="endpoint-identity"><div><h3>${escapeHtml(endpoint.id)}</h3><span class="kind">${formatType(endpoint.api_type)}</span></div><code>${escapeHtml(endpoint.base_url)}</code></div><div class="endpoint-facts"><span><strong>${endpointModels}</strong> models</span><span><strong>${enabledKeys}</strong> of ${endpoint.api_keys.length} keys enabled</span>${endpoint.socks5_proxy ? `<span>Proxy <code>${escapeHtml(endpoint.socks5_proxy)}</code></span>` : ''}</div></header><section class="endpoint-keys"><div class="endpoint-keys-head"><div><h4>Upstream API keys</h4><p>Credentials below belong only to <code>${escapeHtml(endpoint.id)}</code>. Enabled keys share this endpoint’s traffic by weight.</p></div><button class="button secondary add-key" data-provider="${provider.id}" data-endpoint="${endpoint.id}">＋ Add key to this endpoint</button></div><div class="key-list">${endpoint.api_keys.length ? endpoint.api_keys.map(key => `<div class="key-row"><span class="status ${key.enabled ? 'enabled' : ''}"></span><span class="key-name"><strong>${escapeHtml(key.name)}</strong><small>${key.enabled ? 'Enabled for traffic' : 'Disabled'}</small></span><label class="inline-weight"><small>Relative weight</small><input class="weight" type="range" min="1" max="300" value="${key.weight}" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-key="${key.id}"><output>${key.weight}</output></label><button class="key-toggle text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-key="${key.id}" data-enabled="${key.enabled}">${key.enabled ? 'Disable' : 'Enable'}</button></div>`).join('') : `<div class="endpoint-key-empty"><p>No API keys belong to this endpoint yet.</p><button class="text-link add-key" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Add the first key</button></div>`}</div></section></article>`;
  }).join('');
  const discovery = provider.model_discovery_error ? `<span class="error-text">${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `${provider.discovered_models.length} models discovered` : 'Models have not been discovered yet';
  const headerCount = Object.keys(provider.extra_headers || {}).length;
  const bodyCount = Object.keys(provider.extra_body || {}).length;
  const defaultsScope = provider.defaults_endpoint_ids?.length ? `${provider.defaults_endpoint_ids.length} selected endpoint${provider.defaults_endpoint_ids.length === 1 ? '' : 's'}` : 'All endpoints';
  $('#provider-detail').innerHTML = `<nav class="provider-breadcrumb" aria-label="Breadcrumb"><button id="back-to-providers">Providers</button><span>›</span><strong>${escapeHtml(provider.name)}</strong></nav><header class="provider-hero"><div class="provider-hero-mark">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</div><div class="provider-hero-main"><span class="provider-eyebrow">Provider settings</span><h1>${escapeHtml(provider.name)}</h1><p>Requests use <code>${escapeHtml(provider.id)}/model-id</code>. This provider contains ${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} and ${keyCount(provider)} upstream key${keyCount(provider) === 1 ? '' : 's'}.</p></div><button class="delete-provider button danger" data-provider="${provider.id}">Delete provider</button></header><div class="provider-overview"><section class="card model-summary-card"><div class="card-head"><div><span class="section-kicker">Provider-wide</span><h2>Discovered models</h2><p>${discovery}</p></div><div><button class="text-link browse-provider-models">Browse models</button><button class="button secondary refresh-models" data-provider="${provider.id}">Refresh</button></div></div><div class="provider-model-browser" hidden><div class="model-filter"><span>⌕</span><input type="search" placeholder="Filter ${provider.discovered_models.length} models"></div><div class="model-table"></div></div></section><section class="card defaults-card"><div class="card-head"><div><span class="section-kicker">Applies to ${escapeHtml(defaultsScope)}</span><h2>Request defaults</h2><p>Choose all endpoints or a specific group; Endpoint values still override matching defaults.</p></div><button class="button secondary edit-provider-options">Configure</button></div><div class="request-defaults-summary"><div><span class="defaults-count">${headerCount}</span><span><strong>Headers</strong><small>${headerCount ? 'Applied provider-wide' : 'Not configured'}</small></span></div><div><span class="defaults-count">${bodyCount}</span><span><strong>Body fields</strong><small>${bodyCount ? 'Applied provider-wide' : 'Not configured'}</small></span></div></div></section></div><section class="endpoint-group"><div class="endpoint-group-head"><div><span class="section-kicker">Provider children</span><h2>API endpoints</h2><p>Each endpoint is an upstream connection. API keys are configured inside the endpoint they belong to.</p></div><button class="button primary add-endpoint" data-provider="${provider.id}">＋ Add endpoint</button></div><div class="endpoint-stack">${endpointHtml || '<div class="empty endpoint-empty"><h3>No endpoints</h3><p>Add an upstream API endpoint to start routing requests.</p></div>'}</div></section>`;
  const browse = $('#provider-detail .browse-provider-models');
  if (!provider.discovered_models.length) browse.disabled = true;
  const renderModels = query => {
    const matches = provider.discovered_models.filter(model => model.toLowerCase().includes(query.toLowerCase()));
    $('#provider-detail .model-table').replaceChildren(...matches.slice(0, 100).map(model => { const row = document.createElement('div'); row.innerHTML = `<code>${escapeHtml(model)}</code><span>${escapeHtml((provider.model_endpoints[model] || []).join(', '))}</span>`; return row; }));
  };
  browse.addEventListener('click', () => { const browser = $('#provider-detail .provider-model-browser'); browser.hidden = !browser.hidden; browse.textContent = browser.hidden ? 'Browse models' : 'Hide models'; if (!browser.hidden) { renderModels(''); browser.querySelector('input').focus(); } });
  $('#provider-detail .model-filter input').addEventListener('input', event => renderModels(event.target.value));
  $('#provider-detail .edit-provider-options').addEventListener('click', () => openRequestDefaultsDialog(provider));
  $('#provider-detail #back-to-providers').addEventListener('click', () => { selectedProviderId = null; history.pushState({}, '', '/providers'); renderProviderPage(); });
  bindProviderActions();
}

function bindProviderActions() {
  $$('.delete-provider').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    if (confirm(`Delete ${provider.name}?`)) { await fetch(`/admin/providers/${provider.id}`, {method: 'DELETE'}); selectedProviderId = null; await loadProviders(); }
  }));
  $$('.add-key').forEach(button => button.addEventListener('click', () => openKeyDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.key-toggle').forEach(button => button.addEventListener('click', async () => {
    await patchKey(button.dataset.provider, button.dataset.endpoint, button.dataset.key, {enabled: button.dataset.enabled !== 'true'});
  }));
  $$('.weight').forEach(input => {
    input.addEventListener('input', () => { const output = input.parentElement.querySelector('output'); if (output) output.textContent = input.value; });
    input.addEventListener('change', async () => {
      if (input.reportValidity()) await patchKey(input.dataset.provider, input.dataset.endpoint, input.dataset.key, {weight: Number(input.value)});
    });
  });
  $$('.refresh-models').forEach(button => button.addEventListener('click', () => refreshModels(button.dataset.provider, button)));
  $$('.add-endpoint').forEach(button => button.addEventListener('click', () => openEndpointDialog(button.dataset.provider)));
}

async function patchKey(providerId, endpointId, keyId, update) {
  await fetch(`/admin/providers/${providerId}/endpoints/${endpointId}/keys/${keyId}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(update)});
  await loadProviders();
}

const endpointDialog = $('#endpoint-dialog');
function openEndpointDialog(providerId) {
  const form = $('#endpoint-form'); form.reset(); form.elements.provider_id.value = providerId; $('#endpoint-error').textContent = '';
  toggleEndpointKeyRequirement(); bindSecretToggles(endpointDialog); endpointDialog.showModal();
}
function toggleEndpointKeyRequirement() {
  const required = $('#endpoint-form [name="requires_api_key"]').checked;
  $('.endpoint-key-section').hidden = !required; $('#endpoint-form [name="api_key"]').required = required;
}
$('#endpoint-form [name="requires_api_key"]').addEventListener('change', toggleEndpointKeyRequirement);
$$('.close-endpoint').forEach(button => button.addEventListener('click', () => endpointDialog.close()));
$('#endpoint-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const providerId = data.get('provider_id');
  const payload = {id: data.get('id'), api_type: data.get('api_type'), base_url: data.get('base_url'), socks5_proxy: data.get('socks5_proxy') || null, extra_headers: {}, extra_body: {}, requires_api_key: data.get('requires_api_key') === 'on', api_key: data.get('requires_api_key') === 'on' ? data.get('api_key') : null};
  const response = await fetch(`/admin/providers/${providerId}/endpoints`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#endpoint-error'));
  endpointDialog.close(); await refreshModels(providerId, document.createElement('button')); await loadProviders();
});

const keyDialog = $('#key-dialog');
function openKeyDialog(providerId, endpointId = null) {
  const provider = providers.find(item => item.id === providerId);
  $('#key-form').reset();
  $('#key-form [name="provider_id"]').value = providerId;
  $('#key-form [name="secret"]').type = 'password';
  $('#key-form .toggle-key').textContent = 'Show';
  setNewKeyWeight(100);
  $('#key-endpoint').replaceChildren(...provider.endpoints.map(endpoint => new Option(`${endpoint.id} · ${formatType(endpoint.api_type)}`, endpoint.id)));
  if (endpointId) $('#key-endpoint').value = endpointId;
  $('#key-dialog-title').textContent = endpointId ? `Add key to ${endpointId}` : 'Add API key';
  $('#key-dialog-description').textContent = `Add an upstream credential under ${provider.name}${endpointId ? ` › ${endpointId}` : ''}.`;
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

const requestDefaultsDialog = $('#request-defaults-dialog');
let requestDefaultsProviderId = null;

function addDefaultsRow(container, kind, name = '', value = '') {
  const row = document.createElement('div');
  row.className = 'defaults-row';
  row.innerHTML = `<label><span>${kind === 'header' ? 'Header name' : 'Field name'}</span><input class="technical-input defaults-name" placeholder="${kind === 'header' ? 'x-api-version' : 'temperature'}"></label><label><span>${kind === 'header' ? 'Header value' : 'JSON value'}</span>${kind === 'header' ? '<input class="technical-input defaults-value" placeholder="2025-01-01">' : '<textarea class="technical-input defaults-value" rows="2" placeholder="0.7"></textarea>'}</label><button type="button" class="icon-button remove-default-row" aria-label="Remove">×</button><span class="row-error"></span>`;
  row.dataset.kind = kind;
  row.querySelector('.defaults-name').value = name;
  row.querySelector('.defaults-value').value = value;
  row.querySelector('.remove-default-row').addEventListener('click', () => { row.remove(); validateRequestDefaults(); });
  row.querySelectorAll('input, textarea').forEach(input => input.addEventListener('input', validateRequestDefaults));
  container.append(row);
  return row;
}

function collectDefaultsRows(container, kind) {
  const result = {}; let valid = true;
  container.querySelectorAll('.defaults-row').forEach(row => {
    const nameInput = row.querySelector('.defaults-name'); const valueInput = row.querySelector('.defaults-value'); const error = row.querySelector('.row-error');
    const name = nameInput.value.trim(); const rawValue = valueInput.value.trim(); error.textContent = '';
    if (!name && !rawValue) return;
    if (!name) { error.textContent = `${kind === 'header' ? 'Header' : 'Field'} name is required.`; valid = false; return; }
    if (kind === 'header') {
      const managedHeaders = ['host', 'authorization', 'x-api-key', 'content-length', 'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade'];
      const normalizedName = name.toLowerCase();
      if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)) { error.textContent = 'Use a valid HTTP header name.'; valid = false; return; }
      if (Object.hasOwn(result, normalizedName)) { error.textContent = `Duplicate header “${name}”.`; valid = false; return; }
      if (managedHeaders.includes(normalizedName)) { error.textContent = 'This header is managed by Yabane and cannot be overridden.'; valid = false; return; }
      if (!rawValue) { error.textContent = 'Header value is required.'; valid = false; return; }
      if (/[^\t\x20-\x7e\x80-\xff]/.test(rawValue)) { error.textContent = 'Header value contains unsupported control characters.'; valid = false; return; }
      result[normalizedName] = rawValue;
    } else {
      if (Object.hasOwn(result, name)) { error.textContent = `Duplicate field “${name}”.`; valid = false; return; }
      if (!rawValue) { error.textContent = 'JSON value is required.'; valid = false; return; }
      try { result[name] = JSON.parse(rawValue); } catch { error.textContent = 'Enter a valid JSON value, for example true, 0.7, "text", [] or {}.'; valid = false; }
    }
  });
  return {valid, value: result};
}

function validateRequestDefaults() {
  const headers = collectDefaultsRows($('#default-header-rows'), 'header');
  const body = collectDefaultsRows($('#default-body-rows'), 'body');
  const selectedScope = $('#request-defaults-form [name="defaults_scope"]:checked')?.value || 'all';
  const selectedEndpoints = $$('#defaults-endpoint-selection input:checked').map(input => input.value);
  const scopeValid = selectedScope === 'all' || selectedEndpoints.length > 0;
  const valid = headers.valid && body.valid && scopeValid;
  $('#default-headers-empty').hidden = $('#default-header-rows').children.length > 0;
  $('#default-body-empty').hidden = $('#default-body-rows').children.length > 0;
  $('#defaults-json-preview').textContent = JSON.stringify(body.value, null, 2);
  $('#defaults-validation-status').textContent = valid ? 'Valid configuration' : selectedScope === 'selected' && !selectedEndpoints.length ? 'Select at least one endpoint' : 'Fix validation errors';
  $('#defaults-validation-status').classList.toggle('invalid', !valid);
  $('#request-defaults-form button[type="submit"]').disabled = !valid;
  return {valid, headers: headers.value, body: body.value, endpointIds: selectedScope === 'all' ? [] : selectedEndpoints};
}

function openRequestDefaultsDialog(provider) {
  requestDefaultsProviderId = provider.id;
  $('#request-defaults-description').textContent = `Configure values added to every request sent through ${provider.name}.`;
  $('#request-defaults-error').textContent = '';
  $('#default-header-rows').replaceChildren(); $('#default-body-rows').replaceChildren();
  const selectedIds = provider.defaults_endpoint_ids || [];
  const selectedScope = selectedIds.length ? 'selected' : 'all';
  $(`#request-defaults-form [name="defaults_scope"][value="${selectedScope}"]`).checked = true;
  $('#defaults-endpoint-selection').replaceChildren(...provider.endpoints.map(endpoint => { const label = document.createElement('label'); label.className = 'provider-check'; label.innerHTML = `<input type="checkbox" value="${escapeHtml(endpoint.id)}" ${selectedIds.includes(endpoint.id) ? 'checked' : ''}><span class="custom-check">✓</span><span><strong>${escapeHtml(endpoint.id)}</strong><small>${escapeHtml(formatType(endpoint.api_type))}</small></span>`; label.querySelector('input').addEventListener('change', validateRequestDefaults); return label; }));
  $('#defaults-endpoint-selection').hidden = selectedScope !== 'selected';
  Object.entries(provider.extra_headers || {}).forEach(([name, value]) => addDefaultsRow($('#default-header-rows'), 'header', name, value));
  Object.entries(provider.extra_body || {}).forEach(([name, value]) => addDefaultsRow($('#default-body-rows'), 'body', name, JSON.stringify(value, null, 2)));
  validateRequestDefaults(); requestDefaultsDialog.showModal();
}
$$('#request-defaults-form [name="defaults_scope"]').forEach(input => input.addEventListener('change', () => { $('#defaults-endpoint-selection').hidden = input.value !== 'selected' || !input.checked; validateRequestDefaults(); }));
$('.add-default-header').addEventListener('click', () => addDefaultsRow($('#default-header-rows'), 'header').querySelector('.defaults-name').focus());
$('.add-default-body-field').addEventListener('click', () => addDefaultsRow($('#default-body-rows'), 'body').querySelector('.defaults-name').focus());
$$('.close-request-defaults').forEach(button => button.addEventListener('click', () => requestDefaultsDialog.close()));
$('#request-defaults-form').addEventListener('submit', async event => {
  event.preventDefault(); const defaults = validateRequestDefaults(); if (!defaults.valid) return;
  const response = await fetch(`/admin/providers/${requestDefaultsProviderId}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({extra_headers: defaults.headers, extra_body: defaults.body, defaults_endpoint_ids: defaults.endpointIds})});
  if (!response.ok) return showApiError(response, $('#request-defaults-error'));
  requestDefaultsDialog.close(); await loadProviders();
});

const routeDialog = $('#route-dialog');
function routeTargetOptions() { return providers.flatMap(provider => provider.endpoints.flatMap(endpoint => endpoint.api_keys.filter(key => key.enabled).map(key => { const option = new Option(`${provider.name} · ${endpoint.id} · ${key.name}`, `${provider.id}\n${endpoint.id}\n${key.id}`); option.dataset.provider = provider.id; option.dataset.endpoint = endpoint.id; return option; }))); }
function initializeRouteTarget(editor) {
  const select = editor.querySelector('.route-target');
  const existingValue = select.value;
  select.replaceChildren(...routeTargetOptions());
  if (existingValue && [...select.options].some(option => option.value === existingValue)) select.value = existingValue;
  const updateSuggestion = () => {
    const option = select.selectedOptions[0];
    const provider = providers.find(item => item.id === option?.dataset.provider);
    const endpointId = option?.dataset.endpoint;
    const models = provider?.discovered_models.filter(model => (provider.model_endpoints[model] || []).includes(endpointId)) || [];
    const input = editor.querySelector('.upstream-model-input');
    input.placeholder = models[0] ? `e.g. ${models[0]}` : 'e.g. model-name or org/model-name';
  };
  select.addEventListener('change', updateSuggestion); updateSuggestion();
}
function openRouteDialog() {
  $('#route-form').reset(); $('#route-error').textContent = '';
  $$('#route-targets .route-target-editor').slice(1).forEach(editor => editor.remove()); initializeRouteTarget($('#route-targets .route-target-editor'));
  const hasDestinations = $('#route-targets .route-target').options.length > 0;
  $('#route-error').textContent = hasDestinations ? '' : 'Add and enable an upstream API key before creating a route.';
  $('#save-route').disabled = !hasDestinations;
  routeDialog.showModal();
}
$('#models-view').addEventListener('click', event => {
  if (event.target.closest('#open-route, #empty-add-route')) openRouteDialog();
});
$('#add-route-target').addEventListener('click', () => { const editor = $('#route-targets .route-target-editor').cloneNode(true); editor.querySelector('[name="upstream_model"]').value = ''; editor.querySelector('[name="target_weight"]').value = 100; $('#route-targets').append(editor); initializeRouteTarget(editor); });
$$('.close-route').forEach(button => button.addEventListener('click', () => routeDialog.close()));
$('#route-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const targets = [...event.target.querySelectorAll('.route-target-editor')].map(editor => { const [provider_id, endpoint_id, api_key_id] = editor.querySelector('.route-target').value.split('\n'); return {provider_id, endpoint_id, api_key_id, upstream_model: editor.querySelector('[name="upstream_model"]').value, weight: Number(editor.querySelector('[name="target_weight"]').value)}; });
  const response = await fetch('/admin/routes', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({pattern: data.get('pattern'), targets})});
  if (!response.ok) return showApiError(response, $('#route-error'));
  routeDialog.close(); await loadRoutes();
});

async function refreshModels(providerId, button) {
  const original = button.textContent || ''; button.disabled = true; button.textContent = 'Refreshing…';
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
  $('#routes-empty').hidden = modelRoutes.length > 0; $('#routes-table').hidden = modelRoutes.length === 0;
  $('#routes').replaceChildren(...modelRoutes.map(route => { const row = document.createElement('tr'); const targets = route.targets.map(target => `${target.provider_id}/${target.upstream_model} · ${target.endpoint_id} · weight ${target.weight}`).join('<br>'); row.innerHTML = `<td><code>${escapeHtml(route.pattern)}</code></td><td>${targets}</td><td><button class="delete-route text-link" data-pattern="${encodeURIComponent(route.pattern)}">Delete</button></td>`; return row; }));
  $$('.delete-route').forEach(button => button.addEventListener('click', async () => { await fetch(`/admin/routes/${button.dataset.pattern}`, {method: 'DELETE'}); await loadRoutes(); }));
  renderModelReference();
}

function renderModelReference() {
  $('#model-reference').replaceChildren(...providers.map(provider => {
    const item = document.createElement('article'); item.className = 'model-provider';
    const status = provider.model_discovery_error ? `<span class="error-text">Discovery failed: ${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `${provider.discovered_models.length} models` : 'Discovery has not run yet';
    item.innerHTML = `<header><div><strong>${escapeHtml(provider.name)}</strong><small>${status}</small></div><div class="model-actions"><button class="text-link refresh-models" data-provider="${provider.id}">Refresh</button>${provider.discovered_models.length ? '<button class="text-link toggle-models">Browse models</button>' : ''}</div></header>${provider.discovered_models.length ? `<div class="model-browser" hidden><div class="model-filter"><span>⌕</span><input type="search" placeholder="Filter ${provider.discovered_models.length} models"></div><div class="model-list"></div></div>` : ''}`;
    if (provider.discovered_models.length) { const list = item.querySelector('.model-list'); const render = query => { const matches = provider.discovered_models.filter(model => model.toLowerCase().includes(query.toLowerCase())); list.replaceChildren(...matches.slice(0, 100).map(model => { const code = document.createElement('code'); code.textContent = model; return code; })); }; render(''); item.querySelector('.model-filter input').addEventListener('input', event => render(event.target.value)); item.querySelector('.toggle-models').addEventListener('click', event => { const browser = item.querySelector('.model-browser'); browser.hidden = !browser.hidden; event.currentTarget.textContent = browser.hidden ? 'Browse models' : 'Hide models'; }); }
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
  $('#gateway-keys').replaceChildren(...authSettings.api_keys.map(key => { const row = document.createElement('tr'); const expiry = key.expires_at ? new Date(key.expires_at * 1000).toLocaleString() : 'Never'; const access = key.provider_ids.length ? key.provider_ids.join(', ') : 'All providers'; row.innerHTML = `<td><div class="listed-key"><code>${escapeHtml(key.secret || key.prefix)}</code>${key.secret ? `<button class="icon-copy-key" data-secret="${escapeHtml(key.secret)}" title="Copy API key" aria-label="Copy API key">${copyIcon()}</button>` : ''}</div></td><td>${escapeHtml(key.note || '—')}</td><td>${escapeHtml(access)}</td><td>${escapeHtml(expiry)}</td><td><button class="delete-gateway-key text-link" data-key="${key.id}">Delete</button></td>`; return row; }));
  $$('.icon-copy-key').forEach(button => button.addEventListener('click', async () => { await navigator.clipboard.writeText(button.dataset.secret); button.classList.add('copied'); setTimeout(() => button.classList.remove('copied'), 1200); }));
  $$('.delete-gateway-key').forEach(button => button.addEventListener('click', async () => { if (confirm('Delete this API key?')) { await fetch(`/admin/auth/keys/${button.dataset.key}`, {method: 'DELETE'}); await loadAuth(); } }));
}
async function loadAuth() { const response = await fetch('/admin/auth'); authSettings = await response.json(); renderAuth(); }

const managementKeyDialog = $('#management-key-dialog');
$('#open-management-key').addEventListener('click', () => { $('#management-key-form').reset(); $('#management-key-error').textContent = ''; managementKeyDialog.showModal(); });
$$('.close-management-key').forEach(button => button.addEventListener('click', () => managementKeyDialog.close()));
$('#management-key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const expiry = data.get('expires_at');
  const response = await fetch('/admin/management-keys', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({name: data.get('name'), expires_at: expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null})});
  if (!response.ok) return showApiError(response, $('#management-key-error'));
  const created = await response.json(); managementKeyDialog.close(); $('#generated-key').value = created.secret; $('#generated-key-dialog').showModal(); await loadManagementKeys();
});
function renderManagementKeys(keys) {
  $('#management-keys-empty').hidden = keys.length > 0; $('#management-keys-table').hidden = keys.length === 0;
  $('#management-keys').replaceChildren(...keys.map(key => { const row = document.createElement('tr'); row.innerHTML = `<td><code>${escapeHtml(key.prefix)}</code></td><td>${escapeHtml(key.name)}</td><td>${new Date(key.created_at * 1000).toLocaleString()}</td><td>${key.last_used_at ? new Date(key.last_used_at * 1000).toLocaleString() : 'Never'}</td><td>${key.expires_at ? new Date(key.expires_at * 1000).toLocaleString() : 'Never'}</td><td><button class="delete-management-key text-link" data-key="${key.id}">Revoke</button></td>`; return row; }));
  $$('.delete-management-key').forEach(button => button.addEventListener('click', async () => { if (confirm('Revoke this Management API key?')) { await fetch(`/admin/management-keys/${button.dataset.key}`, {method: 'DELETE'}); await loadManagementKeys(); } }));
}
async function loadManagementKeys() { const response = await fetch('/admin/management-keys'); renderManagementKeys(await response.json()); }

const searchItems = [
  {label: 'Home', description: 'Gateway status and usage overview', view: 'home'},
  {label: 'Providers', description: 'Manage LLM providers', view: 'providers'},
  {label: 'Model routing', description: 'Route model IDs to API keys', view: 'models'},
  {label: 'Provider models', description: 'Discover models from providers', view: 'models'},
  {label: 'API access', description: 'Authentication and gateway keys', view: 'access'},
  {label: 'Generate Gateway API key', description: 'Create an inference credential', view: 'access', action: () => $('#open-gateway-key').click()},
  {label: 'Activity', description: 'Requests, tokens, latency, upstream cost, and routing logs', view: 'activity'},
  {label: 'Management API', description: 'Programmatic control keys and live docs', view: 'management'},
  {label: 'Create Management API key', description: 'Create a control-plane credential', view: 'management', action: () => $('#open-management-key').click()},
  {label: 'Live API docs', description: 'Interactive OpenAPI documentation', view: 'management', action: () => location.assign('/docs')}
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

function copyIcon() { return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="9" y="9" width="10" height="10" rx="2"></rect><path d="M15 9V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h2"></path></svg>'; }

async function showApiError(response, target) { const body = await response.json(); target.textContent = body.error?.message || `Request failed (${response.status})`; }
function compactNumber(value) { return Intl.NumberFormat('en', {notation: 'compact', maximumFractionDigits: 1}).format(value || 0); }
function formatCost(value) { return value == null ? '—' : `$${new Intl.NumberFormat('en', {minimumFractionDigits: 2, maximumFractionDigits: 6}).format(value)}`; }
let activityLogs = [];
const activityColors = ['#0b57d0', '#7c4dff', '#00a67e', '#ff8f00', '#d93025', '#00897b'];
function activityBuckets(logs, seconds) {
  const count = seconds <= 3600 ? 12 : seconds <= 86400 ? 24 : seconds <= 604800 ? 14 : 30;
  const end = Math.floor(Date.now() / 1000); const start = end - seconds; const width = seconds / count;
  return Array.from({length: count}, (_, index) => ({start: start + index * width, requests: 0, tokens: 0, cached: 0, cost: 0, latency: 0, samples: 0})).map((bucket, index, buckets) => {
    logs.filter(log => log.timestamp >= bucket.start && (index === buckets.length - 1 || log.timestamp < bucket.start + width)).forEach(log => { bucket.requests++; bucket.tokens += log.input_tokens + log.output_tokens; bucket.cached += log.cached_tokens; bucket.cost += log.cost || 0; bucket.latency += log.latency_ms; bucket.samples++; }); return bucket;
  });
}
function sparkline(values, color) {
  const max = Math.max(...values, 1); const points = values.map((value, index) => `${index * 100 / Math.max(values.length - 1, 1)},${30 - value * 26 / max}`).join(' ');
  return `<svg viewBox="0 0 100 32" preserveAspectRatio="none" aria-hidden="true"><polyline points="${points}" fill="none" stroke="${color}" stroke-width="2" vector-effect="non-scaling-stroke"/></svg>`;
}
function renderActivityChart(buckets, seconds) {
  const maxRequests = Math.max(...buckets.map(bucket => bucket.requests), 1); const maxTokens = Math.max(...buckets.map(bucket => bucket.tokens), 1);
  $('#activity-chart').innerHTML = `<div class="chart-grid"><span></span><span></span><span></span><span></span></div><div class="chart-bars">${buckets.map((bucket, index) => { const date = new Date(bucket.start * 1000); const label = seconds <= 86400 ? date.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : date.toLocaleDateString([], {month: 'short', day: 'numeric'}); return `<div class="chart-column" title="${bucket.requests} requests · ${compactNumber(bucket.tokens)} tokens · ${formatCost(bucket.cost)}"><div class="chart-bars-pair"><i style="height:${Math.max(bucket.requests * 100 / maxRequests, bucket.requests ? 4 : 0)}%"></i><b style="height:${Math.max(bucket.tokens * 100 / maxTokens, bucket.tokens ? 4 : 0)}%"></b></div><small>${index % Math.ceil(buckets.length / 7) === 0 ? label : ''}</small></div>`; }).join('')}</div>`;
}
function aggregateActivity(logs, key) {
  const map = new Map(); logs.forEach(log => { const name = log[key]; const item = map.get(name) || {name, requests: 0, tokens: 0, errors: 0, latency: 0}; item.requests++; item.tokens += log.input_tokens + log.output_tokens; item.errors += Number(log.status >= 400); item.latency += log.latency_ms; map.set(name, item); }); return [...map.values()].sort((a, b) => b.requests - a.requests);
}
function renderRankings(target, items) {
  const max = Math.max(...items.map(item => item.requests), 1); target.innerHTML = items.length ? items.slice(0, 5).map((item, index) => `<div class="activity-ranking"><span class="ranking-number">${index + 1}</span><span class="ranking-dot" style="background:${activityColors[index % activityColors.length]}"></span><div><strong>${escapeHtml(item.name)}</strong><span class="ranking-track"><i style="width:${item.requests * 100 / max}%;background:${activityColors[index % activityColors.length]}"></i></span></div><span><strong>${item.requests}</strong><small>requests</small></span><span><strong>${compactNumber(item.tokens)}</strong><small>tokens</small></span></div>`).join('') : '<div class="activity-empty">No activity in this period.</div>';
}
function statusBadge(status) { const success = status >= 200 && status < 400; return `<span class="status-badge ${success ? 'success' : 'failure'}"><i></i>${status}</span>`; }
function compactPath(path) { return path.replace('/v1/', '').replace('chat/completions', 'Chat').replace('responses', 'Responses').replace('messages', 'Messages'); }
function activityRow(log, detailed = false) {
  const tokens = log.input_tokens + log.output_tokens; const time = new Date(log.timestamp * 1000);
  if (detailed) return `<tr><td>${time.toLocaleString()}</td><td><code>${escapeHtml(log.request_id)}</code></td><td><code>${escapeHtml(log.model)}</code></td><td>${escapeHtml(log.provider)}</td><td>${escapeHtml(log.endpoint)}</td><td><span class="api-kind">${escapeHtml(compactPath(log.path))}${log.streaming ? ' · stream' : ''}</span></td><td>${statusBadge(log.status)}</td><td>${log.latency_ms.toLocaleString()} ms</td><td>${compactNumber(log.input_tokens)}</td><td>${compactNumber(log.output_tokens)}</td><td>${compactNumber(log.cached_tokens)}</td><td>${formatCost(log.cost)}</td></tr>`;
  return `<tr><td><span class="activity-time">${time.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'})}<small>${time.toLocaleDateString()}</small></span></td><td><code>${escapeHtml(log.model)}</code></td><td><span class="route-cell"><strong>${escapeHtml(log.provider)}</strong><small>${escapeHtml(log.endpoint)} · ${escapeHtml(compactPath(log.path))}</small></span></td><td>${statusBadge(log.status)}</td><td>${log.latency_ms.toLocaleString()} ms</td><td><strong>${compactNumber(tokens)}</strong><small class="token-detail">${compactNumber(log.input_tokens)} in · ${compactNumber(log.output_tokens)} out</small></td></tr>`;
}
function renderActivityLogs() {
  const query = $('#activity-search').value.trim().toLowerCase(); const status = $('#activity-status-filter').value;
  const filtered = activityLogs.filter(log => (!query || `${log.request_id} ${log.model} ${log.provider} ${log.endpoint}`.toLowerCase().includes(query)) && (!status || (status === 'success' ? log.status < 400 : log.status >= 400)));
  $('#activity-logs').innerHTML = filtered.length ? filtered.map(log => activityRow(log, true)).join('') : '<tr><td colspan="12"><div class="activity-empty">No requests match these filters.</div></td></tr>';
}
async function loadActivity() {
  const seconds = Number($('#activity-range').value); const since = Math.floor(Date.now() / 1000) - seconds;
  const [stats, logs] = await Promise.all([fetch(`/admin/activity/stats?since=${since}`).then(response => response.json()), fetch(`/admin/activity/logs?since=${since}&limit=1000`).then(response => response.json())]);
  const providerFilter = $('#activity-provider-filter').value; activityLogs = providerFilter ? logs.filter(log => log.provider === providerFilter) : logs;
  const totals = activityLogs.reduce((total, log) => { total.input += log.input_tokens; total.output += log.output_tokens; total.cached += log.cached_tokens; total.cost += log.cost || 0; total.latency += log.latency_ms; total.success += Number(log.status < 400); total.streaming += Number(log.streaming); return total; }, {input: 0, output: 0, cached: 0, cost: 0, latency: 0, success: 0, streaming: 0});
  const buckets = activityBuckets(activityLogs, seconds); const requests = activityLogs.length; const totalTokens = totals.input + totals.output;
  $('#stat-requests').textContent = compactNumber(requests); $('#stat-input').textContent = compactNumber(totals.input); $('#stat-output').textContent = compactNumber(totals.output); $('#stat-cached').textContent = compactNumber(totals.cached); $('#stat-total-tokens').textContent = compactNumber(totalTokens);
  $('#stat-success-rate').textContent = `${requests ? (totals.success * 100 / requests).toFixed(1) : '0.0'}% successful`; $('#stat-cache-rate').textContent = `${totals.input ? (totals.cached * 100 / totals.input).toFixed(1) : '0.0'}%`; $('#stat-cost').textContent = totals.cost ? formatCost(totals.cost) : '$0.00'; $('#stat-latency').textContent = `${requests ? Math.round(totals.latency / requests).toLocaleString() : 0} ms avg · ${totals.streaming} streaming`;
  $('#requests-spark').innerHTML = sparkline(buckets.map(bucket => bucket.requests), '#0b57d0'); $('#tokens-spark').innerHTML = sparkline(buckets.map(bucket => bucket.tokens), '#7c4dff'); $('#cache-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cached), '#00a67e'); $('#cost-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cost), '#ff8f00');
  renderActivityChart(buckets, seconds); renderRankings($('#provider-stats'), aggregateActivity(activityLogs, 'provider')); renderRankings($('#model-stats'), aggregateActivity(activityLogs, 'model'));
  $('#recent-activity-logs').innerHTML = activityLogs.length ? activityLogs.slice(0, 8).map(log => activityRow(log)).join('') : '<tr><td colspan="6"><div class="activity-empty">No requests in this period.</div></td></tr>'; renderActivityLogs();
  const filter = $('#activity-provider-filter'); const previous = filter.value; filter.replaceChildren(new Option('All providers', ''), ...stats.by_provider.map(provider => new Option(provider.provider, provider.provider))); filter.value = [...filter.options].some(option => option.value === previous) ? previous : '';
}
function showActivityTab(tab) { $$('.activity-tabs button').forEach(button => { const active = button.dataset.activityTab === tab; button.classList.toggle('active', active); button.setAttribute('aria-selected', String(active)); }); $('#activity-overview-panel').hidden = tab !== 'overview'; $('#activity-requests-panel').hidden = tab !== 'requests'; }
$('#activity-view').addEventListener('click', event => {
  const tab = event.target.closest('[data-activity-tab]');
  if (tab) { showActivityTab(tab.dataset.activityTab); return; }
  if (event.target.closest('.show-request-explorer')) showActivityTab('requests');
});
$('#activity-search').addEventListener('input', renderActivityLogs); $('#activity-status-filter').addEventListener('change', renderActivityLogs);
$('#activity-range').addEventListener('change', loadActivity); $('#activity-provider-filter').addEventListener('change', loadActivity); $('#refresh-activity').addEventListener('click', loadActivity);
async function loadDashboard() { const since = Math.floor(Date.now() / 1000) - 86400; const stats = await fetch(`/admin/activity/stats?since=${since}`).then(response => response.json()); $('#home-requests').textContent = compactNumber(stats.requests); $('#home-input').textContent = compactNumber(stats.input_tokens); $('#home-output').textContent = compactNumber(stats.output_tokens); $('#home-cached').textContent = compactNumber(stats.cached_tokens); $('#home-providers').replaceChildren(...providers.map(provider => { const item = document.createElement('button'); item.textContent = `${provider.name} · ${provider.discovered_models.length} models`; item.addEventListener('click', () => { selectedProviderId = provider.id; showView('providers'); }); return item; })); }

function formatType(type) { return type === 'anthropic' ? 'Anthropic' : 'OpenAI compatible'; }
function escapeHtml(value) { const node = document.createElement('span'); node.textContent = String(value); return node.innerHTML; }
async function loadProviders() { const response = await fetch('/admin/providers'); providers = await response.json(); renderProviders(); }
async function loadRoutes() { const response = await fetch('/admin/routes'); modelRoutes = await response.json(); renderRoutes(); }
initializeAdmin();
