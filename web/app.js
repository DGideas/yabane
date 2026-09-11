let providers = [];
let authSettings = {enabled: true, api_keys: []};
let modelRoutes = [];
let extensions = [];
const $ = selector => document.querySelector(selector);
const $$ = selector => [...document.querySelectorAll(selector)];
const icon = (name, className = 'ui-icon') => `<svg class="${className}" aria-hidden="true"><use href="#icon-${name}"></use></svg>`;
const providerDialog = $('#provider-dialog');
const providerForm = $('#provider-form');
const providerIdentityDialog = $('#provider-identity-dialog');
let providerStep = 1;
let providerIdEdited = false;
let selectedProviderId = null;
let adminSession = null;
let turnstileWidgetId = null;
let aboutInfo = null;
const LIVE_REFRESH_INTERVAL_MS = 30000;
const ACTIVITY_PAGE_SIZE = 100;
let activityLoadPromise = null;
let activityLogsLimit = 0;
let activityPage = 0;
let activityPageTotal = 0;
let activityPageRequest = 0;
let activityPageUntil = 0;
let activitySearchTimer = null;
let dashboardLoadPromise = null;

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
    requestAnimationFrame(() => $('#login-form [name="username"]').focus({preventScroll: true}));
    return;
  }
  const initial = (adminSession.username || 'A').slice(0, 1).toUpperCase();
  $('#account-menu').textContent = initial; $('.account-avatar-large').textContent = initial;
  $('#account-name').textContent = adminSession.username || 'Administrator'; $('#account-email').textContent = adminSession.email || '';
  await Promise.all([loadProviders(), loadAuth(), loadRoutes(), loadExtensions(), loadAbout()]);
  refreshVisibleView();
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
const mobileNavToggle = $('#mobile-nav-toggle');
const mobileNavBackdrop = $('#mobile-nav-backdrop');
const consoleSidebar = $('#console-sidebar');
function closeAccountMenu() { accountPopover.hidden = true; accountMenu.setAttribute('aria-expanded', 'false'); }
function setMobileNavigation(open, returnFocus = false) {
  consoleSidebar.classList.toggle('mobile-open', open);
  mobileNavBackdrop.hidden = !open;
  mobileNavToggle.setAttribute('aria-expanded', String(open));
  mobileNavToggle.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
  document.body.classList.toggle('mobile-nav-open', open);
  if (open) requestAnimationFrame(() => consoleSidebar.querySelector('.nav.active')?.focus());
  else if (returnFocus) mobileNavToggle.focus();
}
mobileNavToggle.addEventListener('click', () => setMobileNavigation(mobileNavToggle.getAttribute('aria-expanded') !== 'true', true));
mobileNavBackdrop.addEventListener('click', () => setMobileNavigation(false, true));
window.addEventListener('resize', () => { if (innerWidth > 760) setMobileNavigation(false); });
accountMenu.addEventListener('click', event => { event.stopPropagation(); const opening = accountPopover.hidden; accountPopover.hidden = !opening; accountMenu.setAttribute('aria-expanded', String(opening)); });
document.addEventListener('click', event => { if (!event.target.closest('.account-control')) closeAccountMenu(); });
document.addEventListener('keydown', event => { if (event.key === 'Escape') { closeAccountMenu(); if (mobileNavToggle.getAttribute('aria-expanded') === 'true') setMobileNavigation(false, true); } });
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

const viewPaths = {home: '/home', providers: '/providers', models: '/model-routing', extensions: '/extensions', access: '/api-access', activity: '/activity', management: '/management-api'};
function showView(name, updateHistory = true) {
  $('#home-view').hidden = name !== 'home';
  $('#providers-view').hidden = name !== 'providers';
  $('#models-view').hidden = name !== 'models';
  $('#extensions-view').hidden = name !== 'extensions';
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
  if (name === 'extensions') renderExtensions();
  if (name === 'activity') loadActivity();
  if (name === 'management') loadManagementKeys();
  if (updateHistory) history.pushState({}, '', selectedProviderId && name === 'providers' ? `/providers/${encodeURIComponent(selectedProviderId)}` : viewPaths[name]);
  document.title = `${active.textContent.trim()} · Yabane`;
}
$$('.nav[data-view]').forEach(item => item.addEventListener('click', () => { showView(item.dataset.view); if (innerWidth <= 760) setMobileNavigation(false); }));
function routeFromLocation() {
  const path = location.pathname;
  if (path.startsWith('/providers/')) { selectedProviderId = decodeURIComponent(path.slice('/providers/'.length)); showView('providers', false); }
  else if (path === '/home' || path === '/') showView('home', false);
  else if (path === '/model-routing') showView('models', false);
  else if (path === '/extensions') showView('extensions', false);
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
  providerForm.querySelector('.base-url-notice').textContent = '';
  delete $('#base-url').dataset.previousValue;
  providerIdEdited = false;
  $('#provider-error').textContent = '';
  $('#initial-endpoint-id').dataset.edited = '';
  setApiType('openai_compatible');
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
  const subscription = type === 'openai_codex';
  const base = $('#base-url');
  const endpointId = $('#initial-endpoint-id');
  if (!endpointId.dataset.edited) endpointId.value = defaultEndpointId(type);
  [base.closest('.field'), $('#requires-key').closest('.checkbox-row')].forEach(field => field.hidden = subscription);
  if (subscription) { base.dataset.previousValue = base.value; base.value = 'https://chatgpt.com/backend-api'; }
  else if (base.value === 'https://chatgpt.com/backend-api') base.value = base.dataset.previousValue || '';
  base.required = !subscription;
  $('#initial-key-section').hidden = subscription;
  $('#api-key').required = !subscription && $('#requires-key').checked;
  $('#create-provider').textContent = subscription ? 'Connect OpenAI' : 'Add provider';
}
$$('input[name="api_type"]').forEach(input => input.addEventListener('change', () => setApiType(input.value)));
$('#initial-endpoint-id').addEventListener('input', event => {
  event.target.value = slugify(event.target.value);
  event.target.dataset.edited = 'true';
});
function defaultEndpointId(type) {
  return {openai_compatible: 'openai', openai_chat_completions: 'openai-chat', openai_responses: 'openai-responses', openai_codex: 'chatgpt', anthropic: 'anthropic'}[type] || 'endpoint';
}
function availableEndpointId(provider, type) {
  const base = defaultEndpointId(type);
  const used = new Set(provider?.endpoints.map(endpoint => endpoint.id) || []);
  if (!used.has(base)) return base;
  let suffix = 2;
  while (used.has(`${base}-${suffix}`)) suffix++;
  return `${base}-${suffix}`;
}
function operationPathNotice(input) {
  const notice = input.closest('.field').querySelector('.base-url-notice');
  if (!notice) return;
  const path = new URL(input.value, 'http://invalid').pathname.replace(/\/$/, '');
  const operation = ['/chat/completions', '/responses', '/messages', '/models'].find(suffix => path.endsWith(suffix));
  notice.textContent = operation ? `This looks like a specific ${operation} operation URL. Use the shared API root instead so discovery and inference can append their own paths.` : '';
}
$('#base-url').addEventListener('input', event => {
  const hostname = new URL(event.target.value, 'http://invalid').hostname;
  if (hostname === 'api.anthropic.com' || hostname.endsWith('.anthropic.com')) setApiType('anthropic');
  if (hostname.includes('openai.com') || hostname.includes('azure.com') || hostname === 'opencode.ai') setApiType('openai_compatible');
  operationPathNotice(event.target);
});
$('#endpoint-dialog [name="base_url"]').addEventListener('input', event => operationPathNotice(event.target));

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
      id: data.get('endpoint_id'),
      api_type: data.get('api_type'),
      base_url: data.get('base_url'),
      socks5_proxy: data.get('socks5_proxy') || null,
      extra_headers: {},
      extra_body: {},
      requires_api_key: data.get('requires_api_key') === 'on',
      api_key: data.get('requires_api_key') === 'on' ? data.get('api_key') : null
    }
  };
  if (payload.endpoint.api_type === 'openai_codex') {
    return beginOpenAiSubscription({provider_id: payload.id, provider_name: payload.name, endpoint_id: payload.endpoint.id, socks5_proxy: payload.endpoint.socks5_proxy}, $('#provider-error'), providerDialog);
  }
  const response = await fetch('/admin/providers', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#provider-error'));
  providerDialog.close();
  await loadProviders();
  [1000, 3000, 8000].forEach(delay => setTimeout(loadProviders, delay));
});

const openAiSubscriptionDialog = $('#openai-subscription-dialog');
let openAiSubscriptionFlowId = null;
let openAiSubscriptionStartController = null;
let openAiSubscriptionPollController = null;
async function beginOpenAiSubscription(target, errorElement, parentDialog) {
  openAiSubscriptionFlowId = null;
  openAiSubscriptionPollController?.abort();
  openAiSubscriptionPollController = null;
  openAiSubscriptionStartController?.abort();
  const controller = new AbortController();
  openAiSubscriptionStartController = controller;
  parentDialog.addEventListener('close', () => controller.abort(), {once: true});
  const submit = parentDialog.querySelector('[type="submit"]');
  submit.disabled = true;
  let response;
  try {
    response = await fetch('/admin/openai-subscriptions/device-code', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(target), signal: controller.signal});
  } catch (error) {
    const current = openAiSubscriptionStartController === controller;
    if (current) { openAiSubscriptionStartController = null; submit.disabled = false; }
    if (error.name === 'AbortError' || !current) return;
    errorElement.textContent = 'Could not start OpenAI sign-in.';
    return;
  }
  if (openAiSubscriptionStartController !== controller) return;
  openAiSubscriptionStartController = null;
  submit.disabled = false;
  if (!response.ok) return showApiError(response, errorElement);
  const flow = await response.json();
  parentDialog.close();
  $('#openai-subscription-code').textContent = flow.user_code;
  $('#openai-subscription-link').href = flow.verification_uri;
  $('#openai-subscription-status').textContent = 'Waiting for OpenAI sign-in…';
  $('#openai-subscription-error').textContent = '';
  openAiSubscriptionPollController?.abort();
  openAiSubscriptionFlowId = flow.id;
  openAiSubscriptionDialog.showModal();
  const poll = async () => {
    if (openAiSubscriptionFlowId !== flow.id) return;
    const controller = new AbortController();
    openAiSubscriptionPollController = controller;
    let statusResponse;
    try {
      statusResponse = await fetch(`/admin/openai-subscriptions/device-code/${encodeURIComponent(flow.id)}`, {signal: controller.signal});
    } catch (error) {
      if (error.name === 'AbortError') return;
      openAiSubscriptionFlowId = null;
      $('#openai-subscription-error').textContent = 'Could not check OpenAI sign-in status.';
      return;
    }
    if (openAiSubscriptionFlowId !== flow.id) return;
    openAiSubscriptionPollController = null;
    if (!statusResponse.ok) { openAiSubscriptionFlowId = null; return showApiError(statusResponse, $('#openai-subscription-error')); }
    const status = await statusResponse.json();
    if (status.status === 'complete') {
      openAiSubscriptionFlowId = null;
      $('#openai-subscription-status').textContent = 'Connected. Loading your OpenAI subscription…';
      await Promise.all([loadProviders(), loadRoutes()]);
      openAiSubscriptionDialog.close();
      if (target.provider_name) { selectedProviderId = target.provider_id; history.pushState({}, '', `/providers/${encodeURIComponent(target.provider_id)}`); renderProviderPage(); }
      return;
    }
    if (status.status === 'failed') { openAiSubscriptionFlowId = null; $('#openai-subscription-error').textContent = status.error || 'OpenAI sign-in failed.'; return; }
    setTimeout(poll, Math.max(1000, Number(status.interval_seconds || 2) * 1000));
  };
  poll();
}
function stopOpenAiSubscriptionPolling() {
  openAiSubscriptionFlowId = null;
  openAiSubscriptionPollController?.abort();
  openAiSubscriptionPollController = null;
}
$$('.close-openai-subscription').forEach(button => button.addEventListener('click', () => { stopOpenAiSubscriptionPolling(); openAiSubscriptionDialog.close(); }));
openAiSubscriptionDialog.addEventListener('close', stopOpenAiSubscriptionPolling);
$('#openai-subscription-code').addEventListener('click', async () => {
  await navigator.clipboard?.writeText($('#openai-subscription-code').textContent);
  $('#openai-subscription-status').textContent = 'Code copied. Complete sign-in on OpenAI, then return here.';
});

