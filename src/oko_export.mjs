// Materialize every masked conversation runtime as canonical per-session JSONL
// under LAKE_DATA/exports/oko. Oko imports this stable view and no longer
// parses vendor transcript stores for its catalog, search, or statistics.
//
// Normal runs track each append-only partition by size and mtime, merge only
// new rows into affected sessions, deduplicate by a deterministic event UUID,
// and preserve unchanged file mtimes. A first run, explicit --full, partition
// truncation, or same-size rewrite rebuilds from all Lake partitions through
// bounded staging buffers. Session writes and cursor publication are atomic.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  appendFileSync,
  createReadStream,
  existsSync,
  closeSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  readSync,
  statSync,
  renameSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { openWriterLease } from './cursors.mjs';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const NEG_ONE = N('-1');
const FP_LEN = N('32');
const BUFFER_LIMIT = N('8388608');
const READ_CHUNK = N('65536');
const SECOND_MS = N('1000');
const MINUTE_MS = N('60000');
const MINUTES_PER_HOUR = N('60');
const HOURS_PER_DAY = N('24');
const CURSOR_WALK_DEPTH = N('4');
const NEWLINE = '\n'.charCodeAt(ZERO);
const CONVERSATION_EVENTS = new Set([
  'user',
  'assistant',
  'thinking',
  'tool_call',
  'tool_result',
  'meta',
]);

function lakeDataDir(options = {}) {
  return options.dataDir
    || process.env.LAKE_DATA
    || join(homedir(), '.transcript-lake');
}

function okoSupportDir() {
  return join(homedir(), 'Library', 'Application Support', 'Oko');
}

function readDirNames(dir) {
  try {
    return readdirSync(dir);
  } catch (error) {
    if (error && (error.code === 'ENOENT' || error.code === 'ENOTDIR')) return [];
    throw error;
  }
}

function newlineAlignedSize(path, size) {
  if (size === ZERO) return ZERO;
  const handle = openSync(path, 'r');
  try {
    const buffer = Buffer.alloc(Math.min(READ_CHUNK, size));
    let end = size;
    while (end > ZERO) {
      const start = Math.max(ZERO, end - buffer.length);
      const length = end - start;
      readSync(handle, buffer, ZERO, length, start);
      const at = buffer.lastIndexOf(NEWLINE, length + NEG_ONE);
      if (at !== NEG_ONE) return start + at + ONE;
      end = start;
    }
    return ZERO;
  } finally {
    closeSync(handle);
  }
}

// Oko imports this materialized per-session view. Lake remains the sole parser
// of vendor formats; Oko decodes the stable canonical rows written here.
function eventPartitionFiles(dataDir) {
  const files = [];
  const eventsRoot = join(dataDir, 'events');
  for (const runtimeName of readDirNames(eventsRoot)) {
    if (!runtimeName.startsWith('runtime=') || runtimeName === 'runtime=hooks') continue;
    const runtimeDir = join(eventsRoot, runtimeName);
    for (const dateName of readDirNames(runtimeDir)) {
      if (!dateName.startsWith('date=')) continue;
      const dateDir = join(runtimeDir, dateName);
      for (const partName of readDirNames(dateDir)) {
        if (!partName.startsWith('part-') || !partName.endsWith('.ndjson')) continue;
        const path = join(dateDir, partName);
        const stat = statSync(path);
        files.push({
          runtime: runtimeName.slice('runtime='.length),
          path,
          size: newlineAlignedSize(path, stat.size),
          physicalSize: stat.size,
          mtimeMs: stat.mtimeMs,
        });
      }
    }
  }
  return files.sort((a, b) => a.path.localeCompare(b.path));
}

function sessionKey(runtime, sessionId) {
  return runtime + '\n' + sessionId;
}

function hashText(text) {
  return createHash('sha256').update(text).digest('hex');
}

// Deterministic per-event id dedupes explicit full re-ingests and gives Oko
// stable tool-use identifiers without retaining a source filename.
function fingerprint(ev) {
  const hash = createHash('sha256');
  for (const value of [
    ev.runtime,
    ev.session_id,
    ev.ts,
    ev.event_type,
    ev.text,
    ev.tool_name,
    ev.model,
    JSON.stringify(ev.extra || {}),
  ]) {
    hash.update(String(value || ''));
    hash.update('\n');
  }
  return hash.digest('hex').slice(ZERO, FP_LEN);
}

