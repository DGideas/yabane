// ACTIVITY-27: production calendar ticks, not deduplicated sampled hours.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const app = await readFile(new URL('../web/app.js', import.meta.url), 'utf8');
const helpers = app.slice(app.indexOf('function activityDateTicks('), app.indexOf('function activityMetricValue('));
assert.ok(helpers.startsWith('function activityDateTicks('));
const context = vm.createContext({});
vm.runInContext(helpers, context);
const originalTZ = process.env.TZ;
const dayNumber = date => Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86400000;
function check(start, seconds, width) {
  const ticks = context.activityDateTicks([{start}], seconds, width);
  assert.ok(ticks.length > 0);
  const dates = ticks.map(tick => new Date(tick.timestamp * 1000));
  ticks.forEach((tick, index) => {
    assert.equal(dates[index].getHours(), 0);
    assert.equal(dates[index].getMinutes(), 0);
    assert.ok(tick.timestamp >= start && tick.timestamp <= start + seconds);
    assert.equal(tick.position, (tick.timestamp - start) / seconds);
    assert.equal(tick.label, dates[index].toLocaleDateString([], {month: 'short', day: 'numeric'}));
  });
  const strides = dates.slice(1).map((date, index) => dayNumber(date) - dayNumber(dates[index]));
  assert.equal(new Set(strides).size, strides.length ? 1 : 0, 'uniform calendar-day stride');
  assert.equal(new Set(ticks.map(tick => tick.label)).size, ticks.length, 'no duplicate date labels');
  assert.ok(ticks.length <= Math.max(1, Math.min(8, Math.floor(width / 90))));
  return ticks;
}
try {
  process.env.TZ = 'Asia/Shanghai';
  const start = new Date('2026-11-16T15:00:00+08:00').getTime() / 1000;
  const seconds = 7 * 86400;
  const desktop = check(start, seconds, 1350);
  assert.deepEqual(Array.from(desktop, tick => new Date(tick.timestamp * 1000).getDate()), [17, 18, 19, 20, 21, 22, 23]);
  assert.ok(desktop.every((tick, index) => index === 0 || tick.timestamp - desktop[index - 1].timestamp === 86400), 'no doubled gap between adjacent dates');
  for (const width of [230, 285, 630, 1000]) check(start, seconds, width);
  for (const days of [2, 14, 30, 90, 365, 3650]) check(start, days * 86400, 630);
  // Exact midnight may be labelled; a partial end must not be forced onto the axis.
  const midnight = new Date('2026-11-16T00:00:00+08:00').getTime() / 1000;
  const boundaries = check(midnight, 2 * 86400, 1000);
  assert.equal(boundaries[0].timestamp, midnight);
  assert.equal(boundaries.at(-1).timestamp, midnight + 2 * 86400);
  assert.equal(desktop.at(-1).timestamp < start + seconds, true);
  // An offset timezone demonstrates ticks need not coincide with hourly buckets.
  process.env.TZ = 'Asia/Kathmandu';
  const offsetTicks = check(start, seconds, 1350);
  assert.ok(offsetTicks.some(tick => (tick.timestamp - start) % 3600 !== 0));
  process.env.TZ = 'America/New_York';
  for (const [date, transitionHours] of [['2026-03-06T15:00:00-05:00', 23], ['2026-10-30T15:00:00-04:00', 25]]) {
    const ticks = check(new Date(date).getTime() / 1000, seconds, 1350);
    assert.ok(ticks.some((tick, index) => index > 0 && tick.timestamp - ticks[index - 1].timestamp === transitionHours * 3600), `${transitionHours}-hour DST day keeps its real width`);
  }
  assert.equal(context.activityDateTicks([], seconds, 1000).length, 0);
  assert.equal(context.activityDateTicks([{start}], 86400, 1000).length, 0, 'short-range labels unchanged');
  console.log('Activity date axis passed: local midnight, uniform thinning, partial days, offset timezones and DST');
} finally {
  if (originalTZ === undefined) delete process.env.TZ; else process.env.TZ = originalTZ;
}
