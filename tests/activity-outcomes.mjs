// ACTIVITY-54: console counts/rates and badges distinguish cancellation from
// both success and Provider errors, including when all requests were cancelled.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const app = await readFile(fileURLToPath(new URL('../web/app.js', import.meta.url)), 'utf8');
const helpers = app.slice(app.indexOf('function isCancelledActivity('), app.indexOf('function activityBuckets('));
const badge = app.split('\n').find(line => line.startsWith('function statusBadge('));
assert.ok(helpers && badge, 'the production outcome helpers are available');
const context = vm.createContext({});
vm.runInContext(helpers + '\n' + badge, context);
const cancelled = { status: 499, failure: { stage: 'client', category: 'disconnected' } };
assert.equal(context.isCancelledActivity(cancelled), true);
for (const log of [
  { status: 502, failure: cancelled.failure }, // historical records are not reinterpreted
  { status: 499, failure: { stage: 'upstream_response', category: 'http_error' } },
  { status: 499 }, { status: 200 },
]) assert.equal(context.isCancelledActivity(log), false);
const totals = { requests: 10, successful: 2, errors: 1, cancelled: 7 };
const dimension = { requests: 10, errors: 1, cancelled: 7 };
for (const stats of [totals, dimension]) {
  assert.equal(context.activityEvaluatedRequests(stats), 3);
  assert.ok(Math.abs(context.activitySuccessRate(stats) - 200 / 3) < 1e-9);
  assert.equal(context.activitySuccessLabel(stats), '66.7%');
  assert.equal(context.activityOutcomeCounts(stats), '1 error · 7 cancelled');
}
assert.equal(context.activitySuccessLabel({ requests: 3, successful: 2 }), '66.7%');
for (const stats of [
  { requests: 0, successful: 0, errors: 0, cancelled: 0 },
  { requests: 7, successful: 0, errors: 0, cancelled: 7 },
]) {
  assert.equal(context.activitySuccessRate(stats), null);
  assert.equal(context.activitySuccessLabel(stats), '—');
}
assert.match(context.statusBadge(499, true), /class="status-badge cancelled"/);
assert.match(context.statusBadge(499, true), /Cancelled · 499/);
assert.match(context.statusBadge(499), /class="status-badge failure"/);
assert.match(context.statusBadge(200), /class="status-badge success"/);
console.log('Activity outcomes passed: cancellation classification, counts, rates and badges');