function exportLine(ev, fp) {
  return JSON.stringify({
    lake_schema: 'oko-import-v1',
    uuid: fp,
    ts: ev.ts,
    runtime: ev.runtime,
    session_id: ev.session_id,
    project: typeof ev.project === 'string' ? ev.project : null,
    event_type: ev.event_type,
    text: typeof ev.text === 'string' ? ev.text : '',
    tool_name: typeof ev.tool_name === 'string' ? ev.tool_name : null,
    model: typeof ev.model === 'string' ? ev.model : null,
    tokens_in: Number.isFinite(ev.tokens_in) ? ev.tokens_in : null,
    tokens_out: Number.isFinite(ev.tokens_out) ? ev.tokens_out : null,
    extra: ev.extra && typeof ev.extra === 'object' ? ev.extra : {},
  });
}

function atomicWrite(file, content) {
  mkdirSync(dirname(file), { recursive: true });
  const temporary = file + '.tmp-' + process.pid;
  writeFileSync(temporary, content);
  renameSync(temporary, file);
}

function flushBuffers(buffers) {
  for (const [file, chunks] of buffers) {
    mkdirSync(dirname(file), { recursive: true });
    appendFileSync(file, chunks.join(''));
  }
  buffers.clear();
}

async function stageEvents(partitions, stagingRoot, tally) {
  const sessions = new Map();
  const buffers = new Map();
  let bufferedBytes = ZERO;
  for (const partition of partitions) {
    if (partition.size === ZERO) continue;
    const input = createInterface({
      input: createReadStream(partition.path, {
        encoding: 'utf8',
        end: partition.size + NEG_ONE,
      }),
    });
    for await (const line of input) {
      if (!line.trim()) continue;
      let ev;
      try {
        ev = JSON.parse(line);
      } catch (error) {
        tally.malformed += ONE;
        tally.lastError = String(error && error.message ? error.message : error);
        continue;
      }
      if (!ev || typeof ev !== 'object') continue;
      if (!CONVERSATION_EVENTS.has(ev.event_type)) continue;
      if (typeof ev.session_id !== 'string' || !ev.session_id) continue;
      if (typeof ev.ts !== 'string' || !ev.ts) continue;
      const runtime = typeof ev.runtime === 'string' && ev.runtime ? ev.runtime : partition.runtime;
      const key = sessionKey(runtime, ev.session_id);
      const sessionHash = hashText(key);
      const stagedFile = join(stagingRoot, runtime, sessionHash + '.ndjson');
      const fp = fingerprint({ ...ev, runtime });
      const chunk = exportLine({ ...ev, runtime }, fp) + '\n';
      const chunks = buffers.get(stagedFile) || [];
      chunks.push(chunk);
      buffers.set(stagedFile, chunks);
      bufferedBytes += Buffer.byteLength(chunk);
      if (!sessions.has(key)) {
        sessions.set(key, { runtime, sessionId: ev.session_id, sessionHash, stagedFile });
      }
      if (bufferedBytes >= BUFFER_LIMIT) {
        flushBuffers(buffers);
        bufferedBytes = ZERO;
      }
    }
  }
  flushBuffers(buffers);
  return sessions;
}

function materializeSession(entry, outputRoot) {
  const rows = [];
  const seen = new Set();
  for (const line of readFileSync(entry.stagedFile, 'utf8').split('\n')) {
    if (!line) continue;
    let row;
    try {
      row = JSON.parse(line);
    } catch {
      continue;
    }
    if (seen.has(row.uuid)) continue;
    seen.add(row.uuid);
    rows.push(row);
  }
  rows.sort((a, b) => {
    const timestampOrder = String(a.ts).localeCompare(String(b.ts));
    return timestampOrder || String(a.uuid).localeCompare(String(b.uuid));
  });
  const file = join(outputRoot, 'runtime=' + entry.runtime, entry.sessionHash + '.jsonl');
  const content = rows.map(JSON.stringify).join('\n') + '\n';
  const existing = existsSync(file) ? readFileSync(file, 'utf8') : null;
  if (existing === content) return { file, records: rows.length, changed: false };
  atomicWrite(file, content);
  return { file, records: rows.length, changed: true };
}

