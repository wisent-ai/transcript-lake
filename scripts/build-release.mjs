#!/usr/bin/env node

import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  writeFileSync,
} from 'node:fs';
import { basename, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const N = (value) => Number(value);
const [ZERO, TWO] = ['0', '2'].map(N);
const ROOT = fileURLToPath(new URL('..', import.meta.url));
const DIST = join(ROOT, 'dist');
const PACKAGE = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8'));
const VERSION = String(PACKAGE.version);
const EXPECTED_TAG = 'v' + VERSION;

function fail(message) {
  process.stderr.write('release error: ' + message + '\n');
  process.exit(ZERO + N('1'));
}

function capture(name, args) {
  const result = spawnSync(name, args, { cwd: ROOT, encoding: 'utf8' });
  if (result.error) fail(name + ' failed to start: ' + result.error.message);
  if (result.status !== ZERO) {
    fail(name + ' ' + args.join(' ') + ' failed: ' + String(result.stderr || '').trim());
  }
  return String(result.stdout || '').trim();
}

const dirty = capture('git', ['status', '--porcelain']);
if (dirty) fail('working tree is not clean');
const commit = capture('git', ['rev-parse', 'HEAD']);
const tag = capture('git', ['describe', '--tags', '--exact-match']);
if (tag !== EXPECTED_TAG) {
  fail('expected exact tag ' + EXPECTED_TAG + ', found ' + tag);
}

mkdirSync(DIST, { recursive: true });
const archiveName = 'transcript-lake-' + VERSION + '.tgz';
const archivePath = join(DIST, archiveName);
const checksumPath = archivePath + '.sha256';
const provenancePath = join(DIST, 'provenance.json');
for (const output of [archivePath, checksumPath, provenancePath]) {
  if (existsSync(output)) fail('refusing to overwrite ' + output);
}

const packOutput = capture('npm', ['pack', '--json', '--pack-destination', DIST]);
let packed;
try {
  const rows = JSON.parse(packOutput);
  packed = rows.find((row) => row && typeof row.filename === 'string');
} catch (error) {
  fail('npm pack returned invalid JSON: ' + error.message);
}
if (!packed) fail('npm pack returned no archive filename');
const packedPath = join(DIST, basename(packed.filename));
renameSync(packedPath, archivePath);

const digest = createHash('sha256').update(readFileSync(archivePath)).digest('hex');
writeFileSync(checksumPath, digest + '  ' + archiveName + '\n');
const provenance = {
  product: PACKAGE.name,
  version: VERSION,
  sourceCommit: commit,
  tag,
  builtAt: new Date().toISOString(),
  platform: 'darwin',
  architecture: 'any',
  artifact: archiveName,
  sha256: digest,
};
writeFileSync(provenancePath, JSON.stringify(provenance, null, TWO) + '\n');
process.stdout.write(JSON.stringify(provenance, null, TWO) + '\n');
