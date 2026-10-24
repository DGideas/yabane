// Exercise gate dispatch/failure handling without invoking compilers or servers.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

const source = await readFile(new URL('./check.sh', import.meta.url), 'utf8');
const work = await mkdtemp(path.join(tmpdir(), 'yabane-check-gate-'));
try {
  await mkdir(path.join(work, 'tests'));
  await mkdir(path.join(work, 'web'));
  await mkdir(path.join(work, 'bin'));
  await writeFile(path.join(work, 'tests/check.sh'), source);
  await writeFile(path.join(work, 'tests/e2e.sh'), 'echo browser >> "$GATE_CALLS"\n');
  await writeFile(path.join(work, 'tests/behavior-map.sh'), 'true\n');
  await writeFile(path.join(work, 'web/app.js'), '');
  // Shell fakes keep this dispatch-only test cheap even with 32 subsets.
  const fake = `#!/usr/bin/env bash
tool=\${0##*/}
{ printf '%s\\t' "$tool" "$@"; printf '\\n'; } >> "$GATE_CALLS"
if [[ $tool == rustc ]]; then echo 'rustc 1.88.0'; fi
if [[ $GATE_FAIL == cargo && $tool == cargo && \${1:-} == test ]] ||
   [[ $GATE_FAIL == accessibility && $tool == node && \${1:-} == tests/accessibility.mjs ]] ||
   [[ $GATE_FAIL == http && $tool == node && \${1:-} == tests/response-limits.mjs ]]; then exit 7; fi
exit 0
`;
  for (const tool of ['cargo', 'rustc', 'cargo-audit', 'node']) {
    const file = path.join(work, 'bin', tool);
    await writeFile(file, fake);
    await chmod(file, 0o700);
  }
  const log = path.join(work, 'calls');
  async function run(mode, fail = '') {
    await writeFile(log, '');
    const result = spawnSync('bash', [path.join(work, 'tests/check.sh'), ...mode], {
      env: { ...process.env, PATH: `${path.join(work, 'bin')}:${process.env.PATH}`, GATE_CALLS: log, GATE_FAIL: fail },
      encoding: 'utf8', timeout: 30000,
    });
    if (result.error) throw result.error;
    const calls = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean)
      .map(line => {
        if (line === 'browser') return {tool: 'browser', args: []};
        const fields = line.replace(/\t$/, '').split('\t');
        return {tool: fields[0], args: fields.slice(1)};
      });
    return { result, calls };
  }
  const argsFor = (calls, verb) => calls.filter(c => c.tool === 'cargo' && c.args[0] === verb).map(c => c.args);
  const quick = await run(['--quick']);
  assert.equal(quick.result.status, 0);
  assert.equal(argsFor(quick.calls, 'test').length, 1);
  assert.equal(argsFor(quick.calls, 'clippy').length, 1);
  assert.equal(argsFor(quick.calls, 'build').length, 0);
  assert.match(quick.result.stdout, /HTTP and browser tests were NOT run/);
  const normal = await run([]);
  assert.equal(normal.result.status, 0);
  assert.equal(argsFor(normal.calls, 'test').length, 17);
  const filtered = argsFor(normal.calls, 'test').filter(args => args.includes('--features'));
  assert.equal(filtered.length, 15);
  const filters = ['extensions::', 'config::', 'control::', 'gateway::', 'endpoint_signin::', 'models::'];
  assert.ok(filtered.every(args => JSON.stringify(args.slice(args.indexOf('--') + 1)) === JSON.stringify(filters)));
  assert.ok(normal.calls.some(c => c.tool === 'node' && c.args[0] === 'tests/response-limits.mjs'));
  assert.ok(argsFor(normal.calls, 'test').some(args => args.includes('--no-default-features') && !args.includes('--features')));
  assert.equal(argsFor(normal.calls, 'build').length, 1);
  for (const mode of ['--matrix', '--full']) {
    const complete = await run([mode]);
    assert.equal(complete.result.status, 0);
    assert.equal(argsFor(complete.calls, 'test').length, 32);
    const combos = argsFor(complete.calls, 'test').map(args => args.includes('--all-features') ? 'all' : args.includes('--features') ? args[args.indexOf('--features') + 1] : '');
    assert.equal(new Set(combos).size, 32);
    const extensions = ['extension-request-defaults', 'extension-traffic-capture', 'extension-openai-subscription', 'extension-openai', 'extension-codebuddy'];
    const masks = combos.map(combo => combo === 'all' ? 31 : combo.split(',').filter(Boolean).reduce((mask, feature) => {
      assert.ok(extensions.includes(feature), `unknown feature ${feature}`);
      return mask | (1 << extensions.indexOf(feature));
    }, 0)).sort((a, b) => a - b);
    assert.deepEqual(masks, Array.from({length: 32}, (_, i) => i));
    assert.ok(complete.calls.some(c => c.tool === 'node' && c.args[0] === 'tests/response-limits.mjs'));
    assert.equal(complete.calls.some(c => c.tool === 'node' && c.args[0] === 'tests/accessibility.mjs'), mode === '--full');
    assert.equal(complete.calls.some(c => c.tool === 'browser'), mode === '--full');
  }
  const accessibilityFailed = await run(['--full'], 'accessibility');
  assert.equal(accessibilityFailed.result.status, 7);
  assert.ok(!accessibilityFailed.calls.some(c => c.tool === 'browser'), 'accessibility failure blocks browser/success');
  assert.doesNotMatch(accessibilityFailed.result.stdout, /quality checks passed/);
  const failed = await run([], 'cargo');
  assert.equal(failed.result.status, 7);
  assert.equal(argsFor(failed.calls, 'build').length, 0);
  assert.doesNotMatch(failed.result.stdout, /quality checks passed/);
  const httpFailed = await run(['--full'], 'http');
  assert.equal(httpFailed.result.status, 7);
  assert.ok(!httpFailed.calls.some(c => c.tool === 'browser'), 'HTTP failure blocks browser/success');
  assert.doesNotMatch(httpFailed.result.stdout, /quality checks passed/);
  assert.equal((await run(['--unknown'])).result.status, 2);
  assert.equal((await run(['--quick', '--full'])).result.status, 2);
  console.log('Quality gate modes, feature filtering, complete matrix and failure propagation passed.');
} finally {
  await rm(work, { recursive: true, force: true });
}
