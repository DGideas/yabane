// Exercise Git watch paths without rebuilding the gateway for each ref layout.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtemp, mkdir, rm} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const temporary = await mkdtemp(path.join(tmpdir(), 'yabane-build-identity-'));
const repo = path.join(temporary, 'repo');
const tree = path.join(temporary, 'tree');
const executable = path.join(temporary, 'build-script');
function run(command, args, cwd = repo) {
  const result = spawnSync(command, args, {cwd, encoding: 'utf8', timeout: 30000});
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} ${args.join(' ')}: ${result.stderr}`);
  return result.stdout.trim();
}
function inspect(cwd, expectedWatch) {
  const output = run(executable, [], cwd);
  const watched = output.split('\n').filter(line => line.startsWith('cargo:rerun-if-changed='))
    .map(line => path.resolve(cwd, line.slice('cargo:rerun-if-changed='.length)));
  assert.ok(watched.length > 0);
  assert.ok(watched.every(file => existsSync(file)), 'missing watch paths force endless rebuilds');
  assert.ok(watched.includes(path.resolve(cwd, expectedWatch)), `watch ${expectedWatch}`);
  assert.ok(output.includes(`cargo:rustc-env=YABANE_GIT_COMMIT=${run('git', ['rev-parse', '--short=8', 'HEAD'], cwd)}`));
  assert.ok(output.includes(`cargo:rustc-env=YABANE_GIT_COMMIT_TIME=${run('git', ['show', '-s', '--format=%cI', 'HEAD'], cwd)}`));
  return watched;
}
try {
  await mkdir(repo);
  run('rustc', [fileURLToPath(new URL('../build.rs', import.meta.url)), '--edition=2024', '-o', executable]);
  run('git', ['init', '-q']);
  run('git', ['config', 'user.name', 'Fixture']);
  run('git', ['config', 'user.email', 'fixture@example.invalid']);
  run('git', ['config', 'commit.gpgsign', 'false']);
  run('git', ['commit', '--allow-empty', '-qm', 'initial']);
  const branch = run('git', ['symbolic-ref', 'HEAD']);
  inspect(repo, run('git', ['rev-parse', '--git-path', branch]));
  run('git', ['worktree', 'add', '-qb', 'fixture-tree', tree]);
  inspect(tree, run('git', ['rev-parse', '--git-path', 'refs/heads/fixture-tree'], tree));
  run('git', ['pack-refs', '--all']);
  const packedWatches = inspect(tree, run('git', ['rev-parse', '--git-path', 'packed-refs'], tree));
  const refs = path.resolve(tree, run('git', ['rev-parse', '--git-path', 'refs'], tree));
  assert.ok(packedWatches.includes(refs), 'packed branch must also detect a new loose ref');
  run('git', ['commit', '--allow-empty', '-qm', 'advance packed branch'], tree);
  inspect(tree, run('git', ['rev-parse', '--git-path', 'refs/heads/fixture-tree'], tree));
  run('git', ['checkout', '--detach', '-q'], tree);
  inspect(tree, run('git', ['rev-parse', '--git-path', 'HEAD'], tree));
  run('git', ['commit', '--allow-empty', '-qm', 'advance detached HEAD'], tree);
  inspect(tree, run('git', ['rev-parse', '--git-path', 'HEAD'], tree));
  console.log('Build identity: normal, worktree, packed refs, loose-ref creation and detached HEAD passed.');
} finally {
  await rm(temporary, {recursive: true, force: true});
}
