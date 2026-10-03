let providers = [];
let authSettings = {enabled: true, api_keys: []};
let modelRoutes = [];
let globalPricing = {updated_at: 0, models: {}, incoming_models: {}};
let pricingActivityModels = [];
let pricingEditTarget = null;
let modelsDevCatalogPromise = null;
let modelsDevReferenceResults = [];
let modelsDevSearchTimer = null;
let modelsDevSearchSequence = 0;
let extensions = [];
/// Every Endpoint type this process can offer: Core's own plus the ones enabled
/// Extensions declare. The console reads its choices from here instead of
/// knowing any Endpoint type by name.
let endpointTypes = [];
const $ = selector => document.querySelector(selector);
const $$ = selector => [...document.querySelectorAll(selector)];
const icon = (name, className = 'ui-icon') => `<svg class="${className}" aria-hidden="true"><use href="#icon-${name}"></use></svg>`;
const providerDialog = $('#provider-dialog');
const providerForm = $('#provider-form');
const providerIdentityDialog = $('#provider-identity-dialog');
const endpointDialog = $('#endpoint-dialog');
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
let activityModelDimension = 'incoming';
let activityPage = 0;
let activityPageTotal = 0;
let activityPageRequest = 0;
let activityPageSince = 0;
let activityPageUntil = 0;
let activityCustomRange = null;
let activitySearchTimer = null;
const activityFilters = {providers: new Set(), models: new Set(), apiKeys: new Set()};
let activityFilterOptions = {providers: [], models: [], api_keys: []};
let dashboardLoadPromise = null;
let providerActivityLoadPromise = null;
let providerActivity = new Map();
let homeTrafficBuckets = [];
let homeTrafficBucketSize = 0;
let homeTrafficResizeFrame = 0;
let trafficCaptureStatus = null;
let trafficCaptures = [];
let captureFormInitialized = false;
let selectedCapture = null;
let captureDetailBodyMode = 'raw';
let captureDetailBody = '';
let captureDetailAssembled = '';
let captureDetailIsSse = false;

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
  await Promise.all([loadProviders(), loadPricing(), loadAuth(), loadRoutes(), loadExtensions(), loadEndpointTypes(), loadAbout()]);
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

const viewPaths = {home: '/home', providers: '/providers', models: '/model-routing', pricing: '/model-pricing', extensions: '/extensions', capture: '/extensions/traffic-capture', access: '/api-access', activity: '/activity', management: '/management-api'};
function showView(name, updateHistory = true) {
  $('#home-view').hidden = name !== 'home';
  $('#providers-view').hidden = name !== 'providers';
  $('#models-view').hidden = name !== 'models';
  $('#pricing-view').hidden = name !== 'pricing';
  $('#extensions-view').hidden = name !== 'extensions';
  $('#traffic-capture-view').hidden = name !== 'capture';
  $('#access-view').hidden = name !== 'access';
  $('#activity-view').hidden = name !== 'activity';
  $('#management-view').hidden = name !== 'management';
  const navName = name === 'capture' ? 'extensions' : name;
  $$('.nav[data-view]').forEach(item => item.classList.toggle('active', item.dataset.view === navName));
  const active = $(`.nav[data-view="${navName}"]`);
  const indicator = $('.nav-indicator');
  indicator.style.transform = `translateY(${active.offsetTop}px)`;
  selectedProviderId = name === 'providers' ? selectedProviderId : null;
  if (name === 'providers') renderProviderPage();
  if (name === 'providers' && adminSession?.authenticated && !selectedProviderId) loadProviderActivity();
  if (name === 'home' && adminSession?.authenticated) loadDashboard();
  if (name === 'pricing') { showPricingList(); renderPricingPage(); }
  if (name === 'extensions') renderExtensions();
  if (name === 'capture' && adminSession?.authenticated) loadTrafficCapture();
  if (name === 'activity' && adminSession?.authenticated) loadActivity();
  if (name === 'management' && adminSession?.authenticated) loadManagementKeys();
  if (updateHistory) history.pushState({}, '', selectedProviderId && name === 'providers' ? `/providers/${encodeURIComponent(selectedProviderId)}` : viewPaths[name]);
  document.title = `${name === 'capture' ? 'Traffic Capture' : active.textContent.trim()} · Yabane`;
}
$$('.nav[data-view]').forEach(item => item.addEventListener('click', () => { showView(item.dataset.view); if (innerWidth <= 760) setMobileNavigation(false); }));
function routeFromLocation() {
  const path = location.pathname;
  if (path.startsWith('/providers/')) { selectedProviderId = decodeURIComponent(path.slice('/providers/'.length)); showView('providers', false); }
  else if (path === '/providers') { selectedProviderId = null; showView('providers', false); }
  else if (path === '/home' || path === '/') showView('home', false);
  else if (path === '/model-routing') showView('models', false);
  else if (path === '/model-pricing') showView('pricing', false);
  else if (path === '/extensions/traffic-capture') showView('capture', false);
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
  toggleCredentialRequirement();
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

function endpointType(id) { return endpointTypes.find(type => type.id === id) || null; }
function endpointTypeLabel(id) { return endpointType(id)?.label || id; }
function endpointSignIn(id) { return endpointType(id)?.sign_in || null; }
function endpointKind(typeId, kindId) { return (endpointType(typeId)?.credential_kinds || []).find(kind => kind.id === kindId) || null; }
function endpointAcceptsSecret(typeId) { return (endpointType(typeId)?.credential_kinds || []).some(kind => kind.flow === 'secret'); }
function endpointSignInRequired(typeId) { return Boolean(endpointSignIn(typeId)); }
/// An Endpoint type that owns its own connection: the declaration fixes the base
/// URL, or accounts arrive through its sign-in flow.
function endpointDeclaredConnection(declaration) { return Boolean(declaration && (declaration.fixed_base_url || declaration.sign_in)); }

function renderEndpointTypeChoices() {
  const choices = $('#api-type-choices');
  const current = choices.querySelector('input[name="api_type"]:checked')?.value;
  choices.replaceChildren(...endpointTypes.map(type => {
    const label = document.createElement('label');
    label.className = 'choice';
    label.innerHTML = `<input type="radio" name="api_type" value="${escapeHtml(type.id)}"><strong>${escapeHtml(type.label)}</strong><small>${escapeHtml(type.description)}</small>`;
    return label;
  }));
  $$('#api-type-choices input[name="api_type"]').forEach(input => input.addEventListener('change', () => setApiType(input.value)));
  const select = $('#endpoint-form [name="api_type"]');
  if (select) select.replaceChildren(...endpointTypes.map(type => new Option(type.label, type.id)));
  const chosen = endpointTypes.some(type => type.id === current) ? current : endpointTypes[0]?.id;
  if (chosen) setApiType(chosen);
}
function setApiType(type) {
  const input = $(`#api-type-choices input[name="api_type"][value="${type}"]`);
  if (!input) return;
  input.checked = true;
  $$('#api-type-choices .choice').forEach(choice => choice.classList.toggle('selected', choice.contains(input)));
  const declaration = endpointType(type);
  const declaredConnection = endpointDeclaredConnection(declaration);
  const base = $('#base-url');
  const endpointId = $('#initial-endpoint-id');
  if (!endpointId.dataset.edited) endpointId.value = declaration?.default_endpoint_id || type;
  [base.closest('.field'), $('#requires-credential').closest('.checkbox-row')].forEach(field => field.hidden = declaredConnection);
  if (declaredConnection) {
    if (base.dataset.declaredValue !== base.value) base.dataset.previousValue = base.value;
    base.dataset.declaredValue = declaration.fixed_base_url || '';
    base.value = declaration.fixed_base_url || base.value;
  } else if (base.value === base.dataset.declaredValue) {
    base.value = base.dataset.previousValue || '';
  }
  base.required = !declaredConnection;
  const acceptsSecret = endpointAcceptsSecret(type);
  $('#initial-credential-section').hidden = declaredConnection || !acceptsSecret;
  $('#credential-secret').required = !declaredConnection && acceptsSecret && $('#requires-credential').checked;
  $('#create-provider').textContent = declaration?.sign_in ? 'Connect account' : 'Add provider';
}
$$('input[name="api_type"]').forEach(input => input.addEventListener('change', () => setApiType(input.value)));
$('#initial-endpoint-id').addEventListener('input', event => {
  event.target.value = slugify(event.target.value);
  event.target.dataset.edited = 'true';
});
function defaultEndpointId(type) {
  return endpointType(type)?.default_endpoint_id || type;
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

function toggleCredentialRequirement() {
  const required = $('#requires-credential').checked;
  $('#initial-credential-section').classList.toggle('collapsed', !required);
  $('#credential-secret').required = required;
}
$('#requires-credential').addEventListener('change', toggleCredentialRequirement);
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
      requires_credential: data.get('requires_credential') === 'on',
      credential_secret: data.get('requires_credential') === 'on' ? data.get('credential_secret') : null
    }
  };
  if (endpointSignInRequired(payload.endpoint.api_type)) {
    return beginEndpointSignIn({endpoint_type: payload.endpoint.api_type, provider_id: payload.id, provider_name: payload.name, endpoint_id: payload.endpoint.id, socks5_proxy: payload.endpoint.socks5_proxy}, $('#provider-error'), providerDialog);
  }
  const response = await fetch('/admin/providers', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#provider-error'));
  providerDialog.close();
  await loadProviders();
  [1000, 3000, 8000].forEach(delay => setTimeout(loadProviders, delay));
});

const signInDialog = $('#endpoint-sign-in-dialog');
let signInFlowId = null;
let signInTarget = null;
let signInStartController = null;
let signInPollController = null;
/// Which sign-in flows the Endpoint type offers, straight from its declaration.
function signInFlows() { return endpointSignIn(signInTarget?.endpoint_type) || {}; }
function signInUrl(path) { return `/admin/endpoint-types/${encodeURIComponent(signInTarget?.endpoint_type || '')}/sign-in${path}`; }
function showDeviceSignIn(target, flow = null) {
  signInTarget = target;
  const flows = signInFlows();
  $('#sign-in-device').hidden = flow ? false : !flows.device_code;
  $('#sign-in-browser').hidden = true;
  $('#sign-in-code').hidden = !flow;
  $('#sign-in-link').hidden = !flow;
  $('#sign-in-use-oauth').hidden = !flows.browser;
  if (flow) {
    $('#sign-in-code').textContent = flow.user_code;
    $('#sign-in-link').href = flow.verification_uri;
    $('#sign-in-status').textContent = 'Waiting for sign-in to complete…';
  } else {
    $('#sign-in-status').textContent = 'Device-code sign-in could not start. You can use browser sign-in instead.';
  }
  signInDialog.showModal();
}
async function beginEndpointSignIn(target, errorElement, parentDialog = null) {
  signInFlowId = null;
  signInPollController?.abort();
  signInPollController = null;
  signInStartController?.abort();
  signInTarget = target;
  const controller = new AbortController();
  signInStartController = controller;
  parentDialog?.addEventListener('close', () => controller.abort(), {once: true});
  const submit = parentDialog?.querySelector('[type="submit"]');
  if (submit) submit.disabled = true;
  let response;
  try {
    response = await fetch(signInUrl('/device-code'), {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(target), signal: controller.signal});
  } catch (error) {
    const current = signInStartController === controller;
    if (current) { signInStartController = null; if (submit) submit.disabled = false; }
    if (error.name === 'AbortError' || !current) return;
    if (errorElement) errorElement.textContent = 'Could not start sign-in.';
    signInTarget = target;
    parentDialog?.close();
    $('#sign-in-error').textContent = 'Could not start device-code sign-in.';
    showDeviceSignIn(target);
    return;
  }
  if (signInStartController !== controller) return;
  signInStartController = null;
  if (submit) submit.disabled = false;
  if (!response.ok) {
    await showApiError(response, $('#sign-in-error'));
    parentDialog?.close();
    showDeviceSignIn(target);
    return;
  }
  const flow = await response.json();
  parentDialog?.close();
  $('#sign-in-error').textContent = '';
  signInPollController?.abort();
  signInFlowId = flow.id;
  showDeviceSignIn(target, flow);
  const poll = async () => {
    if (signInFlowId !== flow.id) return;
    const controller = new AbortController();
    signInPollController = controller;
    let statusResponse;
    try {
      statusResponse = await fetch(signInUrl(`/device-code/${encodeURIComponent(flow.id)}`), {signal: controller.signal});
    } catch (error) {
      if (error.name === 'AbortError') return;
      signInFlowId = null;
      $('#sign-in-error').textContent = 'Could not check sign-in status.';
      return;
    }
    if (signInFlowId !== flow.id) return;
    signInPollController = null;
    if (!statusResponse.ok) { signInFlowId = null; return showApiError(statusResponse, $('#sign-in-error')); }
    const status = await statusResponse.json();
    if (status.status === 'complete') {
      signInFlowId = null;
      $('#sign-in-status').textContent = 'Connected. Loading the account…';
      await Promise.all([loadProviders(), loadRoutes()]);
      signInDialog.close();
      if (target.provider_name) { selectedProviderId = target.provider_id; history.pushState({}, '', `/providers/${encodeURIComponent(target.provider_id)}`); renderProviderPage(); }
      return;
    }
    if (status.status === 'failed') { signInFlowId = null; $('#sign-in-error').textContent = status.error || 'Sign-in failed.'; return; }
    setTimeout(poll, Math.max(1000, Number(status.interval_seconds || 2) * 1000));
  };
  poll();
}
function stopSignInPolling() {
  signInFlowId = null;
  signInPollController?.abort();
  signInPollController = null;
}
$('#sign-in-use-oauth').addEventListener('click', async () => {
  stopSignInPolling();
  $('#sign-in-error').textContent = '';
  $('#sign-in-use-oauth').disabled = true;
  let response;
  try {
    response = await fetch(signInUrl('/oauth'), {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(signInTarget)});
  } catch {
    $('#sign-in-use-oauth').disabled = false;
    $('#sign-in-error').textContent = 'Could not start browser sign-in.';
    return;
  }
  $('#sign-in-use-oauth').disabled = false;
  if (!response.ok) return showApiError(response, $('#sign-in-error'));
  const flow = await response.json();
  signInFlowId = flow.id;
  $('#sign-in-browser-link').href = flow.authorization_url;
  $('#sign-in-callback').value = '';
  $('#sign-in-browser-error').textContent = '';
  $('#sign-in-device').hidden = true;
  $('#sign-in-browser').hidden = false;
});
$('#sign-in-browser').addEventListener('submit', async event => {
  event.preventDefault();
  const submit = event.currentTarget.querySelector('[type="submit"]');
  submit.disabled = true;
  let response;
  try {
    response = await fetch(signInUrl(`/oauth/${encodeURIComponent(signInFlowId)}/complete`), {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({redirect_url: $('#sign-in-callback').value})});
  } catch {
    submit.disabled = false;
    $('#sign-in-browser-error').textContent = 'Could not complete browser sign-in.';
    return;
  }
  submit.disabled = false;
  if (!response.ok) return showApiError(response, $('#sign-in-browser-error'));
  const target = signInTarget;
  signInFlowId = null;
  await Promise.all([loadProviders(), loadRoutes()]);
  signInDialog.close();
  if (target?.provider_name) { selectedProviderId = target.provider_id; history.pushState({}, '', `/providers/${encodeURIComponent(target.provider_id)}`); renderProviderPage(); }
});
$$('.close-sign-in').forEach(button => button.addEventListener('click', () => { stopSignInPolling(); signInDialog.close(); }));
signInDialog.addEventListener('close', stopSignInPolling);
$('#sign-in-code').addEventListener('click', async () => {
  await navigator.clipboard?.writeText($('#sign-in-code').textContent);
  $('#sign-in-status').textContent = 'Code copied. Complete sign-in with the Provider, then return here.';
});