function credentialCount(provider) {
  return provider.endpoints.reduce((count, endpoint) => count + endpoint.api_keys.length + (endpoint.subscription_connected ? 1 : 0), 0);
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
    card.innerHTML = `<span class="provider-avatar">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</span><span class="provider-list-main"><strong>${escapeHtml(provider.name)}</strong><code>${escapeHtml(provider.id)}/model-id</code></span><span class="provider-list-meta">${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} · ${credentialCount(provider)} credential${credentialCount(provider) === 1 ? '' : 's'}<small class="${provider.model_discovery_error ? 'error-text' : ''}">${escapeHtml(modelStatus)}</small></span><span class="chevron">${icon('chevron-right')}</span>`;
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
    const enabledKeys = endpoint.api_keys.filter(key => key.enabled);
    const shares = trafficShares(enabledKeys);
    if (endpoint.api_type === 'openai_codex') {
      const accessTokenExpiry = endpoint.subscription_expires_at ? new Date(endpoint.subscription_expires_at * 1000).toLocaleString() : null;
      const credentialDetail = !endpoint.subscription_connected
        ? 'No OAuth credential is connected. Delete this Endpoint and reconnect the OpenAI account to resume requests.'
        : endpoint.subscription_expires_at && endpoint.subscription_expires_at * 1000 <= Date.now()
          ? 'The current access token will be renewed when the next request uses this Endpoint.'
          : accessTokenExpiry
            ? `The current access token is valid until ${accessTokenExpiry} and will be renewed automatically when needed.`
            : 'The current access token will be renewed automatically when needed.';
      const renewal = endpoint.subscription_connected
        ? `<div><h4>Automatic renewal enabled</h4><p>Yabane renews temporary access credentials when needed. Reconnect only if renewal fails or OpenAI revokes access.</p><details class="credential-details"><summary>Credential details</summary><p>${escapeHtml(credentialDetail)} Access and refresh tokens are never shown in the console or API.</p></details></div><span class="renewal-status">Automatic renewal</span>`
        : `<div><h4>Reconnect required</h4><p>${escapeHtml(credentialDetail)}</p></div><span class="renewal-status attention">Not connected</span>`;
      return `<article class="endpoint-card subscription-endpoint"><header class="endpoint-head"><span class="endpoint-index">${index + 1}</span><div class="endpoint-identity"><div><h3>${escapeHtml(endpoint.id)}</h3><span class="kind">OpenAI subscription</span></div><code>ChatGPT Plus / Pro · Responses API</code></div><div class="endpoint-facts"><span><strong>${endpointModels}</strong> models</span><span><strong>${endpoint.subscription_connected ? 'Connected' : 'Disconnected'}</strong> account</span>${endpoint.socks5_proxy ? `<span>Proxy <code>${escapeHtml(endpoint.socks5_proxy)}</code></span>` : ''}</div><div class="endpoint-actions"><button class="endpoint-edit text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Edit proxy</button><button class="endpoint-delete text-link danger-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" aria-label="Delete endpoint ${escapeHtml(endpoint.id)}">Delete endpoint</button></div></header><section class="endpoint-keys subscription-credential"><div class="endpoint-keys-head">${renewal}</div></section></article>`;
    }
    return `<article class="endpoint-card"><header class="endpoint-head"><span class="endpoint-index">${index + 1}</span><div class="endpoint-identity"><div><h3>${escapeHtml(endpoint.id)}</h3><span class="kind">${formatType(endpoint.api_type)}</span></div><code>${escapeHtml(endpoint.base_url)}</code></div><div class="endpoint-facts"><span><strong>${endpointModels}</strong> models</span><span><strong>${enabledKeys.length}</strong> of ${endpoint.api_keys.length} keys enabled</span>${endpoint.socks5_proxy ? `<span>Proxy <code>${escapeHtml(endpoint.socks5_proxy)}</code></span>` : ''}</div><div class="endpoint-actions"><button class="endpoint-edit text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Edit settings</button><button class="endpoint-delete text-link danger-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" aria-label="Delete endpoint ${escapeHtml(endpoint.id)}">Delete endpoint</button></div></header><section class="endpoint-keys"><div class="endpoint-keys-head"><div><h4>Upstream API keys</h4><p>Credentials below belong only to <code>${escapeHtml(endpoint.id)}</code>. Traffic is split between enabled keys.</p></div><div class="endpoint-key-actions">${enabledKeys.length > 1 ? `<button class="text-link edit-traffic" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Distribute traffic</button>` : ''}<button class="button secondary add-key" data-provider="${provider.id}" data-endpoint="${endpoint.id}">${icon('plus', 'button-icon')}Add key</button></div></div><div class="key-list">${endpoint.api_keys.length ? endpoint.api_keys.map(key => `<div class="key-row"><span class="status ${key.enabled ? 'enabled' : ''}"></span><span class="key-name"><strong>${escapeHtml(key.name)}</strong><small>${key.enabled ? 'Enabled for traffic' : 'Disabled'}</small></span><span class="traffic-share"><strong>${key.enabled ? `${shares.get(key.id)}%` : '—'}</strong><small>${key.enabled ? 'of default traffic' : 'no traffic'}</small></span><button class="key-toggle text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-key="${key.id}" data-enabled="${key.enabled}">${key.enabled ? 'Disable' : 'Enable'}</button><button class="key-delete text-link danger-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-key="${key.id}" data-name="${escapeHtml(key.name)}" aria-label="Delete API key ${escapeHtml(key.name)}">Delete</button></div>`).join('') : `<div class="endpoint-key-empty"><p>No API keys belong to this endpoint yet.</p><button class="text-link add-key" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Add the first key</button></div>`}</div></section></article>`;
  }).join('');
  const variants = modelEndpointVariants(provider);
  const sharedVariants = variants.filter(variant => variant.endpointIds.length > 1);
  const configuredPreferences = provider.model_endpoint_preferences?.length || 0;
  const discovery = provider.model_discovery_error ? `<span class="error-text">${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `Updated ${escapeHtml(new Date(provider.models_discovered_at * 1000).toLocaleString())}` : 'Discovery has not completed';
  const headerCount = Object.keys(provider.extra_headers || {}).length;
  const bodyCount = Object.keys(provider.extra_body || {}).length;
  const defaultsScope = provider.defaults_endpoint_ids?.length ? `${provider.defaults_endpoint_ids.length} selected endpoint${provider.defaults_endpoint_ids.length === 1 ? '' : 's'}` : 'All endpoints';
  const requestDefaultsExtension = extensions.find(extension => extension.id === 'request-defaults');
  const defaultsAvailability = requestDefaultsExtension ? (requestDefaultsExtension.enabled ? 'Extension enabled' : 'Extension disabled') : 'Extension not included';
  const defaultsDescription = requestDefaultsExtension ? (requestDefaultsExtension.enabled ? 'Add default headers and JSON fields to upstream requests.' : 'Saved defaults are retained but are not currently applied to requests.') : 'Rebuild Yabane with the Request Defaults Extension to configure these values.';
  const defaultsAction = requestDefaultsExtension ? '<button class="button secondary edit-provider-options">Configure</button>' : '<button class="button secondary include-request-defaults-extension" type="button">How to include</button>';
  const coverage = provider.endpoints.map(endpoint => { const count = Object.values(provider.model_endpoints).filter(ids => ids.includes(endpoint.id)).length; return `<div><span><strong>${escapeHtml(endpoint.id)}</strong><small>${escapeHtml(formatType(endpoint.api_type))}</small></span><b>${count.toLocaleString()}</b></div>`; }).join('');
  $('#provider-detail').innerHTML = `<nav class="provider-breadcrumb" aria-label="Breadcrumb"><button id="back-to-providers">Providers</button>${icon('chevron-right', 'breadcrumb-icon')}<strong>${escapeHtml(provider.name)}</strong></nav><header class="provider-hero"><div class="provider-hero-mark">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</div><div class="provider-hero-main"><span class="provider-eyebrow">Provider settings</span><h1>${escapeHtml(provider.name)}</h1><p>Requests use <code>${escapeHtml(provider.id)}/model-id</code>. This provider contains ${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} and ${credentialCount(provider)} upstream credential${credentialCount(provider) === 1 ? '' : 's'}.</p></div><div class="provider-hero-actions"><button class="button secondary contextual-help" data-help-context="provider" data-provider="${provider.id}" type="button">${icon('help', 'button-icon')}Provider guide</button><button class="button secondary edit-provider" data-provider="${provider.id}" type="button">Edit provider</button><button class="delete-provider button danger" data-provider="${provider.id}">Delete provider</button></div></header><div class="provider-overview"><section class="card model-summary-card"><div class="card-head"><div><span class="section-kicker">Model catalog</span><h2>Discovered models</h2><p>${discovery}</p></div><div>${sharedVariants.length ? `<button class="button secondary manage-model-endpoints">Manage endpoint defaults</button>` : ''}<button class="text-link browse-provider-models" aria-expanded="false">Browse catalog</button><button class="text-link refresh-models" data-provider="${provider.id}">Refresh</button></div></div><div class="model-insights"><div class="model-insight"><strong>${provider.discovered_models.length.toLocaleString()}</strong><span>Models</span><small>Unique model IDs</small></div><div class="model-insight ${sharedVariants.length ? 'attention' : ''}"><strong>${sharedVariants.length.toLocaleString()}</strong><span>Shared models</span><small>${sharedVariants.length ? `${configuredPreferences} explicit default${configuredPreferences === 1 ? '' : 's'}` : 'No endpoint overlap'}</small></div><div class="endpoint-coverage"><header><span>Endpoint coverage</span><small>Models reported</small></header>${coverage || '<p>No endpoints configured</p>'}</div></div><div class="provider-model-browser" hidden><div class="model-browser-toolbar"><label class="model-filter"><svg class="model-search-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="10.5" cy="10.5" r="5.5"></circle><path d="m15 15 4 4"></path></svg><input type="text" role="searchbox" placeholder="Search model IDs" autocomplete="off" aria-label="Search model IDs"><button type="button" class="model-search-clear" aria-label="Clear search" hidden>${icon('close')}</button></label><span class="model-result-count"></span></div><div class="model-table"><header><span>Model ID</span><span>Available through</span><span>Default routing</span></header><div class="model-table-body"></div></div><footer class="model-pagination"><span class="model-page-status"></span><div><button type="button" class="button secondary model-page-previous">Previous</button><button type="button" class="button secondary model-page-next">Next</button></div></footer></div></section><section class="card defaults-card"><div class="card-head"><div><span class="section-kicker">${defaultsAvailability} · ${escapeHtml(defaultsScope)}</span><h2>Request defaults</h2><p>${defaultsDescription}</p></div><div class="defaults-actions">${defaultsAction}</div></div><div class="request-defaults-summary"><div><span class="defaults-count">${headerCount}</span><span><strong>Headers</strong><small>${headerCount ? 'Configured' : 'Not configured'}</small></span></div><div><span class="defaults-count">${bodyCount}</span><span><strong>Body fields</strong><small>${bodyCount ? 'Configured' : 'Not configured'}</small></span></div></div></section></div><section class="endpoint-group"><div class="endpoint-group-head"><div><span class="section-kicker">Provider children</span><h2>API endpoints</h2><p>Each endpoint is an upstream connection. API keys or OAuth subscriptions belong only to their configured Endpoint.</p></div><button class="button primary add-endpoint" data-provider="${provider.id}">${icon('plus', 'button-icon')}Add endpoint</button></div><div class="endpoint-stack">${endpointHtml || '<div class="empty endpoint-empty"><h3>No endpoints</h3><p>Add an upstream API endpoint to start routing requests.</p></div>'}</div></section>`;
  const browse = $('#provider-detail .browse-provider-models');
  if (!provider.discovered_models.length) browse.disabled = true;
  const pageSize = 25; let modelPage = 0;
  const modelSearch = $('#provider-detail .model-filter input');
  const renderModels = () => {
    const query = modelSearch.value.trim().toLowerCase();
    const matches = provider.discovered_models.filter(model => model.toLowerCase().includes(query));
    const pageCount = Math.max(1, Math.ceil(matches.length / pageSize)); modelPage = Math.min(modelPage, pageCount - 1);
    const start = modelPage * pageSize; const pageModels = matches.slice(start, start + pageSize);
    $('#provider-detail .model-result-count').textContent = query ? `${matches.length} result${matches.length === 1 ? '' : 's'}` : `${provider.discovered_models.length} models`;
    $('#provider-detail .model-page-status').textContent = matches.length ? `${start + 1}–${Math.min(start + pageSize, matches.length)} of ${matches.length} · Page ${modelPage + 1} of ${pageCount}` : 'No matching models';
    $('#provider-detail .model-page-previous').disabled = modelPage === 0;
    $('#provider-detail .model-page-next').disabled = modelPage >= pageCount - 1;
    $('#provider-detail .model-search-clear').hidden = !modelSearch.value;
    $('#provider-detail .model-table-body').replaceChildren(...pageModels.map(model => {
      const row = document.createElement('div'); row.className = 'model-catalog-row'; const modelVariants = variants.filter(variant => variant.model === model);
      const endpoints = [...new Set(modelVariants.flatMap(variant => variant.endpointIds))];
      const routing = modelVariants.map(variant => `${formatType(variant.apiType)} to ${variant.preferredEndpointId || variant.endpointIds[0]}${variant.preferredEndpointId ? ' (set)' : ''}`).join(' · ');
      row.innerHTML = `<code>${escapeHtml(model)}</code><span>${endpoints.map(id => `<code>${escapeHtml(id)}</code>`).join(' ')}</span><small>${escapeHtml(routing)}</small>`; return row;
    }));
    if (!pageModels.length) $('#provider-detail .model-table-body').innerHTML = '<div class="model-catalog-empty">No models match your search.</div>';
  };
  browse.addEventListener('click', () => {
    const browser = $('#provider-detail .provider-model-browser');
    browser.hidden = !browser.hidden;
    const expanded = !browser.hidden;
    $('#provider-detail .provider-overview').classList.toggle('catalog-open', expanded);
    browse.textContent = expanded ? 'Close catalog' : 'Browse catalog';
    browse.setAttribute('aria-expanded', String(expanded));
    if (expanded) { modelPage = 0; renderModels(); modelSearch.focus(); }
  });
  modelSearch.addEventListener('input', () => { modelPage = 0; renderModels(); });
  $('#provider-detail .model-search-clear').addEventListener('click', event => { event.preventDefault(); modelSearch.value = ''; modelPage = 0; renderModels(); modelSearch.focus(); });
  $('#provider-detail .model-page-previous').addEventListener('click', () => { modelPage--; renderModels(); });
  $('#provider-detail .model-page-next').addEventListener('click', () => { modelPage++; renderModels(); });
  $('#provider-detail .manage-model-endpoints')?.addEventListener('click', () => openModelEndpointsDialog(provider));
  $('#provider-detail .edit-provider-options')?.addEventListener('click', () => openRequestDefaultsDialog(provider));
  $('#provider-detail .include-request-defaults-extension')?.addEventListener('click', () => showView('extensions'));
  $('#provider-detail #back-to-providers').addEventListener('click', () => { selectedProviderId = null; history.pushState({}, '', '/providers'); renderProviderPage(); });
  bindProviderActions();
}

function modelEndpointVariants(provider) {
  const preferences = provider.model_endpoint_preferences || [];
  return provider.discovered_models.flatMap(model => ['openai_compatible', 'openai_chat_completions', 'openai_responses', 'openai_codex', 'anthropic'].map(apiType => {
    const compatibleIds = new Set(provider.endpoints.filter(endpoint => endpoint.api_type === apiType).map(endpoint => endpoint.id));
    const endpointIds = (provider.model_endpoints[model] || []).filter(id => compatibleIds.has(id));
    const preferred = preferences.find(item => item.model === model && item.api_type === apiType);
    return {model, apiType, endpointIds, preferredEndpointId: preferred?.endpoint_id || null};
  }).filter(variant => variant.endpointIds.length));
}

