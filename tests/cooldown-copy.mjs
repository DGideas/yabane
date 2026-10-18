// ENDPOINT-42: historical explanations depend on recorded facts, not today's
// policy, and zero is a real Provider delay. Exercise the console's own code.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../web/app.js', import.meta.url), 'utf8');
const names = ['formatCooldown', 'formatClock', 'cooldownProviderNote', 'endpointPolicyActivity'];
const code = names.map(name => {
  const start = source.indexOf(`function ${name}(`);
  assert.ok(start >= 0, `${name} is missing`);
  const end = source.indexOf('\n}', start);
  assert.ok(end >= 0, `${name} has no closing brace`);
  return source.slice(start, end + 2);
}).join('\n');
const context = vm.createContext({escapeHtml: value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')});
vm.runInContext(code, context);
const fixtures = [
  [{ last_seconds: 300, last_reported_seconds: 120, last_source: 'fixed' }, /fixed duration/, /used unchanged|capped/],
  [{ last_seconds: 120, last_reported_seconds: 120, last_source: 'provider' }, /used unchanged/, /fixed duration|capped/],
  [{ last_seconds: 300, last_reported_seconds: 600, last_source: 'capped' }, /capped/, /used unchanged/],
  [{ last_seconds: 300, last_source: 'fallback' }, /no usable delay.*configured maximum/, /used unchanged/],
  [{ last_seconds: 0, last_reported_seconds: 0, last_source: 'provider' }, /asked for 0 seconds.*used unchanged/, /no usable delay|configured maximum/],
  // Older/mixed-version observations must not be inferred from today's policy.
  [{ last_seconds: 300, last_reported_seconds: 120 }, /source.*not available/i, /used unchanged|capped|fixed duration/],
];
for (const [facts, expected, forbidden] of fixtures) {
  let baseline;
  for (const mode of ['fixed', 'prefer_provider', 'provider_only']) {
    const endpoint = {
      rate_limit_cooldown: { seconds: 900, mode },
      rate_limit_cooldown_activity: { applied: 1, skipped: 0, last_applied_at: 1700000000, ...facts },
    };
    const text = context.endpointPolicyActivity(endpoint);
    assert.match(text, expected);
    assert.doesNotMatch(text, forbidden);
    if (facts.last_seconds === 0) {
      assert.match(text, /no new cooldown/i);
      assert.doesNotMatch(text, /took an identity out for 0/);
    }
    baseline ??= text;
    assert.equal(text, baseline, 'editing the policy cannot change the historical explanation');
  }
}
for (const activity of [undefined, {applied: 0, skipped: 0}, {applied: 0, skipped: 2, last_observed_at: 1700000000}]) {
  const endpoint = {rate_limit_cooldown: {seconds: 300}, rate_limit_cooldown_activity: activity};
  const text = context.endpointPolicyActivity(endpoint);
  assert.match(text, activity ? /Durations selected/ : /not available/);
  if (activity && !activity.skipped) assert.match(text, /No rate-limit answer/);
  if (activity?.skipped) assert.match(text, /reported no usable delay/);
  assert.equal(context.endpointPolicyActivity({...endpoint, rate_limit_cooldown: {seconds: 0}}), '');
}
console.log('Cooldown copy passed: structured history, fixed/provider/capped/fallback, zero, policy edits, legacy/unobserved/skipped observations');
