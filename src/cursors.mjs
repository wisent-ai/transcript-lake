// Cursor store for mutable byte streams and immutable closed segments. Legacy
// records retain their numeric coercion contract; tagged segment commits are
// stored as structured records and never projected onto byte-offset fields.
// Writes are durable: unique temp, file sync, rename, then parent sync.
// No digit characters outside quoted strings and comments.
import { spawnSync } from 'node:child_process';
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { hostname } from 'node:os';
import { join } from 'node:path';

const N = (s) => Number(s);
const ZERO = N('0');
const TWO = N('2');
const SEGMENT_KIND = 'closed-segment';

function coerce(rec) {
  const mtimeMs = Number(rec.mtimeMs);
  const size = Number(rec.size);
  const offset = Number(rec.offset);
  return {
    mtimeMs: Number.isFinite(mtimeMs) ? mtimeMs : ZERO,
    size: Number.isFinite(size) ? size : ZERO,
    offset: Number.isFinite(offset) ? offset : ZERO,
  };
}

// Contractual degradation: any unreadable or corrupt store restarts empty,
// with the cause reported on stderr so the operator can see it happened.
function readStore(filePath) {
  if (!existsSync(filePath)) return {};
  let raw;
  try {
    raw = readFileSync(filePath, 'utf8');
  } catch (error) {
    process.stderr.write('cursors: unreadable store, restarting empty: ' + String(error) + '\n');
    return {};
  }
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) return parsed;
    process.stderr.write('cursors: store is not a JSON object, restarting empty\n');
  } catch (error) {
    process.stderr.write('cursors: corrupt store, restarting empty: ' + String(error) + '\n');
  }
  return {};
}
function syncDirectory(path) {
  const fd = openSync(path, 'r');
  try { fsyncSync(fd); } finally { closeSync(fd); }
}

function processIdentity(pid) {
  const result = spawnSync('/bin/ps', ['-o', 'lstart=', '-p', String(pid)], { encoding: 'utf8' });
  if (result.error || result.status !== ZERO) return null;
  return result.stdout.trim() || null;
}

function ownerIsAlive(owner) {
  if (!owner || !Number.isInteger(owner.pid) || owner.pid <= ZERO) return false;
  if (owner.host !== hostname()) return true;
  const currentIdentity = processIdentity(owner.pid);
  if (owner.started && currentIdentity) return owner.started === currentIdentity;
  try {
    process.kill(owner.pid, ZERO);
    process.stderr.write('cursors: lock process identity unavailable; retaining live pid claim\n');
    return true;
  } catch (error) {
    return !error || error.code !== 'ESRCH';
  }
}

// Build and sync each claim privately, then publish it with one rename. This
// makes an absent or malformed published owner structurally abandoned rather
// than a live process caught between mkdir and writing its identity.
function acquireLock(dataDir, lockPath) {
  const token = randomUUID();
  const owner = { host: hostname(), pid: process.pid, started: processIdentity(process.pid), token };
  for (;;) {
    const prepared = lockPath + '.claim-' + process.pid + '-' + randomUUID();
    mkdirSync(prepared);
    let fd = null;
    try {
      fd = openSync(join(prepared, 'owner.json'), 'wx');
      writeFileSync(fd, JSON.stringify(owner));
      fsyncSync(fd);
      closeSync(fd);
      fd = null;
      syncDirectory(prepared);
      renameSync(prepared, lockPath);
      syncDirectory(dataDir);
      return token;
    } catch (error) {
      if (fd !== null) closeSync(fd);
      if (existsSync(prepared)) rmSync(prepared, { recursive: true, force: true });
      if (!error || (error.code !== 'EEXIST' && error.code !== 'ENOTEMPTY')) throw error;
    }

    let incumbent = null;
    try { incumbent = JSON.parse(readFileSync(join(lockPath, 'owner.json'), 'utf8')); } catch {}
    if (!ownerIsAlive(incumbent)) {
      try { rmSync(lockPath, { recursive: true }); } catch (error) {
        if (!error || error.code !== 'ENOENT') throw error;
      }
    }
  }
}

function releaseLock(dataDir, lockPath, token) {
  let owner = null;
  try { owner = JSON.parse(readFileSync(join(lockPath, 'owner.json'), 'utf8')); } catch {}
  if (!owner || owner.token !== token) return;
  rmSync(lockPath, { recursive: true });
  syncDirectory(dataDir);
}

export function openCursors(dataDir) {
  const filePath = join(dataDir, 'cursors.json');
  const lockPath = join(dataDir, 'cursors.lock');
  let store = readStore(filePath);
  const pending = new Map();
  let dirty = false;

  function get(file) {
    const rec = store[file];
    if (!rec || typeof rec !== 'object') return null;
    if (rec.kind === SEGMENT_KIND) return structuredClone(rec);
    return coerce(rec);
  }

  function set(file, rec) {
    if (!rec || typeof rec !== 'object') return;
    const value = rec.kind === SEGMENT_KIND ? structuredClone(rec) : coerce(rec);
    store[file] = value;
    pending.set(file, value);
    dirty = true;
  }

  function flush() {
    if (!dirty) return;
    mkdirSync(dataDir, { recursive: true });
    const token = acquireLock(dataDir, lockPath);
    const tmpPath = filePath + '.tmp-' + process.pid + '-' + randomUUID();
    let fd = null;
    try {
      // Reload only after taking ownership: the lock protects this entire
      // read-modify-write transaction, not merely the final rename.
      const merged = readStore(filePath);
      for (const [file, value] of pending) merged[file] = value;
      fd = openSync(tmpPath, 'wx');
      writeFileSync(fd, JSON.stringify(merged, null, TWO));
      fsyncSync(fd);
      closeSync(fd);
      fd = null;
      renameSync(tmpPath, filePath);
      syncDirectory(dataDir);
      dirty = false;
      store = merged;
      pending.clear();
    } catch (error) {
      if (fd !== null) closeSync(fd);
      if (existsSync(tmpPath)) unlinkSync(tmpPath);
      throw error;
    } finally {
      releaseLock(dataDir, lockPath, token);
    }
  }

  return { get, set, flush };
}