const modelEndpointsDialog = $('#model-endpoints-dialog');
let modelEndpointsProvider = null;
function openModelEndpointsDialog(provider) {
  modelEndpointsProvider = provider;
  $('#model-endpoints-description').textContent = `Choose the default endpoint for shared models in ${provider.name}. Without a preference, the first configured compatible endpoint is used.`;
  $('#model-endpoints-error').textContent = '';
  $('#model-endpoint-search').value = '';
  renderModelEndpointPreferences(); modelEndpointsDialog.showModal();
}
function renderModelEndpointPreferences() {
  const query = $('#model-endpoint-search').value.trim().toLowerCase();
  const variants = modelEndpointVariants(modelEndpointsProvider).filter(variant => variant.endpointIds.length > 1 && variant.model.toLowerCase().includes(query));
  $('#model-endpoint-count').textContent = `${variants.length} shared model${variants.length === 1 ? '' : 's'}`;
  $('#model-endpoint-rows').replaceChildren(...variants.slice(0, 200).map(variant => {
    const row = document.createElement('label'); row.className = 'model-endpoint-row';
    const select = document.createElement('select'); select.dataset.model = variant.model; select.dataset.apiType = variant.apiType;
    select.append(new Option(`Automatic · ${variant.endpointIds[0]}`, ''));
    variant.endpointIds.forEach(id => select.append(new Option(id, id)));
    select.value = variant.preferredEndpointId || '';
    row.innerHTML = `<code>${escapeHtml(variant.model)}</code><span class="kind">${escapeHtml(formatType(variant.apiType))}</span>`;
    row.append(select); return row;
  }));
  if (!variants.length) $('#model-endpoint-rows').innerHTML = '<div class="editor-empty">No shared models match this search.</div>';
}
$('#model-endpoint-search').addEventListener('input', renderModelEndpointPreferences);
$$('.close-model-endpoints').forEach(button => button.addEventListener('click', () => modelEndpointsDialog.close()));
$('#model-endpoints-form').addEventListener('submit', async event => {
  event.preventDefault();
  const visibleUpdates = new Map($$('#model-endpoint-rows select').map(select => [`${select.dataset.model}\n${select.dataset.apiType}`, select.value]));
  const preferences = (modelEndpointsProvider.model_endpoint_preferences || []).filter(item => !visibleUpdates.has(`${item.model}\n${item.api_type}`));
  $$('#model-endpoint-rows select').forEach(select => { if (select.value) preferences.push({model: select.dataset.model, api_type: select.dataset.apiType, endpoint_id: select.value}); });
  const response = await fetch(`/admin/providers/${modelEndpointsProvider.id}/model-endpoint-preferences`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({preferences})});
  if (!response.ok) return showApiError(response, $('#model-endpoints-error'));
  modelEndpointsDialog.close(); await loadProviders();
});

function trafficShares(keys) {
  const shares = new Map();
  const total = keys.reduce((sum, key) => sum + key.weight, 0);
  if (!total) return shares;
  const calculated = keys.map(key => { const exact = key.weight * 100 / total; return {key, share: Math.floor(exact), remainder: exact % 1}; });
  let remaining = 100 - calculated.reduce((sum, item) => sum + item.share, 0);
  calculated.sort((a, b) => b.remainder - a.remainder).slice(0, remaining).forEach(item => item.share++);
  calculated.forEach(item => shares.set(item.key.id, item.share));
  return shares;
}

