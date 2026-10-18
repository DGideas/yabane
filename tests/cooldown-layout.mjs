// ENDPOINT-40 / ENDPOINT-31 / ENDPOINT-42: render real console helpers for the
// compact summary, sorted rows, current allocation, and disclosed explanations.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../web/app.js', import.meta.url), 'utf8');
const names = ['enabledCredentials', 'identityGroups', 'carryingGroup', 'credentialRows', 'trafficShares', 'identityPool', 'identityNoun', 'identityList', 'cooldownEndsAt', 'formatCooldown', 'formatClock', 'cooldownDelayClause', 'cooldownNoDelayClause', 'cooldownPolicyLabel', 'cooldownProviderNote', 'endpointPolicyActivity', 'groupShareText', 'endpointPoolSummary'];
const context = vm.createContext({
  escapeHtml: value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;'),
  icon: () => '<svg></svg>',
});
vm.runInContext(names.map(name => {
  const start = source.indexOf(`function ${name}(`);
  const end = source.indexOf('\n}', start);
  assert.ok(start >= 0 && end > start, `Missing ${name}`);
  return source.slice(start, end + 2);
}).join('\n'), context);
const provider = {id: 'fixture'};
const credential = (id, priority, extra = {}) => ({id, name: id, priority, weight: 100, enabled: true, ...extra});
const endpoint = {
  id: 'pool', requires_credential: true, rate_limit_cooldown: {seconds: 3600, mode: 'prefer_provider'},
  credentials: [credential('reserve', 9), credential('primary', 4, {cooldown_seconds_remaining: 90}), credential('standby', 6)],
};
const summary = context.endpointPoolSummary(endpoint);
assert.match(summary, /data-tone="steady"/);
assert.match(summary, /Priority 2 is carrying all traffic · standby/);
assert.match(summary, /primary is cooling down until/);
assert.match(summary, /Traffic returns to a higher-priority group/);
assert.match(summary, /data-detail="cooldown"/);
assert.match(summary, /data-detail="routing"/);
assert.doesNotMatch(summary, /<details[^>]*\bopen\b/);
assert.match(summary, /No request is retried/);
assert.match(summary, /pins one identity/);
const rows = context.credentialRows(provider, endpoint);
assert.ok(rows.indexOf('data-credential="primary"') < rows.indexOf('data-credential="standby"'));
assert.ok(rows.indexOf('data-credential="standby"') < rows.indexOf('data-credential="reserve"'));
assert.equal(endpoint.credentials.map(key => key.id).join(','), 'reserve,primary,standby', 'rendering must not reorder configuration');
assert.match(rows, /data-state="cooling"/);
assert.match(rows, /data-state="carrying"/);
assert.match(rows, /data-state="waiting"/);
assert.match(rows, /normally 100%/);
assert.match(rows, /class="clear-cooldown|class="text-link clear-cooldown/);
assert.match(rows, /aria-label="Actions for primary"/);
assert.doesNotMatch(rows, /access token/);

const allCooling = {...endpoint, credentials: endpoint.credentials.map(key => ({...key, cooldown_seconds_remaining: 90}))};
assert.match(context.endpointPoolSummary(allCooling), /data-tone="exhausted"/);
assert.match(context.endpointPoolSummary(allCooling), /Requests still go out through Priority 1/);
assert.match(context.credentialRows(provider, allCooling), /data-state="fallback"/);
assert.match(context.credentialRows(provider, allCooling), /<strong>100%<\/strong><small[^>]*>all cooling · requests still go out/);

const weighted = {...endpoint, credentials: [credential('left', 1, {weight: 30, cooldown_seconds_remaining: 90}), credential('right', 1, {weight: 70})]};
assert.match(context.credentialRows(provider, weighted), /<strong>100%<\/strong><small[^>]*>normally 70%/);
assert.match(context.credentialRows(provider, weighted), /<strong>0%<\/strong><small[^>]*>normally 30%/);
weighted.credentials.forEach(key => { key.cooldown_seconds_remaining = 90; });
assert.match(context.credentialRows(provider, weighted), /<strong>30%<\/strong><small[^>]*>all cooling/);
assert.match(context.credentialRows(provider, weighted), /<strong>70%<\/strong><small[^>]*>all cooling/);

const off = {...endpoint, rate_limit_cooldown: {seconds: 0}, credentials: endpoint.credentials.map(key => ({...key, cooldown_seconds_remaining: 0}))};
assert.match(context.endpointPoolSummary(off), /data-tone="attention"/);
assert.match(context.endpointPoolSummary(off), /Priority 2 never takes over/);
assert.doesNotMatch(context.endpointPoolSummary(off), /data-detail="cooldown"/);
const solo = {...off, credentials: [credential('solo', 1)]};
assert.equal(context.endpointPoolSummary(solo), '');
assert.equal(context.endpointPoolSummary({...solo, requires_credential: false}), '');
assert.match(context.endpointPoolSummary({...endpoint, credentials: [credential('disabled', 1, {enabled: false})]}), /No identities are enabled/);
assert.match(context.credentialRows(provider, {...solo, credentials: [credential('disabled', 1, {enabled: false})]}), /Disabled/);

// A zero-weight identity cannot keep a group alive after its positive-weight
// member cools, or make an all-cooling pool look like a successful takeover.
const zero = {...endpoint, credentials: [credential('zero', 1, {weight: 0}), credential('primary', 1, {cooldown_seconds_remaining: 90}), credential('standby', 2)]};
assert.match(context.endpointPoolSummary(zero), /Priority 2 is carrying all traffic/);
assert.match(context.credentialRows(provider, zero), /No traffic configured/);
assert.match(context.endpointPoolSummary({...zero, credentials: zero.credentials.slice(0, 2)}), /data-tone="exhausted"/);
const hostile = {...endpoint, credentials: [credential('unsafe', 1, {name: '<script>"account"</script>'}), credential('standby', 2)]};
assert.doesNotMatch(context.endpointPoolSummary(hostile), /<script>/);
assert.doesNotMatch(context.credentialRows(provider, hostile), /<script>/);
console.log('Cooldown layout passed: compact/disclosed state, priority ordering, weighted/all-cooling allocation, disabled policy, zero weights, escaping');