function pruneOutputs(outputRoot, expectedFiles) {
  let pruned = ZERO;
  for (const runtimeName of readDirNames(outputRoot)) {
    if (!runtimeName.startsWith('runtime=')) continue;
    const runtimeDir = join(outputRoot, runtimeName);
    for (const name of readDirNames(runtimeDir)) {
      const file = join(runtimeDir, name);
      if (!name.endsWith('.jsonl') || expectedFiles.has(file)) continue;
      unlinkSync(file);
      pruned += ONE;
    }
  }
  return pruned;
}

function readExportCursors(file) {
  if (!existsSync(file)) return null;
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8'));
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

function partitionSnapshot(partitions) {
  const snapshot = {};
  for (const partition of partitions) {
    snapshot[partition.path] = {
      size: partition.size,
      mtimeMs: partition.mtimeMs,
      physicalSize: partition.physicalSize,
    };
  }
  return snapshot;
}

async function incrementalSessions(partitions, cursors, tally) {
  const sessions = new Map();
  for (const partition of partitions) {
    const cursor = cursors[partition.path];
    if (cursor && (
      partition.size < cursor.size
      || (
        partition.size === cursor.size
        && partition.mtimeMs !== cursor.mtimeMs
        && (
          cursor.physicalSize === undefined
            ? partition.physicalSize === partition.size
            : partition.physicalSize <= cursor.physicalSize
        )
      )
    )) {
      return null;
    }
    if (cursor && partition.size === cursor.size) continue;
    if (partition.size === ZERO) continue;
    const input = createInterface({
      input: createReadStream(partition.path, {
        encoding: 'utf8',
        start: cursor ? cursor.size : ZERO,
        end: partition.size + NEG_ONE,
      }),
      crlfDelay: Infinity,
    });
    for await (const line of input) {
      if (!line.trim()) continue;
      let ev;
      try {
        ev = JSON.parse(line);
      } catch (error) {
        tally.malformed += ONE;
        tally.lastError = String(error && error.message ? error.message : error);
        continue;
      }
      if (!ev || typeof ev !== 'object') continue;
      if (!CONVERSATION_EVENTS.has(ev.event_type)) continue;
      if (typeof ev.session_id !== 'string' || !ev.session_id) continue;
      if (typeof ev.ts !== 'string' || !ev.ts) continue;
      const runtime = typeof ev.runtime === 'string' && ev.runtime ? ev.runtime : partition.runtime;
      const key = sessionKey(runtime, ev.session_id);
      let session = sessions.get(key);
      if (!session) {
        session = {
          runtime,
          sessionId: ev.session_id,
          sessionHash: hashText(key),
          rows: [],
        };
        sessions.set(key, session);
      }
      const fp = fingerprint({ ...ev, runtime });
      session.rows.push(JSON.parse(exportLine({ ...ev, runtime }, fp)));
    }
  }
  return sessions;
}

function mergeIncrementalSession(entry, outputRoot) {
  const file = join(outputRoot, 'runtime=' + entry.runtime, entry.sessionHash + '.jsonl');
  const rows = [];
  const existing = existsSync(file) ? readFileSync(file, 'utf8') : null;
  if (existing !== null) {
    for (const line of existing.split('\n')) {
      if (!line) continue;
      try {
        rows.push(JSON.parse(line));
      } catch {
        // A torn derived file is repaired from the valid rows plus the delta.
      }
    }
  }
  rows.push(...entry.rows);
  const seen = new Set();
  const unique = rows.filter((row) => {
    if (seen.has(row.uuid)) return false;
    seen.add(row.uuid);
    return true;
  });
  unique.sort((a, b) => {
    const timestampOrder = String(a.ts).localeCompare(String(b.ts));
    return timestampOrder || String(a.uuid).localeCompare(String(b.uuid));
  });
  const content = unique.map(JSON.stringify).join('\n') + '\n';
  if (existing === content) return { file, records: entry.rows.length, changed: false };
  atomicWrite(file, content);
  return { file, records: entry.rows.length, changed: true };
}
async function fullExport(partitions, outputRoot, stagingRoot, tally) {
  rmSync(stagingRoot, { recursive: true, force: true });
  mkdirSync(stagingRoot, { recursive: true });
  const sessions = await stageEvents(partitions, stagingRoot, tally);
  if (tally.malformed > ZERO) {
    rmSync(stagingRoot, { recursive: true, force: true });
    throw new Error(
      'full Oko export refused malformed Lake rows; authoritative partitions were not modified'
    );
  }
  const expectedFiles = new Set();
  let written = ZERO;
  let unchanged = ZERO;
  let records = ZERO;
  for (const entry of sessions.values()) {
    const result = materializeSession(entry, outputRoot);
    expectedFiles.add(result.file);
    records += result.records;
    if (result.changed) written += ONE;
    else unchanged += ONE;
  }
  const pruned = pruneOutputs(outputRoot, expectedFiles);
  rmSync(stagingRoot, { recursive: true, force: true });
  return { sessions: sessions.size, records, written, unchanged, pruned, mode: 'full' };
}

async function exportOkoLocked(opts) {
  const options = opts && typeof opts === 'object' ? opts : {};
  const startedAt = Date.now();
  const dataDir = lakeDataDir(options);
  const outputRoot = options.outputRoot || join(dataDir, 'exports', 'oko');
  const stagingRoot = join(dataDir, 'staging', 'oko-export');
  const cursorFile = join(outputRoot, 'export-cursors.json');
  const tally = { malformed: ZERO, lastError: null };
  const partitions = eventPartitionFiles(dataDir);
  const cursors = options.full ? null : readExportCursors(cursorFile);
  let result;
  if (!cursors) {
    result = await fullExport(partitions, outputRoot, stagingRoot, tally);
  } else {
    const sessions = await incrementalSessions(partitions, cursors, tally);
    if (sessions === null) {
      result = await fullExport(partitions, outputRoot, stagingRoot, tally);
    } else {
      if (tally.malformed > ZERO) {
        throw new Error(
          'incremental Oko export refused malformed Lake rows; export cursor was not advanced'
        );
      }
      let records = ZERO;
      let written = ZERO;
      let unchanged = ZERO;
      for (const entry of sessions.values()) {
        const merged = mergeIncrementalSession(entry, outputRoot);
        records += merged.records;
        if (merged.changed) written += ONE;
        else unchanged += ONE;
      }
      result = {
        sessions: sessions.size,
        records,
        written,
        unchanged,
        pruned: ZERO,
        mode: 'incremental',
      };
    }
  }
  atomicWrite(cursorFile, JSON.stringify(partitionSnapshot(partitions), null, ONE) + '\n');
  const summary = {
    outputRoot,
    ...result,
    malformed: tally.malformed,
    durationMs: Date.now() - startedAt,
  };
  if (tally.lastError) summary.lastError = tally.lastError;
  if (options.reindex) summary.reindex = runReindex();
  return summary;
}

export async function exportOko(opts) {
  const options = opts && typeof opts === 'object' ? opts : {};
  const dataDir = lakeDataDir(options);
  const lease = openWriterLease(dataDir);
  try {
    return await exportOkoLocked({ ...options, dataDir });
  } finally {
    lease.close();
  }
}

// A flagless reindex discovers the Lake export root and remains incremental:
// unchanged per-session files keep their mtimes, so Oko skips them.
function runReindex() {
  const command = process.env.OKO_CLI || 'oko-cli';
  const args = ['transcripts', 'reindex', '--json'];
  const run = spawnSync(command, args, { encoding: 'utf8' });
  if (run.error) {
    const printable = command + ' ' + args.join(' ');
    return { ran: false, command: printable, error: String(run.error.message) };
  }
  let output = '';
  if (typeof run.stdout === 'string') output = run.stdout.trim();
  return { ran: true, status: run.status, output };
}

function isoOrNa(ms) {
  if (ms === null) return 'n/a';
  return new Date(ms).toISOString();
}

function ageLabel(ms, nowMs) {
  if (ms === null) return 'n/a';
  const minutes = Math.round((nowMs - ms) / MINUTE_MS);
  if (minutes < MINUTES_PER_HOUR) return String(minutes) + 'm';
  const hours = Math.round(minutes / MINUTES_PER_HOUR);
  if (hours < HOURS_PER_DAY) return String(hours) + 'h';
  return String(Math.round(hours / HOURS_PER_DAY)) + 'd';
}

function walkCursorTimes(node, depth, acc) {
  if (!node || typeof node !== 'object' || depth > CURSOR_WALK_DEPTH) return;
  const ms = node.mtimeMs;
  if (typeof ms === 'number' && Number.isFinite(ms)) {
    acc.files += ONE;
    if (acc.max === null || ms > acc.max) acc.max = ms;
    return;
  }
  for (const value of Object.values(node)) walkCursorTimes(value, depth + ONE, acc);
}

// Read-only freshness comparison: Oko's index (sessions.mtime / last_activity are
// epoch seconds, see the TranscriptIndex+SQL.swift schema) versus the lake's cursor
// checkpoints (mtimeMs per source file). Queried via `sqlite3 -readonly` so a live
// Oko holding the write lock is never disturbed.
export function freshness() {
  const nowMs = Date.now();
  const dbPath = join(okoSupportDir(), 'transcript-index.sqlite');
  const oko = { db: dbPath, exists: existsSync(dbPath), sessions: null, maxMtimeMs: null, maxActivityMs: null, error: null };
  if (oko.exists) {
    const sql = 'SELECT MAX(mtime), MAX(COALESCE(last_activity, mtime)), COUNT(*) FROM sessions;';
    const run = spawnSync('sqlite3', ['-readonly', '-separator', '|', dbPath, sql], { encoding: 'utf8' });
    if (run.error) {
      oko.error = String(run.error.message);
    } else if (run.status !== ZERO) {
      oko.error = String(run.stderr).trim();
    } else {
      const [mtimeRaw, activityRaw, countRaw] = String(run.stdout).trim().split('|');
      const mtime = Number(mtimeRaw);
      const activity = Number(activityRaw);
      const count = Number(countRaw);
      if (Number.isFinite(mtime)) oko.maxMtimeMs = mtime * SECOND_MS;
      if (Number.isFinite(activity)) oko.maxActivityMs = activity * SECOND_MS;
      if (Number.isFinite(count)) oko.sessions = count;
    }
  }
  const cursorsPath = join(lakeDataDir(), 'cursors.json');
  const lake = { cursors: cursorsPath, exists: existsSync(cursorsPath), files: ZERO, maxMtimeMs: null, error: null };
  if (lake.exists) {
    try {
      const acc = { files: ZERO, max: null };
      walkCursorTimes(JSON.parse(readFileSync(cursorsPath, 'utf8')), ZERO, acc);
      lake.files = acc.files;
      lake.maxMtimeMs = acc.max;
    } catch (error) {
      // Corrupt cursors mean "no recency signal", which the report states openly.
      lake.error = String(error && error.message ? error.message : error);
    }
  }
  let fresher = 'unknown';
  if (oko.maxMtimeMs !== null && lake.maxMtimeMs !== null) {
    if (lake.maxMtimeMs > oko.maxMtimeMs) fresher = 'lake';
    else if (oko.maxMtimeMs > lake.maxMtimeMs) fresher = 'oko';
    else fresher = 'equal';
  } else if (lake.maxMtimeMs !== null) fresher = 'lake';
  else if (oko.maxMtimeMs !== null) fresher = 'oko';
  const pad = (s) => String(s).padEnd(PAD);
  console.log(pad('source') + pad('latest') + 'age');
  console.log(pad('oko-index (mtime)') + pad(isoOrNa(oko.maxMtimeMs)) + ageLabel(oko.maxMtimeMs, nowMs));
  console.log(pad('oko-index (activity)') + pad(isoOrNa(oko.maxActivityMs)) + ageLabel(oko.maxActivityMs, nowMs));
  console.log(pad('lake cursors') + pad(isoOrNa(lake.maxMtimeMs)) + ageLabel(lake.maxMtimeMs, nowMs));
  console.log('fresher: ' + fresher);
  return { now: new Date(nowMs).toISOString(), oko, lake, fresher };
}