function bindProviderActions() {
  $$('.edit-provider').forEach(button => button.addEventListener('click', () => openProviderIdentityDialog(button.dataset.provider)));
  $$('.delete-provider').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    if (confirm(`Delete ${provider.name}?`)) { await fetch(`/admin/providers/${provider.id}`, {method: 'DELETE'}); selectedProviderId = null; await loadProviders(); }
  }));
  $$('.endpoint-edit').forEach(button => button.addEventListener('click', () => openEndpointDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.endpoint-delete').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    const endpoint = provider.endpoints.find(item => item.id === button.dataset.endpoint);
    const credentialImpact = endpoint.api_type === 'openai_codex' ? 'its connected OAuth subscription' : `its ${endpoint.api_keys.length} API key${endpoint.api_keys.length === 1 ? '' : 's'}`;
    const message = `Delete endpoint “${endpoint.id}”?\n\nThis also deletes ${credentialImpact}, removes its discovered-model availability, and removes destinations that route to it.`;
    if (!confirm(message)) return;
    const response = await fetch(`/admin/providers/${provider.id}/endpoints/${endpoint.id}`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    await loadProviders();
  }));
  $$('.add-key').forEach(button => button.addEventListener('click', () => openKeyDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.edit-traffic').forEach(button => button.addEventListener('click', () => openTrafficDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.key-toggle').forEach(button => button.addEventListener('click', async () => {
    await patchKey(button.dataset.provider, button.dataset.endpoint, button.dataset.key, {enabled: button.dataset.enabled !== 'true'});
  }));
  $$('.key-delete').forEach(button => button.addEventListener('click', async () => {
    if (!confirm(`Delete API key “${button.dataset.name}”?\n\nModel-route destinations using this exact key will also be removed.`)) return;
    const response = await fetch(`/admin/providers/${button.dataset.provider}/endpoints/${button.dataset.endpoint}/keys/${button.dataset.key}`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    await Promise.all([loadProviders(), loadRoutes()]);
  }));
  $$('.refresh-models').forEach(button => button.addEventListener('click', () => refreshModels(button.dataset.provider, button)));
  $$('.add-endpoint').forEach(button => button.addEventListener('click', () => openEndpointDialog(button.dataset.provider)));
}

function openProviderIdentityDialog(providerId) {
  const provider = providers.find(item => item.id === providerId);
  const form = $('#provider-identity-form');
  form.reset();
  form.elements.current_id.value = provider.id;
  form.elements.name.value = provider.name;
  form.elements.id.value = provider.id;
  $('#provider-identity-error').textContent = '';
  providerIdentityDialog.showModal();
  form.elements.name.focus();
}
$$('.close-provider-identity').forEach(button => button.addEventListener('click', () => providerIdentityDialog.close()));
$('#provider-identity-form').addEventListener('submit', async event => {
  event.preventDefault();
  const form = event.target;
  const provider = providers.find(item => item.id === form.elements.current_id.value);
  const response = await fetch(`/admin/providers/${provider.id}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({id: provider.id, name: form.elements.name.value})});
  if (!response.ok) return showApiError(response, $('#provider-identity-error'));
  providerIdentityDialog.close();
  await loadProviders();
});

async function patchKey(providerId, endpointId, keyId, update) {
  const response = await fetch(`/admin/providers/${providerId}/endpoints/${endpointId}/keys/${keyId}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(update)});
  if (!response.ok) return showApiError(response, null);
  await loadProviders();
}

const endpointDialog = $('#endpoint-dialog');
function openEndpointDialog(providerId, endpointId = null) {
  const form = $('#endpoint-form'); form.reset(); form.elements.provider_id.value = providerId; form.dataset.endpointId = endpointId || ''; form.elements.id.dataset.edited = ''; delete form.elements.base_url.dataset.previousValue; $('#endpoint-error').textContent = ''; form.querySelector('.base-url-notice').textContent = '';
  const provider = providers.find(item => item.id === providerId);
  const endpoint = endpointId ? provider?.endpoints.find(item => item.id === endpointId) : null;
  if (!endpoint) form.elements.id.value = availableEndpointId(provider, form.elements.api_type.value);
  form.elements.api_type.querySelector('option[value="openai_codex"]').disabled = Boolean(endpoint && endpoint.api_type !== 'openai_codex');
  $('#endpoint-dialog h2').textContent = endpoint ? `Edit ${endpoint.id}` : 'Add API endpoint';
  $('#endpoint-dialog .dialog-head p').textContent = endpoint?.api_type === 'openai_codex' ? 'Update the proxy used for OpenAI sign-in, token refresh, and inference.' : endpoint ? 'Update this upstream connection. Existing API keys are managed separately.' : 'Models discovered here remain accessible through the same provider prefix.';
  $('#endpoint-dialog button[type="submit"]').textContent = endpoint ? 'Save changes' : 'Add endpoint';
  form.elements.id.disabled = Boolean(endpoint);
  if (endpoint) {
    form.elements.id.value = endpoint.id; form.elements.base_url.value = endpoint.base_url; form.elements.api_type.value = endpoint.api_type;
    form.elements.socks5_proxy.value = endpoint.socks5_proxy || ''; form.elements.requires_api_key.checked = endpoint.requires_api_key;
    operationPathNotice(form.elements.base_url);
  }
  toggleEndpointMode(); bindSecretToggles(endpointDialog); endpointDialog.showModal();
}
function toggleEndpointMode() {
  const form = $('#endpoint-form'); const subscription = form.elements.api_type.value === 'openai_codex'; const editing = Boolean(form.dataset.endpointId);
  const required = form.elements.requires_api_key.checked && !subscription;
  form.elements.base_url.closest('.field').hidden = subscription;
  form.elements.api_type.closest('.field').hidden = subscription && editing;
  form.elements.requires_api_key.closest('.checkbox-row').hidden = subscription;
  form.elements.base_url.required = !subscription;
  if (subscription) { form.elements.base_url.dataset.previousValue = form.elements.base_url.value; form.elements.base_url.value = 'https://chatgpt.com/backend-api'; }
  else if (form.elements.base_url.value === 'https://chatgpt.com/backend-api') form.elements.base_url.value = form.elements.base_url.dataset.previousValue || '';
  $('.endpoint-key-section').classList.toggle('collapsed', !required || editing); form.elements.api_key.required = required && !editing;
  $('#endpoint-dialog button[type="submit"]').textContent = editing ? 'Save changes' : subscription ? 'Connect OpenAI' : 'Add endpoint';
}
$('#endpoint-form [name="requires_api_key"]').addEventListener('change', toggleEndpointMode);
$('#endpoint-form [name="id"]').addEventListener('input', event => { event.target.value = slugify(event.target.value); event.target.dataset.edited = 'true'; });
$('#endpoint-form [name="api_type"]').addEventListener('change', event => {
  const form = $('#endpoint-form');
  if (!form.dataset.endpointId && !form.elements.id.dataset.edited) {
    const provider = providers.find(item => item.id === form.elements.provider_id.value);
    form.elements.id.value = availableEndpointId(provider, event.target.value);
  }
  toggleEndpointMode();
});
$$('.close-endpoint').forEach(button => button.addEventListener('click', () => endpointDialog.close()));
$('#endpoint-form').addEventListener('submit', async event => {
  event.preventDefault(); const form = event.target; const data = new FormData(form); const providerId = data.get('provider_id'); const endpointId = form.dataset.endpointId;
  const payload = endpointId ? {id: endpointId, api_type: data.get('api_type'), base_url: data.get('base_url'), socks5_proxy: data.get('socks5_proxy') || null, requires_api_key: data.get('requires_api_key') === 'on'} : {id: data.get('id'), api_type: data.get('api_type'), base_url: data.get('base_url'), socks5_proxy: data.get('socks5_proxy') || null, extra_headers: {}, extra_body: {}, requires_api_key: data.get('requires_api_key') === 'on', api_key: data.get('requires_api_key') === 'on' ? data.get('api_key') : null};
  if (!endpointId && payload.api_type === 'openai_codex') return beginOpenAiSubscription({provider_id: providerId, endpoint_id: payload.id, socks5_proxy: payload.socks5_proxy}, $('#endpoint-error'), endpointDialog);
  const response = await fetch(endpointId ? `/admin/providers/${providerId}/endpoints/${endpointId}` : `/admin/providers/${providerId}/endpoints`, {method: endpointId ? 'PATCH' : 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#endpoint-error'));
  endpointDialog.close(); await loadProviders();
});

const keyDialog = $('#key-dialog');
function openKeyDialog(providerId, endpointId = null) {
  const provider = providers.find(item => item.id === providerId);
  $('#key-form').reset();
  $('#key-form [name="provider_id"]').value = providerId;
  $('#key-form [name="secret"]').type = 'password';
  $('#key-form .toggle-key').textContent = 'Show';
  $('#key-endpoint').replaceChildren(...provider.endpoints.filter(endpoint => endpoint.api_type !== 'openai_codex').map(endpoint => new Option(`${endpoint.id} · ${formatType(endpoint.api_type)}`, endpoint.id)));
  if (endpointId) $('#key-endpoint').value = endpointId;
  $('#key-dialog-title').textContent = endpointId ? `Add key to ${endpointId}` : 'Add API key';
  $('#key-dialog-description').textContent = `Add an upstream credential under ${provider.name}${endpointId ? ` / ${endpointId}` : ''}.`;
  keyDialog.showModal();
}
$$('.close-key').forEach(button => button.addEventListener('click', () => keyDialog.close()));
$('#key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const providerId = data.get('provider_id');
  const payload = {endpoint_id: data.get('endpoint_id'), name: data.get('name'), secret: data.get('secret'), weight: 100};
  const response = await fetch(`/admin/providers/${providerId}/keys`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, null);
  keyDialog.close(); await loadProviders();
});

const trafficDialog = $('#traffic-dialog');
let trafficEndpoint = null;
function openTrafficDialog(providerId, endpointId) {
  const provider = providers.find(item => item.id === providerId);
  const endpoint = provider.endpoints.find(item => item.id === endpointId);
  const enabledKeys = endpoint.api_keys.filter(key => key.enabled);
  const shares = trafficShares(enabledKeys);
  trafficEndpoint = {providerId, endpointId};
  $('#traffic-description').innerHTML = `Set the percentage of <code>${escapeHtml(endpointId)}</code> traffic sent with each enabled key.`;
  $('#traffic-error').textContent = '';
  $('#traffic-rows').replaceChildren(...enabledKeys.map(key => {
    const row = document.createElement('label'); row.className = 'traffic-row';
    row.innerHTML = `<span><span class="status enabled"></span><strong>${escapeHtml(key.name)}</strong></span><span class="percentage-input"><input type="number" min="1" max="99" required value="${shares.get(key.id)}" data-key="${key.id}" aria-label="Traffic percentage for ${escapeHtml(key.name)}"><b>%</b></span>`;
    row.querySelector('input').addEventListener('input', validateTrafficDistribution);
    return row;
  }));
  validateTrafficDistribution(); trafficDialog.showModal();
}
function validateTrafficDistribution() {
  const inputs = $$('#traffic-rows input');
  const total = inputs.reduce((sum, input) => sum + (Number(input.value) || 0), 0);
  const valid = inputs.length > 1 && inputs.every(input => input.checkValidity()) && total === 100;
  $('#traffic-total').textContent = `${total}%`;
  $('#traffic-total').classList.toggle('invalid', !valid);
  $('#traffic-error').textContent = total === 100 ? '' : `Traffic shares must add up to 100% (currently ${total}%).`;
  $('#save-traffic').disabled = !valid;
  return valid;
}
$$('.close-traffic').forEach(button => button.addEventListener('click', () => trafficDialog.close()));
$('#traffic-form').addEventListener('submit', async event => {
  event.preventDefault(); if (!validateTrafficDistribution()) return;
  const weights = $$('#traffic-rows input').map(input => ({key_id: input.dataset.key, weight: Number(input.value)}));
  const response = await fetch(`/admin/providers/${trafficEndpoint.providerId}/endpoints/${trafficEndpoint.endpointId}/traffic`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({weights})});
  if (!response.ok) return showApiError(response, $('#traffic-error'));
  trafficDialog.close(); await loadProviders();
});

const requestDefaultsDialog = $('#request-defaults-dialog');
let requestDefaultsProviderId = null;

function addDefaultsRow(container, kind, name = '', value = '') {
  const row = document.createElement('div');
  row.className = 'defaults-row';
  row.innerHTML = `<label><span>${kind === 'header' ? 'Header name' : 'Field name'}</span><input class="technical-input defaults-name" placeholder="${kind === 'header' ? 'x-api-version' : 'temperature'}"></label><label><span>${kind === 'header' ? 'Header value' : 'JSON value'}</span>${kind === 'header' ? '<input class="technical-input defaults-value" placeholder="2025-01-01">' : '<textarea class="technical-input defaults-value" rows="2" placeholder="0.7"></textarea>'}</label><button type="button" class="icon-button remove-default-row" aria-label="Remove">${icon('close')}</button><span class="row-error"></span>`;
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
      const managedHeaders = ['host', 'authorization', 'x-api-key', 'cookie', 'set-cookie', 'chatgpt-account-id', 'originator', 'content-length', 'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade'];
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
  $('#defaults-endpoint-selection').replaceChildren(...provider.endpoints.map(endpoint => { const label = document.createElement('label'); label.className = 'provider-check'; label.innerHTML = `<input type="checkbox" value="${escapeHtml(endpoint.id)}" ${selectedIds.includes(endpoint.id) ? 'checked' : ''}><span class="custom-check">${icon('check')}</span><span><strong>${escapeHtml(endpoint.id)}</strong><small>${escapeHtml(formatType(endpoint.api_type))}</small></span>`; label.querySelector('input').addEventListener('change', validateRequestDefaults); return label; }));
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
let editingRoutePattern = null;
function routeTargetOptions() { return providers.flatMap(provider => provider.endpoints.flatMap(endpoint => {
  if (endpoint.api_type === 'openai_codex' && endpoint.subscription_connected) { const option = new Option(`${provider.name} · ${endpoint.id} · ChatGPT subscription`, `${provider.id}\n${endpoint.id}\n`); option.dataset.provider = provider.id; option.dataset.endpoint = endpoint.id; return [option]; }
  return endpoint.api_keys.filter(key => key.enabled).map(key => { const option = new Option(`${provider.name} · ${endpoint.id} · ${key.name}`, `${provider.id}\n${endpoint.id}\n${key.id}`); option.dataset.provider = provider.id; option.dataset.endpoint = endpoint.id; return option; });
})); }
function routeTargetValue(target) { return `${target.provider_id}\n${target.endpoint_id}\n${target.api_key_id}`; }
function routeTargetEditors() { return $$('#route-targets .route-target-editor'); }
function distributeRouteShares(weights) {
  const count = weights.length;
  if (count === 1) return [100];
  const total = weights.reduce((sum, weight) => sum + Math.max(0, weight), 0) || count;
  const exact = weights.map(weight => Math.max(0, weight) / total * 100);
  const shares = exact.map(Math.floor);
  let remainder = 100 - shares.reduce((sum, share) => sum + share, 0);
  exact.map((value, index) => ({index, fraction: value - Math.floor(value)})).sort((a, b) => b.fraction - a.fraction).slice(0, remainder).forEach(item => shares[item.index]++);
  shares.forEach((share, index) => {
    if (share > 0) return;
    const donor = shares.reduce((best, value, candidate) => value > shares[best] ? candidate : best, 0);
    shares[index] = 1; shares[donor]--;
  });
  return shares;
}
function validateRouteSplit() {
  const editors = routeTargetEditors();
  const multiple = editors.length > 1;
  const activeEditors = editors.filter(editor => editor.querySelector('[name="target_enabled"]').checked);
  const total = activeEditors.reduce((sum, editor) => sum + Number(editor.querySelector('[name="target_weight"]').value || 0), 0);
  const validTotal = activeEditors.length > 0 && total === 100;
  const totalLabel = $('#route-split-total');
  totalLabel.textContent = `${total}%`;
  totalLabel.classList.toggle('invalid', multiple && !validTotal);
  totalLabel.setAttribute('aria-label', multiple && !validTotal ? `Invalid active total: ${total}%` : `Active total: ${total}%`);
  editors.forEach(editor => {
    const enabled = editor.querySelector('[name="target_enabled"]').checked;
    editor.classList.toggle('disabled-target', !enabled);
    editor.querySelector('.switch-status').textContent = enabled ? 'Active' : 'Disabled';
    editor.querySelector('[name="target_weight"]').disabled = !enabled;
  });
  const hasDestinations = editors.every(editor => editor.querySelector('.route-target').options.length > 0);
  $('#save-route').disabled = !hasDestinations || (multiple && !validTotal);
}
function updateRouteTargetMode(normalize = false) {
  const editors = routeTargetEditors();
  const multiple = editors.length > 1;
  $('#route-targets').classList.toggle('multiple', multiple);
  $('#route-split-head').hidden = !multiple;
  $('#add-route-target-label').textContent = multiple ? 'Add another destination' : 'Split traffic across destinations';
  const activeEditors = editors.filter(editor => editor.querySelector('[name="target_enabled"]').checked);
  const weights = activeEditors.map(editor => Number(editor.querySelector('[name="target_weight"]').value) || 1);
  const shares = multiple && normalize ? distributeRouteShares(weights) : multiple ? weights : [100];
  editors.forEach((editor, index) => {
    const input = editor.querySelector('[name="target_weight"]');
    if (!multiple) input.value = 100;
    else if (normalize && editor.querySelector('[name="target_enabled"]').checked) input.value = shares[activeEditors.indexOf(editor)];
    input.max = 100;
    const button = editor.querySelector('.remove-route-target');
    button.disabled = !multiple; button.setAttribute('aria-disabled', String(button.disabled));
  });
  validateRouteSplit();
}
function initializeRouteTarget(editor, target = null) {
  const select = editor.querySelector('.route-target');
  const existingValue = target ? routeTargetValue(target) : select.value;
  select.replaceChildren(...routeTargetOptions());
  if (existingValue && [...select.options].some(option => option.value === existingValue)) select.value = existingValue;
  const input = editor.querySelector('.upstream-model-input');
  const notice = editor.querySelector('.upstream-model-notice');
  const updateNotice = () => {
    const models = JSON.parse(input.dataset.suggestions || '[]');
    if (!models.length) notice.textContent = 'No models have been discovered for this endpoint yet. You can still enter a valid custom model ID.';
    else if (input.value.trim() && !models.includes(input.value.trim())) notice.textContent = 'Custom model ID — this value was not reported by the selected endpoint. It will still be saved.';
    else notice.textContent = `${models.length} discovered model${models.length === 1 ? '' : 's'} available as suggestions; custom IDs are also accepted.`;
  };
  const updateSuggestions = () => {
    const option = select.selectedOptions[0];
    const provider = providers.find(item => item.id === option?.dataset.provider);
    const endpointId = option?.dataset.endpoint;
    const models = provider?.discovered_models.filter(model => (provider.model_endpoints[model] || []).includes(endpointId)) || [];
    let list = editor.querySelector('datalist');
    if (!list) { list = document.createElement('datalist'); editor.append(list); }
    list.id = `upstream-model-suggestions-${crypto.randomUUID()}`;
    list.replaceChildren(...models.map(model => new Option(model)));
    input.setAttribute('list', list.id);
    input.dataset.suggestions = JSON.stringify(models);
    input.placeholder = models[0] ? `e.g. ${models[0]}` : 'e.g. model-name or org/model-name';
    updateNotice();
  };
  input.value = target?.upstream_model || input.value;
  if (target) {
    editor.querySelector('[name="target_weight"]').value = target.weight;
    editor.querySelector('[name="target_enabled"]').checked = target.enabled !== false;
  }
  select.addEventListener('change', updateSuggestions); input.addEventListener('input', updateNotice); updateSuggestions();
}
function addRouteTargetEditor(target = null) {
  const template = $('#route-targets .route-target-editor');
  const editor = template.cloneNode(true);
  editor.querySelector('[name="upstream_model"]').value = '';
  editor.querySelector('[name="target_weight"]').value = 100;
  editor.querySelector('[name="target_enabled"]').checked = true;
  editor.querySelector('datalist')?.remove();
  $('#route-targets').append(editor); initializeRouteTarget(editor, target);
  return editor;
}
function openRouteDialog(route = null) {
  const form = $('#route-form'); form.reset(); $('#route-error').textContent = ''; editingRoutePattern = route?.pattern || null;
  routeDialog.querySelector('h2').textContent = route ? 'Edit model route' : 'Add model route';
  routeDialog.querySelector('.dialog-head p').textContent = route ? 'Update the public alias and its destination.' : 'Create a short alias for an upstream model.';
  $('#save-route').textContent = route ? 'Save changes' : 'Save route';
  const editors = $$('#route-targets .route-target-editor'); editors.slice(1).forEach(editor => editor.remove());
  if (route) {
    form.elements.pattern.value = route.pattern;
    initializeRouteTarget(editors[0], route.targets[0]);
    route.targets.slice(1).forEach(addRouteTargetEditor);
  } else initializeRouteTarget(editors[0]);
  updateRouteTargetMode(Boolean(route && route.targets.length > 1));
  const hasDestinations = $('#route-targets .route-target').options.length > 0;
  $('#route-error').textContent = hasDestinations ? '' : 'Connect an upstream credential before creating a route.';
  validateRouteSplit();
  routeDialog.showModal();
}
$('#models-view').addEventListener('click', event => {
  if (event.target.closest('#open-route, #empty-add-route')) openRouteDialog();
});
$('#add-route-target').addEventListener('click', () => { addRouteTargetEditor(); updateRouteTargetMode(true); });
$('#route-targets').addEventListener('input', event => {
  if (event.target.matches('[name="target_weight"]')) validateRouteSplit();
  if (event.target.matches('[name="target_enabled"]')) updateRouteTargetMode(true);
});
$('#route-targets').addEventListener('click', event => {
  const remove = event.target.closest('.remove-route-target');
  if (!remove || remove.disabled) return;
  remove.closest('.route-target-editor').remove(); updateRouteTargetMode(true);
});
$$('.close-route').forEach(button => button.addEventListener('click', () => routeDialog.close()));
$('#route-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const targets = [...event.target.querySelectorAll('.route-target-editor')].map(editor => { const [provider_id, endpoint_id, api_key_id] = editor.querySelector('.route-target').value.split('\n'); return {provider_id, endpoint_id, api_key_id, upstream_model: editor.querySelector('[name="upstream_model"]').value, weight: Number(editor.querySelector('[name="target_weight"]').value), enabled: editor.querySelector('[name="target_enabled"]').checked}; });
  const response = await fetch(editingRoutePattern ? `/admin/routes/${encodeURIComponent(editingRoutePattern)}` : '/admin/routes', {method: editingRoutePattern ? 'PATCH' : 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({pattern: data.get('pattern'), targets})});
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

function routeTargetSummary(target, activeWeightTotal) {
  const provider = providers.find(item => item.id === target.provider_id);
  const endpoint = provider?.endpoints.find(item => item.id === target.endpoint_id);
  const credential = endpoint?.api_type === 'openai_codex'
    ? 'ChatGPT subscription'
    : endpoint?.api_keys.find(key => key.id === target.api_key_id)?.name || target.api_key_id;
  const enabled = target.enabled !== false;
  const share = enabled && activeWeightTotal > 0 ? Math.round(Number(target.weight) / activeWeightTotal * 100) : 0;
  return `<article class="route-destination${enabled ? '' : ' is-disabled'}">
    <div class="route-destination-mark" aria-hidden="true">${icon('arrow-right')}</div>
    <div class="route-destination-main">
      <div class="route-upstream"><span>${escapeHtml(target.provider_id)}</span><b>/</b><code>${escapeHtml(target.upstream_model)}</code></div>
      <div class="route-destination-meta"><span title="Endpoint">Endpoint <code>${escapeHtml(target.endpoint_id)}</code></span><span title="Credential">Credential <strong>${escapeHtml(credential || 'Default')}</strong></span></div>
    </div>
    <div class="route-target-state"><span class="route-status ${enabled ? 'active' : 'disabled'}"><i></i>${enabled ? 'Active' : 'Disabled'}</span><strong>${share}%</strong><small>traffic</small></div>
  </article>`;
}

function renderRoutes() {
  $('#routes-empty').hidden = modelRoutes.length > 0; $('#routes-table').hidden = modelRoutes.length === 0;
  $('#routes').replaceChildren(...modelRoutes.map((route, index) => {
    const row = document.createElement('tr');
    const activeWeightTotal = route.targets.filter(target => target.enabled !== false).reduce((total, target) => total + Number(target.weight || 0), 0);
    const destinations = route.targets.map(target => routeTargetSummary(target, activeWeightTotal)).join('');
    const matchKind = route.pattern.endsWith('*') ? 'Prefix match' : 'Exact match';
    row.innerHTML = `<td class="route-model-cell"><span class="route-match-kind">${matchKind}</span><code>${escapeHtml(route.pattern)}</code><small>${route.targets.filter(target => target.enabled !== false).length} active destination${route.targets.filter(target => target.enabled !== false).length === 1 ? '' : 's'}</small></td><td><div class="route-destinations">${destinations}</div></td><td><div class="route-row-actions"><button class="edit-route text-link" data-index="${index}">Edit</button><button class="delete-route text-link danger-link" data-pattern="${encodeURIComponent(route.pattern)}">Delete</button></div></td>`;
    return row;
  }));
  $$('.edit-route').forEach(button => button.addEventListener('click', () => openRouteDialog(modelRoutes[Number(button.dataset.index)])));
  $$('.delete-route').forEach(button => button.addEventListener('click', async () => { if (confirm('Delete this model route?')) { await fetch(`/admin/routes/${button.dataset.pattern}`, {method: 'DELETE'}); await loadRoutes(); } }));
  renderModelReference();
}

function renderModelReference() {
  $('#model-reference').replaceChildren(...providers.map(provider => {
    const item = document.createElement('article'); item.className = 'model-provider';
    const status = provider.model_discovery_error ? `<span class="error-text">Discovery failed: ${escapeHtml(provider.model_discovery_error)}</span>` : provider.models_discovered_at ? `${provider.discovered_models.length} models` : 'Discovery has not run yet';
    item.innerHTML = `<header><div><strong>${escapeHtml(provider.name)}</strong><small>${status}</small></div><div class="model-actions"><button class="text-link refresh-models" data-provider="${provider.id}">Refresh</button>${provider.discovered_models.length ? '<button class="text-link toggle-models">Browse models</button>' : ''}</div></header>${provider.discovered_models.length ? `<div class="model-browser" hidden><div class="model-filter">${icon('search')}<input type="search" placeholder="Filter ${provider.discovered_models.length} models"></div><div class="model-list"></div></div>` : ''}`;
    if (provider.discovered_models.length) { const list = item.querySelector('.model-list'); const render = query => { const matches = provider.discovered_models.filter(model => model.toLowerCase().includes(query.toLowerCase())); list.replaceChildren(...matches.slice(0, 100).map(model => { const code = document.createElement('code'); code.textContent = model; return code; })); }; render(''); item.querySelector('.model-filter input').addEventListener('input', event => render(event.target.value)); item.querySelector('.toggle-models').addEventListener('click', event => { const browser = item.querySelector('.model-browser'); browser.hidden = !browser.hidden; event.currentTarget.textContent = browser.hidden ? 'Browse models' : 'Hide models'; }); }
    return item;
  }));
  $$('#model-reference .refresh-models').forEach(button => button.addEventListener('click', () => refreshModels(button.dataset.provider, button)));
}

async function loadAbout() {
  const response = await fetch('/about');
  if (!response.ok) return;
  aboutInfo = await response.json();
  const commitDate = aboutInfo.commit_time === 'unknown' ? 'unknown time' : aboutInfo.commit_time.slice(0, 10);
  $('#sidebar-build').textContent = `${aboutInfo.commit} · ${commitDate}`;
}
const aboutDialog = $('#about-dialog');
$$('.open-about').forEach(button => button.addEventListener('click', async () => {
  closeAccountMenu();
  if (!aboutInfo) await loadAbout();
  if (!aboutInfo) return;
  $('#about-commit').textContent = aboutInfo.commit;
  $('#about-commit-time').textContent = aboutInfo.commit_time; $('#about-commit-time').dateTime = aboutInfo.commit_time === 'unknown' ? '' : aboutInfo.commit_time;
  $('#about-license').textContent = aboutInfo.license; $('#about-license-text').textContent = aboutInfo.license_text;
  aboutDialog.showModal();
}));
$$('.close-about').forEach(button => button.addEventListener('click', () => aboutDialog.close()));

const helpDialog = $('#help-dialog');
function openHelp(context = 'general', providerId = null) {
  helpDialog.dataset.context = context;
  selectHelpTab('agent');
  helpDialog.dataset.provider = providerId || '';
  updateHelpGuide();
  helpDialog.showModal();
}
function helpModels() {
  const discovered = providers.flatMap(provider => provider.discovered_models.map(model => `${provider.id}/${model}`));
  return [...new Set([...modelRoutes.map(route => route.pattern).filter(pattern => !pattern.endsWith('*')), ...discovered])].sort();
}
function updateHelpGuide() {
  const modelSelect = $('#help-model'); const keySelect = $('#help-key');
  const previousModel = modelSelect.value; const previousKey = keySelect.value;
  const models = helpModels();
  modelSelect.replaceChildren(...models.map(model => new Option(model, model)));
  if (!models.length) modelSelect.append(new Option('No discovered models yet', ''));
  if (models.includes(previousModel)) modelSelect.value = previousModel;
  keySelect.replaceChildren(...authSettings.api_keys.map(key => new Option(`${key.note || 'Gateway API key'} · ${key.prefix}`, key.secret || '')));
  if (!authSettings.api_keys.length) keySelect.append(new Option('Generate a Gateway API key first', ''));
  if ([...keySelect.options].some(option => option.value === previousKey)) keySelect.value = previousKey;
  const baseUrl = `${location.origin}/v1`; const model = modelSelect.value || 'provider/model-id'; const key = keySelect.value || 'sk-your-yabane-key';
  const context = helpDialog.dataset.context || 'general';
  const provider = providers.find(item => item.id === helpDialog.dataset.provider);
  const title = context === 'provider' && provider ? `Connect clients to ${provider.name}` : context === 'access' ? 'Connect clients to Yabane' : 'Connect your Agent to Yabane';
  helpDialog.querySelector('h2').textContent = title;
  $('#help-provider-check').innerHTML = `<b>${providers.length ? icon('check') : '1'}</b><span><strong>Connect a Provider</strong><small>${providers.length ? `${providers.length} configured` : 'Add an Endpoint and upstream credential'}</small></span>`;
  $('#help-key-check').innerHTML = `<b>${authSettings.api_keys.length ? icon('check') : '2'}</b><span><strong>Generate a Gateway key</strong><small>${authSettings.api_keys.length ? `${authSettings.api_keys.length} available` : 'Required while authentication is enabled'}</small></span>`;
  $('#help-model-check').innerHTML = `<b>${models.length ? icon('check') : '3'}</b><span><strong>Select a model</strong><small>${models.length ? `${models.length} available` : 'Refresh Provider model discovery'}</small></span>`;
  $('#help-pi-code').textContent = JSON.stringify({providers: {yabane: {baseUrl, api: 'openai-completions', apiKey: '$YABANE_API_KEY', models: [{id: model, name: model}]} }}, null, 2);
  $('#help-pi-env').textContent = `export YABANE_API_KEY='${key}'`;
  $('#help-pi-run').textContent = `pi --provider yabane --model '${model}'`;
  $('#help-opencode-code').textContent = JSON.stringify({$schema: 'https://opencode.ai/config.json', provider: {yabane: {npm: '@ai-sdk/openai-compatible', name: 'Yabane', options: {baseURL: baseUrl, apiKey: '{env:YABANE_API_KEY}'}, models: {[model]: {name: model}}}}, model: `yabane/${model}`}, null, 2);
  $('#help-opencode-env').textContent = `export YABANE_API_KEY='${key}'`;
  $('#help-claude-env').textContent = `export ANTHROPIC_BASE_URL='${baseUrl}'\nexport ANTHROPIC_API_KEY='${key}'\nclaude`;
  $('#help-codex-code').textContent = `[model_providers.yabane]\nname = "Yabane"\nbase_url = "${baseUrl}"\nwire_api = "responses"\nenv_key = "YABANE_API_KEY"\n\nmodel_provider = "yabane"\nmodel = "${model}"`;
  $('#help-codex-env').textContent = `export YABANE_API_KEY='${key}'`;
  $('#help-base-url').textContent = baseUrl; $('#help-api-key').textContent = key; $('#help-model-id').textContent = model;
  $('#help-curl-code').textContent = `curl '${baseUrl}/chat/completions' \\\n  -H 'Authorization: Bearer ${key}' \\\n  -H 'content-type: application/json' \\\n  -d '${JSON.stringify({model, messages: [{role: 'user', content: 'Hello'}]})}'`;
  selectHelpTab($('.help-tabs [data-help-tab].active')?.dataset.helpTab || 'agent');
}
document.addEventListener('click', event => { const button = event.target.closest('.contextual-help'); if (button) openHelp(button.dataset.helpContext, button.dataset.provider); });
$$('.close-help').forEach(button => button.addEventListener('click', () => helpDialog.close()));
$('#help-model').addEventListener('change', updateHelpGuide); $('#help-key').addEventListener('change', updateHelpGuide);
let helpHeightTransitionEnd;
function selectHelpTab(tab) {
  const body = $('.help-body');
  const startHeight = body.getBoundingClientRect().height;
  const animateHeight = helpDialog.open && startHeight > 0;
  if (helpHeightTransitionEnd) body.removeEventListener('transitionend', helpHeightTransitionEnd);
  body.style.transition = 'none';
  body.style.height = '';
  $$('.help-tabs [data-help-tab]').forEach(item => { const active = item.dataset.helpTab === tab; item.classList.toggle('active', active); item.setAttribute('aria-selected', String(active)); });
  const agent = $('#help-agent')?.value || 'pi';
  $('.help-agent-select').hidden = tab !== 'agent';
  $$('[data-help-panel]').forEach(panel => {
    const visible = tab === 'agent' ? panel.dataset.helpAgent === 'true' && panel.dataset.helpPanel === agent : panel.dataset.helpPanel === tab;
    panel.hidden = !visible;
  });
  const endHeight = body.getBoundingClientRect().height;
  if (!animateHeight) {
    body.style.transition = '';
    return;
  }
  body.style.height = `${startHeight}px`;
  body.getBoundingClientRect();
  body.style.transition = '';
  requestAnimationFrame(() => {
    body.style.height = `${endHeight}px`;
    helpHeightTransitionEnd = event => {
      if (event.propertyName !== 'height') return;
      body.removeEventListener('transitionend', helpHeightTransitionEnd);
      helpHeightTransitionEnd = null;
      body.style.height = '';
    };
    body.addEventListener('transitionend', helpHeightTransitionEnd);
  });
}
$$('[data-help-tab]').forEach(button => button.addEventListener('click', () => selectHelpTab(button.dataset.helpTab)));
$('#help-agent').addEventListener('change', () => { if ($('.help-tabs [data-help-tab].active')?.dataset.helpTab === 'agent') selectHelpTab('agent'); });
$$('.copy-help-code').forEach(button => button.addEventListener('click', async () => { await navigator.clipboard.writeText($(`#${button.dataset.copy}`).textContent); const original = button.textContent; button.textContent = 'Copied'; setTimeout(() => { button.textContent = original; }, 1200); }));

const gatewayKeyDialog = $('#gateway-key-dialog');
$('#open-gateway-key').addEventListener('click', () => {
  $('#gateway-key-form').reset(); $('#gateway-key-error').textContent = '';
  gatewayProviderChecks($('#gateway-key-providers'));
  gatewayKeyDialog.showModal();
});
$('#empty-gateway-key').addEventListener('click', () => $('#open-gateway-key').click());
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
  const created = await response.json(); gatewayKeyDialog.close(); $('#generated-key').value = created.secret; $('#generated-key-description').textContent = 'Copy this key now or retrieve it later from the API key list.'; $('#generated-key-dialog').showModal(); await loadAuth();
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
  $('#copy-key-status').textContent = 'Store it securely and do not share it.'; $('#generated-key-dialog').close();
});

function gatewayProviderChecks(container, selectedProviderIds = []) {
  container.replaceChildren(...providers.map(provider => { const label = document.createElement('label'); label.className = 'provider-check'; label.innerHTML = `<input type="checkbox" name="provider_ids" value="${escapeHtml(provider.id)}"${selectedProviderIds.includes(provider.id) ? ' checked' : ''}><span class="custom-check">${icon('check')}</span><span><strong>${escapeHtml(provider.name)}</strong><small>${escapeHtml(provider.id)}</small></span>`; return label; }));
}
function localDateTimeValue(timestamp) {
  if (!timestamp) return '';
  const date = new Date(timestamp * 1000); const offset = date.getTimezoneOffset() * 60000;
  return new Date(date.getTime() - offset).toISOString().slice(0, 16);
}
const editGatewayKeyDialog = $('#edit-gateway-key-dialog');
function openEditGatewayKey(key) {
  const form = $('#edit-gateway-key-form'); form.reset(); $('#edit-gateway-key-error').textContent = '';
  form.elements.id.value = key.id; form.elements.note.value = key.note || ''; form.elements.expires_at.value = localDateTimeValue(key.expires_at);
  gatewayProviderChecks($('#edit-gateway-key-providers'), key.provider_ids);
  editGatewayKeyDialog.showModal();
}
$$('.close-edit-gateway-key').forEach(button => button.addEventListener('click', () => editGatewayKeyDialog.close()));
$('#edit-gateway-key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const expiry = data.get('expires_at');
  const payload = {note: data.get('note'), expires_at: expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null, provider_ids: data.getAll('provider_ids')};
  const response = await fetch(`/admin/auth/keys/${encodeURIComponent(data.get('id'))}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#edit-gateway-key-error'));
  editGatewayKeyDialog.close(); await loadAuth();
});
function renderAuth() {
  $('#auth-enabled').checked = authSettings.enabled;
  $('.switch-status').textContent = authSettings.enabled ? 'Enabled' : 'Disabled';
  $('#gateway-keys-empty').hidden = authSettings.api_keys.length > 0; $('#gateway-keys-table').hidden = authSettings.api_keys.length === 0;
  $('#gateway-keys').replaceChildren(...authSettings.api_keys.map(key => {
    const row = document.createElement('tr'); const expired = key.expires_at && key.expires_at * 1000 <= Date.now(); const expiry = key.expires_at ? new Date(key.expires_at * 1000).toLocaleString() : 'Never'; const access = key.provider_ids.length ? key.provider_ids.join(', ') : 'All providers';
    row.innerHTML = `<td><div class="listed-key"><code>${escapeHtml(key.prefix)}</code>${key.secret ? `<button class="icon-copy-key" title="Copy API key" aria-label="Copy API key">${copyIcon()}</button>` : ''}</div></td><td>${escapeHtml(key.note || '—')}</td><td>${escapeHtml(access)}</td><td><span class="key-expiry${expired ? ' expired' : ''}">${expired ? 'Expired · ' : ''}${escapeHtml(expiry)}</span></td><td><div class="gateway-key-actions"><button class="edit-gateway-key text-link" data-key="${key.id}">Edit</button><button class="delete-gateway-key text-link danger-link" data-key="${key.id}">Delete</button></div></td>`;
    const copyButton = row.querySelector('.icon-copy-key');
    copyButton?.addEventListener('click', async () => {
      await navigator.clipboard.writeText(key.secret);
      copyButton.classList.add('copied'); copyButton.title = 'Copied'; copyButton.setAttribute('aria-label', 'API key copied');
      setTimeout(() => { copyButton.classList.remove('copied'); copyButton.title = 'Copy API key'; copyButton.setAttribute('aria-label', 'Copy API key'); }, 1200);
    });
    return row;
  }));
  $$('.edit-gateway-key').forEach(button => button.addEventListener('click', () => openEditGatewayKey(authSettings.api_keys.find(key => key.id === button.dataset.key))));
  $$('.delete-gateway-key').forEach(button => button.addEventListener('click', async () => { if (confirm('Delete this API key?')) { await fetch(`/admin/auth/keys/${button.dataset.key}`, {method: 'DELETE'}); await loadAuth(); } }));
}
async function loadAuth() { const response = await fetch('/admin/auth'); authSettings = await response.json(); renderAuth(); }

const managementKeyDialog = $('#management-key-dialog');
$('#open-management-key').addEventListener('click', () => { $('#management-key-form').reset(); $('#management-key-error').textContent = ''; managementKeyDialog.showModal(); });
$('#empty-management-key').addEventListener('click', () => $('#open-management-key').click());
$$('.close-management-key').forEach(button => button.addEventListener('click', () => managementKeyDialog.close()));
$('#management-key-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const expiry = data.get('expires_at');
  const response = await fetch('/admin/management-keys', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({name: data.get('name'), expires_at: expiry ? Math.floor(new Date(expiry).getTime() / 1000) : null})});
  if (!response.ok) return showApiError(response, $('#management-key-error'));
  const created = await response.json(); managementKeyDialog.close(); $('#generated-key').value = created.secret; $('#generated-key-description').textContent = 'Copy this key now. Management API key secrets are shown only once.'; $('#generated-key-dialog').showModal(); await loadManagementKeys();
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
  {label: 'Model routing', description: 'Route model IDs to upstream credentials', view: 'models'},
  {label: 'Provider models', description: 'Discover models from providers', view: 'models'},
  {label: 'Extensions', description: 'Compiled request Hooks and capabilities', view: 'extensions'},
  {label: 'Request Defaults extension', description: 'Extra Header and Extra JSON Body Hooks', view: 'extensions'},
  {label: 'API access', description: 'Authentication and gateway keys', view: 'access'},
  {label: 'Generate Gateway API key', description: 'Create an inference credential', view: 'access', action: () => $('#open-gateway-key').click()},
  {label: 'Activity', description: 'Requests, tokens, latency, upstream cost, and routing logs', view: 'activity'},
  {label: 'Management API', description: 'Programmatic control keys and live docs', view: 'management'},
  {label: 'Create Management API key', description: 'Create a control-plane credential', view: 'management', action: () => $('#open-management-key').click()},
  {label: 'Getting started', description: 'Connect an Agent to Yabane', view: 'home', action: () => openHelp()},
  {label: 'Live API docs', description: 'Interactive OpenAPI documentation', view: 'management', action: () => location.assign('/docs')}
];
let selectedSearchIndex = -1;
function searchResultButtons() { return $$('#search-results button[role="option"]'); }
function selectSearchResult(index) {
  const buttons = searchResultButtons(); if (!buttons.length) return;
  selectedSearchIndex = (index + buttons.length) % buttons.length;
  buttons.forEach((button, position) => { const selected = position === selectedSearchIndex; button.classList.toggle('selected', selected); button.setAttribute('aria-selected', String(selected)); });
  const selected = buttons[selectedSearchIndex]; $('#settings-search').setAttribute('aria-activedescendant', selected.id); selected.scrollIntoView({block: 'nearest'});
}
function closeSearch() { const input = $('#settings-search'); $('#search-results').hidden = true; input.setAttribute('aria-expanded', 'false'); input.removeAttribute('aria-activedescendant'); selectedSearchIndex = -1; }
$('#settings-search').addEventListener('input', event => {
  const query = event.target.value.trim().toLowerCase(); const results = $('#search-results'); selectedSearchIndex = -1;
  if (!query) return closeSearch();
  const matches = [...searchItems, ...providers.map(provider => ({label: provider.name, description: `Provider · ${provider.id}`, view: 'providers', action: () => { selectedProviderId = provider.id; renderProviderPage(); }}))].filter(item => `${item.label} ${item.description}`.toLowerCase().includes(query));
  results.replaceChildren(...matches.map((item, index) => { const button = document.createElement('button'); button.id = `search-option-${index}`; button.setAttribute('role', 'option'); button.setAttribute('aria-selected', 'false'); button.innerHTML = `<strong>${escapeHtml(item.label)}</strong><small>${escapeHtml(item.description)}</small>`; button.addEventListener('mouseenter', () => selectSearchResult(index)); button.addEventListener('click', () => { showView(item.view); item.action?.(); $('#settings-search').value = ''; closeSearch(); }); return button; }));
  if (!matches.length) { const empty = document.createElement('span'); empty.className = 'search-empty'; empty.textContent = 'No settings found'; results.replaceChildren(empty); }
  results.hidden = false; event.target.setAttribute('aria-expanded', 'true');
  if (matches.length) selectSearchResult(0);
});
$('#settings-search').addEventListener('keydown', event => {
  if (event.key === 'Escape') { event.preventDefault(); event.target.value = ''; closeSearch(); return; }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); selectSearchResult(selectedSearchIndex + (event.key === 'ArrowDown' ? 1 : -1)); return; }
  if (event.key === 'Home' && !$('#search-results').hidden) { event.preventDefault(); selectSearchResult(0); return; }
  if (event.key === 'End' && !$('#search-results').hidden) { event.preventDefault(); selectSearchResult(searchResultButtons().length - 1); return; }
  if (event.key === 'Enter') { event.preventDefault(); searchResultButtons()[Math.max(0, selectedSearchIndex)]?.click(); }
});
document.addEventListener('click', event => { if (!event.target.closest('.search')) closeSearch(); });

function copyIcon() { return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="9" y="9" width="10" height="10" rx="2"></rect><path d="M15 9V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h2"></path></svg>'; }

async function showApiError(response, target) { const body = await response.json(); const message = body.error?.message || `Request failed (${response.status})`; if (target) target.textContent = message; else alert(message); }
function compactNumber(value) { return Intl.NumberFormat('en', {notation: 'compact', maximumFractionDigits: 1}).format(value || 0); }
function formatCost(value) { return value == null ? '—' : `$${new Intl.NumberFormat('en', {minimumFractionDigits: 2, maximumFractionDigits: 6}).format(value)}`; }
let activityLogs = [];
let activityOverviewLogs = [];
const activityColors = ['#0b57d0', '#7c4dff', '#00a67e', '#ff8f00', '#d93025', '#00897b'];
function activityBuckets(logs, seconds, end = Math.floor(Date.now() / 1000)) {
  const count = seconds <= 3600 ? 12 : seconds <= 86400 ? 24 : seconds <= 604800 ? 14 : 30;
  const start = end - seconds; const width = seconds / count;
  return Array.from({length: count}, (_, index) => ({start: start + index * width, requests: 0, tokens: 0, cached: 0, cost: 0, latency: 0, samples: 0, successful: 0, errors: 0})).map((bucket, index, buckets) => {
    logs.filter(log => log.timestamp >= bucket.start && (index === buckets.length - 1 || log.timestamp < bucket.start + width)).forEach(log => { bucket.requests++; bucket.tokens += log.input_tokens + log.output_tokens; bucket.cached += log.cached_tokens; bucket.cost += log.cost || 0; bucket.latency += log.latency_ms; bucket.samples++; bucket.successful += Number(log.status < 400); bucket.errors += Number(log.status >= 400); }); return bucket;
  });
}
function sparkline(values, color) {
  const max = Math.max(...values, 1); const points = values.map((value, index) => `${index * 100 / Math.max(values.length - 1, 1)},${30 - value * 26 / max}`).join(' ');
  return `<svg viewBox="0 0 100 32" preserveAspectRatio="none" aria-hidden="true"><polyline points="${points}" fill="none" stroke="${color}" stroke-width="2" vector-effect="non-scaling-stroke"/></svg>`;
}
let activityChartMetric = 'requests';
let activityChartBuckets = [];
let activityChartSeconds = 86400;
function activityBucketLabel(bucket, seconds, full = false) {
  const start = new Date(bucket.start * 1000); const end = new Date((bucket.start + seconds / activityChartBuckets.length) * 1000);
  if (!full) return seconds <= 86400 ? start.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : start.toLocaleDateString([], {month: 'short', day: 'numeric'});
  const startLabel = seconds <= 86400 ? start.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : start.toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'});
  const endLabel = seconds <= 86400 ? end.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : end.toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'});
  return `${startLabel} – ${endLabel}`;
}
function activityMetricValue(bucket, metric) { return metric === 'tokens' ? bucket.tokens : metric === 'latency' ? (bucket.samples ? bucket.latency / bucket.samples : 0) : bucket.requests; }
function activityMetricLabel(value, metric) { return metric === 'tokens' ? compactNumber(value) : metric === 'latency' ? formatDuration(Math.round(value)) : value.toLocaleString(); }
function inspectActivityBucket(index) {
  const bucket = activityChartBuckets[index]; if (!bucket) return;
  $$('.chart-column').forEach((column, columnIndex) => column.classList.toggle('selected', columnIndex === index));
  $('#chart-inspector-time').textContent = activityBucketLabel(bucket, activityChartSeconds, true);
  const averageLatency = bucket.samples ? Math.round(bucket.latency / bucket.samples) : null;
  const successRate = bucket.requests ? (bucket.successful * 100 / bucket.requests).toFixed(1) + '%' : '—';
  $('#chart-inspector-values').innerHTML = [['Requests', bucket.requests.toLocaleString()], ['Tokens', compactNumber(bucket.tokens)], ['Success', successRate], ['Avg latency', formatDuration(averageLatency)], ['Cached', compactNumber(bucket.cached)], ['Cost', formatCost(bucket.cost || null)]].map(([label, value]) => `<div><span>${label}</span><strong>${value}</strong></div>`).join('');
}
function renderActivityChart(buckets = activityChartBuckets, seconds = activityChartSeconds) {
  activityChartBuckets = buckets; activityChartSeconds = seconds;
  const values = buckets.map(bucket => activityMetricValue(bucket, activityChartMetric)); const max = Math.max(...values, 1);
  const nonEmpty = buckets.reduce((last, bucket, index) => bucket.requests ? index : last, -1); const selected = nonEmpty >= 0 ? nonEmpty : buckets.length - 1;
  $('#activity-chart').innerHTML = `<div class="chart-grid"><span><b>${activityMetricLabel(max, activityChartMetric)}</b></span><span></span><span><b>0</b></span></div><div class="chart-bars">${buckets.map((bucket, index) => { const value = values[index]; const label = activityBucketLabel(bucket, seconds); const detail = `${bucket.requests} requests, ${compactNumber(bucket.tokens)} tokens, ${bucket.errors} errors, ${formatCost(bucket.cost || null)}`; return `<button class="chart-column" type="button" data-chart-index="${index}" aria-label="${escapeHtml(`${label}: ${detail}`)}"><span class="chart-value">${activityMetricLabel(value, activityChartMetric)}</span><span class="chart-bar" style="height:${Math.max(value * 100 / max, value ? 3 : 0)}%"></span><small>${index % Math.ceil(buckets.length / 6) === 0 || index === buckets.length - 1 ? label : ''}</small></button>`; }).join('')}</div>`;
  inspectActivityBucket(selected);
}
function aggregateActivity(logs, key) {
  const map = new Map(); logs.forEach(log => { const name = log[key]; const item = map.get(name) || {name, requests: 0, tokens: 0, errors: 0, latency: 0}; item.requests++; item.tokens += log.input_tokens + log.output_tokens; item.errors += Number(log.status >= 400); item.latency += log.latency_ms; map.set(name, item); }); return [...map.values()].sort((a, b) => b.requests - a.requests);
}
function renderRankings(target, items) {
  const max = Math.max(...items.map(item => item.requests), 1); target.innerHTML = items.length ? items.slice(0, 5).map((item, index) => `<div class="activity-ranking"><span class="ranking-number">${index + 1}</span><span class="ranking-dot" style="background:${activityColors[index % activityColors.length]}"></span><div><strong>${escapeHtml(item.name)}</strong><span class="ranking-track"><i style="width:${item.requests * 100 / max}%;background:${activityColors[index % activityColors.length]}"></i></span></div><span><strong>${item.requests}</strong><small>requests</small></span><span><strong>${compactNumber(item.tokens)}</strong><small>tokens</small></span></div>`).join('') : '<div class="activity-empty">No activity in this period.</div>';
}
function statusBadge(status) { const success = status >= 200 && status < 400; return `<span class="status-badge ${success ? 'success' : 'failure'}"><i></i>${status}</span>`; }
function failureLabel(log) { return log.failure?.message || (log.status >= 400 ? `HTTP ${log.status}` : ''); }
function compactPath(path) { return path.replace('/v1/', '').replace('chat/completions', 'Chat').replace('responses', 'Responses').replace('messages', 'Messages'); }
function activityRow(log, detailed = false, index = -1) {
  const tokens = log.input_tokens + log.output_tokens; const time = new Date(log.timestamp * 1000);
  const conversion = log.caller_protocol && log.upstream_protocol && log.caller_protocol !== log.upstream_protocol ? ` · ${log.caller_protocol.replace('openai_', '').replace('_completions', '')} → ${log.upstream_protocol.replace('openai_', '').replace('_completions', '')}` : '';
  const row = detailed ? `<td><span class="activity-time"><strong>${time.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'})}</strong><small>${time.toLocaleDateString()}</small></span></td><td><span class="activity-model"><code>${escapeHtml(log.model)}</code><small title="${escapeHtml(log.request_id)}">${escapeHtml(log.request_id)}</small></span></td><td><span class="route-cell"><strong>${escapeHtml(log.provider)} <i>→</i> ${escapeHtml(log.endpoint)}</strong><small>Provider to Endpoint</small></span></td><td><span class="api-kind">${escapeHtml(compactPath(log.path))}${log.streaming ? ' · stream' : ''}${escapeHtml(conversion)}</span></td><td><span class="activity-status-cell">${statusBadge(log.status)}${log.failure ? `<small title="${escapeHtml(failureLabel(log))}">${escapeHtml(failureLabel(log))}</small>` : ''}</span></td><td><strong class="activity-number">${formatDuration(log.latency_ms)}</strong></td><td><span class="activity-number">${compactNumber(log.input_tokens)}</span></td><td><span class="activity-number activity-output">${compactNumber(log.output_tokens)}</span></td><td class="activity-secondary-column"><span class="activity-number">${compactNumber(log.cached_tokens)}</span></td><td class="activity-secondary-column"><span class="activity-number">${formatCost(log.cost)}</span></td>` : `<td><span class="activity-time">${time.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'})}<small>${time.toLocaleDateString()}</small></span></td><td><code>${escapeHtml(log.model)}</code></td><td><span class="route-cell"><strong>${escapeHtml(log.provider)}</strong><small>${escapeHtml(log.endpoint)} · ${escapeHtml(compactPath(log.path))}${escapeHtml(conversion)}</small></span></td><td>${statusBadge(log.status)}</td><td>${log.latency_ms.toLocaleString()} ms</td><td><strong>${compactNumber(tokens)}</strong><small class="token-detail">${compactNumber(log.input_tokens)} in · ${compactNumber(log.output_tokens)} out</small></td>`;
  return `<tr class="activity-request-row" data-activity-index="${index}" tabindex="0" aria-label="Open details for ${escapeHtml(log.model)} request">${row}</tr>`;
}
function formatDuration(milliseconds) { return milliseconds == null ? 'Not available' : milliseconds >= 1000 ? `${(milliseconds / 1000).toFixed(milliseconds >= 10000 ? 1 : 2)} s` : `${milliseconds.toLocaleString()} ms`; }
function protocolLabel(protocol) { return protocol ? protocol.replace('openai_', 'OpenAI ').replace('anthropic_messages', 'Anthropic Messages').replace('_completions', ' Completions') : 'Unknown'; }
function openActivityDetail(log) {
  const dialog = $('#activity-detail-dialog'); const firstByte = log.first_byte_ms; const total = log.latency_ms; const generation = log.generation_ms ?? (firstByte == null ? null : Math.max(total - firstByte, 0));
  $('#activity-detail-model').textContent = log.model; $('#activity-detail-time').textContent = new Date(log.timestamp * 1000).toLocaleString(); $('#activity-detail-status').innerHTML = statusBadge(log.status);
  $('#activity-detail-route').textContent = `${log.provider} / ${log.endpoint}`; $('#activity-detail-api').textContent = `${compactPath(log.path)}${log.streaming ? ' · Streaming' : ''}`;
  const failure = $('#activity-detail-failure'); const failureMessage = log.failure?.message; const failureCategory = log.failure?.category; failure.hidden = !failureMessage; failure.querySelector('p').textContent = failureMessage || ''; failure.querySelector('small').textContent = failureCategory === 'proxy_connect_failed' ? 'Check that the proxy is reachable. If HTTPS works with socks5h but not socks5, let the proxy resolve target hostnames.' : 'Use the request ID below to match this failure with server logs if more detail is needed.';
  const timing = [['Gateway', log.gateway_ms, '#7c4dff'], ['Upstream response', log.upstream_response_ms, '#00a67e'], ['Time to first byte', firstByte, '#0b57d0'], ['Generation after first byte', generation, '#ff8f00'], ['Total', total, '#202124']];
  $('#activity-detail-timing').innerHTML = timing.map(([label, value, color]) => `<div><span>${escapeHtml(label)}</span><i><b style="width:${value == null ? 0 : Math.max(2, value * 100 / Math.max(total, 1))}%;background:${color}"></b></i><strong>${escapeHtml(formatDuration(value))}</strong></div>`).join('');
  $('#activity-detail-usage').innerHTML = [['Input tokens', compactNumber(log.input_tokens)], ['Output tokens', compactNumber(log.output_tokens)], ['Cached tokens', compactNumber(log.cached_tokens)], ['Throughput', generation && log.output_tokens ? `${(log.output_tokens * 1000 / generation).toFixed(1)} tok/s` : '—'], ['Upstream cost', formatCost(log.cost)], ['Total tokens', compactNumber(log.input_tokens + log.output_tokens)]].map(([label, value]) => `<div><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></div>`).join('');
  $('#activity-detail-request').innerHTML = [['Request ID', log.request_id], ['Path', log.path], ['Caller protocol', protocolLabel(log.caller_protocol)], ['Upstream protocol', protocolLabel(log.upstream_protocol)], ['Provider', log.provider], ['Endpoint', log.endpoint]].map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd><code>${escapeHtml(value)}</code></dd></div>`).join('');
  dialog.showModal();
}
$$('.close-activity-detail').forEach(button => button.addEventListener('click', () => $('#activity-detail-dialog').close()));
$('#activity-detail-dialog').addEventListener('click', event => { if (event.target === event.currentTarget) event.currentTarget.close(); });
function renderActivityLogs() {
  if ($('#activity-requests-panel').hidden) return;
  const pageCount = Math.max(1, Math.ceil(activityPageTotal / ACTIVITY_PAGE_SIZE));
  const start = activityPage * ACTIVITY_PAGE_SIZE;
  const end = Math.min(start + activityLogs.length, activityPageTotal);
  $('#activity-result-count').textContent = `${activityPageTotal.toLocaleString()} matching`;
  $('#activity-page-status').textContent = activityPageTotal ? `${start + 1}–${end} of ${activityPageTotal.toLocaleString()} · Page ${activityPage + 1} of ${pageCount}` : 'No matching requests';
  $('#activity-page-previous').disabled = activityPage === 0;
  $('#activity-page-next').disabled = activityPage >= pageCount - 1;
  $('#activity-logs').innerHTML = activityLogs.length ? activityLogs.map((log, index) => activityRow(log, true, index)).join('') : '<tr><td colspan="10"><div class="activity-empty">No requests match these filters.</div></td></tr>';
}
async function loadActivityPage() {
  if ($('#activity-requests-panel').hidden) return;
  const request = ++activityPageRequest;
  const seconds = Number($('#activity-range').value); const until = activityPageUntil || Math.floor(Date.now() / 1000); const since = until - seconds;
  const params = new URLSearchParams({since, until, offset: activityPage * ACTIVITY_PAGE_SIZE, limit: ACTIVITY_PAGE_SIZE});
  const provider = $('#activity-provider-filter').value; const query = $('#activity-search').value.trim(); const status = $('#activity-status-filter').value;
  if (provider) params.set('provider', provider); if (query) params.set('query', query); if (status) params.set('status', status);
  const response = await fetch(`/admin/activity/logs/page?${params}`); const page = await response.json();
  if (request !== activityPageRequest) return;
  activityLogs = page.data; activityPageTotal = page.total;
  const lastPage = Math.max(0, Math.ceil(activityPageTotal / ACTIVITY_PAGE_SIZE) - 1);
  if (activityPage > lastPage) { activityPage = lastPage; return loadActivityPage(); }
  renderActivityLogs();
}
async function loadActivity(requestedLogLimit) {
  const explorerVisible = !$('#activity-requests-panel').hidden;
  const logLimit = requestedLogLimit || 100;
  if (activityLoadPromise) {
    await activityLoadPromise;
    if (logLimit <= activityLogsLimit) return;
  }
  activityLoadPromise = (async () => {
    const seconds = Number($('#activity-range').value); const until = Math.floor(Date.now() / 1000); activityPageUntil = until; const since = until - seconds; const bucketCount = seconds <= 3600 ? 12 : seconds <= 86400 ? 24 : seconds <= 604800 ? 14 : 30; const providerFilter = $('#activity-provider-filter').value;
    const providerQuery = providerFilter ? `&provider=${encodeURIComponent(providerFilter)}` : '';
    const statsUrl = `/admin/activity/stats?since=${since}&until=${until}&buckets=${bucketCount}${providerQuery}`;
    const [stats, logs] = await Promise.all([fetch(statsUrl).then(response => response.json()), fetch(`/admin/activity/logs?since=${since}&limit=${logLimit}${providerQuery}`).then(response => response.json())]);
    activityOverviewLogs = logs; activityLogsLimit = logLimit;
    const totals = {input: stats.input_tokens, output: stats.output_tokens, cached: stats.cached_tokens, cost: stats.cost, latency: stats.latency_ms, success: stats.successful, streaming: stats.streaming};
    const buckets = stats.buckets; const requests = stats.requests; const totalTokens = totals.input + totals.output;
    $('#stat-requests').textContent = compactNumber(requests); $('#stat-streaming').textContent = compactNumber(totals.streaming); $('#stat-input').textContent = compactNumber(totals.input); $('#stat-output').textContent = compactNumber(totals.output); $('#stat-cached').textContent = compactNumber(totals.cached); $('#stat-total-tokens').textContent = compactNumber(totalTokens);
    $('#stat-success-rate').textContent = `${requests ? (totals.success * 100 / requests).toFixed(1) : '0.0'}%`; $('#stat-errors').textContent = `${(requests - totals.success).toLocaleString()} error${requests - totals.success === 1 ? '' : 's'}`; $('#stat-cache-rate').textContent = `${totals.input ? (totals.cached * 100 / totals.input).toFixed(1) : '0.0'}%`; $('#stat-cost').textContent = totals.cost ? formatCost(totals.cost) : '$0.00'; $('#stat-latency').textContent = formatDuration(requests ? Math.round(totals.latency / requests) : 0);
    $('#requests-spark').innerHTML = sparkline(buckets.map(bucket => bucket.requests), '#0b57d0'); $('#tokens-spark').innerHTML = sparkline(buckets.map(bucket => bucket.tokens), '#7c4dff'); $('#success-spark').innerHTML = sparkline(buckets.map(bucket => bucket.requests ? bucket.successful / bucket.requests : 0), '#00a67e'); $('#latency-spark').innerHTML = sparkline(buckets.map(bucket => bucket.samples ? bucket.latency / bucket.samples : 0), '#168c9a'); $('#cache-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cached), '#00897b'); $('#cost-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cost), '#ff8f00');
    renderActivityChart(buckets, seconds); renderRankings($('#provider-stats'), stats.by_provider.map(item => ({...item, tokens: item.input_tokens + item.output_tokens}))); renderRankings($('#model-stats'), stats.by_model.map(item => ({...item, tokens: item.input_tokens + item.output_tokens})));
    $('#recent-activity-logs').innerHTML = activityOverviewLogs.length ? activityOverviewLogs.slice(0, 8).map((log, index) => activityRow(log, false, index)).join('') : '<tr><td colspan="6"><div class="activity-empty">No requests in this period.</div></td></tr>';
    const filter = $('#activity-provider-filter'); const previous = filter.value;
    if (!providerFilter) filter.replaceChildren(new Option('All providers', ''), ...stats.by_provider.map(provider => new Option(provider.name, provider.name)));
    filter.value = [...filter.options].some(option => option.value === previous) ? previous : '';
    if (explorerVisible) loadActivityPage();
  })().finally(() => { activityLoadPromise = null; });
  return activityLoadPromise;
}
function showActivityTab(tab) {
  $$('.activity-tabs button').forEach(button => { const active = button.dataset.activityTab === tab; button.classList.toggle('active', active); button.setAttribute('aria-selected', String(active)); });
  $('#activity-overview-panel').hidden = tab !== 'overview'; $('#activity-requests-panel').hidden = tab !== 'requests';
  if (tab === 'requests') { activityPage = 0; activityPageUntil = Math.floor(Date.now() / 1000); $('#activity-page-previous').disabled = true; loadActivityPage(); }
}
$('#activity-view').addEventListener('click', event => {
  const tab = event.target.closest('[data-activity-tab]');
  if (tab) { showActivityTab(tab.dataset.activityTab); return; }
  if (event.target.closest('.show-request-explorer')) showActivityTab('requests');
});
$('#activity-search').addEventListener('input', () => { clearTimeout(activitySearchTimer); activitySearchTimer = setTimeout(() => { activityPage = 0; loadActivityPage(); }, 250); });
$('#activity-status-filter').addEventListener('change', () => { activityPage = 0; loadActivityPage(); });
$('#activity-page-previous').addEventListener('click', () => { if (activityPage > 0) { activityPage--; loadActivityPage(); } });
$('#activity-page-next').addEventListener('click', () => { if ((activityPage + 1) * ACTIVITY_PAGE_SIZE < activityPageTotal) { activityPage++; loadActivityPage(); } });
$('.chart-metric-picker').addEventListener('click', event => { const button = event.target.closest('[data-chart-metric]'); if (!button) return; activityChartMetric = button.dataset.chartMetric; $$('.chart-metric-picker button').forEach(item => { const active = item === button; item.classList.toggle('active', active); item.setAttribute('aria-pressed', String(active)); }); renderActivityChart(); });
$('#activity-chart').addEventListener('pointerover', event => { const column = event.target.closest('.chart-column'); if (column) inspectActivityBucket(Number(column.dataset.chartIndex)); });
$('#activity-chart').addEventListener('focusin', event => { const column = event.target.closest('.chart-column'); if (column) inspectActivityBucket(Number(column.dataset.chartIndex)); });
$('#activity-chart').addEventListener('click', event => { const column = event.target.closest('.chart-column'); if (column) inspectActivityBucket(Number(column.dataset.chartIndex)); });
function activityLogForRow(row) { return (row.closest('#recent-activity-logs') ? activityOverviewLogs : activityLogs)[Number(row.dataset.activityIndex)]; }
$('#activity-view').addEventListener('click', event => { const row = event.target.closest('.activity-request-row'); if (row) openActivityDetail(activityLogForRow(row)); });
$('#activity-view').addEventListener('keydown', event => { const row = event.target.closest('.activity-request-row'); if (row && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); openActivityDetail(activityLogForRow(row)); } });
function resetAndLoadActivity() { activityLogsLimit = 0; activityPage = 0; activityPageUntil = 0; loadActivity(); }
$('#activity-range').addEventListener('change', resetAndLoadActivity); $('#activity-provider-filter').addEventListener('change', resetAndLoadActivity); $('#refresh-activity').addEventListener('click', resetAndLoadActivity);
const activityDataDialog = $('#activity-data-dialog');
let activityImportBytes = null;
function formatBytes(bytes) { if (bytes < 1024) return `${bytes} B`; const units = ['KB', 'MB', 'GB']; let value = bytes / 1024; let unit = units.shift(); while (value >= 1024 && units.length) { value /= 1024; unit = units.shift(); } return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`; }
function formatActivityDate(timestamp) { return timestamp == null ? 'No records' : new Date(timestamp * 1000).toLocaleString(); }
function renderDataSummary(target, items) { target.innerHTML = items.map(([label, value, tone]) => `<div class="${tone || ''}"><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></div>`).join(''); }
function activitySince(range) { const seconds = Number(range); return seconds ? Math.floor(Date.now() / 1000) - seconds : 0; }
async function loadActivityExportPreview() {
  const target = $('#activity-export-summary'); target.innerHTML = '<p>Calculating export…</p>'; $('#activity-export-error').textContent = '';
  const response = await fetch(`/admin/activity/export/preview?since=${activitySince($('#activity-export-range').value)}`);
  if (!response.ok) return showApiError(response, $('#activity-export-error'));
  const summary = await response.json();
  renderDataSummary(target, [['Records', summary.records.toLocaleString()], ['Estimated file size', formatBytes(summary.estimated_bytes)], ['Oldest record', formatActivityDate(summary.oldest_at)], ['Newest record', formatActivityDate(summary.newest_at)]]);
  $('#export-activity').disabled = summary.records === 0;
}
async function loadActivityStorage() {
  const [settingsResponse, summaryResponse] = await Promise.all([fetch('/admin/activity/settings'), fetch('/admin/activity/export/preview?since=0')]);
  if (!settingsResponse.ok || !summaryResponse.ok) return;
  const settings = await settingsResponse.json(); const summary = await summaryResponse.json();
  $('#activity-retention-days').value = settings.retention_days;
  renderDataSummary($('#activity-storage-summary'), [['Currently retained', `${summary.records.toLocaleString()} records`], ['Estimated disk data', formatBytes(summary.estimated_bytes)], ['Oldest retained record', formatActivityDate(summary.oldest_at)], ['Retention limit', `${settings.retention_days} days`]]);
}
function showActivityDataTab(tab) {
  $$('.activity-data-tabs button').forEach(button => button.classList.toggle('active', button.dataset.activityDataTab === tab));
  $$('[data-activity-data-panel]').forEach(panel => panel.hidden = panel.dataset.activityDataPanel !== tab);
  if (tab === 'export') loadActivityExportPreview(); if (tab === 'storage') loadActivityStorage();
}
$('#manage-activity-data').addEventListener('click', () => { showActivityDataTab('export'); activityDataDialog.showModal(); });
$$('.close-activity-data').forEach(button => button.addEventListener('click', () => activityDataDialog.close()));
$('.activity-data-tabs').addEventListener('click', event => { const button = event.target.closest('[data-activity-data-tab]'); if (button) showActivityDataTab(button.dataset.activityDataTab); });
$('#activity-export-range').addEventListener('change', loadActivityExportPreview);
$('#export-activity').addEventListener('click', () => { location.href = `/admin/activity/export?since=${activitySince($('#activity-export-range').value)}`; });
$('#choose-activity-import').addEventListener('click', () => { $('#activity-import-file').value = ''; $('#activity-import-file').click(); });
$('#activity-import-file').addEventListener('change', async event => {
  const file = event.target.files[0]; if (!file) return;
  $('#activity-import-error').textContent = ''; $('#import-activity').disabled = true;
  if (file.size > 64 * 1024 * 1024) { activityImportBytes = null; $('#activity-import-summary').hidden = true; $('#activity-import-error').textContent = 'This file is larger than the 64 MB import limit.'; return; }
  activityImportBytes = await file.arrayBuffer();
  const fileInfo = $('#activity-import-file-info'); fileInfo.hidden = false; fileInfo.innerHTML = `<strong>${escapeHtml(file.name)}</strong><span>${formatBytes(file.size)}</span>`;
  const summary = $('#activity-import-summary'); summary.hidden = false; summary.innerHTML = '<p>Validating file and checking existing records…</p>';
  const response = await fetch('/admin/activity/import/preview', {method: 'POST', headers: {'content-type': 'application/json'}, body: activityImportBytes});
  if (!response.ok) { summary.hidden = true; activityImportBytes = null; return showApiError(response, $('#activity-import-error')); }
  const result = await response.json();
  renderDataSummary(summary, [['Records in file', result.total.toLocaleString()], ['New records', result.imported.toLocaleString(), 'positive'], ['Already present', result.duplicates.toLocaleString()], ['Outside retention', result.expired.toLocaleString(), result.expired ? 'warning' : ''], ['Oldest record', formatActivityDate(result.oldest_at)], ['Newest record', formatActivityDate(result.newest_at)]]);
  $('#import-activity').disabled = result.imported === 0;
  $('#import-activity').textContent = result.imported ? `Import ${result.imported.toLocaleString()} new records` : 'Nothing new to import';
});
$('#import-activity').addEventListener('click', async () => {
  if (!activityImportBytes) return; const button = $('#import-activity'); button.disabled = true; button.textContent = 'Importing…';
  const response = await fetch('/admin/activity/import', {method: 'POST', headers: {'content-type': 'application/json'}, body: activityImportBytes});
  if (!response.ok) { button.disabled = false; return showApiError(response, $('#activity-import-error')); }
  const result = await response.json(); activityImportBytes = null;
  renderDataSummary($('#activity-import-summary'), [['Imported', result.imported.toLocaleString(), 'positive'], ['Duplicates skipped', result.duplicates.toLocaleString()], ['Outside retention', result.expired.toLocaleString()], ['Result', 'Import complete', 'positive']]);
  button.textContent = 'Import complete'; await loadActivity();
});
$('#save-activity-retention').addEventListener('click', async () => {
  const days = Number($('#activity-retention-days').value); const button = $('#save-activity-retention'); $('#activity-retention-error').textContent = '';
  if (!Number.isInteger(days) || days < 1 || days > 3650) { $('#activity-retention-error').textContent = 'Enter a retention period between 1 and 3650 days.'; return; }
  button.disabled = true; button.textContent = 'Saving…';
  const response = await fetch('/admin/activity/settings', {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({retention_days: days})});
  if (!response.ok) await showApiError(response, $('#activity-retention-error')); else { await loadActivityStorage(); await loadActivity(); }
  button.disabled = false; button.textContent = 'Save retention';
});
function renderHomeTraffic(buckets) {
  const max = Math.max(...buckets.map(bucket => bucket.requests), 1); const requests = buckets.reduce((total, bucket) => total + bucket.requests, 0);
  $('#home-traffic-chart').innerHTML = `<div class="home-chart-grid"><i></i><i></i><i></i></div><div class="home-chart-bars">${buckets.map((bucket, index) => { const hour = new Date(bucket.start * 1000); const label = index % 6 === 0 || index === buckets.length - 1 ? hour.toLocaleTimeString([], {hour: '2-digit'}) : ''; return `<div class="home-chart-column" title="${bucket.requests} request${bucket.requests === 1 ? '' : 's'}"><span style="height:${bucket.requests * 100 / max}%;animation-delay:${index * 16}ms"></span><small>${label}</small></div>`; }).join('')}</div>${requests ? '' : '<p class="home-chart-empty">No requests in the last 24 hours</p>'}`;
}
function openHomeView(name) { showView(name); }
$('#home-open-activity').addEventListener('click', () => openHomeView('activity'));
$('.home-explore-activity').addEventListener('click', () => openHomeView('activity'));
$('#home-open-providers').addEventListener('click', () => openHomeView('providers'));
$('.home-manage-providers').addEventListener('click', () => openHomeView('providers'));
async function loadDashboard() {
  if (dashboardLoadPromise) return dashboardLoadPromise;
  dashboardLoadPromise = (async () => {
    const since = Math.floor(Date.now() / 1000) - 86400;
    const stats = await fetch(`/admin/activity/stats?since=${since}&buckets=24`).then(response => response.json());
    $('#home-requests').textContent = compactNumber(stats.requests); $('#home-input').textContent = compactNumber(stats.input_tokens); $('#home-output').textContent = compactNumber(stats.output_tokens); $('#home-cached').textContent = compactNumber(stats.cached_tokens);
    $('#home-updated').textContent = `Updated ${new Date().toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})}`;
    $('#home-provider-summary').textContent = `${providers.length} provider${providers.length === 1 ? '' : 's'} · ${providers.reduce((sum, provider) => sum + provider.discovered_models.length, 0).toLocaleString()} discovered models`;
    renderHomeTraffic(stats.buckets);
    const providerItems = providers.map(provider => {
      const item = document.createElement('button'); item.className = 'home-provider';
      const credentials = credentialCount(provider); const status = provider.model_discovery_error ? 'Discovery issue' : provider.models_discovered_at ? 'Catalog ready' : 'Discovering…';
      item.innerHTML = `<span class="home-provider-mark">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</span><span class="home-provider-main"><strong>${escapeHtml(provider.name)}</strong><small>${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} · ${credentials} credential${credentials === 1 ? '' : 's'}</small></span><span class="home-provider-meta"><strong>${provider.discovered_models.length.toLocaleString()}</strong><small>${escapeHtml(status)}</small></span>`;
      item.addEventListener('click', () => { selectedProviderId = provider.id; showView('providers'); }); return item;
    });
    if (providerItems.length) $('#home-providers').replaceChildren(...providerItems);
    else $('#home-providers').innerHTML = `<div class="home-providers-empty">${icon('provider')}<strong>No providers connected</strong><p>Add an upstream Endpoint to start routing requests.</p><button class="button secondary" type="button">Add provider</button></div>`;
    $('#home-providers .home-providers-empty .button')?.addEventListener('click', openProviderDialog);
  })().finally(() => { dashboardLoadPromise = null; });
  return dashboardLoadPromise;
}

function refreshVisibleView() {
  if (!adminSession?.authenticated || document.hidden) return;
  if (!$('#home-view').hidden) loadDashboard();
  else if (!$('#activity-view').hidden) loadActivity();
}
setInterval(refreshVisibleView, LIVE_REFRESH_INTERVAL_MS);
document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshVisibleView(); });

$$('.copy-extension-command').forEach(button => button.addEventListener('click', async () => {
  await navigator.clipboard.writeText(button.dataset.command);
  const original = button.textContent; button.textContent = 'Copied';
  setTimeout(() => { button.textContent = original; }, 1200);
}));

function renderExtensions() {
  const list = $('#extensions-list');
  if (!extensions.length) { list.innerHTML = '<section class="card empty extensions-empty"><h3>No extensions in this build</h3><p>The commands above show how to include bundled Extensions in the next build.</p></section>'; return; }
  list.innerHTML = extensions.map(extension => {
    const configuredProviders = extension.id === 'request-defaults' ? providers.filter(provider => Object.keys(provider.extra_headers || {}).length || Object.keys(provider.extra_body || {}).length || provider.endpoints.some(endpoint => Object.keys(endpoint.extra_headers || {}).length || Object.keys(endpoint.extra_body || {}).length)) : [];
    const status = extension.enabled ? 'Enabled' : extension.runtime_configurable ? 'Disabled' : 'Disabled by CLI';
    return `<article class="extension-card${extension.enabled ? '' : ' extension-disabled'}"><header><span class="extension-mark">${icon('extension')}</span><div><span class="section-kicker">Included in this build</span><h2>${escapeHtml(extension.name)}</h2><code>${escapeHtml(extension.id)} · v${escapeHtml(extension.version)}</code></div><label class="switch-label extension-toggle" title="${extension.runtime_configurable ? 'Enable or disable this Extension' : 'Restart without --no-extensions to manage Extensions'}"><input type="checkbox" data-extension-toggle="${escapeHtml(extension.id)}" ${extension.enabled ? 'checked' : ''} ${extension.runtime_configurable ? '' : 'disabled'}><span class="switch-track" aria-hidden="true"><i></i></span><span class="switch-status">${status}</span></label></header><p>${escapeHtml(extension.description)}</p><div class="extension-facts"><span><small>Implementation</small><strong>Native Rust</strong></span><span><small>Extension API</small><strong>v${extension.api_version}</strong></span><span><small>Hooks</small><strong>${extension.hooks.map(hook => hook.replaceAll('_', ' ')).join(' · ')}</strong></span><span><small>Configured</small><strong>${configuredProviders.length} Provider${configuredProviders.length === 1 ? '' : 's'}</strong></span></div>${extension.id === 'request-defaults' ? `<footer><div><strong>Configured in Provider context</strong><p>Header and body defaults remain next to the Provider and Endpoint resources they affect.${extension.enabled ? '' : ' They are retained while this Extension is disabled.'}</p></div>${configuredProviders.length ? `<div class="extension-provider-links">${configuredProviders.map(provider => `<button class="text-link" type="button" data-extension-provider="${escapeHtml(provider.id)}">${escapeHtml(provider.name)}</button>`).join('')}</div>` : '<button class="button secondary extension-open-providers" type="button">Choose a Provider</button>'}</footer>` : ''}</article>`;
  }).join('');
  $$('[data-extension-toggle]').forEach(toggle => toggle.addEventListener('change', async () => {
    toggle.disabled = true;
    const response = await fetch(`/admin/extensions/${encodeURIComponent(toggle.dataset.extensionToggle)}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({enabled: toggle.checked})});
    if (!response.ok) { toggle.checked = !toggle.checked; toggle.disabled = false; window.alert((await response.json()).error?.message || 'Could not update Extension'); return; }
    const updated = await response.json();
    extensions = extensions.map(extension => extension.id === updated.id ? updated : extension);
    renderExtensions(); renderProviders();
  }));
  $$('.extension-open-providers').forEach(button => button.addEventListener('click', () => showView('providers')));
  $$('[data-extension-provider]').forEach(button => button.addEventListener('click', () => { selectedProviderId = button.dataset.extensionProvider; showView('providers'); }));
}
async function loadExtensions() { const response = await fetch('/admin/extensions'); extensions = response.ok ? await response.json() : []; renderExtensions(); }

function formatType(type) { return type === 'anthropic' ? 'Anthropic Messages' : type === 'openai_codex' ? 'OpenAI subscription' : type === 'openai_chat_completions' ? 'OpenAI Chat Completions' : type === 'openai_responses' ? 'OpenAI Responses' : 'OpenAI compatible'; }
function escapeHtml(value) { const node = document.createElement('span'); node.textContent = String(value); return node.innerHTML; }
async function loadProviders() { const response = await fetch('/admin/providers'); providers = await response.json(); renderProviders(); }
async function loadRoutes() { const response = await fetch('/admin/routes'); modelRoutes = await response.json(); renderRoutes(); }
initializeAdmin();
