#!/usr/bin/env node
// Qualifies one committed revision of transcript-lake by its Cargo test
// targets, for a defect register's revision-bound completion probe:
//
//   node tests/qualify.mjs <revision> [<test-target>...]
//
// where a test target is a [[test]] name of Cargo.toml (harness-ingestion,
// devin-chains, ...). The revision's committed tree is exported with git
// archive into target/qualify/<commit> of this checkout. There the crate and
// every test target are compiled first (cargo test --locked --offline
// --no-run), reported as `compile` when that fails, and then each named
// target runs with cargo test --locked --offline --test <target>, so neither
// another session's uncommitted edit nor a later test reads in. Standard
// output lists what failed, one per line, and is empty when everything
// passed; cargo's own output goes to standard error. A revision git cannot
// name or export ends the run as failed.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
// argv holds node and this script before the caller's words.
const [, , revision, ...targets] = process.argv;
assert.ok(revision, 'usage: node tests/qualify.mjs <revision> [<test-target>...]');
// A run succeeded when it started, no signal ended it and it exited zero
// (https://nodejs.org/api/child_process.html#child_processspawnsynccommand-args-options).
const succeeded = (run) => !run.error && run.signal === null && !run.status;

const resolved = spawnSync('git', ['-C', root, 'rev-parse', '--verify', `${revision}^{commit}`], { encoding: 'utf8' });
assert.ok(succeeded(resolved), `git cannot name ${revision} as a commit: ${resolved.stderr}`);
const commit = resolved.stdout.trim();
const exported = join(root, 'target', 'qualify', commit);
rmSync(exported, { recursive: true, force: true });
mkdirSync(exported, { recursive: true });
const archive = spawnSync('git', ['-C', root, 'archive', '--format=tar', commit], { maxBuffer: Infinity });
assert.ok(succeeded(archive), `git archive ${commit} failed: ${archive.stderr}`);
const unpacked = spawnSync('tar', ['-x', '-C', exported], { input: archive.stdout });
assert.ok(succeeded(unpacked), `tar could not unpack ${commit}: ${unpacked.stderr}`);

// One build directory for every qualified revision, so a later revision
// recompiles only what changed.
const env = { ...process.env, CARGO_TARGET_DIR: join(root, 'target', 'qualify-build') };
function cargo(label, args) {
  const ran = spawnSync('cargo', ['test', '--locked', '--offline', ...args], {
    cwd: exported,
    env,
    stdio: ['ignore', process.stderr, process.stderr],
  });
  if (ran.error) throw new Error(`cargo could not be started: ${ran.error.message}`, { cause: ran.error });
  if (!succeeded(ran)) console.log(`${label}: failed (${ran.status ?? ran.signal})`);
  return succeeded(ran);
}
if (cargo('compile', ['--no-run'])) {
  for (const target of targets) cargo(target, ['--test', target]);
}