function credentialCount(provider) {
  return provider.endpoints.reduce((count, endpoint) => count + endpoint.credentials.length, 0);
}
function enabledCredentials(endpoint) {
  return endpoint.credentials.filter(credential => credential.enabled);
}
function credentialKindLabel(credential) {
  return credential.kind_label || credential.kind;
}
function credentialDetail(credential) {
  const label = credentialKindLabel(credential);
  if (!credential.subscription_expires_at) return label;
  if (credential.subscription_expires_at * 1000 <= Date.now()) return `${label} · access token renews on the next request`;
  return `${label} · access token valid until ${new Date(credential.subscription_expires_at * 1000).toLocaleString()}`;
}
function accountCredentialDetail(endpoint) {
  const expiries = endpoint.credentials.map(credential => credential.subscription_expires_at).filter(value => value);
  const soonest = expiries.length ? Math.min(...expiries) * 1000 : null;
  if (!soonest) return 'The current access token will be renewed automatically when needed.';
  return soonest > Date.now()
    ? `The current access token is valid until ${new Date(soonest).toLocaleString()} and will be renewed automatically when needed.`
    : 'The current access token will be renewed when the next request uses this Endpoint.';
}
/// One Endpoint's identities grouped the way the console shows them, lowest group
/// first. A group is a standby when a lower-numbered group exists: it carries
/// traffic only while every identity in those groups is out, so its percentages
/// describe that moment rather than a slice of one shared pool. The number a group
/// carries is its position here, not the value the configuration stores: the group
/// that carries traffic first is Priority 1 to whoever reads the console whether the
/// file calls it 1 or 4, and the distribution dialog writes these positions back.
/// Without that, a group whose number disappeared could not be chosen again and the
/// numbers would only ever climb.
function identityGroups(endpoint) {
  const byPriority = new Map();
  enabledCredentials(endpoint).forEach(credential => {
    const priority = credential.priority || 1;
    if (!byPriority.has(priority)) byPriority.set(priority, []);
    byPriority.get(priority).push(credential);
  });
  return [...byPriority.entries()].sort((a, b) => a[0] - b[0]).map(([, members], index) => {
    const eligible = members.filter(credential => !credential.cooldown_seconds_remaining);
    return {priority: index + 1, members, eligible, standby: index > 0, configured: trafficShares(members), effective: trafficShares(eligible)};
  });
}
/// The group the next request uses: the lowest-numbered group that still has an
/// identity which is not out, or the lowest-numbered group at all when every
/// identity in the Endpoint is exhausted.
function carryingGroup(groups) {
  return groups.find(group => group.eligible.length) || groups[0] || null;
}
/// Even percentages inside one group, so moving an identity in or out leaves a
/// total of exactly 100 without the administrator doing arithmetic.
function evenShares(count) {
  if (!count) return [];
  const base = Math.floor(100 / count);
  const remainder = 100 - base * count;
  return Array.from({length: count}, (_, index) => base + (index < remainder ? 1 : 0));
}
function credentialRows(provider, endpoint) {
  const pool = identityPool(endpoint);
  const groups = identityGroups(endpoint);
  const carrier = carryingGroup(groups);
  return endpoint.credentials.map(credential => {
    const group = groups.find(item => item.members.some(member => member.id === credential.id));
    const cooldown = credential.cooldown_seconds_remaining
      ? `<span class="credential-cooldown">Cooling down · resumes in ${formatCooldown(credential.cooldown_seconds_remaining)} <button class="text-link clear-cooldown" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-credential="${credential.id}">Resume now</button></span>`
      : '';
    // The share column states what this identity carries now, and names the
    // configured value next to it whenever those two differ. Only the group that
    // currently carries traffic hands anything out, so an identity in a standby
    // group reads 0% while the groups above it can still serve.
    const carries = group && carrier && group.priority === carrier.priority;
    const configured = group?.configured.get(credential.id) ?? 0;
    const scope = groups.length > 1 ? `of Priority ${group?.priority} traffic` : 'of default traffic';
    const share = !credential.enabled
      ? {value: '—', note: 'no traffic'}
      : credential.cooldown_seconds_remaining
        ? {value: '0%', note: 'while cooling down'}
        : !carries
          ? {value: '0%', note: `only while Priority ${carrier?.priority} is out`}
          : carrier.eligible.length < carrier.members.length
            ? {value: `${carrier.effective.get(credential.id) ?? 0}%`, note: `right now · normally ${configured}%`}
            : {value: `${configured}%`, note: scope};
    const tier = groups.length > 1
      ? `<span class="credential-tier" data-tone="${group?.standby ? 'standby' : 'first'}">${group?.standby ? `Priority ${group.priority} · standby` : 'Priority 1 · first'}</span>`
      : '';
    return `<div class="key-row"><span class="status ${credential.enabled ? 'enabled' : ''}"></span><span class="key-name"><strong>${escapeHtml(credential.name)}</strong>${tier}<small>${escapeHtml(credentialDetail(credential))}</small>${cooldown}</span><span class="traffic-share"><strong>${share.value}</strong><small>${share.note}</small></span><button class="credential-rename text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-credential="${credential.id}" aria-label="Rename credential ${escapeHtml(credential.name)}">Rename</button><button class="credential-toggle text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-credential="${credential.id}" data-enabled="${credential.enabled}">${credential.enabled ? 'Disable' : 'Enable'}</button><button class="credential-delete text-link danger-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" data-credential="${credential.id}" data-name="${escapeHtml(credential.name)}" aria-label="Delete credential ${escapeHtml(credential.name)}">Delete</button></div>`;
  }).join('');
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
    const activity = providerActivity.get(provider.id) || [];
    const requests = activity.reduce((total, value) => total + value, 0);
    const activityLabel = `${requests.toLocaleString()} request${requests === 1 ? '' : 's'} in the last 24 hours`;
    // A credential the Provider rate-limited is out of its Endpoint's rotation
    // until the cooldown ends, which is capacity this Provider does not currently
    // offer. The row states that count and the next return instead of leaving an
    // operator to open every Endpoint.
    const cooling = provider.endpoints.flatMap(endpoint => endpoint.credentials.filter(credential => credential.enabled && credential.cooldown_seconds_remaining).map(credential => ({endpoint, credential})));
    const cooldownNote = cooling.length
      ? `<span class="provider-cooling" title="${escapeHtml(cooling.map(entry => `${entry.endpoint.id} · ${entry.credential.name} resumes in ${formatCooldown(entry.credential.cooldown_seconds_remaining)}`).join(' · '))}">${cooling.length} credential${cooling.length === 1 ? '' : 's'} cooling down · ${cooling.length === 1 ? 'resumes' : 'next resumes'} in ${formatCooldown(Math.min(...cooling.map(entry => entry.credential.cooldown_seconds_remaining)))}</span>`
      : '';
    card.innerHTML = `<span class="provider-avatar">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</span><span class="provider-list-main"><strong>${escapeHtml(provider.name)}</strong><code>${escapeHtml(provider.id)}/model-id</code>${cooldownNote}</span><span class="provider-list-activity" aria-label="${escapeHtml(activityLabel)}"><span class="provider-sparkline">${sparkline(activity.length ? activity : [0, 0], '#0b57d0')}</span><small>${compactNumber(requests)} requests · 24h</small></span><span class="provider-list-meta">${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} · ${credentialCount(provider)} credential${credentialCount(provider) === 1 ? '' : 's'}<small class="${provider.model_discovery_error ? 'error-text' : ''}">${escapeHtml(modelStatus)}</small></span><span class="chevron">${icon('chevron-right')}</span>`;
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
    const enabled = enabledCredentials(endpoint);
    // The Endpoint itself reports whether its declaration owns a sign-in flow.
    const subscription = Boolean(endpoint.sign_in);
    const credentialLabel = subscription ? 'account' : 'credential';
    const credentialFact = endpoint.requires_credential
      ? `<span><strong>${enabled.length}</strong> of ${endpoint.credentials.length} ${credentialLabel}s enabled</span>`
      : '<span>No credential</span>';
    const cooldownSeconds = endpoint.rate_limit_cooldown?.seconds || 0;
    const cooldownSource = cooldownPolicyLabel(endpoint.rate_limit_cooldown);
    const cooldownPolicy = cooldownSeconds > 0
      ? `<span>Rate-limit cooldown <strong>${formatCooldown(cooldownSeconds)}</strong>${cooldownSource ? ` · ${cooldownSource}` : ''}</span>`
      : '';
    const rows = credentialRows(provider, endpoint);
    const poolSummary = endpointPoolSummary(endpoint);
    const headCopy = !subscription
      ? `<h4>Credentials</h4><p>${endpoint.requires_credential ? `Credentials belong only to <code>${escapeHtml(endpoint.id)}</code>. ${identityGroups(endpoint).length > 1 ? 'Each priority group splits its own traffic between the credentials in it.' : 'Traffic is distributed between enabled credentials.'}` : 'This Endpoint sends requests without an identity.'}</p>`
      : enabled.length
        ? `<h4>Automatic renewal enabled</h4><p>Yabane renews temporary access credentials when needed. Reconnect only if renewal fails or the Provider revokes access.</p><details class="credential-details"><summary>Credential details</summary><p>${escapeHtml(accountCredentialDetail(endpoint))} Access and refresh tokens are never shown in the console or API.</p></details>`
        : '<h4>Reconnect required</h4><p>No account is connected. Use Connect account to sign in and resume requests through this Endpoint.</p>';
    const renewalStatus = subscription
      ? enabled.length
        ? '<span class="renewal-status">Automatic renewal</span>'
        : '<span class="renewal-status attention">Not connected</span>'
      : '';
    const emptyRow = !endpoint.requires_credential
      ? ''
      : endpoint.credentials.length
        ? ''
        : `<div class="endpoint-key-empty"><p>${subscription ? `No ${escapeHtml(endpointTypeLabel(endpoint.api_type))} account is connected to this Endpoint yet. Use Connect account to sign in.` : 'No credential belongs to this Endpoint yet. Add one to start serving traffic.'}</p></div>`;
    const addAction = !endpoint.requires_credential
      ? ''
      : subscription
        ? `<button class="button secondary connect-account" data-provider="${provider.id}" data-endpoint="${endpoint.id}">${icon('plus', 'button-icon')}Connect account</button>`
        : `<button class="button secondary add-credential" data-provider="${provider.id}" data-endpoint="${endpoint.id}">${icon('plus', 'button-icon')}Add credential</button>`;
    const endpointKind = endpoint.endpoint_type_label || formatType(endpoint.api_type);
    return `<article class="endpoint-card${subscription ? ' subscription-endpoint' : ''}"><header class="endpoint-head"><span class="endpoint-index">${index + 1}</span><div class="endpoint-identity"><div><h3>${escapeHtml(endpoint.id)}</h3><span class="kind">${escapeHtml(endpointKind)}</span></div><code>${escapeHtml(endpoint.fixed_base_url || endpoint.base_url)}</code></div><div class="endpoint-facts"><span><strong>${endpointModels}</strong> models</span>${credentialFact}${endpoint.socks5_proxy ? `<span>Proxy <code>${escapeHtml(endpoint.socks5_proxy)}</code></span>` : ''}${cooldownPolicy}</div><div class="endpoint-actions"><button class="endpoint-edit text-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Edit settings</button><button class="endpoint-delete text-link danger-link" data-provider="${provider.id}" data-endpoint="${endpoint.id}" aria-label="Delete endpoint ${escapeHtml(endpoint.id)}">Delete endpoint</button></div></header>${poolSummary}<section class="endpoint-keys${subscription ? ' subscription-credential' : ''}"><div class="endpoint-keys-head"><div>${headCopy}</div><div class="endpoint-key-actions">${renewalStatus}${enabled.length > 1 ? `<button class="text-link edit-traffic" data-provider="${provider.id}" data-endpoint="${endpoint.id}">Distribute traffic</button>` : ''}${addAction}</div></div>${endpoint.requires_credential ? `<div class="key-list">${rows}${emptyRow}</div>` : ''}</section></article>`;
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
  const defaultsDescription = requestDefaultsExtension ? (requestDefaultsExtension.enabled ? 'Add default headers and JSON fields to Provider requests.' : 'Saved defaults are retained but are not currently applied to requests.') : 'Rebuild Yabane with the Request Defaults Extension to configure these values.';
  const defaultsStateClass = requestDefaultsExtension ? (requestDefaultsExtension.enabled ? 'defaults-enabled' : 'defaults-disabled') : 'defaults-unavailable';
  const defaultsEnableGuidance = requestDefaultsExtension?.runtime_configurable ? 'Enable it from Extensions to apply them again.' : 'Restart Yabane without --no-extensions to apply them again.';
  const defaultsNotice = requestDefaultsExtension && !requestDefaultsExtension.enabled ? `<div class="defaults-disabled-notice" role="status"><span class="defaults-disabled-mark" aria-hidden="true">!</span><span><strong>Request defaults are off</strong><small>No saved headers or body fields will be added to Provider requests. ${defaultsEnableGuidance}</small></span></div>` : '';
  const defaultsAction = requestDefaultsExtension ? '<button class="button secondary edit-provider-options">Configure</button>' : '<button class="button secondary include-request-defaults-extension" type="button">How to include</button>';
  const coverage = provider.endpoints.map(endpoint => { const count = Object.values(provider.model_endpoints).filter(ids => ids.includes(endpoint.id)).length; return `<div><span><strong>${escapeHtml(endpoint.id)}</strong><small>${escapeHtml(formatType(endpoint.api_type))}</small></span><b>${count.toLocaleString()}</b></div>`; }).join('');
  $('#provider-detail').innerHTML = `<nav class="provider-breadcrumb" aria-label="Breadcrumb"><button id="back-to-providers">Providers</button>${icon('chevron-right', 'breadcrumb-icon')}<strong>${escapeHtml(provider.name)}</strong></nav><header class="provider-hero"><div class="provider-hero-mark">${escapeHtml(provider.name.slice(0, 1).toUpperCase())}</div><div class="provider-hero-main"><span class="provider-eyebrow">Provider settings</span><h1>${escapeHtml(provider.name)}</h1><p>Requests use <code>${escapeHtml(provider.id)}/model-id</code>. This provider contains ${provider.endpoints.length} endpoint${provider.endpoints.length === 1 ? '' : 's'} and ${credentialCount(provider)} Provider credential${credentialCount(provider) === 1 ? '' : 's'}.</p></div><div class="provider-hero-actions"><button class="button secondary contextual-help" data-help-context="provider" data-provider="${provider.id}" type="button">${icon('help', 'button-icon')}Provider guide</button><button class="button secondary edit-provider" data-provider="${provider.id}" type="button">Edit provider</button><button class="delete-provider button danger" data-provider="${provider.id}">Delete provider</button></div></header><div class="provider-overview"><section class="card model-summary-card"><div class="card-head"><div><span class="section-kicker">Model catalog</span><h2>Discovered models</h2><p>${discovery}</p></div><div>${sharedVariants.length ? `<button class="button secondary manage-model-endpoints">Manage endpoint defaults</button>` : ''}<button class="text-link browse-provider-models" aria-expanded="false">Browse catalog</button><button class="text-link refresh-models" data-provider="${provider.id}">Refresh</button></div></div><div class="model-insights"><div class="model-insight"><strong>${provider.discovered_models.length.toLocaleString()}</strong><span>Models</span><small>Unique model IDs</small></div><div class="model-insight ${sharedVariants.length ? 'attention' : ''}"><strong>${sharedVariants.length.toLocaleString()}</strong><span>Shared models</span><small>${sharedVariants.length ? `${configuredPreferences} explicit default${configuredPreferences === 1 ? '' : 's'}` : 'No endpoint overlap'}</small></div><div class="endpoint-coverage"><header><span>Endpoint coverage</span><small>Models reported</small></header>${coverage || '<p>No endpoints configured</p>'}</div></div><div class="provider-model-browser" hidden><div class="model-browser-toolbar"><label class="model-filter"><svg class="model-search-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="10.5" cy="10.5" r="5.5"></circle><path d="m15 15 4 4"></path></svg><input type="text" role="searchbox" placeholder="Search model IDs" autocomplete="off" aria-label="Search model IDs"><button type="button" class="model-search-clear" aria-label="Clear search" hidden>${icon('close')}</button></label><span class="model-result-count"></span></div><div class="model-table"><header><span>Model ID</span><span>Available through</span><span>Default routing</span></header><div class="model-table-body"></div></div><footer class="model-pagination"><span class="model-page-status"></span><div><button type="button" class="button secondary model-page-previous">Previous</button><button type="button" class="button secondary model-page-next">Next</button></div></footer></div></section><section class="card defaults-card ${defaultsStateClass}"><div class="card-head"><div><span class="section-kicker">${defaultsAvailability} · ${escapeHtml(defaultsScope)}</span><h2>Request defaults</h2><p>${defaultsDescription}</p></div><div class="defaults-actions">${defaultsAction}</div></div>${defaultsNotice}<div class="request-defaults-summary"><div><span class="defaults-count">${headerCount}</span><span><strong>Headers</strong><small>${headerCount ? 'Configured' : 'Not configured'}</small></span></div><div><span class="defaults-count">${bodyCount}</span><span><strong>Body fields</strong><small>${bodyCount ? 'Configured' : 'Not configured'}</small></span></div></div></section></div><section class="endpoint-group"><div class="endpoint-group-head"><div><span class="section-kicker">Provider children</span><h2>API endpoints</h2><p>Each Endpoint is a connection to the Provider. Credentials and connected accounts belong only to their Endpoint.</p></div><button class="button primary add-endpoint" data-provider="${provider.id}">${icon('plus', 'button-icon')}Add endpoint</button></div><div class="endpoint-stack">${endpointHtml || '<div class="empty endpoint-empty"><h3>No endpoints</h3><p>Add an Endpoint to start routing requests.</p></div>'}</div></section>`;
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
  return provider.discovered_models.flatMap(model => endpointTypes.map(type => type.id).map(apiType => {
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

/// One Endpoint's identities and what each of them can serve right now. An
/// identity the Provider rate-limited leaves the pool for the length of its
/// cooldown, so the configured shares describe the healthy case while the
/// effective shares describe this moment.
function identityPool(endpoint) {
  const enabled = enabledCredentials(endpoint);
  const usable = enabled.filter(credential => credential.weight > 0);
  const eligible = usable.filter(credential => !credential.cooldown_seconds_remaining);
  const cooling = usable.filter(credential => credential.cooldown_seconds_remaining);
  return {enabled, eligible, cooling, configured: trafficShares(enabled), effective: trafficShares(eligible), seconds: endpoint.rate_limit_cooldown?.seconds || 0};
}
/// The wall-clock moment a live cooldown ends, so the pool can be reasoned about
/// without watching the page.
function cooldownEndsAt(seconds) {
  return new Date(Date.now() + seconds * 1000).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'});
}
function identityList(credentials) {
  const names = credentials.map(credential => escapeHtml(credential.name));
  const last = names.at(-1);
  return names.length < 3 ? names.join(' and ') : `${names.slice(0, -1).join(', ')} and ${last}`;
}
/// How long an identity stays out under a cooldown policy, in the words the
/// Endpoint card, the traffic dialog, and the edit form all use, so one policy is
/// never described three different ways. The configured length is the ceiling in
/// every mode, and only the Provider-only mode can pass without effect.
function cooldownDelayClause(policy) {
  const duration = formatCooldown(policy?.seconds || 0);
  if (policy?.mode === 'provider_only') return `for the delay the Provider reports, never longer than ${duration}`;
  if (policy?.mode === 'prefer_provider') return `for the delay the Provider reports, or ${duration} when it reports none, never longer than ${duration}`;
  return `for ${duration}`;
}
function cooldownNoDelayClause(policy) {
  return policy?.mode === 'provider_only'
    ? ' When the Provider reports no usable delay, nothing changes and requests keep reaching that identity.'
    : '';
}
function cooldownPolicyLabel(policy) {
  if (policy?.mode === 'provider_only') return 'Provider delay only';
  if (policy?.mode === 'prefer_provider') return 'Provider delay first';
  return '';
}
function formatClock(seconds) {
  return new Date(seconds * 1000).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'});
}
/// What this instance has observed of the Endpoint's cooldown policy. A policy
/// whose length comes from the Provider can be configured correctly and still
/// never arm, so the console reports what happened instead of leaving the
/// administrator unable to tell a working policy from a dead one.
function endpointPolicyActivity(endpoint) {
  const activity = endpoint.rate_limit_cooldown_activity;
  if (!activity || !endpoint.rate_limit_cooldown?.seconds) return '';
  const applied = activity.applied || 0;
  const skipped = activity.skipped || 0;
  const plural = (count, noun) => `${count} ${noun}${count === 1 ? '' : 's'}`;
  if (!applied && !skipped) return 'No rate-limit answer has reached this Endpoint since Yabane started.';
  if (!applied) return `${plural(skipped, 'rate-limit answer')} reported no usable delay, so no identity has been taken out yet.`;
  const last = activity.last_applied_at
    ? ` Most recently it took an identity out for ${formatCooldown(activity.last_seconds)} at ${formatClock(activity.last_applied_at)}.`
    : '';
  const skippedNote = skipped ? ` ${plural(skipped, 'rate-limit answer')} reported no usable delay.` : '';
  return `This policy has taken an identity out ${plural(applied, 'time')} since Yabane started.${last}${skippedNote}`;
}
/// How a group splits its own traffic, named the way the pool states it: a single
/// identity is just itself, because 100% of one group states nothing.
function groupShareText(group) {
  const names = group.members.map(member => `<strong>${escapeHtml(member.name)}</strong>`);
  if (names.length === 1) return names[0];
  return `${identityList(group.members)} — ${group.members.map(member => `${group.configured.get(member.id) ?? 0}%`).join(' / ')}`;
}
/// States the identity pool as the mechanism it is, instead of leaving the
/// administrator to assemble it from a policy field and a percentage: how
/// traffic is split while every identity is healthy, what a Provider rate limit
/// does to that split, and what is true right now.
function endpointPoolSummary(endpoint) {
  if (!endpoint.requires_credential || !endpoint.credentials.length) return '';
  const pool = identityPool(endpoint);
  const groups = identityGroups(endpoint);
  const carrier = carryingGroup(groups);
  const standbyGroups = groups.filter(group => group.standby);
  // The pool explains something only while a policy can take an identity out,
  // while one is out, while there is another identity to carry the traffic, or
  // while a group waits behind another one. Restating an Endpoint that only ever
  // sends as its single identity would be noise, and the edit dialog is where a
  // policy is chosen.
  if (pool.seconds === 0 && !pool.cooling.length && pool.enabled.length < 2 && !standbyGroups.length) return '';
  const noun = identityNoun(endpoint, 2);
  const singular = identityNoun(endpoint, 1);
  const retryHint = 'The request that hit the limit still receives the Provider’s own answer.';
  if (!pool.enabled.length) {
    return `<div class="endpoint-pool" data-tone="idle"><p><strong>No ${escapeHtml(noun)} are enabled.</strong> Requests to this Endpoint fail until one is enabled.</p></div>`;
  }
  // A standby group is a second claim about the same traffic, so the pool states
  // where one group ends and the next begins instead of calling every identity
  // part of one rotation.
  const steady = standbyGroups.length
    ? `Traffic always uses <strong>Priority ${groups[0].priority}</strong> first (${groupShareText(groups[0])}).${standbyGroups.map(group => ` <strong>Priority ${group.priority}</strong> (${groupShareText(group)}) only carries it while every identity in the ${standbyGroups.length > 1 ? 'groups' : 'group'} above it is cooling down.`).join('')}`
    : pool.enabled.length === 1
      ? `Every request leaves as <strong>${escapeHtml(pool.enabled[0].name)}</strong>.`
      : `Traffic rotates between ${pool.enabled.length} ${escapeHtml(noun)} by weight — ${pool.enabled.map(key => `${pool.configured.get(key.id) ?? 0}%`).join(' / ')}.`;
  const policy = {seconds: pool.seconds, mode: endpoint.rate_limit_cooldown?.mode || 'fixed'};
  let failure;
  if (pool.seconds > 0) {
    const delay = cooldownDelayClause(policy);
    const noDelay = cooldownNoDelayClause(policy);
    failure = standbyGroups.length
      ? `If the Provider rate-limits one, it leaves the pool ${delay}, and its own group carries every request until the whole group is out; the next priority group takes over from there and hands the traffic back when the cooldown ends.${noDelay} ${retryHint}`
      : pool.enabled.length === 1
        ? `If the Provider rate-limits this ${escapeHtml(singular)}, it leaves the pool ${delay}. Nothing else can carry the traffic, so requests still go out.${noDelay} ${retryHint}`
        : `If the Provider rate-limits one, it leaves the pool ${delay}, and the others carry every request until it returns automatically.${noDelay}`;
  } else {
    failure = standbyGroups.length
      ? 'Rate limits are not tracked here, so an identity the Provider rate-limits keeps its configured share and a lower priority group never takes over. Only disabling every identity above it hands that traffic a lower group.'
      : pool.enabled.length === 1
        ? 'Rate limits are not tracked here, so a request that hits one receives the Provider’s own answer.'
        : 'Rate limits are not tracked here, so an identity the Provider rate-limits keeps its configured share. Edit settings to let the others take over while it is limited.';
  }
  let now = '';
  let tone = 'steady';
  if (pool.cooling.length) {
    tone = 'cooling';
    // Under priority groups only the group that carries traffic hands it out, so
    // this sentence names that group's identities rather than every healthy
    // identity, which would include the ones waiting in a standby group.
    const carrying = standbyGroups.length ? carrier.members.filter(key => !key.cooldown_seconds_remaining) : pool.eligible;
    now = pool.cooling.length === pool.enabled.length
      ? `Every ${escapeHtml(noun)} is cooling down right now, so requests still go out and the Provider’s own 429 reaches the caller.`
      : `Right now ${identityList(pool.cooling)} ${pool.cooling.length === 1 ? 'is' : 'are'} out until ${pool.cooling.map(key => cooldownEndsAt(key.cooldown_seconds_remaining)).join(' / ')}, so ${identityList(carrying)} carr${carrying.length === 1 ? 'ies' : 'y'} every request.`;
    if (standbyGroups.length) {
      now += carrier.standby
        ? ` Priority ${carrier.priority} is carrying the traffic while the groups above it are out, and hands it back as they return.`
        : ` Priority ${carrier.priority} still carries the traffic, so the groups below it wait.`;
    }
  }
  const observed = endpointPolicyActivity(endpoint);
  // A standby group that can never be reached is a configuration the
  // administrator has to see: nothing leaves the rotation while no cooldown
  // policy is configured, so the lower group would only take over by disabling
  // every identity above it.
  const unreachable = standbyGroups.length && pool.seconds === 0
    ? `<p class="pool-standby">Priority ${standbyGroups[0].priority} never takes over: this Endpoint does not track rate limits, so no identity above it ever leaves the rotation. Set a rate-limit cooldown, or move those identities into Priority ${groups[0].priority}.</p>`
    : '';
  return `<div class="endpoint-pool" data-tone="${tone}">${now ? `<p class="pool-now">${now}</p>` : ''}<p class="pool-steady">${steady}</p><p class="pool-failure">${failure}</p>${unreachable}${observed ? `<p class="pool-observed">${observed}</p>` : ''}</div>`;
}

function bindProviderActions() {
  $$('.edit-provider').forEach(button => button.addEventListener('click', () => openProviderIdentityDialog(button.dataset.provider)));
  $$('.delete-provider').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    const affectedRoutes = modelRoutes.filter(route => route.targets.some(target => target.provider_id === provider.id));
    const affectedTargets = affectedRoutes.reduce((count, route) => count + route.targets.filter(target => target.provider_id === provider.id).length, 0);
    const removedRoutes = affectedRoutes.filter(route => route.targets.every(target => target.provider_id === provider.id)).length;
    const scopedKeys = authSettings.api_keys.filter(key => key.provider_ids.includes(provider.id));
    const revokedKeys = scopedKeys.filter(key => key.provider_ids.length === 1).length;
    const updatedKeys = scopedKeys.length - revokedKeys;
    const routeImpact = affectedTargets
      ? `This also removes ${affectedTargets} model-route destination${affectedTargets === 1 ? '' : 's'}${removedRoutes ? ` and deletes ${removedRoutes} route${removedRoutes === 1 ? '' : 's'} left without a destination` : ''}.`
      : 'No model-route destinations currently use this Provider.';
    const keyActions = [];
    if (revokedKeys) keyActions.push(`revokes ${revokedKeys} Gateway API key${revokedKeys === 1 ? '' : 's'} scoped only to this Provider`);
    if (updatedKeys) keyActions.push(`removes this Provider from ${updatedKeys} other key allowlist${updatedKeys === 1 ? '' : 's'}`);
    const keyImpact = keyActions.length ? ` It ${keyActions.join(' and ')}.` : '';
    const message = `Delete provider “${provider.name}”?\n\nThis permanently deletes its ${provider.endpoints.length} Endpoint${provider.endpoints.length === 1 ? '' : 's'} and ${credentialCount(provider)} Provider credential${credentialCount(provider) === 1 ? '' : 's'}. ${routeImpact}${keyImpact}`;
    if (!confirm(message)) return;
    const response = await fetch(`/admin/providers/${provider.id}`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    selectedProviderId = null;
    await Promise.all([loadProviders(), loadRoutes(), loadAuth()]);
  }));
  $$('.endpoint-edit').forEach(button => button.addEventListener('click', () => openEndpointDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.endpoint-delete').forEach(button => button.addEventListener('click', async () => {
    const provider = providers.find(item => item.id === button.dataset.provider);
    const endpoint = provider.endpoints.find(item => item.id === button.dataset.endpoint);
    const credentialImpact = endpoint.credentials.length
      ? `its ${endpoint.credentials.length} credential${endpoint.credentials.length === 1 ? '' : 's'}`
      : 'no credentials';
    const affectedRoutes = modelRoutes.filter(route => route.targets.some(target => target.provider_id === provider.id && target.endpoint_id === endpoint.id));
    const affectedTargets = affectedRoutes.reduce((count, route) => count + route.targets.filter(target => target.provider_id === provider.id && target.endpoint_id === endpoint.id).length, 0);
    const removedRoutes = affectedRoutes.filter(route => route.targets.every(target => target.provider_id === provider.id && target.endpoint_id === endpoint.id)).length;
    const routeImpact = affectedTargets
      ? ` It removes ${affectedTargets} model-route destination${affectedTargets === 1 ? '' : 's'}${removedRoutes ? ` and deletes ${removedRoutes} route${removedRoutes === 1 ? '' : 's'} left without a destination` : ''}.`
      : '';
    const message = `Delete endpoint “${endpoint.id}”?\n\nThis also deletes ${credentialImpact}, removes its discovered-model availability, and removes destinations that route to it.${routeImpact}`;
    if (!confirm(message)) return;
    const response = await fetch(`/admin/providers/${provider.id}/endpoints/${endpoint.id}`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    await Promise.all([loadProviders(), loadRoutes()]);
  }));
  $$('.add-credential').forEach(button => button.addEventListener('click', () => openCredentialDialog(button.dataset.provider, button.dataset.endpoint)));
  // Connecting one more account is addressed by Endpoint type, which the Endpoint
  // itself reports; the sign-in request reuses the Endpoint's own proxy.
  $$('.connect-account').forEach(button => button.addEventListener('click', () => {
    const endpoint = providers.find(item => item.id === button.dataset.provider)?.endpoints.find(item => item.id === button.dataset.endpoint);
    if (!endpoint?.sign_in) return;
    beginEndpointSignIn({endpoint_type: endpoint.api_type, provider_id: button.dataset.provider, endpoint_id: button.dataset.endpoint}, null);
  }));
  $$('.edit-traffic').forEach(button => button.addEventListener('click', () => openTrafficDialog(button.dataset.provider, button.dataset.endpoint)));
  $$('.credential-rename').forEach(button => button.addEventListener('click', () => openCredentialNameDialog(button.dataset.provider, button.dataset.endpoint, button.dataset.credential)));
  $$('.credential-toggle').forEach(button => button.addEventListener('click', async () => {
    await patchCredential(button.dataset.provider, button.dataset.endpoint, button.dataset.credential, {enabled: button.dataset.enabled !== 'true'});
  }));
  $$('.credential-delete').forEach(button => button.addEventListener('click', async () => {
    if (!confirm(`Delete credential “${button.dataset.name}”?\n\nModel-route destinations pinning this exact credential will also be removed.`)) return;
    const response = await fetch(`/admin/providers/${button.dataset.provider}/endpoints/${button.dataset.endpoint}/credentials/${button.dataset.credential}`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    await Promise.all([loadProviders(), loadRoutes()]);
  }));
  $$('.clear-cooldown').forEach(button => button.addEventListener('click', async () => {
    const response = await fetch(`/admin/providers/${button.dataset.provider}/endpoints/${button.dataset.endpoint}/credentials/${button.dataset.credential}/cooldown`, {method: 'DELETE'});
    if (!response.ok) return showApiError(response, null);
    await loadProviders();
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

async function patchCredential(providerId, endpointId, credentialId, update) {
  const response = await fetch(`/admin/providers/${providerId}/endpoints/${endpointId}/credentials/${credentialId}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(update)});
  if (!response.ok) return showApiError(response, null);
  await loadProviders();
}

function emptyPricingTable() { return {updated_at: 0, models: {}, incoming_models: {}}; }
function pricingTargetName() { return $('#central-pricing-form [name="price_name"]:checked')?.value === 'incoming' ? 'incoming' : 'outgoing'; }
function pricingTable(scope, providerId = null, endpointId = null) {
  if (scope === 'global') return globalPricing;
  const provider = providers.find(item => item.id === providerId);
  if (scope === 'provider') return provider?.pricing || emptyPricingTable();
  return provider?.endpoints.find(item => item.id === endpointId)?.pricing || emptyPricingTable();
}

function pricingEntries() {
  const entries = [];
  const add = (scope, name, providerId, endpointId, models, updatedAt) => {
    for (const [model, rates] of Object.entries(models || {})) entries.push({scope, name, providerId, endpointId, model, rates, updatedAt});
  };
  add('global', 'outgoing', null, null, globalPricing.models, globalPricing.updated_at);
  add('global', 'incoming', null, null, globalPricing.incoming_models, globalPricing.updated_at);
  for (const provider of providers) {
    add('provider', 'outgoing', provider.id, null, provider.pricing?.models, provider.pricing?.updated_at);
    for (const endpoint of provider.endpoints) add('endpoint', 'outgoing', provider.id, endpoint.id, endpoint.pricing?.models, endpoint.pricing?.updated_at);
  }
  return entries.sort((a, b) => a.model.localeCompare(b.model) || a.scope.localeCompare(b.scope) || (a.providerId || '').localeCompare(b.providerId || '') || (a.endpointId || '').localeCompare(b.endpointId || ''));
}

function pricingNameLabel(name) { return name === 'incoming' ? 'Incoming' : 'Outgoing'; }
function pricingScopeText(entry) {
  if (entry.scope === 'global') return {label: 'Global', detail: 'All Providers'};
  if (entry.scope === 'provider') return {label: 'Provider', detail: entry.providerId};
  return {label: 'Endpoint', detail: `${entry.providerId} → ${entry.endpointId}`};
}
function formatPricingRate(value) { return value == null ? 'Inherit' : `$${new Intl.NumberFormat('en', {maximumFractionDigits: 12}).format(value)}`; }
function showPricingList() { $('#pricing-list-page').hidden = false; $('#pricing-editor-page').hidden = true; pricingEditTarget = null; }
function renderPricingPage() {
  if (!$('#pricing-view')) return;
  const entries = pricingEntries();
  $('#pricing-global-count').textContent = entries.filter(entry => entry.scope === 'global').length.toLocaleString();
  $('#pricing-provider-count').textContent = entries.filter(entry => entry.scope === 'provider').length.toLocaleString();
  $('#pricing-endpoint-count').textContent = entries.filter(entry => entry.scope === 'endpoint').length.toLocaleString();
  const query = $('#pricing-search').value.trim().toLowerCase(); const scope = $('#pricing-scope-filter').value;
  const filtered = entries.filter(entry => (!scope || entry.scope === scope) && (!query || `${entry.model} ${entry.name} ${pricingNameLabel(entry.name)} ${entry.scope} ${entry.providerId || ''} ${entry.endpointId || ''}`.toLowerCase().includes(query)));
  $('#pricing-list-empty').hidden = entries.length > 0;
  $('#pricing-table-wrap').hidden = entries.length === 0;
  $('#pricing-table-body').innerHTML = filtered.length ? filtered.map((entry, index) => {
    const scopeText = pricingScopeText(entry); const incomplete = entry.rates.input_per_million == null || entry.rates.output_per_million == null;
    const incompleteText = entry.name === 'incoming' ? 'Incomplete fallback rate' : 'Incomplete effective rate';
    return `<tr><td><code>${escapeHtml(entry.model)}</code><span class="pricing-name-chip ${entry.name}" title="${entry.name === 'incoming' ? 'Matched against the model name the caller sends' : 'Matched against the model ID Yabane sends to the Provider'}">${pricingNameLabel(entry.name)}</span>${incomplete ? `<small class="pricing-incomplete">${incompleteText}</small>` : ''}</td><td><span class="pricing-scope-badge ${entry.scope}">${scopeText.label}</span><small>${escapeHtml(scopeText.detail)}</small></td><td>${formatPricingRate(entry.rates.input_per_million)}</td><td>${formatPricingRate(entry.rates.output_per_million)}</td><td>${formatPricingRate(entry.rates.cache_read_per_million)}</td><td>${entry.updatedAt ? escapeHtml(new Date(entry.updatedAt * 1000).toLocaleDateString()) : '—'}</td><td><button class="text-link edit-pricing-entry" type="button" data-pricing-index="${index}">Edit</button></td></tr>`;
  }).join('') : '<tr><td colspan="7"><div class="activity-empty">No prices match this search.</div></td></tr>';
  $$('.edit-pricing-entry').forEach(button => button.addEventListener('click', () => openCentralPricingEditor(filtered[Number(button.dataset.pricingIndex)])));
}

function pricingSuggestions(scope, providerId, endpointId, name = 'outgoing') {
  if (name === 'incoming') {
    // These rules are Global-only, and the caller's name is what routes accept.
    const names = new Set(Object.keys(globalPricing.incoming_models || {}));
    for (const route of modelRoutes) if (route.pattern) names.add(route.pattern);
    for (const activity of pricingActivityModels) if (activity.incomingModel) names.add(activity.incomingModel);
    return [...names].filter(Boolean).sort();
  }
  const models = new Set(Object.keys(globalPricing.models || {}));
  const provider = providers.find(item => item.id === providerId);
  const candidates = scope === 'global' ? providers : provider ? [provider] : [];
  for (const item of candidates) {
    item.discovered_models.forEach(model => {
      if (scope !== 'endpoint' || (item.model_endpoints[model] || []).includes(endpointId)) models.add(model);
    });
    Object.keys(item.pricing?.models || {}).forEach(model => models.add(model));
    for (const endpoint of item.endpoints) {
      if (scope !== 'endpoint' || endpoint.id === endpointId) Object.keys(endpoint.pricing?.models || {}).forEach(model => models.add(model));
    }
  }
  for (const activity of pricingActivityModels) {
    if (scope === 'global' || activity.providerId === providerId && (scope !== 'endpoint' || activity.endpointId === endpointId)) models.add(activity.model);
  }
  for (const route of modelRoutes) for (const target of route.targets) {
    if (scope === 'global' || target.provider_id === providerId && (scope !== 'endpoint' || target.endpoint_id === endpointId)) models.add(target.upstream_model);
  }
  return [...models].filter(Boolean).sort();
}
function updatePricingEndpoints() {
  const provider = providers.find(item => item.id === $('#pricing-provider').value);
  const select = $('#pricing-endpoint'); const previous = select.value;
  select.replaceChildren(...(provider?.endpoints || []).map(endpoint => new Option(endpoint.id, endpoint.id)));
  if ([...select.options].some(option => option.value === previous)) select.value = previous;
}
function pricingRouteMatches(routePattern, pricePattern) {
  if (!routePattern) return false;
  if (pricePattern.endsWith('*')) {
    const prefix = pricePattern.slice(0, -1);
    return routePattern.startsWith(prefix) || routePattern === prefix;
  }
  return routePattern === pricePattern || routePattern.endsWith('*') && pricePattern.startsWith(routePattern.slice(0, -1));
}
function renderPricingRouteExample(name, value) {
  const container = $('#pricing-route-example');
  const pattern = value.replace(/\*$/, '');
  const routes = name === 'incoming' && value.length >= 2 ? modelRoutes.filter(route => pricingRouteMatches(route.pattern || '', value)) : [];
  const destinations = routes.flatMap(route => route.targets.map(target => `<code>${escapeHtml(target.provider_id)}/${escapeHtml(target.endpoint_id)}</code> as <code>${escapeHtml(target.upstream_model || 'the requested name')}</code>`));
  if (!destinations.length) { container.hidden = true; container.replaceChildren(); return; }
  const list = destinations.length === 1 ? destinations[0] : `${destinations.slice(0, -1).join(', ')} and ${destinations.at(-1)}`;
  container.innerHTML = `<p>Requests for <code>${escapeHtml(pattern)}</code> reach ${list}.</p><p>This price applies to each of those destinations that has no outgoing price of its own.</p>`;
  container.hidden = false;
}
function updatePricingTargetFields() {
  const incoming = pricingTargetName() === 'incoming';
  $$('#central-pricing-form .pricing-override-badge').forEach(badge => { badge.textContent = incoming ? 'Optional' : 'Optional override'; });
  for (const field of ['input_per_million', 'output_per_million']) $('#central-pricing-form [name="' + field + '"]').placeholder = incoming ? 'Not set' : 'Inherit';
  // A caller-side name belongs to no single connection, so its price is always Global.
  const locked = Boolean(pricingEditTarget?.originalModel);
  const globalScope = $('#central-pricing-form [name="scope"][value="global"]');
  const narrowedScope = incoming && !globalScope.checked;
  if (narrowedScope) globalScope.checked = true;
  $$('#central-pricing-form [name="scope"]').forEach(input => {
    const unavailable = incoming && input.value !== 'global';
    input.disabled = locked || unavailable;
    input.closest('label').classList.toggle('locked', (locked && !input.checked) || unavailable);
  });
  $('#pricing-scope-hint').textContent = incoming
    ? 'This price applies to every Provider, so there is nothing left to narrow.'
    : 'Start with a Global rule and add a narrower override only where a Provider or Endpoint has different rates.';
  $('#pricing-incoming-scope-note').hidden = !incoming;
  // A forced Global scope still has to hide the Provider/Endpoint fields and refresh suggestions.
  if (narrowedScope) updatePricingScopeFields();
}
function updatePricingModelSuggestions() {
  const scope = $('#central-pricing-form [name="scope"]:checked').value; const name = pricingTargetName();
  const suggestions = pricingSuggestions(scope, $('#pricing-provider').value, $('#pricing-endpoint').value, name);
  $('#pricing-model-suggestions').replaceChildren(...suggestions.map(model => new Option(model)));
  $('#pricing-model').setAttribute('list', 'pricing-model-suggestions');
  const value = $('#pricing-model').value.trim();
  const validPattern = value && /^[^*]+\*?$/.test(value);
  const outgoing = name !== 'incoming';
  $('#pricing-model-notice').textContent = !value
    ? `${suggestions.length} known ${outgoing ? 'Provider model pattern' : 'incoming model name'}${suggestions.length === 1 ? '' : 's'} available as suggestions. Custom patterns are allowed.`
    : !validPattern
      ? 'Enter an exact model ID or one trailing * for a prefix match.'
      : value.endsWith('*')
        ? outgoing
          ? `Matches every Provider model beginning with "${value.slice(0, -1)}". Exact rules still take priority.`
          : `Matches every incoming model beginning with "${value.slice(0, -1)}". Destinations with their own outgoing price keep it.`
        : !suggestions.includes(value)
          ? `Custom ${outgoing ? 'Provider model ID' : 'incoming model name'} — it was not discovered or used by a route, but it will still be saved as entered.`
          : outgoing ? 'Exact model ID.' : 'Matches requests carrying this name where the destination has no outgoing price.';
  renderPricingRouteExample(name, value);
  scheduleModelsDevReference(value);
}

function loadModelsDevCatalog() {
  if (!modelsDevCatalogPromise) {
    modelsDevCatalogPromise = fetch('https://models.dev/api.json', {credentials: 'omit'})
      .then(response => {
        if (!response.ok) throw new Error(`models.dev returned HTTP ${response.status}`);
        return response.json();
      })
      .then(catalog => Object.entries(catalog).flatMap(([providerId, provider]) =>
        Object.entries(provider.models || {}).flatMap(([modelId, model]) => {
          const cost = model.cost;
          if (!cost || !Number.isFinite(cost.input) || !Number.isFinite(cost.output)) return [];
          return [{providerId, providerName: provider.name || providerId, modelId, modelName: model.name || modelId, cost}];
        })
      ))
      .catch(error => { modelsDevCatalogPromise = null; throw error; });
  }
  return modelsDevCatalogPromise;
}

function modelsDevMatchScore(reference, query) {
  const id = reference.modelId.toLowerCase(); const name = reference.modelName.toLowerCase();
  if (id === query) return 0;
  if (id.endsWith(`/${query}`)) return 1;
  if (name === query) return 2;
  if (id.startsWith(query)) return 3;
  if (id.includes(`/${query}`)) return 4;
  if (id.includes(query)) return 5;
  if (name.includes(query)) return 6;
  return null;
}

function renderModelsDevReferences(results, message = '') {
  modelsDevReferenceResults = results;
  $('#pricing-reference-status').textContent = message;
  $('#pricing-reference-status').hidden = !message;
  $('#pricing-reference-results').innerHTML = results.map((reference, index) => {
    const cache = Number.isFinite(reference.cost.cache_read) ? `$${formatPricingRate(reference.cost.cache_read).slice(1)}` : 'Not listed';
    return `<article class="pricing-reference-item"><header><div><strong>${escapeHtml(reference.modelName)}</strong><small>${escapeHtml(reference.providerName)}</small></div><span>${escapeHtml(reference.providerId)}</span></header><code title="${escapeHtml(reference.modelId)}">${escapeHtml(reference.modelId)}</code><dl><div><dt>Input / M</dt><dd>$${escapeHtml(reference.cost.input)}</dd></div><div><dt>Output / M</dt><dd>$${escapeHtml(reference.cost.output)}</dd></div><div><dt>Cache read / M</dt><dd>${escapeHtml(cache)}</dd></div></dl><button class="button secondary use-reference-rates" type="button" data-reference-index="${index}">Use rates</button></article>`;
  }).join('');
}

function scheduleModelsDevReference(value) {
  clearTimeout(modelsDevSearchTimer);
  const query = value.trim().replace(/\*$/, '').toLowerCase(); const sequence = ++modelsDevSearchSequence;
  if (!matchMedia('(min-width:1280px)').matches || query.length < 2) {
    renderModelsDevReferences([], 'Enter at least two model characters to find references.');
    return;
  }
  $('#pricing-reference-status').hidden = false;
  $('#pricing-reference-status').textContent = 'Loading models.dev references…';
  $('#pricing-reference-results').replaceChildren();
  modelsDevSearchTimer = setTimeout(async () => {
    try {
      const catalog = await loadModelsDevCatalog();
      if (sequence !== modelsDevSearchSequence) return;
      const results = catalog
        .map(reference => ({reference, score: modelsDevMatchScore(reference, query)}))
        .filter(item => item.score != null)
        .sort((left, right) => left.score - right.score || left.reference.providerName.localeCompare(right.reference.providerName) || left.reference.modelId.localeCompare(right.reference.modelId))
        .slice(0, 8)
        .map(item => item.reference);
      renderModelsDevReferences(results, results.length ? '' : 'No priced models.dev references match this pattern.');
    } catch {
      if (sequence === modelsDevSearchSequence) renderModelsDevReferences([], 'models.dev references are unavailable. You can still enter rates manually.');
    }
  }, 350);
}
function updatePricingScopeFields() {
  const scope = $('#central-pricing-form [name="scope"]:checked').value;
  const incoming = $('#central-pricing-form [name="price_name"][value="incoming"]');
  const global = scope === 'global';
  incoming.disabled = !global;
  if (!global && incoming.checked) $('#central-pricing-form [name="price_name"][value="outgoing"]').checked = true;
  incoming.closest('label').classList.toggle('locked', !global);
  $('#pricing-resource-fields').hidden = global;
  $('#pricing-endpoint-field').hidden = scope !== 'endpoint';
  $('#pricing-provider').required = !global;
  $('#pricing-endpoint').required = scope === 'endpoint';
  updatePricingEndpoints(); updatePricingTargetFields(); updatePricingModelSuggestions();
}
function openCentralPricingEditor(options = {}) {
  const form = $('#central-pricing-form'); form.reset(); $('#central-pricing-error').textContent = '';
  const scope = options.scope || 'global'; const providerId = options.providerId || options.provider_id || providers[0]?.id || ''; const endpointId = options.endpointId || options.endpoint_id || '';
  const model = options.model || options.modelId || '';
  const table = pricingTable(scope, providerId, endpointId);
  const incomingModel = Boolean(model && table.incoming_models?.[model]);
  const name = options.name || (incomingModel ? 'incoming' : 'outgoing');
  const existing = Boolean(options.rates) || Boolean(model && (table.models?.[model] || table.incoming_models?.[model]));
  const rates = options.rates || table.models?.[model] || table.incoming_models?.[model] || {};
  pricingEditTarget = {scope, providerId, endpointId, originalModel: existing ? model : null, originalName: existing ? name : null};
  $('#pricing-provider').replaceChildren(...providers.map(provider => new Option(provider.name, provider.id)));
  if (providerId && [...$('#pricing-provider').options].some(option => option.value === providerId)) $('#pricing-provider').value = providerId;
  form.elements.scope.value = scope;
  form.elements.price_name.value = name;
  $$('#central-pricing-form [name="scope"]').forEach(input => { input.disabled = existing; input.closest('label').classList.toggle('locked', existing && !input.checked); });
  updatePricingEndpoints();
  if (endpointId && [...$('#pricing-endpoint').options].some(option => option.value === endpointId)) $('#pricing-endpoint').value = endpointId;
  form.elements.model.value = model;
  for (const field of ['input_per_million', 'output_per_million', 'cache_read_per_million']) form.elements[field].value = rates[field] ?? '';
  $('#pricing-editor-title').textContent = existing ? `Edit ${model}` : 'Add model price';
  $('#pricing-editor-breadcrumb').textContent = existing ? 'Edit price' : 'Add price';
  $('#pricing-editor-description').textContent = existing ? 'Update this explicit rule. Activity history keeps the rate it recorded at request time.' : 'Price the model name the caller sends, or the model ID Yabane sends to the Provider.';
  $('#delete-pricing-rule').hidden = !existing;
  $('#pricing-list-page').hidden = true; $('#pricing-editor-page').hidden = false;
  updatePricingScopeFields(); $('#pricing-model').focus();
}
async function savePricingScope(scope, providerId, endpointId, table) {
  const url = scope === 'global' ? '/admin/pricing' : scope === 'provider' ? `/admin/pricing/providers/${encodeURIComponent(providerId)}` : `/admin/pricing/providers/${encodeURIComponent(providerId)}/endpoints/${encodeURIComponent(endpointId)}`;
  return fetch(url, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify(table)});
}
async function refreshPricingConfiguration() { await Promise.all([loadPricing(), loadProviders()]); renderPricingPage(); }

$('#open-pricing-editor').addEventListener('click', () => openCentralPricingEditor());
$('#empty-add-pricing').addEventListener('click', () => openCentralPricingEditor());
$('#back-to-pricing').addEventListener('click', () => { showPricingList(); renderPricingPage(); });
$('#cancel-pricing-edit').addEventListener('click', () => { showPricingList(); renderPricingPage(); });
$('#pricing-search').addEventListener('input', renderPricingPage);
$('#pricing-scope-filter').addEventListener('change', renderPricingPage);
$$('#central-pricing-form [name="scope"]').forEach(input => input.addEventListener('change', updatePricingScopeFields));
$('#pricing-provider').addEventListener('change', () => { updatePricingEndpoints(); updatePricingModelSuggestions(); });
$('#pricing-endpoint').addEventListener('change', updatePricingModelSuggestions);
$('#pricing-model').addEventListener('input', updatePricingModelSuggestions);
$$('#central-pricing-form [name="price_name"]').forEach(input => input.addEventListener('change', () => { updatePricingTargetFields(); updatePricingModelSuggestions(); }));
$('#pricing-reference-results').addEventListener('click', event => {
  const button = event.target.closest('.use-reference-rates');
  if (!button) return;
  const reference = modelsDevReferenceResults[Number(button.dataset.referenceIndex)];
  if (!reference) return;
  const form = $('#central-pricing-form');
  form.elements.input_per_million.value = reference.cost.input;
  form.elements.output_per_million.value = reference.cost.output;
  form.elements.cache_read_per_million.value = Number.isFinite(reference.cost.cache_read) ? reference.cost.cache_read : '';
  $('#central-pricing-error').textContent = '';
});
$('#central-pricing-form').addEventListener('submit', async event => {
  event.preventDefault(); const form = event.currentTarget; const scope = form.elements.scope.value; const providerId = form.elements.provider_id.value; const endpointId = form.elements.endpoint_id.value;
  const name = pricingTargetName(); const model = form.elements.model.value.trim(); const outgoing = name !== 'incoming';
  if (!/^[^*]+\*?$/.test(model)) return ($('#central-pricing-error').textContent = 'Use an exact model ID or one trailing * for a prefix match.');
  const values = ['input_per_million', 'output_per_million', 'cache_read_per_million'].map(field => form.elements[field].value === '' ? null : Number(form.elements[field].value));
  if (values.every(value => value == null)) return ($('#central-pricing-error').textContent = 'Enter at least one rate.');
  if (values.some(value => value != null && (!Number.isFinite(value) || value < 0))) return ($('#central-pricing-error').textContent = 'Rates must be finite, non-negative numbers.');
  const table = structuredClone(pricingTable(scope, providerId, endpointId)); table.models ||= {}; table.incoming_models ||= {};
  const {originalModel: oldModel = null, originalName: oldName = null} = pricingEditTarget || {};
  const normalized = name === 'incoming' ? table.incoming_models : table.models;
  const other = name === 'incoming' ? table.models : table.incoming_models;
  const oldMap = oldName === 'incoming' ? table.incoming_models : table.models;
  const preservedCacheWrite = oldModel && oldName === name ? oldMap[oldModel]?.cache_write_per_million ?? null : null;
  if (oldModel) delete oldMap[oldModel];
  if (normalized[model]) return ($('#central-pricing-error').textContent = `A ${outgoing ? 'outgoing' : 'incoming'} price already exists for this model pattern in the selected scope. Edit the existing rule instead.`);
  if (other[model]) return ($('#central-pricing-error').textContent = `This pattern is already priced as ${outgoing ? 'an incoming' : 'an outgoing'} name in this scope. Prices for the two names must be separate patterns.`);
  normalized[model] = {input_per_million: values[0], output_per_million: values[1], cache_read_per_million: values[2], cache_write_per_million: preservedCacheWrite};
  const response = await savePricingScope(scope, providerId, endpointId, table);
  if (!response.ok) return showApiError(response, $('#central-pricing-error'));
  await refreshPricingConfiguration(); showPricingList();
});
$('#delete-pricing-rule').addEventListener('click', async () => {
  const {scope, providerId, endpointId, originalModel, originalName} = pricingEditTarget || {};
  if (!originalModel || !confirm(`Delete the ${originalName === 'incoming' ? 'incoming' : 'outgoing'} price for “${originalModel}”?\n\nActivity records keep the cost they recorded at request time.`)) return;
  const table = structuredClone(pricingTable(scope, providerId, endpointId)); table.incoming_models ||= {};
  delete (originalName === 'incoming' ? table.incoming_models : table.models)[originalModel];
  const response = await savePricingScope(scope, providerId, endpointId, table);
  if (!response.ok) return showApiError(response, $('#central-pricing-error'));
  await refreshPricingConfiguration(); showPricingList();
});

/// Connection settings and the rate-limit policy are read and changed at different
/// times, so the Endpoint dialog keeps them on separate tabs instead of one column
/// that has to be scrolled to reach either half.
function selectEndpointTab(tab) {
  $$('#endpoint-form [data-endpoint-tab]').forEach(item => {
    const active = item.dataset.endpointTab === tab;
    item.classList.toggle('active', active);
    item.setAttribute('aria-selected', String(active));
  });
  $$('#endpoint-form [data-endpoint-panel]').forEach(panel => { panel.hidden = panel.dataset.endpointPanel !== tab; });
}
/// Brings the tab that owns a control forward, so a required field or a rejected
/// value is never reported while its field is on the other tab.
function revealEndpointField(control) {
  const panel = control?.closest?.('[data-endpoint-panel]');
  if (panel) selectEndpointTab(panel.dataset.endpointPanel);
}
function openEndpointDialog(providerId, endpointId = null) {
  const form = $('#endpoint-form'); form.reset(); form.elements.provider_id.value = providerId; form.dataset.endpointId = endpointId || ''; form.elements.id.dataset.edited = ''; delete form.elements.base_url.dataset.previousValue; $('#endpoint-error').textContent = ''; form.querySelector('.base-url-notice').textContent = '';
  const provider = providers.find(item => item.id === providerId);
  const endpoint = endpointId ? provider?.endpoints.find(item => item.id === endpointId) : null;
  if (!endpoint) form.elements.id.value = availableEndpointId(provider, form.elements.api_type.value);
  if (endpoint) {
    // Converting an Endpoint into one whose declaration owns sign-in is refused
    // by the API; the console does not offer it either.
    Array.from(form.elements.api_type.options).forEach(option => {
      option.disabled = option.value !== endpoint.api_type && endpointSignInRequired(option.value);
    });
  }
  $('#endpoint-dialog h2').textContent = endpoint ? `Edit ${endpoint.id}` : 'Add API endpoint';
  $('#endpoint-dialog .dialog-head p').textContent = endpoint && endpointSignInRequired(endpoint.api_type) ? `Update this Endpoint ID or the proxy used for ${endpointTypeLabel(endpoint.api_type)} sign-in, token refresh, and inference.` : endpoint ? 'Update this Provider connection. Existing credentials are managed separately.' : 'Models discovered here remain accessible through the same provider prefix.';
  $('#endpoint-dialog button[type="submit"]').textContent = endpoint ? 'Save changes' : 'Add endpoint';
  const editing = Boolean(endpoint);
  form.elements.id.disabled = false;
  form.elements.id.closest('.endpoint-id-input').classList.remove('immutable-input');
  form.querySelector('.endpoint-id-required').hidden = false;
  form.querySelector('.endpoint-id-permanent').hidden = true;
  form.querySelector('.endpoint-id-lock').hidden = true;
  const endpointIdHelp = form.querySelector('#endpoint-id-help');
  endpointIdHelp.classList.remove('immutable-help');
  endpointIdHelp.innerHTML = editing
    ? 'Renaming updates model availability, endpoint preferences, model-route destinations, Request Defaults scope, and an inactive Traffic Capture scope. Credentials stay attached; historical Activity and captures keep the ID recorded at request time.'
    : 'Identifies this connection inside the Provider. A unique suggestion is filled in automatically and can be changed later.';
  if (endpoint) {
    form.elements.id.value = endpoint.id; form.elements.base_url.value = endpoint.base_url; form.elements.api_type.value = endpoint.api_type;
    form.elements.socks5_proxy.value = endpoint.socks5_proxy || ''; form.elements.requires_credential.checked = endpoint.requires_credential;
    operationPathNotice(form.elements.base_url);
  }
  const policy = endpoint?.rate_limit_cooldown;
  form.elements.cooldown_enabled.checked = (policy?.seconds || 0) > 0;
  selectCooldownSeconds(form.elements.cooldown_duration, policy?.seconds || 60);
  form.elements.cooldown_source.value = !policy?.mode || policy.mode === 'fixed' ? 'fixed' : 'provider';
  form.elements.cooldown_missing.value = policy?.mode === 'provider_only' ? 'skip' : 'fallback';
  // What the chosen policy does depends on how many identities could take over,
  // so the dialog states the scenario for this Endpoint instead of a definition.
  form.dataset.identityCount = String(endpoint ? enabledCredentials(endpoint).filter(key => key.weight > 0).length : 1);
  toggleEndpointMode(); updateCooldownPreview(); bindSecretToggles(endpointDialog); selectEndpointTab('connection'); endpointDialog.showModal();
}
// The figure is a configuration illustration, not live health or a rescued
// request. Its bypass only exists when another positive-weight identity exists.
function updateCooldownPreview() {
  const form = $('#endpoint-form');
  const disabled = !form.elements.cooldown_enabled.checked;
  const fromProvider = form.elements.cooldown_source.value === 'provider';
  const duration = Number(form.elements.cooldown_duration.value);
  // The form asks independent questions; the wire policy stays compatible with
  // existing configurations. Turning it off never destroys the in-dialog choices.
  form.elements.cooldown_seconds.value = disabled ? '0' : String(duration);
  form.elements.cooldown_mode.value = !fromProvider ? 'fixed' : form.elements.cooldown_missing.value === 'skip' ? 'provider_only' : 'prefer_provider';
  const count = Number(form.dataset.identityCount || 0);
  const keyless = !form.elements.requires_credential.checked;
  $('#cooldown-enable-status').textContent = disabled ? 'Off' : 'On';
  $('#cooldown-enable-help').textContent = disabled
    ? 'Cooldown is off. Settings below apply only when enabled.'
    : keyless ? 'This Endpoint sends requests without credentials, so no identity can be paused.' : 'Applies to the identity that receives a rate-limit response.';
  $('#cooldown-duration-label').textContent = fromProvider ? 'Maximum cooldown' : 'Fixed cooldown';
  $('#cooldown-duration-help').textContent = fromProvider
    ? 'Uses Retry-After in whole seconds, capped at this maximum.'
    : 'Uses this duration for every 429, ignoring the Provider’s wait time.';
  $('#cooldown-missing-field').hidden = !fromProvider;
  form.elements.cooldown_missing.options[0].textContent = `Use the maximum · ${formatCooldown(duration)}`;
  const flow = $('#cooldown-flow');
  flow.dataset.state = keyless ? 'direct' : !count ? 'empty' : count === 1 ? 'solo' : disabled ? 'shared' : 'bypass';
  flow.dataset.enabled = String(!disabled && !keyless && count > 0);
  $('#cooldown-flow-identity').textContent = count > 1 ? 'Identity A' : 'Identity';
  $('#cooldown-flow-others').textContent = count === 2 ? 'Identity B' : 'Others';
  $('#cooldown-flow-status').textContent = disabled ? 'In rotation' : count === 1 ? 'Still used' : 'Cooling down';
  $('#cooldown-flow-others-status').textContent = disabled ? 'In rotation' : 'Taking traffic';
  $('#cooldown-flow-heading').textContent = keyless ? 'No identity to pause' : !count ? 'No eligible identity' : disabled ? 'Rotation stays unchanged' : count === 1 ? 'No alternative identity' : 'Let another identity take over';
  $('#cooldown-flow-caption').textContent = keyless
    ? 'This Endpoint sends requests without credentials; cooldown has no identity to affect.'
    : !count ? 'Requests fail until an identity is enabled with a positive traffic share.'
    : disabled ? 'A rate-limited identity keeps its share. Nothing is taken out of rotation.'
    : count === 1 ? 'There is nobody else to take over. Requests still go out with the same identity.'
    : 'When a cooldown starts, later requests use the others. The paused identity rejoins automatically.';
  $('#cooldown-preview').textContent = 'The original 429 is returned unchanged. No request is retried; pinned identities are not bypassed.';
}
function selectCooldownSeconds(select, seconds) {
  const value = String(seconds);
  // A duration the console does not offer is appended for this dialog only, so
  // reopening with another Endpoint must not accumulate stale custom options.
  Array.from(select.options).filter(option => option.dataset.custom === 'true' && option.value !== value).forEach(option => option.remove());
  if (!Array.from(select.options).some(option => option.value === value)) {
    const option = new Option(`${formatCooldown(seconds)} · custom`, value);
    option.dataset.custom = 'true';
    select.add(option);
  }
  select.value = value;
}
function toggleEndpointMode() {
  const form = $('#endpoint-form');
  const declaration = endpointType(form.elements.api_type.value);
  const declaredConnection = endpointDeclaredConnection(declaration);
  const editing = Boolean(form.dataset.endpointId);
  const acceptsSecret = endpointAcceptsSecret(form.elements.api_type.value);
  const required = form.elements.requires_credential.checked && !declaredConnection && acceptsSecret;
  form.elements.base_url.closest('.field').hidden = declaredConnection;
  form.elements.api_type.closest('.field').hidden = declaredConnection && editing;
  form.elements.requires_credential.closest('.checkbox-row').hidden = declaredConnection;
  form.elements.base_url.required = !declaredConnection;
  const selected = form.elements.api_type.value;
  if (declaredConnection) {
    if (form.elements.base_url.dataset.declaredType !== selected) form.elements.base_url.dataset.previousValue = form.elements.base_url.value;
    form.elements.base_url.dataset.declaredType = selected;
    form.elements.base_url.value = declaration.fixed_base_url || form.elements.base_url.value;
  } else if (form.elements.base_url.dataset.declaredType === selected) {
    form.elements.base_url.value = form.elements.base_url.dataset.previousValue || '';
  }
  form.elements.base_url.dataset.previousValue = form.elements.base_url.dataset.previousValue || '';
  $('.endpoint-key-section').classList.toggle('collapsed', !required || editing); form.elements.credential_secret.required = required && !editing;
  $('#endpoint-dialog button[type="submit"]').textContent = editing ? 'Save changes' : declaration?.sign_in ? 'Connect account' : 'Add endpoint';
}
$('#endpoint-form [name="requires_credential"]').addEventListener('change', () => { toggleEndpointMode(); updateCooldownPreview(); });
$$('#endpoint-form [name="cooldown_enabled"], #endpoint-form [name="cooldown_duration"], #endpoint-form [name="cooldown_source"], #endpoint-form [name="cooldown_missing"]').forEach(input => input.addEventListener('change', updateCooldownPreview));
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
$$('#endpoint-form [data-endpoint-tab]').forEach(button => button.addEventListener('click', () => selectEndpointTab(button.dataset.endpointTab)));
// Native validation has to report a field the administrator can see, which means
// the tab holding it has to be the one on screen first.
$('#endpoint-form').addEventListener('invalid', event => revealEndpointField(event.target), true);
$('#endpoint-form').addEventListener('submit', async event => {
  event.preventDefault(); const form = event.target; const data = new FormData(form); const providerId = data.get('provider_id'); const endpointId = form.dataset.endpointId;
  const requiresCredential = data.get('requires_credential') === 'on';
  const rateLimitCooldown = {seconds: Number(data.get('cooldown_seconds') || 0), mode: data.get('cooldown_mode') || 'fixed'};
  const payload = endpointId
    ? {id: data.get('id'), api_type: data.get('api_type'), base_url: data.get('base_url'), socks5_proxy: data.get('socks5_proxy') || null, requires_credential: requiresCredential, rate_limit_cooldown: rateLimitCooldown}
    : {id: data.get('id'), api_type: data.get('api_type'), base_url: data.get('base_url'), socks5_proxy: data.get('socks5_proxy') || null, extra_headers: {}, extra_body: {}, requires_credential: requiresCredential, credential_secret: requiresCredential ? data.get('credential_secret') : null, rate_limit_cooldown: rateLimitCooldown};
  if (!endpointId && endpointSignInRequired(payload.api_type)) return beginEndpointSignIn({endpoint_type: payload.api_type, provider_id: providerId, endpoint_id: payload.id, socks5_proxy: payload.socks5_proxy}, $('#endpoint-error'), endpointDialog);
  const response = await fetch(endpointId ? `/admin/providers/${providerId}/endpoints/${endpointId}` : `/admin/providers/${providerId}/endpoints`, {method: endpointId ? 'PATCH' : 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) {
    const message = await apiErrorMessage(response);
    // A rejected policy is reported where the policy is set; every other rejection
    // belongs to the connection the dialog explains on its first tab.
    selectEndpointTab(/rate-limit|cooldown/i.test(message) ? 'ratelimits' : 'connection');
    $('#endpoint-error').textContent = message;
    return;
  }
  const renamed = Boolean(endpointId && endpointId !== payload.id);
  endpointDialog.close();
  await (renamed ? Promise.all([loadProviders(), loadRoutes()]) : loadProviders());
});

const credentialDialog = $('#credential-dialog');
function openCredentialDialog(providerId, endpointId = null) {
  const provider = providers.find(item => item.id === providerId);
  $('#credential-form').reset();
  $('#credential-error').textContent = '';
  $('#credential-form [name="provider_id"]').value = providerId;
  $('#credential-form [name="secret"]').type = 'password';
  $('#credential-form .toggle-key').textContent = 'Show';
  $('#credential-endpoint').replaceChildren(...provider.endpoints.filter(endpoint => endpoint.requires_credential && endpointAcceptsSecret(endpoint.api_type)).map(endpoint => new Option(`${endpoint.id} · ${formatType(endpoint.api_type)}`, endpoint.id)));
  if (endpointId) $('#credential-endpoint').value = endpointId;
  $('#credential-dialog-title').textContent = endpointId ? `Add credential to ${endpointId}` : 'Add credential';
  $('#credential-dialog-description').textContent = `Add an identity under ${provider.name}${endpointId ? ` / ${endpointId}` : ''}.`;
  credentialDialog.showModal();
}
$$('.close-credential').forEach(button => button.addEventListener('click', () => credentialDialog.close()));
$('#credential-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const providerId = data.get('provider_id');
  const payload = {endpoint_id: data.get('endpoint_id'), name: data.get('name'), secret: data.get('secret'), weight: 100};
  const response = await fetch(`/admin/providers/${providerId}/credentials`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)});
  if (!response.ok) return showApiError(response, $('#credential-error'));
  credentialDialog.close(); await loadProviders();
});

const credentialNameDialog = $('#credential-name-dialog');
function openCredentialNameDialog(providerId, endpointId, credentialId) {
  const provider = providers.find(item => item.id === providerId);
  const endpoint = provider?.endpoints.find(item => item.id === endpointId);
  const credential = endpoint?.credentials.find(item => item.id === credentialId);
  if (!credential) return;
  const form = $('#credential-name-form');
  form.reset();
  form.elements.provider_id.value = providerId;
  form.elements.endpoint_id.value = endpointId;
  form.elements.credential_id.value = credentialId;
  form.elements.name.value = credential.name;
  $('#credential-name-description').textContent = `Rename this ${credentialKindLabel(credential).toLowerCase()} under ${provider.name} / ${endpoint.id}. Model routes keep using the same identity.`;
  $('#credential-name-error').textContent = '';
  credentialNameDialog.showModal();
  form.elements.name.focus();
  form.elements.name.select();
}
$$('.close-credential-name').forEach(button => button.addEventListener('click', () => credentialNameDialog.close()));
$('#credential-name-form').addEventListener('submit', async event => {
  event.preventDefault();
  const form = event.target;
  const response = await fetch(`/admin/providers/${form.elements.provider_id.value}/endpoints/${form.elements.endpoint_id.value}/credentials/${form.elements.credential_id.value}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({name: form.elements.name.value})});
  if (!response.ok) return showApiError(response, $('#credential-name-error'));
  credentialNameDialog.close();
  await Promise.all([loadProviders(), loadRoutes()]);
});

const trafficDialog = $('#traffic-dialog');
let trafficEndpoint = null;
/// The distribution as the dialog is editing it: one row per enabled identity
/// carrying the group position it belongs to and the percentage it takes inside
/// that group, plus how many groups the draft holds, so a group can exist before
/// an identity is moved into it. Rows live in a draft so what is saved describes
/// exactly what was on screen, including a move that only takes effect when the
/// dialog is saved.
let trafficRows = [];
let trafficGroupCount = 1;
/// True while the last group is one the administrator just added and has not
/// filled yet. A group exists while it holds an identity, and the only exception
/// is the position someone created to move an identity into.
let trafficPendingGroup = false;
/// A group is named by the order it carries traffic in — the preferred group
/// first, then its standby groups — while the priority number stays next to that
/// name, because the pool summary and the API both name groups by the number.
function trafficGroupName(priority) {
  if (priority === 1) return 'Preferred group';
  return trafficGroupCount > 2 ? `Standby group ${priority - 1}` : 'Standby group';
}
/// The draft's groups in the order they carry traffic, including one that holds
/// no identity yet: an empty group is a position the administrator is about to
/// move an identity into.
function trafficRowGroups() {
  return Array.from({length: trafficGroupCount}, (_, index) => ({priority: index + 1, standby: index > 0, rows: trafficRows.filter(row => row.priority === index + 1)}));
}
/// A group that only lost its last identity is not left behind as an empty
/// position to clean up, so moving every identity out of a group is how that
/// group is closed. The position added for an identity to be moved into survives
/// until it holds one or is removed.
function collapseTrafficGroups() {
  for (let priority = trafficGroupCount; priority >= 1; priority -= 1) {
    if (trafficRows.some(row => row.priority === priority)) continue;
    if (trafficPendingGroup && priority === trafficGroupCount) continue;
    trafficRows.forEach(row => { if (row.priority > priority) row.priority -= 1; });
    trafficGroupCount -= 1;
  }
}
/// Even percentages again in one group, so a move never leaves a group that no
/// longer adds up to 100.
function rebalanceTrafficGroup(priority) {
  const rows = trafficRows.filter(row => row.priority === priority);
  evenShares(rows.length).forEach((share, index) => { rows[index].weight = share; });
}
function trafficRowElement(row, group, groups) {
  const element = document.createElement('div');
  element.className = 'traffic-row';
  // A group that holds one identity carries that group's whole traffic, so it
  // asks for no percentage: the number would be 100 by definition.
  const shared = group.rows.length > 1;
  // Moving an identity is its own control, so creating a group stays a separate
  // action instead of hiding inside the list of groups that already exist.
  const destinations = groups.filter(item => item.priority !== row.priority);
  const choices = destinations.map(item => `<option value="${item.priority}">${trafficGroupName(item.priority)} · Priority ${item.priority}</option>`).join('');
  element.innerHTML = `<span class="traffic-identity"><span class="status ${row.enabled ? 'enabled' : ''}"></span><strong>${escapeHtml(row.name)}</strong><small class="identity-state" data-tone="${row.state.tone}">${escapeHtml(row.kind)} · ${escapeHtml(row.state.text)}</small></span>${shared ? `<span class="percentage-input"><input type="number" min="1" max="100" required value="${row.weight}" data-credential="${row.id}" aria-label="Share of ${trafficGroupName(group.priority)} taken by ${escapeHtml(row.name)}"><b>%</b></span>` : ''}${destinations.length ? `<select class="traffic-move" data-credential="${row.id}" aria-label="Move ${escapeHtml(row.name)} to another group"><option value="" selected>Move to group…</option>${choices}</select>` : ''}`;
  element.querySelector('input')?.addEventListener('input', input => {
    row.weight = Number(input.target.value) || 0;
    validateTrafficDistribution();
  });
  element.querySelector('select')?.addEventListener('change', select => {
    const target = Number(select.target.value);
    const previous = row.priority;
    if (!target || target === previous) return;
    row.priority = target;
    // Both the group that loses an identity and the group that gains one are
    // shared out again, because each group has to total 100 on its own.
    rebalanceTrafficGroup(previous);
    rebalanceTrafficGroup(target);
    renderTrafficGroups(trafficEndpoint.endpoint);
  });
  return element;
}
function renderTrafficGroups(endpoint) {
  collapseTrafficGroups();
  const groups = trafficRowGroups();
  const cooldownOff = !(endpoint?.rate_limit_cooldown?.seconds > 0);
  const noun = identityNoun(endpoint, 1);
  $('#traffic-rows').replaceChildren(...groups.map(group => {
    const section = document.createElement('section');
    section.className = 'traffic-group';
    section.dataset.priority = String(group.priority);
    section.dataset.tone = group.standby ? 'standby' : 'first';
    // A standby group says when it is used and when it is not, so a group that a
    // disabled cooldown makes unreachable reads as a warning instead of a plan.
    const explanation = groups.length === 1
      ? 'Every request is shared between these identities.'
      : group.priority === groups[0].priority
        ? 'Requests use these identities while any of them can serve.'
        : cooldownOff
          ? 'Never used while rate limits are untracked: no cooldown ever takes an identity above this group out of the rotation.'
          : `Only used while every identity in ${groups.filter(item => item.priority < group.priority).map(item => `Priority ${item.priority}`).join(' and ')} is cooling down.`;
    const head = document.createElement('div');
    head.className = 'traffic-group-head';
    // The order of the groups is the dialog's subject, so the buttons that change
    // it sit on the group itself and the total belongs only to a group whose
    // identities actually share it. An empty position can only be filled or
    // removed, so it has no order to change.
    const stepButtons = group.rows.length ? `<button type="button" class="icon-button traffic-group-step" data-action="up" data-priority="${group.priority}" aria-label="Move ${trafficGroupName(group.priority)} above the group before it"${group.priority === 1 ? ' disabled' : ''}>${icon('chevron-up')}</button><button type="button" class="icon-button traffic-group-step" data-action="down" data-priority="${group.priority}" aria-label="Move ${trafficGroupName(group.priority)} below the group after it"${group.priority === trafficGroupCount ? ' disabled' : ''}>${icon('chevron-down')}</button>` : `<button type="button" class="icon-button traffic-group-remove" data-action="remove" data-priority="${group.priority}" aria-label="Remove ${trafficGroupName(group.priority)}">${icon('close')}</button>`;
    head.innerHTML = `<div class="traffic-group-title"><span class="traffic-group-name"><strong>${trafficGroupName(group.priority)}</strong><span class="traffic-group-priority">Priority ${group.priority}</span></span><small>${explanation}</small></div><div class="traffic-group-tools">${group.rows.length > 1 ? `<span class="traffic-group-total" data-priority="${group.priority}">100%</span>` : ''}${stepButtons}</div>`;
    const rows = document.createElement('div');
    rows.className = 'traffic-rows';
    rows.append(...group.rows.map(row => trafficRowElement(row, group, groups)));
    section.append(head, rows);
    // A percentage that only ever reads 100 is stated as what it means instead of
    // being offered as a field, and a group waiting for its first identity says so
    // rather than looking like a group that carries nothing.
    const note = document.createElement('p');
    if (group.rows.length === 1) {
      note.className = 'traffic-group-note';
      note.textContent = `This is the only ${noun} in this group, so it takes all of this group’s traffic.`;
      section.append(note);
    } else if (!group.rows.length) {
      note.className = 'traffic-group-empty';
      note.textContent = 'No identities in this group yet. Move one here with Move to group, or remove this group.';
      section.append(note);
    }
    return section;
  }));
  validateTrafficDistribution();
}
function openTrafficDialog(providerId, endpointId) {
  const provider = providers.find(item => item.id === providerId);
  const endpoint = provider.endpoints.find(item => item.id === endpointId);
  const groups = identityGroups(endpoint);
  const configured = new Map();
  const position = new Map();
  groups.forEach(group => group.members.forEach(member => position.set(member.id, group.priority)));
  groups.forEach(group => group.configured.forEach((share, id) => configured.set(id, share)));
  trafficEndpoint = {providerId, endpointId, endpoint};
  trafficGroupCount = groups.length;
  trafficPendingGroup = false;
  trafficRows = enabledCredentials(endpoint).map(credential => ({
    id: credential.id,
    name: credential.name,
    kind: credentialKindLabel(credential),
    enabled: credential.enabled,
    state: credential.cooldown_seconds_remaining
      ? {text: `cooling down, resumes in ${formatCooldown(credential.cooldown_seconds_remaining)}`, tone: 'cooling'}
      : {text: 'healthy', tone: 'healthy'},
    // The draft works in group positions while the file stores a number of its
    // own, so what was stored is remembered next to the position: saving then
    // writes the position back exactly where the two differ, and a moved identity
    // is never a no-op because its number happened to read the same.
    priority: position.get(credential.id) ?? 1,
    savedPriority: credential.priority || 1,
    weight: configured.get(credential.id) ?? 100,
  }));
  // A group holding one identity always carries that group's whole traffic, so the
  // draft holds the 100% the dialog states instead of a stored number the shares
  // no longer describe.
  trafficRowGroups().forEach(group => { if (group.rows.length === 1) group.rows[0].weight = 100; });
  const standbyRows = trafficGroupCount > 1;
  const noun = identityNoun(endpoint, 2);
  const singular = identityNoun(endpoint, 1);
  // A tiered Endpoint has one total per group, so the line above the groups says
  // which traffic the numbers describe instead of implying a single split.
  $('#traffic-description').innerHTML = standbyRows
    ? `Set the order these groups carry <code>${escapeHtml(endpointId)}</code> traffic in, and the share each of its enabled ${escapeHtml(noun)} takes inside its own group.`
    : `Set the percentage of <code>${escapeHtml(endpointId)}</code> traffic sent with each enabled ${escapeHtml(noun)}.`;
  // The percentages describe the healthy case, so the dialog names each
  // identity's state and states what a Provider rate limit does to the split.
  const cooldown = endpoint.rate_limit_cooldown || {seconds: 0, mode: 'fixed'};
  // Which group is used first is the dialog's subject, so the order is stated once
  // above the groups instead of being reassembled from each group heading.
  const order = !standbyRows
    ? `Every enabled ${escapeHtml(singular)} is in one group, so requests are shared between them by the percentages below. Add a standby group to keep one back instead of giving it a share.`
    : cooldown.seconds > 0
      ? `<b>Yabane uses the preferred group first.</b> A standby group carries traffic only while every ${escapeHtml(singular)} above it is cooling down, and the preferred group takes the traffic back when those identities return.`
      : `<b>Yabane uses the preferred group first.</b> This Endpoint does not track rate limits, so a standby group never takes over while the preferred group still has an ${escapeHtml(singular)} that can serve.`;
  $('#traffic-order').innerHTML = `${icon('chevron-down', 'traffic-order-mark')}<span>${order}</span>`;
  const standbyClause = !standbyRows
    ? ''
    : cooldown.seconds > 0
      ? ' A group above a standby group hands the traffic back as soon as its cooldown ends, so percentages in each group describe only the time that group carries the traffic.'
      : ' A lower priority group never takes over here, because nothing leaves the rotation while rate limits are untracked.';
  $('#traffic-consequence').innerHTML = (cooldown.seconds > 0
    ? `Percentages apply whenever a request reaches this Endpoint without a pinned identity, including model routes that keep the Endpoint policy. While every identity is healthy, traffic is split exactly as configured; if the Provider rate-limits an identity, it leaves the pool ${cooldownDelayClause(cooldown)}, so the remaining ones carry every request until it returns automatically.${cooldownNoDelayClause(cooldown)}`
    : 'Percentages apply whenever a request reaches this Endpoint without a pinned identity, including model routes that keep the Endpoint policy. This Endpoint does not track rate limits, so an identity the Provider rate-limits keeps receiving its share.') + standbyClause;
  $('#traffic-set-cooldown').hidden = cooldown.seconds > 0;
  $('#traffic-error').textContent = '';
  renderTrafficGroups(endpoint);
  trafficDialog.showModal();
}
function validateTrafficDistribution() {
  const groups = trafficRowGroups();
  const totals = groups.map(group => ({priority: group.priority, total: group.rows.reduce((sum, row) => sum + (Number(row.weight) || 0), 0)}));
  // Percentages are read inside one group, so each group is checked on its own
  // and the message names the group that still has to be adjusted.
  totals.forEach(item => {
    const total = $(`.traffic-group-total[data-priority="${item.priority}"]`);
    if (total) { total.textContent = `${item.total}%`; total.classList.toggle('invalid', item.total !== 100); }
  });
  const weightsValid = groups.every(group => group.rows.every(row => Number.isInteger(row.weight) && row.weight >= 1 && row.weight <= 100));
  // A group the administrator has to fill is stated as that rather than as a total
  // that reads zero, because an empty position is not a distribution at all.
  const empty = groups.find(group => !group.rows.length);
  const unbalanced = totals.find(item => item.total !== 100);
  const valid = weightsValid && !empty && !unbalanced && trafficRows.length > 1;
  $('#traffic-error').textContent = empty
    ? `${trafficGroupName(empty.priority)} has no identities. Move an identity into it, or remove the group.`
    : unbalanced
      ? `${groups.length > 1 ? `Priority ${unbalanced.priority}` : 'Traffic shares'} must add up to 100% (currently ${unbalanced.total}%).`
      : '';
  $('#save-traffic').disabled = !valid;
  return valid;
}
/// A group is a position, so changing its order swaps every identity in the two
/// groups, and removing one closes the gap it leaves so the positions that follow
/// stay contiguous.
function moveTrafficGroup(priority, action) {
  if (action === 'remove') {
    trafficRows.forEach(row => { if (row.priority > priority) row.priority -= 1; });
    trafficGroupCount -= 1;
    trafficPendingGroup = false;
  } else {
    const target = priority + (action === 'up' ? -1 : 1);
    trafficRows.forEach(row => {
      if (row.priority === priority) row.priority = target;
      else if (row.priority === target) row.priority = priority;
    });
  }
  renderTrafficGroups(trafficEndpoint.endpoint);
}
$('#traffic-rows').addEventListener('click', event => {
  const button = event.target.closest('[data-action]');
  if (button) moveTrafficGroup(Number(button.dataset.priority), button.dataset.action);
});
// Creating a group is its own action, so an identity is moved into a group that
// already exists and the list of groups stays a list of groups.
$('#traffic-add-group').addEventListener('click', () => {
  trafficGroupCount += 1;
  trafficPendingGroup = true;
  renderTrafficGroups(trafficEndpoint.endpoint);
  $('#traffic-rows').lastElementChild?.scrollIntoView({block: 'nearest'});
});
$$('.close-traffic').forEach(button => button.addEventListener('click', () => trafficDialog.close()));
// The pool is one mechanism on both screens, so the dialog that sets the split
// hands the administrator to the policy that changes what the split means
// instead of describing that policy only in passing.
$('#traffic-set-cooldown').addEventListener('click', () => {
  const target = trafficEndpoint;
  trafficDialog.close();
  if (target) openEndpointDialog(target.providerId, target.endpointId);
});
$('#traffic-form').addEventListener('submit', async event => {
  event.preventDefault(); if (!validateTrafficDistribution()) return;
  const base = `/admin/providers/${trafficEndpoint.providerId}/endpoints/${trafficEndpoint.endpointId}`;
  // Group membership is a property of the identity, so it is saved before the
  // percentages that are read within those groups.
  for (const row of trafficRows.filter(row => row.priority !== row.savedPriority)) {
    const response = await fetch(`${base}/credentials/${row.id}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({priority: row.priority})});
    if (!response.ok) return showApiError(response, $('#traffic-error'));
  }
  const weights = trafficRows.map(row => ({credential_id: row.id, weight: Number(row.weight)}));
  const response = await fetch(`${base}/traffic`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({weights})});
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
/// The console draws its own option lists. A native select renders the operating
/// system's dropdown, which cannot show Provider → Endpoint → identity as three
/// levels, cannot annotate a row with weight, cooling, or availability, and
/// cannot use this console's wording.
function createPicker(host) {
  if (host.pickerApi) return host.pickerApi;
  host.classList.add('picker');
  const trigger = document.createElement('button');
  trigger.type = 'button';
  trigger.className = 'picker-trigger';
  trigger.setAttribute('aria-haspopup', 'listbox');
  trigger.setAttribute('aria-expanded', 'false');
  trigger.innerHTML = '<span class="picker-value"></span><svg class="ui-icon picker-chevron" aria-hidden="true"><use href="#icon-chevron-right"></use></svg>';
  const popup = document.createElement('div');
  popup.className = 'picker-popup';
  popup.setAttribute('role', 'listbox');
  popup.hidden = true;
  // A cloned destination editor carries the previous picker markup, so the host is
  // reset before this picker builds its own trigger and list.
  host.replaceChildren(trigger, popup);

  const api = {host, trigger, popup, groups: [], value: '', highlighted: -1, disabled: false, onChange: null, placeholder: ''};
  host.pickerApi = api;
  const rows = () => api.groups.flatMap(group => group.options).filter(option => !option.separator);
  const selected = () => rows().find(option => option.value === api.value) || null;
  const selectable = () => [...popup.querySelectorAll('.picker-option:not([disabled])')];

  const renderTrigger = () => {
    const option = selected();
    trigger.querySelector('.picker-value').textContent = option ? (option.trigger ?? option.title) : (api.placeholder || 'Nothing available');
    trigger.classList.toggle('is-empty', !option);
    trigger.classList.toggle('is-warning', Boolean(option?.warning));
    trigger.classList.toggle('is-disabled', Boolean(option?.disabled));
    trigger.disabled = api.disabled || !rows().length;
  };
  const renderPopup = () => {
    if (!rows().length) {
      const empty = document.createElement('p');
      empty.className = 'picker-empty';
      empty.textContent = api.placeholder || 'Nothing available yet.';
      popup.replaceChildren(empty);
      return;
    }
    popup.replaceChildren(...api.groups.map(group => {
      const section = document.createElement('div');
      section.className = 'picker-group';
      if (group.label) {
        const head = document.createElement('div');
        head.className = 'picker-group-label';
        head.textContent = group.label;
        section.append(head);
      }
      group.options.forEach(option => {
        if (option.separator) {
          const rule = document.createElement('div');
          rule.className = 'picker-rule';
          section.append(rule);
          return;
        }
        const row = document.createElement('button');
        row.type = 'button';
        row.className = 'picker-option';
        row.setAttribute('role', 'option');
        row.dataset.value = option.value;
        row.disabled = Boolean(option.disabled);
        row.setAttribute('aria-selected', String(option.value === api.value));
        if (option.warning) row.classList.add('is-warning');
        if (option.disabled) row.classList.add('is-disabled');
        row.innerHTML = `<span class="picker-option-text"><strong>${escapeHtml(option.title)}</strong>${option.meta ? `<small>${escapeHtml(option.meta)}</small>` : ''}</span><svg class="ui-icon picker-check" aria-hidden="true"><use href="#icon-check"></use></svg>`;
        row.addEventListener('click', () => { if (row.disabled) return; api.pick(option.value); });
        section.append(row);
      });
      return section;
    }));
    // Pointer feedback follows the same current-row idea as the keyboard, so a row
    // is never hovered and highlighted at the same time with two different states.
    popup.querySelectorAll('.picker-option').forEach(row => row.addEventListener('mouseenter', () => {
      if (!row.disabled) highlight(selectable().indexOf(row));
    }));
  };
  const highlight = index => {
    const options = selectable();
    if (!options.length) return;
    api.highlighted = Math.max(0, Math.min(index, options.length - 1));
    options.forEach((row, position) => row.classList.toggle('is-highlighted', position === api.highlighted));
    options[api.highlighted].scrollIntoView({block: 'nearest'});
  };
  const open = () => {
    if (api.disabled || !rows().length) return;
    for (const other of document.querySelectorAll('.picker[data-open]')) if (other !== host) other.pickerApi?.close();
    host.dataset.open = 'true';
    popup.hidden = false;
    trigger.setAttribute('aria-expanded', 'true');
    const position = selectable().findIndex(row => row.dataset.value === api.value);
    highlight(position === -1 ? 0 : position);
  };
  const close = (refocus = false) => {
    delete host.dataset.open;
    popup.hidden = true;
    api.highlighted = -1;
    trigger.setAttribute('aria-expanded', 'false');
    if (refocus) trigger.focus();
  };
  api.open = open;
  api.close = close;
  api.setOptions = (groups, wanted) => {
    api.groups = groups;
    const options = groups.flatMap(group => group.options).filter(option => !option.separator);
    const candidate = wanted ?? api.value;
    const match = options.find(option => option.value === candidate && !option.disabled);
    const fallback = options.find(option => !option.disabled);
    api.value = (match || fallback)?.value ?? '';
    renderTrigger();
    renderPopup();
    if (host.dataset.open) open();
  };
  api.setDisabled = disabled => { api.disabled = disabled; if (disabled) close(); renderTrigger(); };
  api.pick = value => {
    const changed = value !== api.value;
    api.value = value;
    renderTrigger();
    renderPopup();
    close(true);
    if (changed) api.onChange?.(value);
  };
  trigger.addEventListener('click', event => { event.stopPropagation(); if (host.dataset.open) close(); else open(); });
  trigger.addEventListener('keydown', event => {
    if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(event.key)) { event.preventDefault(); open(); }
    else if (event.key === 'Escape') close();
  });
  popup.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); close(true); return; }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      selectable()[api.highlighted]?.click();
      return;
    }
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    if (event.key === 'Home') highlight(0);
    else if (event.key === 'End') highlight(selectable().length - 1);
    else highlight(api.highlighted + (event.key === 'ArrowDown' ? 1 : -1));
  });
  document.addEventListener('click', event => { if (!host.contains(event.target)) close(); });
  return api;
}
function destinationPicker(editor, level) { return createPicker(editor.querySelector(`.route-${level}`)); }
function destinationValue(editor, level) { return destinationPicker(editor, level).value; }
function destinationProvider(editor) { return providers.find(item => item.id === destinationValue(editor, 'provider')) || null; }
function destinationEndpoint(editor) {
  const provider = destinationProvider(editor);
  return provider?.endpoints.find(item => item.id === destinationValue(editor, 'endpoint')) || null;
}
function destinationTarget(editor) {
  // A pinned identity is a claim about which identity must send the request; an
  // Endpoint that chooses for itself never carries one here.
  const pinned = identityMode(editor) === 'pin' ? destinationValue(editor, 'identity') : '';
  return {provider_id: destinationValue(editor, 'provider'), endpoint_id: destinationValue(editor, 'endpoint'), credential_id: pinned};
}
let routeIdentityModeSequence = 0;
/// Each destination owns its identity decision, so a cloned destination gets a
/// radio group of its own instead of sharing one group with every other editor.
function ensureIdentityModeGroup(editor) {
  const name = `route_identity_mode_${++routeIdentityModeSequence}`;
  editor.querySelectorAll('.route-identity-policy input').forEach(input => { input.name = name; });
}
function identityMode(editor) { return editor.querySelector('.route-identity-policy input:checked')?.value === 'pin' ? 'pin' : 'endpoint'; }
function setIdentityMode(editor, mode) {
  editor.querySelectorAll('.route-identity-policy input').forEach(input => { input.checked = input.value === mode; });
  renderDestinationIdentityState(editor);
}
/// An Endpoint type is usable only while the Extension providing it is enabled,
/// which is exactly what the published Endpoint type catalog says.
function endpointTypeUsable(apiType) { return endpointTypes.some(type => type.id === apiType); }
function endpointModels(provider, endpointId) { return provider.discovered_models.filter(model => (provider.model_endpoints[model] || []).includes(endpointId)); }
/// The share an identity takes inside its own priority group, because a group is a
/// separate pool: a standby identity's percentage is not a slice of the group that
/// carries traffic first.
function identityShare(endpoint, credential) {
  const group = identityGroups(endpoint).find(item => item.members.some(member => member.id === credential.id));
  return `${group?.configured.get(credential.id) ?? 0}%`;
}
/// Which group an identity belongs to, named only when the Endpoint separates its
/// identities into more than one group; a single group is the whole Endpoint.
function identityTier(endpoint, credential) {
  const groups = identityGroups(endpoint);
  if (groups.length < 2) return '';
  const group = groups.find(item => item.members.some(member => member.id === credential.id));
  return group ? `Priority ${group.priority}${group.standby ? ' standby' : ''} · ` : '';
}
function identityState(endpoint, credential) {
  if (!credential.enabled) return 'disabled';
  const remaining = credential.cooldown_seconds_remaining;
  return remaining ? `cooling down ${formatCooldown(remaining)}` : 'healthy';
}
function identityNoun(endpoint, count) {
  const label = endpoint.credentials[0]?.kind_label;
  if (!label) return count === 1 ? 'identity' : 'identities';
  return count === 1 ? label : `${label}s`;
}
/// Identities that can serve right now: enabled, and not cooling down.
function coolingIdentities(endpoint) { return enabledCredentials(endpoint).filter(credential => credential.cooldown_seconds_remaining); }
function renderDestination(editor, wanted = {}) {
  const providerPicker = destinationPicker(editor, 'provider');
  providerPicker.setOptions([{
    label: 'Provider',
    options: providers.map(provider => ({
      value: provider.id,
      title: provider.name,
      meta: `${provider.id} · ${provider.endpoints.length} ${provider.endpoints.length === 1 ? 'Endpoint' : 'Endpoints'}`,
    })),
  }], wanted.providerId);
  const provider = destinationProvider(editor);
  const endpointPicker = destinationPicker(editor, 'endpoint');
  endpointPicker.setDisabled(!provider);
  endpointPicker.placeholder = provider ? 'This Provider has no Endpoint' : 'Choose a Provider first';
  endpointPicker.setOptions([{
    label: provider ? `${provider.name} · Endpoints` : 'Endpoints',
    options: (provider?.endpoints || []).map(endpoint => {
      const usable = endpointTypeUsable(endpoint.api_type);
      const models = endpointModels(provider, endpoint.id).length;
      const notes = [endpoint.endpoint_type_label || endpointTypeLabel(endpoint.api_type), `${models} model${models === 1 ? '' : 's'}`];
      if (!endpoint.requires_credential) notes.push('no identity');
      if (!usable) notes.push('unavailable: its Extension is not enabled');
      return {
        value: endpoint.id,
        title: endpoint.id,
        meta: notes.join(' · '),
        disabled: !usable && endpoint.id !== wanted.endpointId,
        warning: !usable,
      };
    }),
  }], wanted.endpointId);
  renderDestinationIdentities(editor, wanted.credentialId);
  updateDestinationModels(editor);
  updateDestinationEffect(editor);
}
/// Which identity sends the request is a choice between two different guarantees,
/// so the editor asks for that decision directly instead of presenting every value
/// — including "the Endpoint decides" — as one list of identities.
function renderDestinationIdentities(editor, wanted = null) {
  const picker = destinationPicker(editor, 'identity');
  const endpoint = destinationEndpoint(editor);
  if (!endpoint || !endpointTypeUsable(endpoint.api_type)) {
    picker.setDisabled(true);
    picker.placeholder = endpoint ? 'Unavailable until its Extension is enabled' : 'Choose an Endpoint first';
    picker.setOptions([], null);
    renderDestinationIdentityState(editor);
    return;
  }
  picker.placeholder = 'No identity to pin';
  picker.setDisabled(!endpoint.credentials.length);
  picker.setOptions(endpoint.credentials.length ? [{
    label: 'Identities',
    options: endpoint.credentials.map(credential => ({
      value: credential.id,
      title: credential.name,
      trigger: credential.name,
      meta: credential.enabled
        ? `${identityTier(endpoint, credential)}${identityShare(endpoint, credential)} · ${identityState(endpoint, credential)}`
        : 'disabled · excluded from rotation',
      disabled: !credential.enabled && credential.id !== wanted,
    })),
  }] : [], wanted ?? null);
  renderDestinationIdentityState(editor);
}
/// The identity fields only exist while the Endpoint needs an identity, and the
/// pinned list only while the administrator actually pins one.
function renderDestinationIdentityState(editor) {
  const endpoint = destinationEndpoint(editor);
  const needingIdentity = Boolean(endpoint?.requires_credential) && endpointTypeUsable(endpoint?.api_type);
  const pinning = needingIdentity && identityMode(editor) === 'pin';
  editor.querySelector('.route-identity-policy').hidden = !needingIdentity;
  editor.querySelectorAll('.route-identity-policy input').forEach(input => { input.checked = input.value === (pinning ? 'pin' : 'endpoint'); });
  editor.querySelector('.route-identity-step').hidden = !pinning;
  updateDestinationEffect(editor);
}
function updateDestinationEffect(editor) {
  const effect = editor.querySelector('.route-destination-effect');
  const provider = destinationProvider(editor);
  const endpoint = destinationEndpoint(editor);
  const {credential_id: pinned} = destinationTarget(editor);
  if (!provider || !endpoint) { effect.dataset.tone = 'warn'; effect.textContent = 'Choose a Provider and one of its Endpoints for this destination.'; return; }
  if (!endpointTypeUsable(endpoint.api_type)) {
    effect.dataset.tone = 'warn';
    effect.textContent = `Endpoint type “${endpoint.endpoint_type_label || endpointTypeLabel(endpoint.api_type)}” is unavailable because its Extension is not enabled; choose another Endpoint before saving.`;
    return;
  }
  // The pickers above already name the destination, so this line states only what
  // the choice means for traffic instead of repeating Provider and Endpoint.
  if (!endpoint.requires_credential) {
    effect.dataset.tone = 'info';
    effect.textContent = 'Needs no identity; every matching request goes to this Endpoint as configured.';
    return;
  }
  if (identityMode(editor) === 'pin') {
    const credential = endpoint.credentials.find(item => item.id === pinned);
    if (!credential) { effect.dataset.tone = 'warn'; effect.textContent = 'Choose the identity this route must use, or let the Endpoint choose.'; return; }
    // A stored pin stays the choice even after its identity is disabled, so the line
    // says why the route cannot be served instead of showing another identity.
    if (!credential.enabled) {
      effect.dataset.tone = 'warn';
      effect.textContent = `Pins ${credential.name}, which is disabled: this route cannot be served until that identity is enabled again.`;
      return;
    }
    effect.dataset.tone = 'pin';
    effect.textContent = `Pins ${credential.name}: used exactly as configured, even while it is cooling down; this Endpoint's other identities are never used for this route.`;
    return;
  }
  const enabled = enabledCredentials(endpoint);
  const groups = identityGroups(endpoint);
  const cooling = coolingIdentities(endpoint);
  const eligible = enabled.length - cooling.length;
  effect.dataset.tone = eligible ? 'info' : 'warn';
  // A destination that cannot carry traffic at all is a warning, and the sentence
  // is only written when the claim it makes is true.
  if (!enabled.length) effect.textContent = 'This Endpoint has no enabled identity, so requests fail until one is added.';
  else {
    // A rate limit only removes an identity from the rotation while the Endpoint
    // configures a cooldown; without that policy it keeps receiving its share.
    const rateLimit = (endpoint.rate_limit_cooldown?.seconds || 0) > 0
      ? 'an identity that hits the Provider rate limit drops out of the rotation until its cooldown ends'
      : 'this Endpoint does not track rate limits, so an identity that hits the Provider rate limit keeps receiving its share';
    const rotation = groups.length > 1
      ? `Uses Priority ${groups[0].priority} first and only falls back to Priority ${groups[1].priority} while every identity above it is cooling down`
      : enabled.length === 1
        ? `Uses its only enabled ${identityNoun(endpoint, 1)} for every matching request`
        : `Rotates between its ${enabled.length} enabled ${identityNoun(endpoint, enabled.length)} by weight`;
    // Every identity cooling down still sends the request, so this cannot claim
    // that requests fail while one remains configured.
    const state = !cooling.length ? ''
      : eligible ? ` ${cooling.length} ${cooling.length === 1 ? 'is' : 'are'} cooling down right now, so requests go to the others.`
      : ' Every enabled identity is cooling down right now; requests still go out with one of them and return the Provider’s own answer until a cooldown ends.';
    effect.textContent = `${rotation}; ${rateLimit}.${state}`;
  }
}
function updateDestinationModels(editor) {
  const input = editor.querySelector('.upstream-model-input');
  const provider = destinationProvider(editor);
  const endpoint = destinationEndpoint(editor);
  const usable = provider && endpoint && endpointTypeUsable(endpoint.api_type);
  const models = usable ? endpointModels(provider, endpoint.id) : [];
  let list = editor.querySelector('datalist');
  if (!list) { list = document.createElement('datalist'); editor.append(list); }
  list.id = `upstream-model-suggestions-${crypto.randomUUID()}`;
  list.replaceChildren(...models.map(model => new Option(model)));
  input.setAttribute('list', list.id);
  input.dataset.suggestions = JSON.stringify(models);
  input.placeholder = models[0] ? `e.g. ${models[0]}` : 'e.g. model-name or org/model-name';
  updateDestinationNotice(editor);
}
function updateDestinationNotice(editor) {
  const input = editor.querySelector('.upstream-model-input');
  const notice = editor.querySelector('.upstream-model-notice');
  const models = JSON.parse(input.dataset.suggestions || '[]');
  const typed = input.value.trim();
  if (!models.length) notice.textContent = 'No models reported for this Endpoint yet; a custom ID is still accepted.';
  else if (typed && !models.includes(typed)) notice.textContent = 'Custom model ID — not reported by this Endpoint. It will still be saved.';
  else notice.textContent = '';
}
function routeTargetEditors() { return $$('#route-targets .route-target-editor'); }
/// How the route uses its destinations. The two modes are the two things an
/// operator can want: sharing traffic between interchangeable destinations, or
/// holding a preferred one until the Provider rate-limits every identity behind
/// it and then handing the traffic to the next group.
function routeMode() { return $('#route-mode-choice input:checked')?.value === 'failover' ? 'failover' : 'weighted'; }
function setRouteMode(mode) { $$('#route-mode-choice input').forEach(input => { input.checked = input.value === mode; }); }
/// The group a destination belongs to lives on the editor itself rather than in the
/// select: the select is rebuilt from these values, and while it is still empty a value
/// written into it is dropped, which would silently merge every group into Priority 1.
function routePriorityValue(editor) { return Math.max(1, Number(editor.dataset.priority) || 1); }
function setRoutePriority(editor, value) { editor.dataset.priority = String(Math.max(1, Number(value) || 1)); }
/// Shown under the mode choice so the difference between the two is stated in
/// words instead of being inferred from the editor's layout.
function renderRouteModeEffect() {
  const effect = $('#route-mode-effect');
  const failover = routeMode() === 'failover';
  effect.textContent = failover
    ? 'Destinations are grouped by priority. The lowest-numbered group with a destination that still has an identity to use carries the traffic; a group is left only while every destination in it is cooling down, and it takes the traffic back as soon as a cooldown ends.'
    : 'Every enabled destination receives its configured share. A rate limit does not change the split: the destination keeps its share and returns the Provider’s own answer.';
  // Shares add up per priority group in one mode and across the route in the other,
  // so the heading states which total is being kept.
  $('#route-split-help').textContent = failover
    ? 'Shares are exact percentages inside one priority group and must total 100% there. Setting a share to 0% turns that destination off.'
    : 'Shares are exact percentages and must total 100%. Setting a share to 0% turns that destination off.';
}
/// Priority numbers are the console's own ordering rather than a copy of the
/// stored value: groups read as Priority 1, 2, 3 … in the order they carry
/// traffic, and a group can always be added below the last one.
function refreshPrioritySelects() {
  const editors = routeTargetEditors();
  const highest = editors.reduce((max, editor) => Math.max(max, routePriorityValue(editor)), 1);
  editors.forEach(editor => {
    const select = editor.querySelector('[name="target_priority"]');
    const current = routePriorityValue(editor);
    select.replaceChildren(...Array.from({length: highest + 1}, (_, index) => {
      const priority = index + 1;
      return new Option(priority === 1 ? 'Priority 1 · First' : `Priority ${priority} · Standby`, String(priority));
    }));
    select.value = String(current);
  });
}
/// The destinations of each priority group, keyed by the group's position.
function routePriorityGroups() {
  const groups = new Map();
  routeTargetEditors().forEach(editor => {
    const priority = routePriorityValue(editor);
    if (!groups.has(priority)) groups.set(priority, []);
    groups.get(priority).push(editor);
  });
  return [...groups.entries()].sort((left, right) => left[0] - right[0]);
}
function routeGroupTotals() {
  const totals = new Map();
  routeTargetEditors().forEach(editor => {
    if (!editor.querySelector('[name="target_enabled"]').checked) return;
    const priority = routePriorityValue(editor);
    totals.set(priority, (totals.get(priority) || 0) + Number(editor.querySelector('[name="target_weight"]').value || 0));
  });
  return [...totals.entries()].sort((left, right) => left[0] - right[0]);
}
/// Reads in the order traffic will use the groups: one heading per group, the
/// destinations that share it below, so a standby group never looks like part of
/// the rotation above it.
function renderRouteGroups() {
  const container = $('#route-targets');
  container.querySelectorAll('.route-group-head').forEach(head => head.remove());
  const editors = routeTargetEditors();
  const failover = routeMode() === 'failover';
  editors.forEach(editor => editor.toggleAttribute('data-priority', failover));
  if (!failover) return;
  const ordered = [...editors].sort((left, right) => routePriorityValue(left) - routePriorityValue(right));
  container.append(...ordered);
  let shown = null;
  ordered.forEach(editor => {
    const priority = routePriorityValue(editor);
    editor.dataset.priority = String(priority);
    if (priority === shown) return;
    const first = shown === null;
    shown = priority;
    const head = document.createElement('div');
    head.className = 'route-group-head';
    head.dataset.tone = first ? 'first' : 'standby';
    head.innerHTML = `<strong>Priority ${priority}</strong><small>${first ? 'Carries the traffic while a destination in this group can serve.' : 'Used only while every destination above it is cooling down.'}</small><output class="route-group-total" data-priority="${priority}" aria-live="polite"></output>`;
    container.insertBefore(head, editor);
  });
}
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
function setRouteTargetEnabled(editor, enabled) {
  const toggle = editor.querySelector('[name="target_enabled"]');
  const input = editor.querySelector('[name="target_weight"]');
  toggle.checked = enabled;
  input.disabled = false;
  editor.classList.toggle('disabled-target', !enabled);
  editor.querySelector('.switch-status').textContent = enabled ? 'On' : 'Off';
  if (!enabled) input.value = 0;
}
function validateRouteSplit() {
  const editors = routeTargetEditors();
  const multiple = editors.length > 1;
  const failover = routeMode() === 'failover';
  editors.forEach(editor => {
    const input = editor.querySelector('[name="target_weight"]');
    const enabled = Number(input.value) > 0;
    setRouteTargetEnabled(editor, enabled);
  });
  const summary = $('#route-split-summary');
  let validTotal;
  if (failover) {
    // A standby group is not a slice of the group above it: each group splits
    // its own 100%, exactly as an Endpoint's priority groups do.
    const totals = routeGroupTotals();
    validTotal = totals.length > 0 && totals.every(([, total]) => total === 100);
    const printed = totals.length ? totals.map(([priority, total]) => `Priority ${priority} ${total}%`).join(' · ') : 'no destination is on';
    summary.innerHTML = `Each priority group splits its own 100% <b id="route-split-total">${escapeHtml(printed)}</b>`;
    const totalLabel = $('#route-split-total');
    totalLabel.classList.toggle('invalid', !validTotal);
    totalLabel.setAttribute('aria-label', validTotal ? 'Every priority group totals 100%' : `A priority group does not total 100%: ${printed}`);
  } else {
    const total = editors.reduce((sum, editor) => sum + Number(editor.querySelector('[name="target_weight"]').value || 0), 0);
    validTotal = total === 100;
    summary.innerHTML = `Total traffic <b id="route-split-total">${total}%</b>`;
    const totalLabel = $('#route-split-total');
    totalLabel.classList.toggle('invalid', multiple && !validTotal);
    totalLabel.setAttribute('aria-label', multiple && !validTotal ? `Invalid traffic total: ${total}%` : `Traffic total: ${total}%`);
  }
  $$('#route-targets .route-group-total').forEach(output => {
    const total = routeGroupTotals().find(([priority]) => priority === Number(output.dataset.priority))?.[1] || 0;
    output.textContent = `${total}% of this group`;
    output.classList.toggle('invalid', total !== 100);
  });
  // A pinned destination is incomplete until it names the identity it must use;
  // without one, saving would silently fall back to the Endpoint's own choice.
  const hasDestinations = editors.every(editor => {
    if (!destinationValue(editor, 'provider') || !destinationValue(editor, 'endpoint')) return false;
    const endpoint = destinationEndpoint(editor);
    return identityMode(editor) !== 'pin' || !endpoint?.requires_credential || Boolean(destinationValue(editor, 'identity'));
  });
  $('#save-route').disabled = !hasDestinations || !validTotal;
}
function updateRouteTargetMode(rebalance = false) {
  const editors = routeTargetEditors();
  const multiple = editors.length > 1;
  const failover = routeMode() === 'failover';
  $('#route-targets').classList.toggle('multiple', multiple);
  $('#route-targets').classList.toggle('failover', failover);
  $('#route-split-head').hidden = !multiple && !failover;
  $('#add-route-target-label').textContent = multiple ? 'Add another destination' : 'Add a destination';
  editors.forEach(editor => { editor.querySelector('.route-priority-field').hidden = !failover; });
  refreshPrioritySelects();
  if (!multiple) {
    const input = editors[0].querySelector('[name="target_weight"]');
    input.value = 100;
  } else if (rebalance) {
    // Adding or moving a destination redistributes the split it joined, so a
    // group never has to be repaired by hand afterwards. A weighted route has
    // one split; a failover route has one per priority group.
    const splits = failover ? routePriorityGroups().map(([, members]) => members) : [editors];
    splits.forEach(members => {
      const enabled = members.filter(editor => editor.querySelector('[name="target_enabled"]').checked);
      const shares = enabled.length ? distributeRouteShares(enabled.map(() => 1)) : [];
      members.forEach(editor => editor.querySelector('[name="target_weight"]').value = 0);
      enabled.forEach((editor, index) => { editor.querySelector('[name="target_weight"]').value = shares[index]; });
    });
  }
  editors.forEach(editor => {
    const button = editor.querySelector('.remove-route-target');
    button.disabled = !multiple; button.setAttribute('aria-disabled', String(button.disabled));
  });
  renderRouteGroups();
  renderRouteModeEffect();
  validateRouteSplit();
}
function initializeRouteTarget(editor, target = null) {
  // Each editor owns its identity decision, so the radios never share a group with
  // another destination in the same route.
  ensureIdentityModeGroup(editor);
  editor.querySelector('.route-policy-details').open = false;
  renderDestination(editor, target
    ? {providerId: target.provider_id, endpointId: target.endpoint_id, credentialId: target.credential_id || ''}
    : {});
  // The identity decision is applied after the destination exists, because it is
  // read against the chosen Endpoint: deciding first would let the empty
  // destination reset a stored pin to the Endpoint's own choice, and the route
  // would silently lose the identity it was configured with.
  setIdentityMode(editor, target?.credential_id ? 'pin' : 'endpoint');
  const input = editor.querySelector('.upstream-model-input');
  if (target?.upstream_model) input.value = target.upstream_model;
  setRoutePriority(editor, target?.priority ?? 1);
  const enabled = target ? target.enabled !== false && target.weight > 0 : true;
  const weight = editor.querySelector('[name="target_weight"]');
  weight.value = target ? (enabled ? target.weight : 0) : weight.value;
  editor.querySelector('[name="target_enabled"]').checked = enabled;
  // Changing a level re-renders the levels below it, so Provider → Endpoint →
  // identity can never drift out of sync with what is displayed.
  destinationPicker(editor, 'provider').onChange = value => renderDestination(editor, {providerId: value});
  destinationPicker(editor, 'endpoint').onChange = value => renderDestination(editor, {providerId: destinationValue(editor, 'provider'), endpointId: value});
  destinationPicker(editor, 'identity').onChange = () => updateDestinationEffect(editor);
  // Switching the identity decision reveals or hides the pinned list and rewrites
  // the line that states what this destination promises.
  editor.querySelectorAll('.route-identity-policy input').forEach(input => { input.onchange = () => renderDestinationIdentityState(editor); });
  input.addEventListener('input', () => updateDestinationNotice(editor));
  updateDestinationNotice(editor);
}
function addRouteTargetEditor(target = null) {
  const template = $('#route-targets .route-target-editor');
  const editor = template.cloneNode(true);
  // A clone arrives carrying the identity radios of the destination it was copied
  // from, group name and checked state included. Once it joins the same form, the
  // browser keeps only the newest selection in that shared radio group and clears
  // the selection of the destination it was copied from, which then shows no chosen
  // identity decision at all. The copy starts with nothing selected and takes its
  // own group when it is initialized.
  editor.querySelectorAll('.route-identity-policy input').forEach(input => { input.checked = false; });
  editor.querySelector('[name="upstream_model"]').value = '';
  editor.querySelector('[name="target_weight"]').value = 100;
  editor.querySelector('[name="target_enabled"]').checked = true;
  editor.querySelector('datalist')?.remove();
  $('#route-targets').append(editor); initializeRouteTarget(editor, target);
  return editor;
}
function openRouteDialog(route = null) {
  const form = $('#route-form'); form.reset(); $('#route-error').textContent = ''; editingRoutePattern = route?.pattern || null;
  setRouteMode(route?.mode === 'failover' ? 'failover' : 'weighted');
  routeDialog.querySelector('h2').textContent = route ? 'Edit model route' : 'Add model route';
  routeDialog.querySelector('.dialog-head p').textContent = route ? 'Update the public alias and its destination.' : 'Create a short alias for a Provider model.';
  $('#save-route').textContent = route ? 'Save changes' : 'Save route';
  const editors = $$('#route-targets .route-target-editor'); editors.slice(1).forEach(editor => editor.remove());
  if (route) {
    form.elements.pattern.value = route.pattern;
    initializeRouteTarget(editors[0], route.targets[0]);
    route.targets.slice(1).forEach(addRouteTargetEditor);
  } else initializeRouteTarget(editors[0]);
  updateRouteTargetMode(false);
  const hasDestinations = Boolean(destinationValue($('#route-targets .route-target-editor'), 'endpoint'));
  $('#route-error').textContent = hasDestinations ? '' : 'Configure an eligible Endpoint before creating a route.';
  validateRouteSplit();
  routeDialog.showModal();
}
$('#models-view').addEventListener('click', event => {
  if (event.target.closest('#open-route, #empty-add-route')) openRouteDialog();
});
$('#route-search').addEventListener('input', renderRoutes);
$('#add-route-target').addEventListener('click', () => { addRouteTargetEditor(); updateRouteTargetMode(true); });
$('#route-targets').addEventListener('input', event => {
  if (event.target.matches('[name="target_weight"]')) {
    const wasEnabled = event.target.closest('.route-target-editor').querySelector('[name="target_enabled"]').checked;
    validateRouteSplit();
    if (wasEnabled && event.target.value !== '' && Number(event.target.value) === 0) updateRouteTargetMode(true);
  }
  if (event.target.matches('[name="target_enabled"]')) {
    const editor = event.target.closest('.route-target-editor');
    editor.querySelector('[name="target_weight"]').value = event.target.checked ? 1 : 0;
    setRouteTargetEnabled(editor, event.target.checked);
    updateRouteTargetMode(true);
  }
});
$('#route-targets').addEventListener('click', event => {
  const remove = event.target.closest('.remove-route-target');
  if (!remove || remove.disabled) return;
  remove.closest('.route-target-editor').remove(); updateRouteTargetMode(true);
});
$$('.close-route').forEach(button => button.addEventListener('click', () => routeDialog.close()));
$('#route-mode-choice').addEventListener('change', () => { updateRouteTargetMode(false); });
$('#route-targets').addEventListener('change', event => {
  if (!event.target.matches('[name="target_priority"]')) return;
  // A destination that moves between groups leaves the split it joined and
  // joins another one, so both are shared out again instead of being left
  // adding up to something other than 100%.
  setRoutePriority(event.target.closest('.route-target-editor'), event.target.value);
  updateRouteTargetMode(true);
});
$('#route-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.target); const targets = [...event.target.querySelectorAll('.route-target-editor')].map(editor => { const {provider_id, endpoint_id, credential_id} = destinationTarget(editor); const weight = Number(editor.querySelector('[name="target_weight"]').value); return {provider_id, endpoint_id, credential_id, upstream_model: editor.querySelector('[name="upstream_model"]').value, weight, priority: routePriorityValue(editor), enabled: weight > 0}; });
  const response = await fetch(editingRoutePattern ? `/admin/routes/${encodeURIComponent(editingRoutePattern)}` : '/admin/routes', {method: editingRoutePattern ? 'PATCH' : 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({pattern: data.get('pattern'), mode: routeMode(), targets})});
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

function pinnedCredentialKind(endpoint, credentialId) {
  const credential = endpoint.credentials.find(item => item.id === credentialId);
  return credential?.kind_label || 'Identity';
}
/// One destination is two lines: where the traffic goes (the Endpoint and the
/// identity) and which model Yabane asks for there. Provider and Endpoint are
/// resources and follow the console's resource path — sans-serif with the same `→`
/// Activity uses — so the one shape reserved for model IDs, a monospace
/// `provider/model`, can never be read as the destination.
///
/// A failover destination also states the priority group it sits in and what the
/// route does with it right now, because "the second Provider" only means
/// something together with "while the first one is cooling down".
function routeTargetSummary(route, target, activeWeightTotal) {
  const provider = providers.find(item => item.id === target.provider_id);
  const endpoint = provider?.endpoints.find(item => item.id === target.endpoint_id);
  const enabled = target.enabled !== false && target.weight > 0;
  const failover = route.mode === 'failover';
  // A weighted route splits one total, so a share is read against the traffic
  // that is on. A failover route gives every priority group its own 100%, so the
  // stored weight is already that destination's share of its group.
  const share = enabled ? (failover ? Math.round(Number(target.weight)) : (activeWeightTotal > 0 ? Math.round(Number(target.weight) / activeWeightTotal * 100) : 0)) : 0;
  const endpointRequiresIdentity = Boolean(endpoint && endpoint.requires_credential);
  let identity, mark;
  if (!endpoint) {
    // A route can name a Provider and Endpoint that are no longer configured while it is
    // being edited, so the row says which resource it points at instead of guessing.
    identity = '<span class="route-identity is-policy" title="This destination names an Endpoint that is not configured">Unknown Endpoint</span>';
  } else if (!endpointRequiresIdentity) {
    identity = '<span class="route-identity is-policy" title="This Endpoint sends requests without an identity">No identity</span>';
    mark = 'exact';
  } else if (target.credential_id) {
    const credential = endpoint.credentials.find(item => item.id === target.credential_id);
    identity = `<span class="route-identity" title="Pinned identity — used exactly as configured, even while it is cooling down">${escapeHtml(pinnedCredentialKind(endpoint, target.credential_id))} <strong>${escapeHtml(credential?.name || target.credential_id)}</strong></span>`;
    mark = 'pinned';
  } else {
    const groups = identityGroups(endpoint);
    const rotating = groups[0]?.members.length || 0;
    const standby = groups.length - 1;
    // A policy with one enabled identity has nothing to rotate and nothing to
    // hand over to, so the row names that identity instead of leaving a policy
    // label where the reader expects the identity that carries the traffic.
    const sole = rotating === 1 && !standby ? groups[0].members[0] : null;
    // A destination that keeps the Endpoint policy states the group that carries
    // the traffic and, when the Endpoint separates them, that another group waits
    // instead of presenting every identity as part of one rotation.
    const policyTitle = standby
      ? 'Endpoint policy — uses Priority 1 first, shares it by weight, and hands over to a standby group only while every identity above it is cooling down'
      : sole
        ? 'Endpoint policy — every request leaves with this Endpoint\'s only enabled identity'
        : 'Endpoint policy — rotates the Endpoint\'s eligible identities by weight and skips one that is cooling down';
    // An Endpoint that needs an identity and has none enabled cannot serve at all,
    // so the row states that instead of describing a rotation of nothing.
    identity = groups.length === 0
      ? '<span class="route-identity is-policy" title="This Endpoint requires an identity and has none enabled">No enabled identity</span>'
      : `<span class="route-identity is-policy" title="${policyTitle}">Endpoint policy${sole ? ` · <strong>${escapeHtml(sole.name)}</strong>` : rotating > 1 ? ` · <strong>${rotating} rotating</strong>` : ''}${standby ? ` · <strong>${standby} standby ${standby === 1 ? 'group' : 'groups'}</strong>` : ''}</span>`;
  }
  const state = enabled ? 'Receives traffic' : 'Inactive, 0% share';
  const shareLabel = enabled ? `${share}%` : `${share}% <small>inactive</small>`;
  // The runtime state is what the route would do with this destination now: a
  // standby destination is healthy but waiting, and a cooling one is out until
  // its cooldown ends. Neither is called "healthy", because no share of this
  // gateway knows how much quota the Provider has left.
  const stateLabels = {
    standby: ['standby', 'Eligible, but the group above it carries the traffic'],
    cooling: ['cooling down', 'Every identity this destination could use is rate-limited until a cooldown ends'],
    unusable: ['cannot serve', 'This destination needs its configuration repaired before it can carry traffic'],
  };
  const badge = stateLabels[target.state]
    ? `<span class="route-destination-state" data-state="${target.state}" title="${stateLabels[target.state][1]}">${stateLabels[target.state][0]}</span>`
    : '';
  const group = failover ? `<span class="route-group-mark">Priority ${Number(target.priority) || 1}</span>` : '';
  // Pinning one identity and reaching an Endpoint that selects no identity are different
  // guarantees and must not look alike: only a named identity is marked as pinned to
  // exactly one, and an Endpoint that sends no identity is marked as used exactly as
  // configured. The mark carries the claim, so its tooltip states the same consequence.
  const note = mark === 'pinned'
    ? 'pinned identity — used exactly as configured, even while it is cooling down'
    : 'Endpoint used exactly as configured — no identity is selected on the caller\u2019s behalf';
  const identityLine = mark
    ? identity.replace('</span>', `<svg class="route-identity-mark is-${mark}" role="img" aria-label="${note}"><use href="#icon-lock"></use></svg></span>`)
    : identity;
  return `<article class="route-destination${enabled ? '' : ' is-disabled'}" title="${state}">
    <div class="route-destination-main">${group}<div class="route-destination-where"><span class="route-destination-route" title="Provider and Endpoint that receive this traffic"><span class="route-provider-name">${escapeHtml(target.provider_id)}</span><b class="route-path-arrow">→</b><span class="route-endpoint-name">${escapeHtml(target.endpoint_id)}</span></span>${identityLine}${badge}</div><div class="route-destination-sends" title="Provider model ID — the model Yabane sends to this Endpoint"><span class="route-sends-label">Sends</span><code class="route-upstream-model">${escapeHtml(target.upstream_model)}</code></div></div>
    <span class="route-share" title="${failover ? 'Share of this priority group' : 'Share of this rule\u2019s traffic'}">${shareLabel}</span>
  </article>`;
}

/// An operator looks for a rule by the names they think in: the public pattern,
/// the Provider and Endpoint it points at, the identity, and the model sent on.
function routeSearchTokens() {
  return ($('#route-search').value || '').toLowerCase().split(/\s+/).filter(Boolean);
}
function routeMatchesSearch(route, tokens) {
  if (!tokens.length) return true;
  const parts = [route.pattern, route.pattern.endsWith('*') ? 'prefix' : 'exact'];
  route.targets.forEach(target => {
    const provider = providers.find(item => item.id === target.provider_id);
    const endpoint = provider?.endpoints.find(item => item.id === target.endpoint_id);
    const credential = endpoint?.credentials.find(item => item.id === target.credential_id);
    parts.push(target.provider_id, provider?.name || '', target.endpoint_id, target.upstream_model, target.credential_id || '', credential?.name || '');
  });
  const haystack = parts.join(' ').toLowerCase();
  return tokens.every(token => haystack.includes(token));
}
function renderRoutes() {
  const tokens = routeSearchTokens();
  const visible = modelRoutes.map((route, index) => ({route, index})).filter(entry => routeMatchesSearch(entry.route, tokens));
  $('#routes-empty').hidden = modelRoutes.length > 0;
  $('#routes-table').hidden = modelRoutes.length === 0 || visible.length === 0;
  $('#route-search-tools').hidden = modelRoutes.length === 0;
  // The three-step guide teaches the task once; from the first rule on, the list
  // itself is the page and permanent teaching copy is noise.
  $('.routing-explainer').hidden = modelRoutes.length > 0;
  const noMatch = $('#routes-no-match');
  noMatch.hidden = tokens.length === 0 || visible.length > 0 || modelRoutes.length === 0;
  if (!noMatch.hidden) noMatch.textContent = `No rules match “${$('#route-search').value.trim()}”.`;
  $('#routes').replaceChildren(...visible.map(({route, index}) => {
    const row = document.createElement('tr');
    const failover = route.mode === 'failover';
    const activeWeightTotal = route.targets.filter(target => target.enabled !== false && target.weight > 0).reduce((total, target) => total + Number(target.weight || 0), 0);
    // A failover rule is read in the order its groups carry traffic, so the list
    // shows the same order the proxy will use instead of the order they were
    // added in.
    const ordered = failover ? route.targets.map((target, order) => ({target, order})).sort((left, right) => (Number(left.target.priority) || 1) - (Number(right.target.priority) || 1) || left.order - right.order).map(entry => entry.target) : route.targets;
    const destinations = ordered.map(target => routeTargetSummary(route, target, activeWeightTotal)).join('');
    const activeDestinationCount = route.targets.filter(target => target.enabled !== false && target.weight > 0).length;
    const groupCount = new Set(route.targets.filter(target => target.enabled !== false && target.weight > 0).map(target => Number(target.priority) || 1)).size;
    const matchKind = route.pattern.endsWith('*') ? 'Prefix' : 'Exact';
    // One destination is the normal case, so only a split states its count, and
    // only a failover rule states how many groups can take over.
    const destinationSummary = route.targets.length > 1
      ? (failover
        ? `${route.targets.length} destinations · ${groupCount === 1 ? 'falls back when rate-limited' : `${groupCount} priority groups`}`
        : (route.targets.length === activeDestinationCount ? `${route.targets.length} destinations` : `${route.targets.length} destinations · ${activeDestinationCount} active`))
      : '';
    row.innerHTML = `<td class="route-model-cell"><div class="route-model-heading"><code>${escapeHtml(route.pattern)}</code><span class="route-match-kind">${matchKind}</span></div>${destinationSummary ? `<small>${destinationSummary}</small>` : ''}</td><td><div class="route-destinations">${destinations}</div></td><td><div class="route-row-actions"><button class="edit-route text-link" data-index="${index}">Edit</button><button class="delete-route text-link danger-link" data-pattern="${encodeURIComponent(route.pattern)}">Delete</button></div></td>`;
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
  helpDialog.dataset.provider = providerId || '';
  selectHelpTab('agent');
  updateHelpGuide(providerId);
  helpDialog.showModal();
}
function helpModels() {
  const discovered = providers.flatMap(provider => provider.discovered_models.map(model => `${provider.id}/${model}`));
  return [...new Set([...modelRoutes.map(route => route.pattern).filter(pattern => !pattern.endsWith('*')), ...discovered])].sort();
}
function updateHelpGuide(preferredProviderId = null) {
  const modelSelect = $('#help-model'); const keySelect = $('#help-key');
  const previousModel = modelSelect.value; const previousKey = keySelect.value;
  const models = helpModels();
  const provider = providers.find(item => item.id === (preferredProviderId || helpDialog.dataset.provider));
  const preferredModel = preferredProviderId && provider?.discovered_models.length ? `${provider.id}/${provider.discovered_models[0]}` : null;
  modelSelect.replaceChildren(...models.map(model => new Option(model, model)));
  if (!models.length) modelSelect.append(new Option('No discovered models yet', ''));
  if (preferredModel && models.includes(preferredModel)) modelSelect.value = preferredModel;
  else if (models.includes(previousModel)) modelSelect.value = previousModel;
  keySelect.replaceChildren(...authSettings.api_keys.map(key => new Option(`${key.note || 'Gateway API key'} · ${key.prefix}`, key.secret || '')));
  if (!authSettings.api_keys.length) keySelect.append(new Option('Generate a Gateway API key first', ''));
  const preferredKey = preferredProviderId && authSettings.api_keys.find(key => key.secret && (!key.expires_at || key.expires_at * 1000 > Date.now()) && (!key.provider_ids.length || key.provider_ids.includes(preferredProviderId)));
  if (preferredKey) keySelect.value = preferredKey.secret || '';
  else if ([...keySelect.options].some(option => option.value === previousKey)) keySelect.value = previousKey;
  const baseUrl = `${location.origin}/v1`; const model = modelSelect.value || 'provider/model-id'; const key = keySelect.value || 'sk-your-yabane-key';
  const context = helpDialog.dataset.context || 'general';
  const title = context === 'provider' && provider ? `Connect clients to ${provider.name}` : context === 'access' ? 'Connect clients to Yabane' : 'Connect your Agent to Yabane';
  helpDialog.querySelector('h2').textContent = title;
  $('#help-provider-check').innerHTML = `<b>${providers.length ? icon('check') : '1'}</b><span><strong>Connect a Provider</strong><small>${providers.length ? `${providers.length} configured` : 'Add an Endpoint and Provider credential'}</small></span>`;
  $('#help-key-check').innerHTML = `<b>${authSettings.api_keys.length ? icon('check') : '2'}</b><span><strong>Generate a Gateway key</strong><small>${authSettings.api_keys.length ? `${authSettings.api_keys.length} available` : 'Required while authentication is enabled'}</small></span>`;
  $('#help-model-check').innerHTML = `<b>${models.length ? icon('check') : '3'}</b><span><strong>Select a model</strong><small>${models.length ? `${models.length} available` : 'Refresh Provider model discovery'}</small></span>`;
  $('#help-pi-code').textContent = JSON.stringify({providers: {yabane: {baseUrl, api: 'openai-completions', apiKey: '$YABANE_API_KEY', models: [{id: model, name: model}]} }}, null, 2);
  $('#help-pi-env').textContent = `export YABANE_API_KEY='${key}'`;
  $('#help-pi-run').textContent = `pi --provider yabane --model '${model}'`;
  $('#help-opencode-code').textContent = JSON.stringify({$schema: 'https://opencode.ai/config.json', provider: {yabane: {npm: '@ai-sdk/openai-compatible', name: 'Yabane', options: {baseURL: baseUrl, apiKey: '{env:YABANE_API_KEY}'}, models: {[model]: {name: model}}}}, model: `yabane/${model}`}, null, 2);
  $('#help-opencode-env').textContent = `export YABANE_API_KEY='${key}'`;
  // Claude Code appends `/v1/messages` to the base URL it is given and sends
  // `ANTHROPIC_API_KEY` in the `x-api-key` header, which Yabane does not
  // authenticate. This panel therefore names the bare origin, the variable
  // Claude Code sends as `Authorization: Bearer`, and the selected model, so the
  // request reaches Yabane instead of Claude Code's own model names.
  $('#help-claude-env').textContent = `export ANTHROPIC_BASE_URL='${location.origin}'\nexport ANTHROPIC_AUTH_TOKEN='${key}'\nclaude --model '${model}'`;
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
  $('#models-example-code').textContent = `curl '${location.origin}/v1/models' \\\n  -H 'Authorization: Bearer sk-your-yabane-key'`;
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
  {label: 'Model routing', description: 'Route model IDs to Provider credentials', view: 'models'},
  {label: 'Provider models', description: 'Discover models from providers', view: 'models'},
  {label: 'Model pricing', description: 'Global model rates with Provider and Endpoint overrides', view: 'pricing'},
  {label: 'Extensions', description: 'Compiled request Hooks and capabilities', view: 'extensions'},
  {label: 'API access', description: 'Authentication and gateway keys', view: 'access'},
  {label: 'Generate Gateway API key', description: 'Create an inference credential', view: 'access', action: () => $('#open-gateway-key').click()},
  {label: 'Activity', description: 'Requests, tokens, latency, usage value, and routing logs', view: 'activity'},
  {label: 'Management API', description: 'Programmatic control keys and live docs', view: 'management'},
  {label: 'Create Management API key', description: 'Create a control-plane credential', view: 'management', action: () => $('#open-management-key').click()},
  {label: 'Getting started', description: 'Connect an Agent to Yabane', view: 'home', action: () => openHelp()},
  {label: 'Live API docs', description: 'Interactive OpenAPI documentation', view: 'management', action: () => location.assign('/docs')}
];
function extensionSearchItems() {
  return extensions.map(extension => ({
    label: `${extension.name} extension`,
    description: extension.description,
    keywords: `${extension.id} ${extension.hooks.join(' ')} ${extension.hooks.map(extensionHookLabel).join(' ')}`,
    view: extension.id === 'traffic-capture' && extension.enabled ? 'capture' : 'extensions'
  }));
}
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
  const matches = [...searchItems, ...extensionSearchItems(), ...providers.map(provider => ({label: provider.name, description: `Provider · ${provider.id}`, view: 'providers', action: () => { selectedProviderId = provider.id; renderProviderPage(); }}))].filter(item => `${item.label} ${item.description} ${item.keywords || ''}`.toLowerCase().includes(query));
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

async function apiErrorMessage(response) { const body = await response.json().catch(() => null); return body?.error?.message || `Request failed (${response.status})`; }
async function showApiError(response, target) { const message = await apiErrorMessage(response); if (target) target.textContent = message; else alert(message); }
function compactNumber(value) { return Intl.NumberFormat('en', {notation: 'compact', maximumFractionDigits: 1}).format(value || 0); }
function formatCost(value) {
  if (value == null) return '—';
  const absolute = Math.abs(value);
  const maximumFractionDigits = absolute > 0 && absolute < 0.01 ? Math.min(15, Math.max(6, Math.ceil(-Math.log10(absolute)) + 2)) : 6;
  return `$${new Intl.NumberFormat('en', {minimumFractionDigits: 2, maximumFractionDigits}).format(value)}`;
}
let activityLogs = [];
let activityOverviewLogs = [];
const activityColors = ['#0b57d0', '#7c4dff', '#00a67e', '#ff8f00', '#d93025', '#00897b'];
function activeActivityFilterCount() { return activityFilters.providers.size + activityFilters.models.size + activityFilters.apiKeys.size; }
function activityFilterGroups() {
  return [
    {key: 'providers', label: 'Providers', values: activityFilterOptions.providers.map(value => ({value, label: value, detail: 'Provider'}))},
    {key: 'models', label: 'Requested models', values: activityFilterOptions.models.map(value => ({value, label: value, detail: 'Public model'}))},
    {key: 'apiKeys', label: 'Gateway API keys', values: activityFilterOptions.api_keys.map(item => ({value: item.id, label: item.name, detail: item.prefix || 'No authenticated key'}))},
  ];
}
function updateActivityFilterState() {
  const count = activeActivityFilterCount(); const badge = $('#activity-filter-count');
  badge.textContent = count; badge.hidden = !count; $('#activity-filter-trigger').classList.toggle('active', count > 0);
  const labels = new Map(activityFilterGroups().flatMap(group => group.values.map(item => [`${group.key}:${item.value}`, item.label])));
  const chips = [];
  for (const [key, values] of Object.entries(activityFilters)) for (const value of values) chips.push(`<button type="button" data-remove-filter="${escapeHtml(key)}" data-filter-value="${escapeHtml(value)}" title="Remove filter">${escapeHtml(labels.get(`${key}:${value}`) || value)}${icon('close')}</button>`);
  $('#activity-filter-chips').innerHTML = chips.join(''); $('#activity-filter-chips').hidden = !chips.length;
}
function renderActivityFilterOptions() {
  const search = $('#activity-filter-search').value.trim().toLowerCase();
  $('#activity-filter-options').innerHTML = activityFilterGroups().map(group => {
    const options = group.values.map(item => {
      const hidden = search && !`${item.label} ${item.detail}`.toLowerCase().includes(search);
      return `<label class="activity-filter-option"${hidden ? ' hidden' : ''}><input type="checkbox" data-filter-group="${group.key}" value="${escapeHtml(item.value)}"${activityFilters[group.key].has(item.value) ? ' checked' : ''}><span class="activity-filter-check" aria-hidden="true">${icon('check')}</span><span><strong>${escapeHtml(item.label)}</strong><small>${escapeHtml(item.detail)}</small></span></label>`;
    }).join('');
    const visible = group.values.some(item => !search || `${item.label} ${item.detail}`.toLowerCase().includes(search));
    return `<section${visible ? '' : ' hidden'}><h3>${group.label}<span>${group.values.length}</span></h3><div>${options || '<p>No values in this time range.</p>'}</div></section>`;
  }).join('');
  updateActivityFilterState();
}
function addActivityFilterParams(params) {
  if (activityFilters.providers.size) params.set('providers', [...activityFilters.providers].join(','));
  if (activityFilters.models.size) params.set('models', [...activityFilters.models].join(','));
  if (activityFilters.apiKeys.size) params.set('api_keys', [...activityFilters.apiKeys].join(','));
  return params;
}
function activityBucketPlan(seconds, now = Math.floor(Date.now() / 1000), align = true) {
  const bucketSeconds = seconds <= 900 ? 60 : seconds <= 3600 ? 300 : seconds <= 21600 ? 600 : seconds <= 43200 ? 900 : seconds <= 86400 ? 1800 : seconds <= 259200 ? 3600 : seconds <= 604800 ? 3600 : seconds <= 1209600 ? 7200 : seconds <= 2592000 ? 21600 : 86400;
  const bucketCount = Math.min(336, Math.max(1, Math.ceil(seconds / bucketSeconds)));
  return {bucketCount, bucketSeconds, until: align ? Math.ceil(now / bucketSeconds) * bucketSeconds : now};
}
function selectedActivityRange() {
  if (activityCustomRange) {
    const seconds = activityCustomRange.until - activityCustomRange.since;
    return {...activityCustomRange, seconds, ...activityBucketPlan(seconds, activityCustomRange.until, false), pageUntil: activityCustomRange.until};
  }
  const seconds = Number($('#activity-range').value); const now = Math.floor(Date.now() / 1000); const plan = activityBucketPlan(seconds, now);
  return {seconds, since: plan.until - seconds, until: plan.until, pageUntil: now, bucketCount: plan.bucketCount, bucketSeconds: plan.bucketSeconds};
}
function activityBuckets(logs, seconds, end = activityBucketPlan(seconds).until) {
  const {bucketCount: count} = activityBucketPlan(seconds, end);
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
let activityChartRenderWidth = 0;
let activityChartResizeFrame = 0;
function activityBucketLabel(bucket, seconds, full = false) {
  const start = new Date(bucket.start * 1000); const end = new Date((bucket.start + seconds / activityChartBuckets.length) * 1000);
  if (!full) return seconds <= 86400 ? start.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : start.toLocaleDateString([], {month: 'short', day: 'numeric'});
  const startLabel = seconds <= 86400 ? start.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : start.toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'});
  const endLabel = seconds <= 86400 ? end.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'}) : end.toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'});
  return `${startLabel} – ${endLabel}`;
}
function activityMetricValue(bucket, metric) { return metric === 'tokens' ? bucket.tokens : metric === 'requests' ? bucket.requests : (bucket.samples ? bucket.latency / bucket.samples : null); }
function activityFirstByteValue(bucket) { return bucket.first_byte_samples ? bucket.first_byte / bucket.first_byte_samples : null; }
function activityThroughputValue(bucket) { return bucket.generation && bucket.generation_tokens ? bucket.generation_tokens * 1000 / bucket.generation : null; }
function activityTokenValues(bucket) {
  const input = bucket.input ?? Math.max((bucket.tokens || 0) - (bucket.output || 0), 0); const output = bucket.output || 0;
  return {input, output, cacheRate: input ? Math.min(100, bucket.cached * 100 / input) : 0};
}
function activityMetricLabel(value, metric) { return metric === 'tokens' ? compactNumber(value) : metric === 'requests' ? Math.round(value).toLocaleString() : formatDuration(Math.round(value)); }
function activityThroughputLabel(value) { return value == null ? 'Not available' : `${value >= 100 ? Math.round(value).toLocaleString() : value.toFixed(1)} tok/s`; }
// The right axis names the unit in the legend instead of repeating it, so a long label
// cannot run past the plot on narrow screens.
function activityThroughputAxisLabel(value) { return value >= 100 ? Math.round(value).toLocaleString() : value.toFixed(1); }
function formatTrackedCost(cost, coverage) { return coverage ? formatCost(cost) : '—'; }
function pricedRequestCount(stats) {
  const explicit = Number.isFinite(stats.priced_requests) ? stats.priced_requests : 0;
  const sourced = (stats.reported_requests || 0) + (stats.estimated_requests || 0);
  return Math.max(explicit, sourced);
}
function costCoverageLabel(stats, requests = stats.requests) {
  const sources = [];
  if (stats.reported_requests) sources.push(`${stats.reported_requests.toLocaleString()} reported`);
  if (stats.estimated_requests) sources.push(`${stats.estimated_requests.toLocaleString()} estimated value`);
  return sources.length ? `${sources.join(' · ')} · ${pricedRequestCount(stats).toLocaleString()} / ${requests.toLocaleString()} requests valued` : 'No cost data';
}
function activityTrend(buckets) {
  const bucketSeconds = activityChartSeconds / Math.max(buckets.length, 1); const now = Date.now() / 1000;
  let currentIndex = buckets.findLastIndex(bucket => bucket.start + bucketSeconds <= now);
  if (currentIndex < 0) currentIndex = buckets.length - 1;
  const current = buckets[currentIndex]?.requests || 0; const history = buckets.slice(Math.max(0, currentIndex - 6), currentIndex).map(bucket => bucket.requests);
  const baseline = history.length ? history.reduce((sum, value) => sum + value, 0) / history.length : 0;
  if (!current && !baseline) return {label: 'No recent traffic', short: 'No trend', tone: 'steady'};
  if (!baseline) return {label: `${current.toLocaleString()} in last full interval`, short: 'New traffic', tone: 'up'};
  const change = (current - baseline) * 100 / baseline; const direction = change >= 0 ? 'above' : 'below';
  return {label: `${Math.abs(change).toFixed(0)}% ${direction} recent pace`, short: `${change >= 0 ? '+' : ''}${change.toFixed(0)}% vs recent`, tone: Math.abs(change) < 10 ? 'steady' : change > 0 ? 'up' : 'down'};
}
function inspectActivityBucket(index) {
  const bucket = activityChartBuckets[index]; if (!bucket) return;
  $$('.chart-column').forEach(column => column.classList.toggle('selected', Number(column.dataset.chartIndex) === index));
  $('#chart-inspector-time').textContent = activityBucketLabel(bucket, activityChartSeconds, true);
  const averageLatency = bucket.samples ? Math.round(bucket.latency / bucket.samples) : null;
  const firstByteLatency = bucket.first_byte_samples ? Math.round(bucket.first_byte / bucket.first_byte_samples) : null;
  const throughput = bucket.generation && bucket.generation_tokens ? bucket.generation_tokens * 1000 / bucket.generation : null;
  const successRate = bucket.requests ? (bucket.successful * 100 / bucket.requests).toFixed(1) + '%' : '—';
  const tokenValues = activityTokenValues(bucket);
  const pricedRequests = pricedRequestCount(bucket);
  $('#chart-inspector-values').innerHTML = [
    ['Requests', bucket.requests.toLocaleString()],
    ['Input', compactNumber(tokenValues.input)],
    ['Output', compactNumber(tokenValues.output)],
    ['Cached input', compactNumber(bucket.cached)],
    ['Cache hit', `${tokenValues.cacheRate.toFixed(1)}%`],
    ['Success', successRate],
    ['Avg latency', formatDuration(averageLatency)],
    ['Time to first token', formatDuration(firstByteLatency)],
    ['Throughput', throughput == null ? 'Not available' : activityThroughputLabel(throughput)],
    ['Usage value', formatTrackedCost(bucket.cost, pricedRequests)],
  ].map(([label, value]) => `<div><span>${label}</span><strong>${value}</strong></div>`).join('');
}
function selectActivityChartSeries(column, clientY) {
  if (!Number.isFinite(clientY)) return;
  const chartBox = $('#activity-chart').getBoundingClientRect();
  const pointerY = (clientY - chartBox.top) * 250 / chartBox.height;
  const candidates = activityChartMetric === 'tokens' ? [
    {name: 'input', label: 'Input', y: Number(column.dataset.inputY), value: column.dataset.inputValue, color: '#7c4dff'},
    {name: 'output', label: 'Output', y: Number(column.dataset.outputY), value: column.dataset.outputValue, color: '#0b57d0'},
    {name: 'cache', label: 'Cache hit', y: Number(column.dataset.cacheY), value: column.dataset.cacheValue, color: '#00897b'},
  ] : activityChartMetric === 'performance' ? [
    {name: 'throughput', label: 'Throughput', y: Number(column.dataset.throughputY), value: column.dataset.throughputValue, color: '#7c4dff'},
    {name: 'first-byte', label: 'Time to first token', y: Number(column.dataset.firstByteY), value: column.dataset.firstByteValue, color: '#168c9a'},
    {name: 'latency', label: 'Average latency', y: Number(column.dataset.latencyY), value: column.dataset.latencyValue, color: '#0b57d0'},
  ] : [
    {name: 'requests', label: 'Requests', y: Number(column.dataset.requestsY), value: column.dataset.requestsValue, color: '#0b57d0'},
    {name: 'error-rate', label: 'Error rate', y: Number(column.dataset.errorY), value: column.dataset.errorValue, color: '#b86f67'},
  ];
  const series = candidates.filter(candidate => Number.isFinite(candidate.y)).reduce((nearest, candidate) => !nearest || Math.abs(candidate.y - pointerY) < Math.abs(nearest.y - pointerY) ? candidate : nearest, null);
  if (!series) return;
  column.dataset.activeSeries = series.name;
  delete column.dataset.measured;
  column.style.setProperty('--point-y', `${series.y}px`);
  column.style.setProperty('--point-color', series.color);
  column.querySelector('.chart-value').textContent = `${series.label} ${series.value}`;
}
function smoothActivityPath(points) {
  if (points.length < 2) return points.length ? `M${points[0].x.toFixed(1)},${points[0].y.toFixed(1)}` : '';
  const slopes = points.slice(0, -1).map((point, index) => (points[index + 1].y - point.y) / (points[index + 1].x - point.x));
  const tangents = points.map((_, index) => {
    if (index === 0) return slopes[0];
    if (index === points.length - 1) return slopes.at(-1);
    return slopes[index - 1] * slopes[index] <= 0 ? 0 : (slopes[index - 1] + slopes[index]) / 2;
  });
  slopes.forEach((slope, index) => {
    if (slope === 0) { tangents[index] = 0; tangents[index + 1] = 0; return; }
    const left = tangents[index] / slope; const right = tangents[index + 1] / slope; const magnitude = left * left + right * right;
    if (magnitude <= 9) return;
    const scale = 3 / Math.sqrt(magnitude); tangents[index] = scale * left * slope; tangents[index + 1] = scale * right * slope;
  });
  return points.slice(1).reduce((path, point, index) => {
    const previous = points[index]; const width = point.x - previous.x;
    const firstControl = `${(previous.x + width / 3).toFixed(1)},${(previous.y + tangents[index] * width / 3).toFixed(1)}`;
    const secondControl = `${(point.x - width / 3).toFixed(1)},${(point.y - tangents[index + 1] * width / 3).toFixed(1)}`;
    return `${path} C${firstControl} ${secondControl} ${point.x.toFixed(1)},${point.y.toFixed(1)}`;
  }, `M${points[0].x.toFixed(1)},${points[0].y.toFixed(1)}`);
}
// Measured series such as Provider throughput and first-token latency only exist
// for intervals that streamed a response, so a missing measurement breaks the
// line instead of drawing a zero that never happened.
function smoothActivitySegments(values, points) {
  const segments = []; let segment = [];
  const flush = () => { if (segment.length) segments.push(smoothActivityPath(segment)); segment = []; };
  values.forEach((value, index) => { if (value == null) flush(); else segment.push(points[index]); });
  flush();
  return segments.join(' ');
}
function renderActivityChart(buckets = activityChartBuckets, seconds = activityChartSeconds) {
  activityChartBuckets = buckets; activityChartSeconds = seconds;
  const chart = $('#activity-chart'); const measuredWidth = Math.round(chart.clientWidth);
  if (!measuredWidth) return;
  activityChartRenderWidth = measuredWidth;
  const values = buckets.map(bucket => activityMetricValue(bucket, activityChartMetric)); const tokenMode = activityChartMetric === 'tokens'; const requestMode = activityChartMetric === 'requests'; const performanceMode = activityChartMetric === 'performance';
  const tokenSeries = buckets.map(activityTokenValues); const max = tokenMode ? Math.max(...tokenSeries.flatMap(value => [value.input, value.output]), 1) : Math.max(...values, 1);
  const firstByteValues = performanceMode ? buckets.map(activityFirstByteValue) : [];
  const throughputValues = performanceMode ? buckets.map(activityThroughputValue) : [];
  const leftMax = performanceMode ? Math.max(...values, ...firstByteValues, 1) : max;
  const throughputMax = performanceMode ? Math.max(...throughputValues, 1) : 1;
  const errorRates = requestMode ? buckets.map(bucket => bucket.requests ? Math.min(100, bucket.errors * 100 / bucket.requests) : 0) : [];
  const errorScaleMax = requestMode ? Math.min(100, Math.max(1, Math.ceil(Math.max(...errorRates, 0)))) : 100;
  const measured = values.filter(value => value != null);
  const average = measured.length ? measured.reduce((sum, value) => sum + value, 0) / measured.length : null;
  const width = Math.max(measuredWidth, 320); const height = 250; const left = 48; const right = tokenMode || requestMode || performanceMode ? 42 : 16; const top = 18; const bottom = 38; const plotWidth = width - left - right; const plotHeight = height - top - bottom;
  const pointsFor = (series, scale) => series.map((value, index) => ({x: left + (index + .5) * plotWidth / Math.max(series.length, 1), y: value == null ? null : top + (1 - value / scale) * plotHeight}));
  const primaryValues = tokenMode ? tokenSeries.map(value => value.input) : values; const points = pointsFor(primaryValues, leftMax); const line = performanceMode ? smoothActivitySegments(primaryValues, points) : smoothActivityPath(points);
  const inputPoints = tokenMode ? pointsFor(tokenSeries.map(value => value.input), max) : []; const outputPoints = tokenMode ? pointsFor(tokenSeries.map(value => value.output), max) : []; const cachePoints = tokenMode ? pointsFor(tokenSeries.map(value => value.cacheRate), 100) : [];
  const firstBytePoints = performanceMode ? pointsFor(firstByteValues, leftMax) : []; const throughputPoints = performanceMode ? pointsFor(throughputValues, throughputMax) : [];
  const errorPoints = requestMode ? pointsFor(errorRates, errorScaleMax) : [];
  const grid = [0, .5, 1].map(ratio => { const y = top + ratio * plotHeight; const value = leftMax * (1 - ratio); return `<line x1="${left}" y1="${y}" x2="${width - right}" y2="${y}"></line><text x="${left - 9}" y="${y + 3}" text-anchor="end">${escapeHtml(activityMetricLabel(value, activityChartMetric))}</text>`; }).join('');
  const labelBudget = Math.max(4, Math.min(8, Math.floor(width / 110)));
  const intervalEvery = Math.max(1, Math.ceil(buckets.length / labelBudget));
  const columnLabels = buckets.map(bucket => activityBucketLabel(bucket, seconds));
  const shownLabels = [];
  let lastShownLabel = '';
  columnLabels.forEach((label, index) => {
    const candidate = index % intervalEvery === 0 || index === columnLabels.length - 1;
    const show = candidate && label !== lastShownLabel;
    if (show) lastShownLabel = label;
    shownLabels.push(show ? label : '');
  });
  const overlays = buckets.map((bucket, index) => {
    const detail = `${bucket.requests} requests, ${compactNumber(bucket.tokens)} tokens, ${bucket.errors} errors, ${formatTrackedCost(bucket.cost, pricedRequestCount(bucket))}`; const label = columnLabels[index]; const pointLabel = tokenMode ? `Input ${compactNumber(primaryValues[index])}` : requestMode ? `Requests ${activityMetricLabel(values[index], activityChartMetric)}` : values[index] == null ? 'Not available' : activityMetricLabel(values[index], activityChartMetric);
    const seriesData = tokenMode
      ? ` data-input-y="${inputPoints[index].y}" data-output-y="${outputPoints[index].y}" data-cache-y="${cachePoints[index].y}" data-input-value="${compactNumber(tokenSeries[index].input)}" data-output-value="${compactNumber(tokenSeries[index].output)}" data-cache-value="${tokenSeries[index].cacheRate.toFixed(1)}%" data-active-series="input"`
      : requestMode
        ? ` data-requests-y="${points[index].y}" data-error-y="${errorPoints[index].y}" data-requests-value="${activityMetricLabel(values[index], activityChartMetric)}" data-error-value="${errorRates[index].toFixed(1)}%" data-active-series="requests"`
        : ` data-latency-y="${points[index].y ?? NaN}" data-first-byte-y="${firstBytePoints[index].y ?? NaN}" data-throughput-y="${throughputPoints[index].y ?? NaN}" data-latency-value="${values[index] == null ? 'Not available' : activityMetricLabel(values[index], activityChartMetric)}" data-first-byte-value="${firstByteValues[index] == null ? 'Not available' : activityMetricLabel(firstByteValues[index], activityChartMetric)}" data-throughput-value="${activityThroughputLabel(throughputValues[index])}" data-active-series="latency"`;
    return `<button class="chart-column" type="button" data-measured="${points[index].y == null ? 'false' : 'true'}" style="--point-y:${points[index].y ?? 0}px" data-chart-index="${index}"${seriesData} aria-label="${escapeHtml(`${label}: ${detail}`)}"><span class="chart-value">${pointLabel}</span><small>${shownLabels[index]}</small></button>`;
  }).join('');
  let seriesMarkup;
  if (tokenMode) {
    const inputLine = smoothActivityPath(inputPoints);
    const outputLine = smoothActivityPath(outputPoints);
    const cacheLine = smoothActivityPath(cachePoints);
    const rightAxis = [0, .5, 1].map(ratio => `<text class="traffic-axis-right token-axis" x="${width - right + 9}" y="${top + ratio * plotHeight + 3}">${Math.round((1 - ratio) * 100)}%</text>`).join('');
    seriesMarkup = `<path class="traffic-glow token-input-glow" d="${inputLine}"></path><path class="traffic-line token-input-line" d="${inputLine}"></path><path class="traffic-line token-output-line" d="${outputLine}"></path><path class="traffic-line token-cache-line" d="${cacheLine}"></path>${rightAxis}`;
  } else {
    const averageY = average == null ? null : top + (1 - average / leftMax) * plotHeight;
    const showPointMarkers = plotWidth / Math.max(points.length, 1) >= 12;
    const averageLine = averageY == null ? '' : `<line class="traffic-average" x1="${left}" y1="${averageY}" x2="${width - right}" y2="${averageY}"></line>`;
    const primarySeries = `${averageLine}<path class="traffic-glow" d="${line}"></path><path class="traffic-line" d="${line}"></path><g class="traffic-points">${showPointMarkers ? points.map((point, index) => values[index] ? `<circle cx="${point.x}" cy="${point.y}" r="2.5"></circle>` : '').join('') : ''}</g>`;
    if (requestMode) {
      const errorLine = smoothActivityPath(errorPoints);
      const rightAxis = [0, .5, 1].map(ratio => { const value = errorScaleMax * (1 - ratio); const label = Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1); return `<text class="traffic-axis-right error-axis" x="${width - right + 9}" y="${top + ratio * plotHeight + 3}">${label}%</text>`; }).join('');
      seriesMarkup = `${primarySeries}<path class="error-rate-line" d="${errorLine}"></path>${rightAxis}`;
    } else if (performanceMode) {
      const firstByteLine = smoothActivitySegments(firstByteValues, firstBytePoints); const throughputLine = smoothActivitySegments(throughputValues, throughputPoints);
      const rightAxis = [0, .5, 1].map(ratio => `<text class="traffic-axis-right performance-axis" x="${width - right + 9}" y="${top + ratio * plotHeight + 3}">${escapeHtml(activityThroughputAxisLabel(throughputMax * (1 - ratio)))}</text>`).join('');
      seriesMarkup = `${primarySeries}<path class="first-byte-line" d="${firstByteLine}"></path><path class="throughput-line" d="${throughputLine}"></path>${rightAxis}`;
    } else seriesMarkup = primarySeries;
  }
  $('#activity-chart').innerHTML = `<svg class="traffic-area-chart" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" aria-hidden="true"><g class="traffic-grid">${grid}</g>${seriesMarkup}</svg><div class="chart-bars" style="--activity-buckets:${buckets.length};--chart-right:${right}px;--point-color:${tokenMode ? '#7c4dff' : '#0b57d0'}">${overlays}</div>`;
  const legend = $('#activity-chart-legend');
  legend.hidden = false;
  legend.innerHTML = tokenMode ? '<span><i class="token-input"></i>Input</span><span><i class="token-output"></i>Output</span><span><i class="token-cache"></i>Cache hit rate</span>' : requestMode ? '<span><i class="request-volume"></i>Requests</span><span><i class="request-errors"></i>Error rate</span>' : '<span><i class="performance-throughput"></i>Throughput tok/s</span><span><i class="performance-first-byte"></i>Time to first token</span><span><i class="performance-latency"></i>Average latency</span>';
  const trend = activityTrend(buckets); const signal = $('#activity-trend-signal'); signal.textContent = trend.label; signal.className = `trend-signal ${trend.tone}`; $('#stat-request-trend').textContent = trend.short;
  const bucketSeconds = seconds / Math.max(buckets.length, 1); const interval = bucketSeconds < 3600 ? Math.round(bucketSeconds / 60) + '-minute' : bucketSeconds < 86400 ? Math.round(bucketSeconds / 3600) + '-hour' : Math.round(bucketSeconds / 86400) + '-day';
  $('#traffic-granularity').textContent = `${interval} intervals · ${tokenMode ? 'input, output, and cache hit rate' : requestMode ? 'requests with error rate overlay' : 'generation throughput in tok/s, time to first token, and average latency'}`;
  const nonEmpty = buckets.reduce((last, bucket, index) => bucket.requests ? index : last, -1); inspectActivityBucket(nonEmpty >= 0 ? nonEmpty : buckets.length - 1);
}
function renderProviderStats(target, items, totalRequests) {
  const max = Math.max(...items.map(item => item.requests), 1); target.innerHTML = items.length ? items.map((item, index) => { const cacheRate = item.input_tokens ? item.cached_tokens * 100 / item.input_tokens : 0; return `<div class="activity-ranking"><span class="ranking-number">${index + 1}</span><span class="ranking-dot" style="background:${activityColors[index % activityColors.length]}"></span><div><strong>${escapeHtml(item.name)}</strong><span class="ranking-track"><i style="width:${item.requests * 100 / max}%;background:${activityColors[index % activityColors.length]}"></i></span><small>${(item.requests * 100 / Math.max(totalRequests, 1)).toFixed(1)}% traffic · ${cacheRate.toFixed(1)}% cached input</small></div><span><strong>${item.requests.toLocaleString()}</strong><small>requests</small></span><span><strong>${compactNumber(item.input_tokens + item.output_tokens)}</strong><small>tokens</small></span></div>`; }).join('') : '<div class="activity-empty">No activity in this period.</div>';
}
function renderApiKeyStats(target, items, totalRequests) {
  const max = Math.max(...items.map(item => item.requests), 1);
  target.innerHTML = items.length ? items.map((item, index) => {
    const successRate = item.requests ? (item.requests - item.errors) * 100 / item.requests : 0;
    const identity = item.prefix || 'No authenticated key';
    return `<div class="activity-ranking api-key-ranking"><span class="ranking-key-icon">${icon('key')}</span><div><strong>${escapeHtml(item.name)}</strong><code>${escapeHtml(identity)}</code><span class="ranking-track"><i style="width:${item.requests * 100 / max}%;background:${activityColors[(index + 2) % activityColors.length]}"></i></span><small>${(item.requests * 100 / Math.max(totalRequests, 1)).toFixed(1)}% traffic · ${successRate.toFixed(1)}% success</small></div><span><strong>${item.requests.toLocaleString()}</strong><small>requests</small></span><span><strong>${formatDuration(item.requests ? Math.round(item.latency_ms / item.requests) : null)}</strong><small>avg latency</small></span></div>`;
  }).join('') : '<div class="activity-empty">No API key activity in this period.</div>';
}
function modelSuccessTone(successRate) { return successRate >= 99 ? 'model-healthy' : successRate >= 95 ? 'model-warning' : 'model-critical'; }
function renderModelDimensionCopy() {
  const outgoing = activityModelDimension === 'outgoing';
  $('#model-dimension-column').textContent = outgoing ? 'Outgoing model' : 'Incoming model';
  $('#model-analysis-note').textContent = outgoing
    ? 'Usage efficiency and estimated or reported value grouped by the model ID Yabane sent to the Provider. Requests without a recorded model ID are grouped as Not recorded.'
    : 'Usage efficiency and estimated or reported value grouped by the model name clients requested.';
}
function renderModelStats(target, items, totalRequests) {
  $('#model-count').textContent = `${items.length.toLocaleString()} model${items.length === 1 ? '' : 's'}`;
  target.innerHTML = items.length ? items.map(item => { const cacheRate = item.input_tokens ? item.cached_tokens * 100 / item.input_tokens : 0; const successRate = item.requests ? (item.requests - item.errors) * 100 / item.requests : 0; const averageLatency = item.requests ? item.latency_ms / item.requests : 0; const coverage = pricedRequestCount(item); return `<tr><td data-label="${activityModelDimension === 'outgoing' ? 'Outgoing model' : 'Incoming model'}">${item.name ? `<code>${escapeHtml(item.name)}</code>` : '<span class="activity-model-unavailable">Not recorded</span>'}<small>${(item.requests * 100 / Math.max(totalRequests, 1)).toFixed(1)}% of traffic</small></td><td data-label="Requests"><strong>${item.requests.toLocaleString()}</strong></td><td data-label="Input"><strong>${compactNumber(item.input_tokens)}</strong></td><td data-label="Output"><strong>${compactNumber(item.output_tokens)}</strong></td><td data-label="Input cache hit"><span class="cache-rate"><span><i style="width:${Math.min(cacheRate, 100)}%"></i></span><strong>${cacheRate.toFixed(1)}%</strong></span><small>${compactNumber(item.cached_tokens)} tokens</small></td><td data-label="Success"><strong class="${modelSuccessTone(successRate)}">${successRate.toFixed(1)}%</strong><small>${item.errors.toLocaleString()} errors</small></td><td data-label="Avg latency"><strong>${formatDuration(Math.round(averageLatency))}</strong></td><td data-label="Usage value"><strong>${formatTrackedCost(item.cost, coverage)}</strong><small>${coverage ? `${formatCost(item.cost / coverage)} avg · ` : ''}${costCoverageLabel(item)}</small></td></tr>`; }).join('') : '<tr><td colspan="8"><div class="activity-empty">No activity in this period.</div></td></tr>';
}
function statusBadge(status) { const success = status >= 200 && status < 400; return `<span class="status-badge ${success ? 'success' : 'failure'}"><i></i>${status}</span>`; }
function failureLabel(log) { return log.failure?.message || (log.status >= 400 ? `HTTP ${log.status}` : ''); }
function compactPath(path) { return path.replace('/v1/', '').replace('chat/completions', 'Chat').replace('responses', 'Responses').replace('messages', 'Messages'); }
function activityModelCell(log, detailed) {
  const upstreamModel = log.upstream_model;
  const providerValue = !upstreamModel
    ? '<span class="activity-model-unavailable">Not recorded</span>'
    : upstreamModel === log.model
      ? '<span class="activity-model-unchanged">Same model ID</span>'
      : `<code title="Model sent to the Provider">${escapeHtml(upstreamModel)}</code>`;
  return `<span class="activity-model activity-model-route"><span class="activity-model-leg"><b>Incoming</b><code title="Model name sent by the caller">${escapeHtml(log.model)}</code></span><span class="activity-model-leg activity-upstream-model"><b>Outgoing</b>${providerValue}</span>${detailed ? `<small title="${escapeHtml(log.request_id)}">${escapeHtml(log.request_id)}</small>` : ''}</span>`;
}
function activityRow(log, detailed = false, index = -1) {
  const tokens = log.input_tokens + log.output_tokens; const time = new Date(log.timestamp * 1000);
  const conversion = log.caller_protocol && log.upstream_protocol && log.caller_protocol !== log.upstream_protocol ? ` · ${log.caller_protocol.replace('openai_', '').replace('_completions', '')} → ${log.upstream_protocol.replace('openai_', '').replace('_completions', '')}` : '';
  const modelCell = activityModelCell(log, detailed);
  const costSource = log.cost == null ? '' : log.cost_source === 'estimated' ? ' · estimated' : ' · reported';
  const row = detailed ? `<td><span class="activity-time"><strong>${time.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'})}</strong><small>${time.toLocaleDateString()}</small></span></td><td>${modelCell}</td><td><span class="route-cell"><strong>${escapeHtml(log.provider)} <i>→</i> ${escapeHtml(log.endpoint)}</strong><small>Provider to Endpoint</small></span></td><td><span class="api-kind">${escapeHtml(compactPath(log.path))}${log.streaming ? ' · stream' : ''}${escapeHtml(conversion)}</span></td><td><span class="activity-status-cell">${statusBadge(log.status)}${log.failure ? `<small title="${escapeHtml(failureLabel(log))}">${escapeHtml(failureLabel(log))}</small>` : ''}</span></td><td><strong class="activity-number">${formatDuration(log.latency_ms)}</strong></td><td><span class="activity-number">${compactNumber(log.input_tokens)}</span></td><td><span class="activity-number activity-output">${compactNumber(log.output_tokens)}</span></td><td class="activity-secondary-column"><span class="activity-number">${compactNumber(log.cached_tokens)}</span></td><td class="activity-secondary-column"><span class="activity-number" title="${log.cost == null ? 'No cost available' : log.cost_source === 'estimated' ? 'Yabane estimated usage value' : 'Reported by the Provider'}">${formatCost(log.cost)}${costSource}</span></td>` : `<td><span class="activity-time">${time.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'})}<small>${time.toLocaleDateString()}</small></span></td><td>${modelCell}</td><td><span class="route-cell"><strong>${escapeHtml(log.provider)}</strong><small>${escapeHtml(log.endpoint)} · ${escapeHtml(compactPath(log.path))}${escapeHtml(conversion)}</small></span></td><td>${statusBadge(log.status)}</td><td>${log.latency_ms.toLocaleString()} ms</td><td><strong>${compactNumber(tokens)}</strong><small class="token-detail">${compactNumber(log.input_tokens)} in · ${compactNumber(log.output_tokens)} out</small></td>`;
  return `<tr class="activity-request-row" data-activity-index="${index}" tabindex="0" aria-label="Open details for ${escapeHtml(log.model)} request">${row}</tr>`;
}
function pricingSourceText(source) {
  const scope = {global: 'Global', provider: 'Provider', endpoint: 'Endpoint'}[source.scope] || source.scope;
  return `${scope} ${source.name === 'incoming' ? 'incoming' : 'outgoing'} rule “${source.pattern}”`;
}
function pricingSourceSummary(sources) {
  if (!sources) return '';
  const grouped = new Map();
  for (const [field, label] of [['input', 'Input'], ['output', 'Output'], ['cache_read', 'Cache read']]) {
    const source = sources[field];
    if (!source) continue;
    const key = `${source.scope}|${source.name}|${source.pattern}`;
    if (!grouped.has(key)) grouped.set(key, {source, fields: []});
    grouped.get(key).fields.push(label);
  }
  return [...grouped.values()].map(({source, fields}) => `${fields.length > 2 ? `${fields.slice(0, -1).join(', ')}, and ${fields.at(-1)}` : fields.join(' and ')} from the ${pricingSourceText(source)}`).join('; ');
}
function formatDuration(milliseconds) { return milliseconds == null ? 'Not available' : milliseconds >= 1000 ? `${(milliseconds / 1000).toFixed(milliseconds >= 10000 ? 1 : 2)} s` : `${milliseconds.toLocaleString()} ms`; }
// Cooldowns are configured in seconds and can span a month, so they read in the
// two largest whole units instead of a raw number of seconds.
function formatCooldown(seconds) {
  const parts = [];
  let remaining = Math.max(0, Math.round(seconds));
  for (const [size, unit] of [[86400, 'day'], [3600, 'hour'], [60, 'minute'], [1, 'second']]) {
    const value = Math.floor(remaining / size);
    if (value) {
      parts.push(`${value} ${unit}${value === 1 ? '' : 's'}`);
      remaining %= size;
    }
    if (parts.length === 2) break;
  }
  return parts.length ? parts.join(' ') : '0 seconds';
}
function protocolLabel(protocol) {
  return {
    openai_chat_completions: 'OpenAI Chat Completions',
    openai_responses: 'OpenAI Responses',
    anthropic_messages: 'Anthropic Messages',
  }[protocol] || 'Unknown';
}
/// The credential that carried a request is recorded as a stable ID plus the
/// name it had at the time. The console names it the way the instance that
/// carried the request did: this instance resolves its own records against the
/// Endpoint that holds the ID now, so a rename reads here exactly as it does in
/// the Providers page, while an imported record keeps the name recorded with it
/// so an identity that only exists on the source instance cannot borrow a local
/// name. Only an identity with neither name is shown as its bare stable ID, with
/// the reason stated next to it.
/// Where the model route sent the request, for the one line that names the
/// destination. A `failover` route states the priority group that carried the
/// request and whether a group above it was left behind, because that is what
/// makes a rate-limit answer readable after its cooldown expired; a `weighted`
/// route splits its destinations instead of ordering them, so it has no group to
/// name; a request that was not matched by a route records nothing.
function activityRouteSelection(log) {
  if (!log.route_mode) return '';
  if (log.route_mode !== 'failover') return ' · Weighted share';
  const priority = log.route_priority ? `Priority ${log.route_priority}` : 'Priority not recorded';
  return ` · ${priority}${log.route_failover ? ' · switched after every destination above it cooled down' : ''}`;
}
function activityCarryingCredential(log) {
  const configured = !log.source_instance_id
    && providers.find(provider => provider.id === log.provider)?.endpoints
      .find(endpoint => endpoint.id === log.endpoint)?.credentials
      .find(credential => credential.id === log.upstream_credential_id);
  if (configured) return {name: configured.name, note: ''};
  if (log.upstream_credential_name) return {name: log.upstream_credential_name, note: log.source_instance_id ? '' : 'no longer configured'};
  return {name: log.upstream_credential_id, note: log.source_instance_id ? 'recorded by another instance without a name' : 'no matching credential is configured now'};
}
function openActivityDetail(log) {
  const dialog = $('#activity-detail-dialog'); const firstByte = log.first_byte_ms; const total = log.latency_ms; const gateway = log.gateway_ms; const upstreamHeaders = log.upstream_response_ms; const headersAt = gateway == null || upstreamHeaders == null ? null : gateway + upstreamHeaders; const generation = log.generation_ms ?? (firstByte == null ? null : Math.max(total - firstByte, 0));
  $('#activity-detail-model').textContent = log.model; $('#activity-detail-time').textContent = new Date(log.timestamp * 1000).toLocaleString(); $('#activity-detail-status').innerHTML = statusBadge(log.status);
  $('#activity-detail-route').textContent = `Provider ${log.provider} · Endpoint ${log.endpoint}${activityRouteSelection(log)}`; $('#activity-detail-api').textContent = `Client API ${protocolLabel(log.caller_protocol)}${log.streaming ? ' · Streaming' : ''}`; $('#activity-detail-total').textContent = formatDuration(total);
  const upstreamModel = log.upstream_model || 'Not available (older record)'; const modelUnchanged = log.upstream_model && log.model === log.upstream_model;
  $('#activity-detail-model-route').innerHTML = `<div><span>Incoming model</span><code>${escapeHtml(log.model)}</code><small>Model name received by Yabane</small></div><span class="activity-model-route-arrow" aria-hidden="true">${icon('arrow-right')}</span><div><span>Outgoing model</span><code id="activity-detail-upstream-model">${escapeHtml(upstreamModel)}</code><small>Model ID Yabane sent to the Provider</small></div>`;
  const modelOutcome = $('#activity-detail-model-outcome');
  modelOutcome.textContent = !log.upstream_model ? 'Outgoing model not recorded' : modelUnchanged ? 'Model ID unchanged' : 'Model ID changed';
  modelOutcome.className = `activity-model-outcome ${!log.upstream_model ? 'unavailable' : modelUnchanged ? 'unchanged' : 'changed'}`;
  const clientProtocol = protocolLabel(log.caller_protocol); const providerProtocol = protocolLabel(log.upstream_protocol); const protocolUnchanged = log.caller_protocol && log.caller_protocol === log.upstream_protocol;
  $('#activity-detail-api-route').innerHTML = `<div><span>Client API</span><strong>${escapeHtml(clientProtocol)}</strong><small>Format received by Yabane</small></div><div><span>Provider API</span><strong>${escapeHtml(providerProtocol)}</strong><small class="activity-routing-result ${protocolUnchanged ? 'unchanged' : ''}">${protocolUnchanged ? 'No API conversion' : log.upstream_protocol ? 'Converted by Yabane' : 'Not recorded'}</small></div>`;
  const credential = log.upstream_credential_id ? activityCarryingCredential(log) : null;
  const credentialOutcome = log.credential_cooling ? 'Carried the request while cooling down · no eligible identity was left' : 'Identity that carried the request';
  $('#activity-detail-destination').innerHTML = `<div><span>Provider</span><code>${escapeHtml(log.provider)}</code><small>Configured Provider</small></div><div><span>Endpoint</span><code>${escapeHtml(log.endpoint)}</code><small>Selected connection</small></div>${credential ? `<div><span>Credential</span><code>${escapeHtml(credential.name)}</code><small>${escapeHtml(credential.note ? `${credentialOutcome} · ${credential.note}` : credentialOutcome)}</small></div>` : ''}`;
  const failure = $('#activity-detail-failure'); const failureMessage = log.failure?.message; const failureCategory = log.failure?.category; failure.hidden = !failureMessage; failure.querySelector('p').textContent = failureMessage || ''; failure.querySelector('small').textContent = failureCategory === 'proxy_connect_failed' ? 'Check that the proxy is reachable. If HTTPS works with socks5h but not socks5, let the proxy resolve target hostnames.' : 'Use the request ID below to match this failure with server logs if more detail is needed.';
  const stages = [];
  if (gateway != null) stages.push({label: 'Gateway processing', detail: 'Route and prepare request', start: 0, duration: gateway, color: '#0b57d0', icon: 'route'});
  if (upstreamHeaders != null) stages.push({label: 'Provider response', detail: 'Connect and await headers', start: gateway || 0, duration: upstreamHeaders, color: '#009b84', icon: 'provider'});
  if (firstByte != null && headersAt != null && firstByte > headersAt) stages.push({label: 'First body byte', detail: 'Wait after response headers', start: headersAt, duration: firstByte - headersAt, color: '#168c9a', icon: 'pulse'});
  if (generation != null) stages.push({label: 'Generation', detail: 'Read response body', start: firstByte ?? Math.max(total - generation, 0), duration: generation, color: '#7c4dff', icon: 'arrow-right'});
  const accountedUntil = stages.reduce((end, stage) => Math.max(end, stage.start + stage.duration), 0);
  if (accountedUntil < total) stages.push({label: 'Response completion', detail: 'Remaining response processing', start: accountedUntil, duration: total - accountedUntil, color: '#697386', icon: 'clock'});
  $('#activity-detail-timing').innerHTML = `<div class="timeline-axis"><span>Stage</span><div class="timeline-scale"><span>0</span><span>${escapeHtml(formatDuration(total / 2))}</span><span>${escapeHtml(formatDuration(total))}</span></div><span>Duration</span></div>${stages.map(stage => { const left = Math.min(100, stage.start * 100 / Math.max(total, 1)); const width = Math.max(0, Math.min(100 - left, stage.duration * 100 / Math.max(total, 1))); return `<div class="timeline-stage"><div class="timeline-stage-label"><span class="timeline-stage-icon" style="--stage-color:${stage.color}">${icon(stage.icon)}</span><div><strong>${escapeHtml(stage.label)}</strong><small>${escapeHtml(stage.detail)}</small></div></div><div class="timeline-track" aria-hidden="true"><span class="timeline-stage-bar" style="--stage-left:${left}%;--stage-width:${width}%;--stage-color:${stage.color}"></span></div><strong>${escapeHtml(formatDuration(stage.duration))}</strong></div>`; }).join('')}`;
  const costLabel = log.cost_source === 'estimated' ? 'Estimated usage value' : log.cost == null ? 'Cost' : 'Provider-reported cost';
  const rateSource = log.cost_source === 'estimated' ? pricingSourceSummary(log.pricing_sources) : '';
  const canEditPricing = log.cost == null;
  const canRefreshCost = log.cost_source === 'estimated' || log.cost == null;
  const costActions = [canEditPricing ? '<button type="button" class="text-link edit-activity-pricing">Set price</button>' : '', canRefreshCost ? '<button type="button" class="text-link refresh-activity-cost">Refresh cost</button>' : ''].filter(Boolean).join(' ');
  $('#activity-detail-usage').innerHTML = [['Input tokens', compactNumber(log.input_tokens)], ['Output tokens', compactNumber(log.output_tokens)], ['Cached tokens', compactNumber(log.cached_tokens)], ['Throughput', generation && log.output_tokens ? `${(log.output_tokens * 1000 / generation).toFixed(1)} tok/s` : '—'], [costLabel, formatCost(log.cost), costActions], ['Total tokens', compactNumber(log.input_tokens + log.output_tokens)]].map(([label, value, action = '']) => `<div><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong>${action}</div>`).join('') + (rateSource ? `<div class="activity-pricing-source"><span>Rate source</span><strong>${escapeHtml(rateSource)}</strong><small>Each estimate follows the rules saved at request time; changing prices does not rewrite existing values.</small></div>` : '');
  $('#activity-detail-usage .edit-activity-pricing')?.addEventListener('click', () => {
    dialog.close();
    showView('pricing');
    // The caller's model name is always recorded, and one such price covers every destination of an alias.
    openCentralPricingEditor({scope: 'global', name: 'incoming', model: log.model, providerId: log.provider, endpointId: log.endpoint});
  });
  $('#activity-detail-usage .refresh-activity-cost')?.addEventListener('click', async () => {
    dialog.close();
    await recalculateActivityCosts(log.request_id, log.source_instance_id);
  });
  const gatewayKey = log.gateway_api_key_note ? `${log.gateway_api_key_note} (${log.gateway_api_key_prefix || log.gateway_api_key_id})` : log.gateway_api_key_prefix || log.gateway_api_key_id || 'Unattributed (authentication disabled or older record)';
  $('#activity-detail-request').innerHTML = [['Request ID', log.request_id], ['Gateway API key', gatewayKey], ['API path', log.path], ['Provider finish reason', log.finish_reason || 'Not reported']].map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd><code>${escapeHtml(value)}</code></dd></div>`).join('');
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
  const range = selectedActivityRange(); const until = activityPageUntil || range.pageUntil; const since = activityPageSince || range.since;
  const params = addActivityFilterParams(new URLSearchParams({since, until, offset: activityPage * ACTIVITY_PAGE_SIZE, limit: ACTIVITY_PAGE_SIZE}));
  const query = $('#activity-search').value.trim(); const status = $('#activity-status-filter').value;
  if (query) params.set('query', query); if (status) params.set('status', status);
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
    const range = selectedActivityRange(); const {seconds, since, until, bucketCount, pageUntil} = range; activityPageSince = since; activityPageUntil = pageUntil;
    const statsParams = addActivityFilterParams(new URLSearchParams({since, until, buckets: bucketCount, model_dimension: activityModelDimension}));
    const logsParams = addActivityFilterParams(new URLSearchParams({since, until: pageUntil, limit: logLimit}));
    const [stats, logs] = await Promise.all([fetch(`/admin/activity/stats?${statsParams}`).then(response => response.json()), fetch(`/admin/activity/logs?${logsParams}`).then(response => response.json())]);
    activityOverviewLogs = logs; activityLogsLimit = logLimit;
    const totals = {input: stats.input_tokens, output: stats.output_tokens, cached: stats.cached_tokens, cost: stats.cost, priced: pricedRequestCount(stats), reported_requests: stats.reported_requests || 0, estimated_requests: stats.estimated_requests || 0, latency: stats.latency_ms, success: stats.successful, streaming: stats.streaming};
    const buckets = stats.buckets; const requests = stats.requests; const totalTokens = totals.input + totals.output;
    $('#stat-requests').textContent = compactNumber(requests); $('#stat-streaming').textContent = compactNumber(totals.streaming); $('#stat-input').textContent = compactNumber(totals.input); $('#stat-output').textContent = compactNumber(totals.output); $('#stat-cached').textContent = compactNumber(totals.cached); $('#stat-total-tokens').textContent = compactNumber(totalTokens);
    $('#stat-success-rate').textContent = `${requests ? (totals.success * 100 / requests).toFixed(1) : '0.0'}%`; $('#stat-errors').textContent = `${(requests - totals.success).toLocaleString()} error${requests - totals.success === 1 ? '' : 's'}`; $('#stat-cache-rate').textContent = `${totals.input ? (totals.cached * 100 / totals.input).toFixed(1) : '0.0'}%`; $('#stat-cost').textContent = formatTrackedCost(totals.cost, totals.priced); $('#stat-cost-coverage').textContent = costCoverageLabel(totals, requests); $('#stat-latency').textContent = formatDuration(requests ? Math.round(totals.latency / requests) : 0);
    $('#requests-spark').innerHTML = sparkline(buckets.map(bucket => bucket.requests), '#0b57d0'); $('#tokens-spark').innerHTML = sparkline(buckets.map(bucket => bucket.tokens), '#7c4dff'); $('#success-spark').innerHTML = sparkline(buckets.map(bucket => bucket.requests ? bucket.successful / bucket.requests : 0), '#00a67e'); $('#latency-spark').innerHTML = sparkline(buckets.map(bucket => bucket.samples ? bucket.latency / bucket.samples : 0), '#168c9a'); $('#cache-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cached), '#00897b'); $('#cost-spark').innerHTML = sparkline(buckets.map(bucket => bucket.cost), '#ff8f00');
    renderActivityChart(buckets, seconds); renderProviderStats($('#provider-stats'), stats.by_provider, requests); renderApiKeyStats($('#api-key-stats'), stats.by_api_key || [], requests); renderModelDimensionCopy(); renderModelStats($('#model-stats'), stats.by_model, requests);
    $('#recent-activity-logs').innerHTML = activityOverviewLogs.length ? activityOverviewLogs.slice(0, 8).map((log, index) => activityRow(log, false, index)).join('') : '<tr><td colspan="6"><div class="activity-empty">No requests in this period.</div></td></tr>';
    activityFilterOptions = stats.filter_options || {providers: [], models: [], api_keys: []};
    renderActivityFilterOptions();
    if (explorerVisible) loadActivityPage();
  })().finally(() => { activityLoadPromise = null; });
  return activityLoadPromise;
}
function showActivityTab(tab) {
  $$('.activity-tabs button').forEach(button => { const active = button.dataset.activityTab === tab; button.classList.toggle('active', active); button.setAttribute('aria-selected', String(active)); });
  $('#activity-overview-panel').hidden = tab !== 'overview'; $('#activity-requests-panel').hidden = tab !== 'requests';
  if (tab === 'overview') requestAnimationFrame(() => renderActivityChart());
  if (tab === 'requests') { activityPage = 0; $('#activity-page-previous').disabled = true; loadActivityPage(); }
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
$('#chart-metric-picker').addEventListener('click', event => { const button = event.target.closest('[data-chart-metric]'); if (!button) return; activityChartMetric = button.dataset.chartMetric; $$('#chart-metric-picker button').forEach(item => { const active = item === button; item.classList.toggle('active', active); item.setAttribute('aria-pressed', String(active)); }); renderActivityChart(); });
$('#activity-chart').addEventListener('pointerover', event => { const column = event.target.closest('.chart-column'); if (column) { inspectActivityBucket(Number(column.dataset.chartIndex)); selectActivityChartSeries(column, event.clientY); } });
$('#activity-chart').addEventListener('pointermove', event => { const column = event.target.closest('.chart-column'); if (column) selectActivityChartSeries(column, event.clientY); });
$('#activity-chart').addEventListener('focusin', event => { const column = event.target.closest('.chart-column'); if (column) inspectActivityBucket(Number(column.dataset.chartIndex)); });
$('#activity-chart').addEventListener('click', event => { const column = event.target.closest('.chart-column'); if (column) { inspectActivityBucket(Number(column.dataset.chartIndex)); if (event.detail) selectActivityChartSeries(column, event.clientY); } });
new ResizeObserver(entries => {
  const width = Math.round(entries[0].contentRect.width);
  if (!width || !activityChartBuckets.length || Math.abs(width - activityChartRenderWidth) < 2) return;
  cancelAnimationFrame(activityChartResizeFrame); activityChartResizeFrame = requestAnimationFrame(() => renderActivityChart());
}).observe($('#activity-chart'));
function activityLogForRow(row) { return (row.closest('#recent-activity-logs') ? activityOverviewLogs : activityLogs)[Number(row.dataset.activityIndex)]; }
$('#activity-view').addEventListener('click', event => { const row = event.target.closest('.activity-request-row'); if (row) openActivityDetail(activityLogForRow(row)); });
$('#activity-view').addEventListener('keydown', event => { const row = event.target.closest('.activity-request-row'); if (row && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); openActivityDetail(activityLogForRow(row)); } });
function closeActivityRangePicker() { const popover = $('#activity-range-popover'); popover.hidden = true; $('#activity-range-trigger').setAttribute('aria-expanded', 'false'); $('#activity-range-search').value = ''; $$('#activity-range-options button').forEach(button => { button.hidden = false; }); }
function formatCustomRangeLabel(since, until) {
  const format = value => new Date(value * 1000).toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'});
  return `${format(since)} – ${format(until)}`;
}
function renderActivityRangeOptions() {
  const selected = activityCustomRange ? '' : $('#activity-range').value;
  $('#activity-range-options').replaceChildren(...[...$('#activity-range').options].map(option => {
    const button = document.createElement('button'); button.type = 'button'; button.dataset.seconds = option.value; button.setAttribute('role', 'option'); button.setAttribute('aria-selected', String(option.value === selected));
    button.innerHTML = `<span>${escapeHtml(option.textContent)}</span>${option.value === selected ? icon('check') : ''}`;
    button.addEventListener('click', () => { $('#activity-range').value = option.value; $('#activity-range').dispatchEvent(new Event('change')); });
    return button;
  }));
}
function openActivityRangePicker() {
  const popover = $('#activity-range-popover'); const range = selectedActivityRange();
  renderActivityRangeOptions(); $('#activity-range-from').value = localDateTimeValue(range.since); $('#activity-range-to').value = localDateTimeValue(range.pageUntil); $('#activity-range-error').textContent = '';
  popover.hidden = false; $('#activity-range-trigger').setAttribute('aria-expanded', 'true'); $('#activity-range-search').focus();
}
$('#activity-range-trigger').addEventListener('click', () => { if ($('#activity-range-popover').hidden) openActivityRangePicker(); else closeActivityRangePicker(); });
$('#activity-range-trigger').addEventListener('keydown', event => { if (event.key === 'ArrowDown') { event.preventDefault(); openActivityRangePicker(); $('#activity-range-options button:not([hidden])')?.focus(); } });
$('#activity-range-search').addEventListener('input', event => { const query = event.target.value.trim().toLowerCase(); $$('#activity-range-options button').forEach(button => { button.hidden = !button.textContent.toLowerCase().includes(query); }); });
$('#activity-range-search').addEventListener('keydown', event => { if (event.key === 'ArrowDown') { event.preventDefault(); $('#activity-range-options button:not([hidden])')?.focus(); } });
$('#activity-range-options').addEventListener('keydown', event => {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return; event.preventDefault(); const buttons = $$('#activity-range-options button:not([hidden])'); const index = buttons.indexOf(event.target); const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length; buttons[next]?.focus();
});
$('#activity-range').addEventListener('change', () => { activityCustomRange = null; const option = $('#activity-range').selectedOptions[0]; $('#activity-range-label').textContent = option?.textContent || 'Select range'; closeActivityRangePicker(); renderActivityRangeOptions(); resetAndLoadActivity(); });
$('#apply-activity-range').addEventListener('click', () => {
  const since = Math.floor(new Date($('#activity-range-from').value).getTime() / 1000); const until = Math.floor(new Date($('#activity-range-to').value).getTime() / 1000); const error = $('#activity-range-error');
  if (!Number.isFinite(since) || !Number.isFinite(until)) { error.textContent = 'Choose both a start and end time.'; return; }
  if (until <= since) { error.textContent = 'End time must be later than start time.'; return; }
  if (until - since < 60) { error.textContent = 'Choose a range of at least one minute.'; return; }
  activityCustomRange = {since, until}; $('#activity-range-label').textContent = formatCustomRangeLabel(since, until); closeActivityRangePicker(); renderActivityRangeOptions(); resetAndLoadActivity();
});
document.addEventListener('keydown', event => { if (event.key === 'Escape' && !$('#activity-range-popover').hidden) { closeActivityRangePicker(); $('#activity-range-trigger').focus(); } });
document.addEventListener('click', event => { if (!event.target.closest('.activity-range-picker')) closeActivityRangePicker(); });
function closeActivityFilterPicker() { const popover = $('#activity-filter-popover'); popover.hidden = true; $('#activity-filter-trigger').setAttribute('aria-expanded', 'false'); $('#activity-filter-search').value = ''; }
function openActivityFilterPicker() { renderActivityFilterOptions(); $('#activity-filter-popover').hidden = false; $('#activity-filter-trigger').setAttribute('aria-expanded', 'true'); $('#activity-filter-search').focus(); }
$('#activity-filter-trigger').addEventListener('click', () => { if ($('#activity-filter-popover').hidden) openActivityFilterPicker(); else closeActivityFilterPicker(); });
$('#activity-filter-search').addEventListener('input', renderActivityFilterOptions);
$('#activity-filter-options').addEventListener('change', event => {
  const input = event.target.closest('[data-filter-group]'); if (!input) return;
  const values = activityFilters[input.dataset.filterGroup];
  if (input.checked) values.add(input.value); else values.delete(input.value);
  updateActivityFilterState(); resetAndLoadActivity();
});
$('#reset-activity-filters').addEventListener('click', () => { Object.values(activityFilters).forEach(values => values.clear()); renderActivityFilterOptions(); resetAndLoadActivity(); });
$('#activity-filter-chips').addEventListener('click', event => { const chip = event.target.closest('[data-remove-filter]'); if (!chip) return; activityFilters[chip.dataset.removeFilter].delete(chip.dataset.filterValue); renderActivityFilterOptions(); resetAndLoadActivity(); });
document.addEventListener('keydown', event => { if (event.key === 'Escape' && !$('#activity-filter-popover').hidden) { closeActivityFilterPicker(); $('#activity-filter-trigger').focus(); } });
document.addEventListener('click', event => { if (!event.target.closest('.activity-filter-picker')) closeActivityFilterPicker(); });
async function recalculateActivityCosts(requestId = null, sourceInstanceId = null) {
  const button = $('#refresh-missing-costs');
  const status = $('#activity-cost-refresh-status');
  const message = requestId
    ? 'Recalculate this Activity cost using current explicit pricing? Official Provider costs will not change.'
    : 'Recalculate every retained non-reported Activity cost using current explicit pricing? Official Provider costs will not change.';
  if (!confirm(message)) return;
  if (button) button.disabled = true;
  if (status) status.textContent = 'Refreshing costs…';
  try {
    const response = await fetch('/admin/activity/recalculate-costs', {
      method: 'POST',
      headers: {'content-type': 'application/json'},
      body: JSON.stringify(requestId ? {request_id: requestId, source_instance_id: sourceInstanceId} : {}),
    });
    if (!response.ok) {
      if (status) status.textContent = 'Could not refresh Activity costs.';
      return;
    }
    const result = await response.json();
    if (status) status.textContent = result.updated
      ? `Updated ${result.updated} cost${result.updated === 1 ? '' : 's'} (${result.recalculated} recalculated, ${result.filled} filled).`
      : 'No non-reported costs could be recalculated.';
    await resetAndLoadActivity();
  } catch {
    if (status) status.textContent = 'Could not refresh Activity costs.';
  } finally {
    if (button) button.disabled = false;
  }
}

async function resetAndLoadActivity() { if (activityLoadPromise) await activityLoadPromise; activityLogsLimit = 0; activityPage = 0; activityPageSince = 0; activityPageUntil = 0; return loadActivity(); }
async function reloadActivityOverview() { if (activityLoadPromise) await activityLoadPromise; activityLogsLimit = 0; return loadActivity(); }
$('#model-dimension-picker').addEventListener('click', event => {
  const button = event.target.closest('[data-model-dimension]');
  if (!button || button.dataset.modelDimension === activityModelDimension) return;
  activityModelDimension = button.dataset.modelDimension;
  $$('#model-dimension-picker button').forEach(item => { const active = item === button; item.classList.toggle('active', active); item.setAttribute('aria-pressed', String(active)); });
  renderModelDimensionCopy();
  reloadActivityOverview();
});
$('#refresh-activity').addEventListener('click', resetAndLoadActivity);
$('#refresh-missing-costs').addEventListener('click', () => recalculateActivityCosts());
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
function aggregateHomeTraffic(buckets, groupSize) {
  if (groupSize === 1) return buckets;
  const aggregated = [];
  for (let index = 0; index < buckets.length; index += groupSize) {
    const group = buckets.slice(index, index + groupSize);
    aggregated.push({...group[0], requests: group.reduce((total, bucket) => total + bucket.requests, 0)});
  }
  return aggregated;
}
function homeTrafficPlan(width, bucketCount) {
  const visibleBuckets = width >= 768 ? 48 : width >= 400 ? 24 : 12;
  const groupSize = Math.max(1, Math.ceil(bucketCount / visibleBuckets));
  return {groupSize, intervalMinutes: groupSize * 30};
}
function renderHomeTraffic(buckets) {
  homeTrafficBuckets = buckets;
  const chart = $('#home-traffic-chart');
  const {groupSize, intervalMinutes} = homeTrafficPlan(chart.clientWidth, buckets.length);
  const visibleBuckets = aggregateHomeTraffic(buckets, groupSize);
  homeTrafficBucketSize = groupSize;
  const max = Math.max(...visibleBuckets.map(bucket => bucket.requests), 1); const requests = visibleBuckets.reduce((total, bucket) => total + bucket.requests, 0);
  const intervalLabel = intervalMinutes < 60 ? `${intervalMinutes}-minute` : intervalMinutes === 60 ? 'Hourly' : `${intervalMinutes / 60}-hour`;
  $('#home-traffic-description').textContent = `${intervalLabel} traffic intervals reveal changes in demand`;
  chart.innerHTML = `<div class="home-chart-grid"><i></i><i></i><i></i></div><div class="home-chart-bars">${visibleBuckets.map((bucket, index) => { const start = new Date(bucket.start * 1000); const end = new Date((bucket.start + intervalMinutes * 60) * 1000); const label = index % Math.max(1, Math.ceil(visibleBuckets.length / 6)) === 0 || index === visibleBuckets.length - 1 ? start.toLocaleTimeString([], {hour: '2-digit'}) : ''; const range = `${start.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})}–${end.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})}`; return `<div class="home-chart-column" title="${range}: ${bucket.requests} request${bucket.requests === 1 ? '' : 's'}"><span style="height:${bucket.requests * 100 / max}%;animation-delay:${index * 16}ms"></span><small>${label}</small></div>`; }).join('')}</div>${requests ? '' : '<p class="home-chart-empty">No requests in the last 24 hours</p>'}`;
}
new ResizeObserver(entries => {
  const width = Math.round(entries[0].contentRect.width);
  if (!width || !homeTrafficBuckets.length || homeTrafficPlan(width, homeTrafficBuckets.length).groupSize === homeTrafficBucketSize) return;
  cancelAnimationFrame(homeTrafficResizeFrame); homeTrafficResizeFrame = requestAnimationFrame(() => renderHomeTraffic(homeTrafficBuckets));
}).observe($('#home-traffic-chart'));
function openHomeView(name) { showView(name); }
$('#home-open-activity').addEventListener('click', () => openHomeView('activity'));
$('.home-explore-activity').addEventListener('click', () => openHomeView('activity'));
$('#home-open-providers').addEventListener('click', () => openHomeView('providers'));
$('.home-manage-providers').addEventListener('click', () => openHomeView('providers'));
async function loadDashboard() {
  if (dashboardLoadPromise) return dashboardLoadPromise;
  dashboardLoadPromise = (async () => {
    const now = Math.floor(Date.now() / 1000); const plan = activityBucketPlan(86400, now); const since = plan.until - 86400;
    const stats = await fetch(`/admin/activity/stats?since=${since}&until=${plan.until}&buckets=${plan.bucketCount}`).then(response => response.json());
    const errors = stats.requests - stats.successful; const successRate = stats.requests ? stats.successful * 100 / stats.requests : 0; const totalTokens = stats.input_tokens + stats.output_tokens; const cacheRate = stats.input_tokens ? stats.cached_tokens * 100 / stats.input_tokens : 0;
    $('#home-requests').textContent = compactNumber(stats.requests); $('#home-success').textContent = `${successRate.toFixed(1)}%`; $('#home-errors').textContent = errors ? `${errors.toLocaleString()} error${errors === 1 ? '' : 's'}` : 'No errors'; $('#home-tokens').textContent = compactNumber(totalTokens); $('#home-token-detail').textContent = `${compactNumber(stats.input_tokens)} input · ${compactNumber(stats.output_tokens)} output`; $('#home-cache-rate').textContent = `${cacheRate.toFixed(1)}%`; $('#home-cache-detail').textContent = `${compactNumber(stats.cached_tokens)} cached tokens`;
    $('#home-health-success').textContent = stats.requests ? `${successRate.toFixed(1)}%` : 'No traffic'; $('#home-health-latency').textContent = stats.requests ? formatDuration(Math.round(stats.latency_ms / stats.requests)) : 'No traffic'; $('#home-provider-health').textContent = providers.length ? `${providers.length} connected` : 'Not configured';
    $('#home-updated').textContent = `Updated ${new Date().toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})}`;
    $('#home-provider-summary').textContent = `${providers.length} provider${providers.length === 1 ? '' : 's'} · ${providers.reduce((sum, provider) => sum + provider.discovered_models.length, 0).toLocaleString()} discovered models`;
    renderHomeTraffic(stats.buckets);
    const providerItems = stats.by_provider.slice(0, 5).map(providerStat => {
      const provider = providers.find(item => item.id === providerStat.name); const item = document.createElement('button'); item.className = 'home-provider'; const success = providerStat.requests ? (providerStat.requests - providerStat.errors) * 100 / providerStat.requests : 0;
      item.innerHTML = `<span class="home-provider-mark">${escapeHtml((provider?.name || providerStat.name).slice(0, 1).toUpperCase())}</span><span class="home-provider-main"><strong>${escapeHtml(provider?.name || providerStat.name)}</strong><small>${compactNumber(providerStat.input_tokens + providerStat.output_tokens)} tokens · ${success.toFixed(1)}% success</small></span><span class="home-provider-meta"><strong>${providerStat.requests.toLocaleString()}</strong><small>requests</small></span>`;
      item.addEventListener('click', () => { selectedProviderId = providerStat.name; showView('providers'); }); return item;
    });
    if (providerItems.length) $('#home-providers').replaceChildren(...providerItems);
    else $('#home-providers').innerHTML = `<div class="home-providers-empty">${icon('provider')}<strong>No provider traffic</strong><p>Connect an Endpoint or send a request to populate this view.</p><button class="button secondary" type="button">Manage providers</button></div>`;
    $('#home-providers .home-providers-empty .button')?.addEventListener('click', () => openHomeView('providers'));
    const keyItems = (stats.by_api_key || []).slice(0, 4);
    $('#home-api-keys').innerHTML = keyItems.length ? keyItems.map(item => `<div class="home-key"><span>${icon('key')}</span><div><strong>${escapeHtml(item.name)}</strong><code>${escapeHtml(item.prefix || 'Unattributed')}</code></div><b>${item.requests.toLocaleString()}</b></div>`).join('') : '<div class="home-keys-empty">No API key traffic in the last 24 hours.</div>';
  })().finally(() => { dashboardLoadPromise = null; });
  return dashboardLoadPromise;
}

async function loadProviderActivity() {
  if (providerActivityLoadPromise) return providerActivityLoadPromise;
  providerActivityLoadPromise = (async () => {
    const now = Math.floor(Date.now() / 1000); const plan = activityBucketPlan(86400, now); const since = plan.until - 86400;
    const response = await fetch(`/admin/activity/stats?since=${since}&until=${plan.until}&buckets=${plan.bucketCount}`);
    if (!response.ok) return;
    const stats = await response.json();
    providerActivity = new Map((stats.provider_buckets || []).map(item => [item.name, item.requests]));
    if (!$('#providers-view').hidden && !selectedProviderId) renderProviders();
  })().finally(() => { providerActivityLoadPromise = null; });
  return providerActivityLoadPromise;
}

function refreshVisibleView() {
  if (!adminSession?.authenticated || document.hidden) return;
  if (!$('#home-view').hidden) loadDashboard();
  else if (!$('#activity-view').hidden) loadActivity();
  else if (!$('#providers-view').hidden && !selectedProviderId) loadProviderActivity();
}
setInterval(refreshVisibleView, LIVE_REFRESH_INTERVAL_MS);
document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshVisibleView(); });

$$('.copy-extension-command').forEach(button => button.addEventListener('click', async () => {
  await navigator.clipboard.writeText(button.dataset.command);
  const original = button.textContent; button.textContent = 'Copied';
  setTimeout(() => { button.textContent = original; }, 1200);
}));

// Every Hook stage a card can print is named the way the Provider side names it. The
// stable Hook IDs stay in the Extension API, so an unknown stage keeps the Provider
// framing and is spelled out in words instead of leaking its underscored ID.
const extensionHookNames = {upstream_request: 'Provider request', upstream_headers: 'Provider headers', upstream_exchange: 'Provider exchange', provider_endpoint: 'Provider Endpoint'};
function extensionHookLabel(hook) { return extensionHookNames[hook] || `Provider ${hook.replaceAll('_', ' ').toLowerCase()}`; }

function renderExtensions() {
  const list = $('#extensions-list');
  if (!extensions.length) { list.innerHTML = '<section class="card empty extensions-empty"><h3>No extensions in this build</h3><p>The commands above show how to include bundled Extensions in the next build.</p></section>'; return; }
  list.innerHTML = extensions.map(extension => {
    const configuredProviders = extension.id === 'request-defaults' ? providers.filter(provider => Object.keys(provider.extra_headers || {}).length || Object.keys(provider.extra_body || {}).length || provider.endpoints.some(endpoint => Object.keys(endpoint.extra_headers || {}).length || Object.keys(endpoint.extra_body || {}).length)) : [];
    const status = extension.enabled ? 'Enabled' : extension.runtime_configurable ? 'Disabled' : 'Disabled by CLI';
    const ownedTypes = new Set((extension.endpoint_types || []).map(type => type.id));
    const ownedEndpoints = providers.reduce((count, provider) => count + provider.endpoints.filter(endpoint => ownedTypes.has(endpoint.api_type)).length, 0);
    const configured = extension.id === 'request-defaults' ? `${configuredProviders.length} Provider${configuredProviders.length === 1 ? '' : 's'}` : ownedTypes.size ? `${ownedEndpoints} Endpoint${ownedEndpoints === 1 ? '' : 's'}` : 'On demand';
    const footer = extension.id === 'request-defaults' ? `<footer><div><strong>Configured in Provider context</strong><p>Header and body defaults remain next to the Provider and Endpoint resources they affect.${extension.enabled ? '' : ' They are retained while this Extension is disabled.'}</p></div>${configuredProviders.length ? `<div class="extension-provider-links">${configuredProviders.map(provider => `<button class="text-link" type="button" data-extension-provider="${escapeHtml(provider.id)}">${escapeHtml(provider.name)}</button>`).join('')}</div>` : '<button class="button secondary extension-open-providers" type="button">Choose a Provider</button>'}</footer>` : extension.id === 'traffic-capture' ? `<footer><div><strong>Sensitive diagnostic data</strong><p>Capture is stopped by default. Credentials are always redacted and content remains separate from Activity.</p></div><button class="button secondary open-traffic-capture" type="button" ${extension.enabled ? '' : 'disabled'}>Configure capture</button></footer>` : '';
    return `<article class="extension-card${extension.enabled ? '' : ' extension-disabled'}"><header><span class="extension-mark">${icon('extension')}</span><div><span class="section-kicker">Included in this build</span><h2>${escapeHtml(extension.name)}</h2><code>${escapeHtml(extension.id)} · v${escapeHtml(extension.version)}</code></div><label class="switch-label extension-toggle" title="${extension.runtime_configurable ? 'Enable or disable this Extension' : 'Restart without --no-extensions to manage Extensions'}"><input type="checkbox" data-extension-toggle="${escapeHtml(extension.id)}" ${extension.enabled ? 'checked' : ''} ${extension.runtime_configurable ? '' : 'disabled'}><span class="switch-track" aria-hidden="true"><i></i></span><span class="switch-status">${status}</span></label></header><p>${escapeHtml(extension.description)}</p><div class="extension-facts"><span><small>Implementation</small><strong>Native Rust</strong></span><span><small>Extension API</small><strong>v${extension.api_version}</strong></span><span><small>Hooks</small><strong>${extension.hooks.map(extensionHookLabel).join(' · ')}</strong></span><span><small>Configuration</small><strong>${configured}</strong></span></div>${footer}</article>`;
  }).join('');
  $$('[data-extension-toggle]').forEach(toggle => toggle.addEventListener('change', async () => {
    const extensionId = toggle.dataset.extensionToggle;
    const extension = extensions.find(item => item.id === extensionId);
    const ownedTypes = new Set((extension?.endpoint_types || []).map(type => type.id));
    const endpointCount = ownedTypes.size ? providers.reduce((count, provider) => count + provider.endpoints.filter(endpoint => ownedTypes.has(endpoint.api_type)).length, 0) : 0;
    if (!toggle.checked && endpointCount && !confirm(`Disable ${extension.name}?\n\n${endpointCount} configured Endpoint${endpointCount === 1 ? '' : 's'} will remain saved, but everything this Extension provides stops until it is enabled again.`)) {
      toggle.checked = true;
      return;
    }
    toggle.disabled = true;
    const response = await fetch(`/admin/extensions/${encodeURIComponent(extensionId)}`, {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({enabled: toggle.checked})});
    if (!response.ok) { toggle.checked = !toggle.checked; toggle.disabled = false; window.alert((await response.json()).error?.message || 'Could not update Extension'); return; }
    const updated = await response.json();
    extensions = extensions.map(extension => extension.id === updated.id ? updated : extension);
    renderExtensions(); renderProviders();
  }));
  $$('.extension-open-providers').forEach(button => button.addEventListener('click', () => showView('providers')));
  $$('.open-traffic-capture').forEach(button => button.addEventListener('click', () => showView('capture')));
  $$('[data-extension-provider]').forEach(button => button.addEventListener('click', () => { selectedProviderId = button.dataset.extensionProvider; showView('providers'); }));
}
async function loadExtensions() { const response = await fetch('/admin/extensions'); extensions = response.ok ? await response.json() : []; renderExtensions(); if (selectedProviderId) renderProviderPage(); }
/// Endpoint types come from the process, so a newly included Extension shows up
/// without a console change.
async function loadEndpointTypes() {
  // Endpoint types come from the API so an included Extension is offered without
  // a console change. When the request fails the built-in choices stay in place
  // instead of leaving the dialog without any option.
  let declared = [];
  try {
    const response = await fetch('/admin/endpoint-types');
    if (response.ok) declared = await response.json();
  } catch { declared = []; }
  if (!declared.length) return;
  endpointTypes = declared;
  renderEndpointTypeChoices();
  if (selectedProviderId) renderProviderPage();
}

function renderCaptureSelectors() {
  const form = $('#capture-form'); const providerSelect = form.elements.provider_id; const current = providerSelect.value || trafficCaptureStatus?.config.provider_id;
  providerSelect.innerHTML = '<option value="">Choose a Provider</option>' + providers.map(provider => `<option value="${escapeHtml(provider.id)}">${escapeHtml(provider.name)}</option>`).join('');
  providerSelect.value = current || '';
  const provider = providers.find(item => item.id === providerSelect.value); const endpointSelect = form.elements.endpoint_id; const selectedEndpoint = endpointSelect.value || trafficCaptureStatus?.config.endpoint_id;
  endpointSelect.innerHTML = '<option value="">Choose an Endpoint</option>' + (provider?.endpoints || []).map(endpoint => `<option value="${escapeHtml(endpoint.id)}">${escapeHtml(endpoint.id)} · ${escapeHtml(formatType(endpoint.api_type))}</option>`).join('');
  endpointSelect.value = selectedEndpoint || '';
}
function renderCaptureScopeSummary() {
  const form = $('#capture-form'); const provider = form.elements.provider_id.selectedOptions[0]?.textContent || 'a Provider'; const endpoint = form.elements.endpoint_id.value; const model = form.elements.model.value.trim(); const count = Math.max(1, Number(form.elements.remaining.value) || 1); const window = form.elements.timeout.selectedOptions[0]?.textContent || 'the selected window';
  $('#capture-scope-summary').textContent = endpoint ? `Capture the next ${count} matching request${count === 1 ? '' : 's'} sent through ${provider} / ${endpoint}, for ${model ? `exact model ${model}` : 'all models on this Endpoint'}, for up to ${window}.` : 'Choose a Provider and Endpoint to define where capture applies.';
}
function hydrateCaptureForm(config) {
  if (captureFormInitialized) return;
  const form = $('#capture-form'); form.elements.model.value = config.model || ''; if (config.remaining > 0) form.elements.remaining.value = config.remaining; form.elements.body_limit.value = String(config.body_limit); form.elements.retention_days.value = String(config.retention_days); form.elements.redacted_headers.value = (config.redacted_headers || []).join(', '); captureFormInitialized = true;
}
function captureStatus(capture) {
  const failed = capture.status == null || capture.status >= 400 || capture.outcome !== 'complete'; const label = capture.status == null ? 'No response' : String(capture.status); const detail = capture.truncated ? 'Truncated' : capture.outcome !== 'complete' ? capture.outcome.replaceAll('_', ' ') : '';
  return `<span class="capture-result${failed ? ' failed' : ''}"><span><i></i>${escapeHtml(label)}</span>${detail ? `<small>${escapeHtml(detail)}</small>` : ''}</span>`;
}
function renderTrafficCapture() {
  if (!trafficCaptureStatus) return;
  renderCaptureSelectors(); const config = trafficCaptureStatus.config; hydrateCaptureForm(config); const active = config.active && config.remaining > 0; const form = $('#capture-form');
  $('#capture-state').textContent = active ? 'Capture active' : 'Capture stopped'; $('#capture-active-summary').hidden = !active;
  if (active) { $('#capture-active-title').textContent = `Capturing next ${config.remaining} matching request${config.remaining === 1 ? '' : 's'}`; $('#capture-active-detail').textContent = `${config.provider_id} → ${config.endpoint_id} · ${config.model || 'all models'} · stops ${new Date(config.expires_at * 1000).toLocaleString()}`; }
  [...form.elements].forEach(element => { element.disabled = active; }); form.querySelector('[type="submit"]').hidden = active; form.classList.toggle('capture-form-locked', active); renderCaptureScopeSummary();
  $('#stop-capture').hidden = !active; $('#capture-count').textContent = trafficCaptureStatus.retained; $('#capture-dropped').textContent = trafficCaptureStatus.dropped;
  $('#captures-empty').hidden = trafficCaptures.length > 0; $('#capture-list-head').hidden = trafficCaptures.length === 0; $('#delete-all-captures').disabled = !trafficCaptures.length;
  $('#captures-list').innerHTML = trafficCaptures.map(capture => { const failed = capture.status == null || capture.status >= 400 || capture.outcome !== 'complete' || capture.truncated; return `<button class="capture-row${failed ? ' capture-row-attention' : ''}" type="button" data-capture-id="${escapeHtml(capture.request_id)}"><span><strong>${escapeHtml(capture.public_model)}</strong><code>${escapeHtml(capture.request_id)}</code><small>${new Date(capture.timestamp * 1000).toLocaleString()}</small></span><span>${escapeHtml(capture.provider_id)} → ${escapeHtml(capture.endpoint_id)}</span>${captureStatus(capture)}<span title="Provider duration"><strong>${capture.duration_ms == null ? '—' : escapeHtml(formatDuration(capture.duration_ms))}</strong><small>${Math.ceil(capture.bytes / 1024).toLocaleString()} KiB</small></span></button>`; }).join('');
}
async function loadTrafficCapture() {
  const [statusResponse, capturesResponse] = await Promise.all([fetch('/admin/extensions/traffic-capture/status'), fetch('/admin/extensions/traffic-capture/captures')]);
  if (!statusResponse.ok) return showApiError(statusResponse, $('#capture-error'));
  trafficCaptureStatus = await statusResponse.json(); trafficCaptures = capturesResponse.ok ? await capturesResponse.json() : []; renderTrafficCapture();
}
$('#refresh-captures').addEventListener('click', async event => {
  const button = event.currentTarget;
  button.disabled = true;
  try { await loadTrafficCapture(); } finally { button.disabled = false; }
});
$('#back-to-extensions').addEventListener('click', () => showView('extensions'));
$('#capture-form').elements.provider_id.addEventListener('change', () => { renderCaptureSelectors(); renderCaptureScopeSummary(); });
$('#capture-form').addEventListener('input', renderCaptureScopeSummary);
$('#capture-form').addEventListener('change', renderCaptureScopeSummary);
$('#capture-form').addEventListener('submit', async event => {
  event.preventDefault(); const data = new FormData(event.currentTarget); $('#capture-error').textContent = '';
  const response = await fetch('/admin/extensions/traffic-capture/status', {method: 'PATCH', headers: {'content-type': 'application/json'}, body: JSON.stringify({active: true, remaining: Number(data.get('remaining')), expires_at: Math.floor(Date.now() / 1000) + Number(data.get('timeout')), provider_id: data.get('provider_id'), endpoint_id: data.get('endpoint_id'), model: data.get('model'), body_limit: Number(data.get('body_limit')), retention_days: Number(data.get('retention_days')), redacted_headers: String(data.get('redacted_headers')).split(',').map(value => value.trim()).filter(Boolean)})});
  if (!response.ok) return showApiError(response, $('#capture-error')); trafficCaptureStatus = await response.json(); renderTrafficCapture();
});
$('#stop-capture').addEventListener('click', async () => { const response = await fetch('/admin/extensions/traffic-capture/stop', {method: 'POST'}); if (response.ok) { trafficCaptureStatus = await response.json(); renderTrafficCapture(); } });
$('#delete-all-captures').addEventListener('click', async () => { if (!confirm('Delete all captured request and response content?')) return; const response = await fetch('/admin/extensions/traffic-capture/captures', {method: 'DELETE'}); if (response.ok) loadTrafficCapture(); });
function captureBytes(bytes) { const array = Uint8Array.from(bytes || []); return new TextDecoder().decode(array); }
function parseCaptureSse(body) {
  const events = []; let done = false; let malformed = false;
  body.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split(/\n\n+/).forEach(frame => {
    const data = [];
    frame.split('\n').forEach(line => {
      if (!line.startsWith('data:')) return;
      let value = line.slice(5); if (value.startsWith(' ')) value = value.slice(1); data.push(value);
    });
    if (!data.length) return;
    const payload = data.join('\n');
    if (payload === '[DONE]') { done = true; return; }
    try { events.push(JSON.parse(payload)); } catch { malformed = true; }
  });
  return {events, done, malformed};
}
function assembleChatCompletion(parsed) {
  const first = parsed.events[0] || {}; const choices = new Map(); let usage = null;
  parsed.events.forEach(chunk => {
    if (chunk.usage) usage = chunk.usage;
    (chunk.choices || []).forEach(part => {
      const index = part.index || 0; const choice = choices.get(index) || {index, message: {role: 'assistant', content: ''}, finish_reason: null}; const delta = part.delta || {};
      if (delta.role) choice.message.role = delta.role;
      if (typeof delta.content === 'string') choice.message.content += delta.content;
      if (typeof delta.refusal === 'string') choice.message.refusal = (choice.message.refusal || '') + delta.refusal;
      (delta.tool_calls || []).forEach(call => {
        choice.message.tool_calls ||= []; const toolIndex = call.index || 0; const tool = choice.message.tool_calls[toolIndex] ||= {id: '', type: 'function', function: {name: '', arguments: ''}};
        if (call.id) tool.id = call.id; if (call.type) tool.type = call.type;
        if (call.function?.name) tool.function.name += call.function.name; if (call.function?.arguments) tool.function.arguments += call.function.arguments;
      });
      if (part.finish_reason !== undefined && part.finish_reason !== null) choice.finish_reason = part.finish_reason;
      if (part.logprobs !== undefined) choice.logprobs = part.logprobs; choices.set(index, choice);
    });
  });
  if (!parsed.done && ![...choices.values()].some(choice => choice.finish_reason !== null)) return null;
  const result = {id: first.id || '', object: 'chat.completion', created: first.created || 0, model: first.model || '', choices: [...choices.values()].sort((a, b) => a.index - b.index)};
  if (usage) result.usage = usage; if (first.system_fingerprint !== undefined) result.system_fingerprint = first.system_fingerprint; if (first.service_tier !== undefined) result.service_tier = first.service_tier;
  return result;
}
function assembleAnthropicMessage(parsed) {
  const start = parsed.events.find(event => event.type === 'message_start')?.message; const stopped = parsed.events.some(event => event.type === 'message_stop');
  if (!start || !stopped) return null;
  const message = JSON.parse(JSON.stringify(start)); message.content ||= []; const partialInputs = new Map();
  parsed.events.forEach(event => {
    const index = event.index || 0;
    if (event.type === 'content_block_start') message.content[index] = JSON.parse(JSON.stringify(event.content_block));
    if (event.type === 'content_block_delta') {
      const block = message.content[index] ||= {};
      if (event.delta?.type === 'text_delta') block.text = (block.text || '') + (event.delta.text || '');
      if (event.delta?.type === 'thinking_delta') block.thinking = (block.thinking || '') + (event.delta.thinking || '');
      if (event.delta?.type === 'signature_delta') block.signature = (block.signature || '') + (event.delta.signature || '');
      if (event.delta?.type === 'input_json_delta') partialInputs.set(index, (partialInputs.get(index) || '') + (event.delta.partial_json || ''));
    }
    if (event.type === 'message_delta') { Object.assign(message, event.delta || {}); message.usage = {...(message.usage || {}), ...(event.usage || {})}; }
  });
  partialInputs.forEach((input, index) => { try { message.content[index].input = JSON.parse(input); } catch { message.content[index].input = input; } });
  return message;
}
function assembleCaptureResponse(protocol, parsed) {
  if (parsed.malformed) return null;
  if (protocol === 'openai_responses') {
    const terminal = [...parsed.events].reverse().find(event => ['response.completed', 'response.incomplete', 'response.failed'].includes(event.type) && event.response);
    return terminal?.response || null;
  }
  if (protocol === 'openai_chat_completions') return assembleChatCompletion(parsed);
  if (protocol === 'anthropic_messages') return assembleAnthropicMessage(parsed);
  return null;
}
function highlightJson(text) {
  const pattern = /("(?:\\.|[^"\\])*")(?=\s*:)|"(?:\\.|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|\b(?:true|false|null)\b/g; let html = ''; let offset = 0;
  for (const match of text.matchAll(pattern)) {
    html += escapeHtml(text.slice(offset, match.index)); const token = match[0]; const kind = token.startsWith('"') ? (text.slice(match.index + token.length).match(/^\s*:/) ? 'key' : 'string') : /^(true|false)$/.test(token) ? 'boolean' : token === 'null' ? 'null' : 'number';
    html += `<span class="syntax-${kind}">${escapeHtml(token)}</span>`; offset = match.index + token.length;
  }
  return html + escapeHtml(text.slice(offset));
}
function highlightSse(text) {
  return text.split(/(\r?\n)/).map(line => {
    if (/^\r?\n$/.test(line)) return line;
    const match = line.match(/^(event|data|id|retry):( ?)(.*)$/);
    if (!match) return escapeHtml(line);
    const value = match[1] === 'data' ? highlightJson(match[3]) : `<span class="syntax-string">${escapeHtml(match[3])}</span>`;
    return `<span class="syntax-sse">${match[1]}</span>:${match[2]}${value}`;
  }).join('');
}
function resetCaptureDetailScroll() {
  const body = $('#capture-detail-body'); const headers = $('#capture-detail-headers'); const detail = $('.capture-detail-body');
  body.scrollTop = 0; body.scrollLeft = 0; headers.scrollTop = 0; headers.scrollLeft = 0; detail.scrollTop = 0; detail.scrollLeft = 0;
}
function renderCaptureBodyMode() {
  const assembled = captureDetailBodyMode === 'assembled'; const display = assembled ? captureDetailAssembled : captureDetailBody;
  $('#capture-detail-body').innerHTML = assembled ? highlightJson(display) : captureDetailIsSse ? highlightSse(display) : (() => { try { return highlightJson(JSON.stringify(JSON.parse(display), null, 2)); } catch { return escapeHtml(display || '(empty)'); } })();
  $$('#capture-body-modes button').forEach(button => { const active = button.dataset.captureBodyMode === captureDetailBodyMode; button.classList.toggle('active', active); button.setAttribute('aria-selected', String(active)); });
  $('#capture-detail-body').scrollTop = 0; $('#capture-detail-body').scrollLeft = 0; $('.capture-detail-body').scrollTop = 0;
}
function renderCaptureBody(body, protocol, canAssemble) {
  captureDetailBody = body; const parsed = parseCaptureSse(body); captureDetailIsSse = parsed.events.length > 0; const assembled = canAssemble && captureDetailIsSse ? assembleCaptureResponse(protocol, parsed) : null;
  captureDetailAssembled = assembled ? JSON.stringify(assembled, null, 2) : ''; $('#capture-body-modes').hidden = parsed.events.length === 0;
  $('#capture-body-modes [data-capture-body-mode="assembled"]').disabled = !captureDetailAssembled;
  const note = $('#capture-assembly-note'); note.textContent = parsed.events.length && !captureDetailAssembled ? 'A complete non-streaming response cannot be assembled because the capture is truncated, malformed, or has no terminal event.' : ''; note.hidden = !note.textContent;
  captureDetailBodyMode = captureDetailAssembled ? 'assembled' : 'raw'; renderCaptureBodyMode();
}
function renderCaptureDetail(direction) {
  const response = direction === 'response'; $$('.capture-direction-tabs button').forEach(button => { const active = button.dataset.captureDirection === direction; button.classList.toggle('active', active); button.setAttribute('aria-selected', String(active)); });
  $('#capture-detail-headers-title').textContent = `${response ? 'Received from Provider' : 'Sent to Provider'} · Headers`; $('#capture-detail-body-title').textContent = `${response ? 'Received from Provider' : 'Sent to Provider'} · Body`;
  const headers = response ? selectedCapture.response_headers : selectedCapture.request_headers; const body = captureBytes(response ? selectedCapture.response_body : selectedCapture.request_body); const truncated = response ? selectedCapture.response_truncated : selectedCapture.request_truncated;
  $('#capture-detail-headers').innerHTML = headers.map(header => `<span class="syntax-key">${escapeHtml(header.name)}</span>: ${escapeHtml(header.value)}`).join('\n') || '(none)'; $('#capture-detail-truncated').hidden = !truncated; renderCaptureBody(body, selectedCapture.upstream_protocol, response && !truncated && selectedCapture.outcome === 'complete'); resetCaptureDetailScroll();
}
async function copyCaptureText(button, text) {
  await navigator.clipboard.writeText(text); const label = button.querySelector('span'); const original = label.textContent; label.textContent = 'Copied'; button.classList.add('copied');
  setTimeout(() => { label.textContent = original; button.classList.remove('copied'); }, 1200);
}
async function openCaptureDetail(requestId) {
  const response = await fetch(`/admin/extensions/traffic-capture/captures/${encodeURIComponent(requestId)}`); if (!response.ok) return;
  selectedCapture = await response.json(); $('#capture-detail-title').textContent = selectedCapture.public_model; $('#capture-detail-meta').textContent = `${selectedCapture.provider_id} → ${selectedCapture.endpoint_id} · ${selectedCapture.outcome} · ${selectedCapture.duration_ms == null ? 'Duration unavailable' : `${formatDuration(selectedCapture.duration_ms)} at the Provider`}`; renderCaptureDetail('request'); resetCaptureDetailScroll(); $('#traffic-capture-detail-dialog').showModal();
}
$('#captures-list').addEventListener('click', event => { const row = event.target.closest('[data-capture-id]'); if (row) openCaptureDetail(row.dataset.captureId); });
$$('.capture-direction-tabs button').forEach(button => button.addEventListener('click', () => renderCaptureDetail(button.dataset.captureDirection)));
$$('#capture-body-modes button').forEach(button => button.addEventListener('click', () => { captureDetailBodyMode = button.dataset.captureBodyMode; renderCaptureBodyMode(); }));
$$('.capture-copy-button').forEach(button => button.addEventListener('click', () => {
  const text = button.dataset.captureCopy === 'headers' ? $('#capture-detail-headers').textContent : captureDetailBodyMode === 'assembled' ? captureDetailAssembled : captureDetailBody;
  copyCaptureText(button, text);
}));
$$('.close-capture-detail').forEach(button => button.addEventListener('click', () => $('#traffic-capture-detail-dialog').close()));
$('#delete-capture').addEventListener('click', async () => { if (!selectedCapture) return; const response = await fetch(`/admin/extensions/traffic-capture/captures/${encodeURIComponent(selectedCapture.request_id)}`, {method: 'DELETE'}); if (response.ok) { $('#traffic-capture-detail-dialog').close(); loadTrafficCapture(); } });

function formatType(type) { return endpointType(type)?.label || type; }
function escapeHtml(value) { const node = document.createElement('span'); node.textContent = String(value); return node.innerHTML; }
async function loadProviders() { const response = await fetch('/admin/providers'); providers = await response.json(); renderProviders(); if (!$('#providers-view').hidden && !selectedProviderId) await loadProviderActivity(); if (!$('#pricing-view').hidden) renderPricingPage(); }
async function loadPricing() {
  const [pricingResponse, activityResponse] = await Promise.all([
    fetch('/admin/pricing'),
    fetch('/admin/activity/logs?since=0&limit=100'),
  ]);
  globalPricing = await pricingResponse.json();
  if (activityResponse.ok) pricingActivityModels = (await activityResponse.json())
    .filter(log => log.upstream_model || log.model)
    .map(log => ({model: log.upstream_model || '', incomingModel: log.model || '', providerId: log.provider, endpointId: log.endpoint}));
  if (!$('#pricing-view').hidden) renderPricingPage();
}
async function loadRoutes() { const response = await fetch('/admin/routes'); modelRoutes = await response.json(); renderRoutes(); if (!$('#pricing-view').hidden) renderPricingPage(); }
initializeAdmin();
