// Streaming ingest driver for the transcript lake (frozen interface, see the
// build contract). Walks adapter roots, resumes files from newline-aligned
// cursor offsets, feeds raw lines to adapter parsers, masks every text and
// extra string through one masker instance, appends canonical events to
// runtime/date partitions, checkpoints cursors after every flushed batch.
// No digit characters outside quoted strings and comments.
import { createHash } from 'node:crypto';
import { appendFileSync, createReadStream, existsSync, mkdirSync, readdirSync, statSync } from 'node:fs';
import { ingestClosedHookSegments, mapCanonicalHookRecord } from './hook_segments/index.mjs';
import { homedir, hostname } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { openCursors, openWriterLease } from './cursors.mjs';
import { createMasker } from './redact.mjs';

const N = (s) => Number(s);
const [ZERO, ONE, TEN, TWELVE, NEG_ONE] = ['0', '1', '10', '12', '-1'].map(N);
const [TEXT_CAP, BATCH_EVENTS, EXTRA_DEPTH] = ['65536', '512', '4'].map(N);
const NEWLINE = '\n'.charCodeAt(ZERO);
const ADAPTER_NAMES = ['claude', 'codex', 'omp', 'droid', 'kimi'];
const HOOKS = 'hooks';
export const SUPPORTED_SOURCES = Object.freeze([...ADAPTER_NAMES, HOOKS]);
const DATE_SHAPE = new RegExp('^[' + '0-9' + ']{' + '4' + '}-[' + '0-9' + ']{' + '2' + '}-[' + '0-9' + ']{' + '2' + '}');

export function resolveDataDir(opts = {}) {
  return resolve(opts.dataDir || process.env.LAKE_DATA || join(homedir(), '.transcript-lake'));
}

const errText = (error) => String(error && error.message ? error.message : error);
const warn = (message) => process.stderr.write('ingest: ' + message + '\n');
const clip = (text) => (text.length > TEXT_CAP ? text.slice(ZERO, TEXT_CAP) : text);

// Masks every string inside extra, to a small depth bound (extra stays small).
// Non-string leaves pass through untouched; JSON serialization later renders
// them exactly as the adapter emitted them.
function maskDeep(value, masker, depth) {
  if (typeof value === 'string') return clip(masker.mask(value));
  if (Array.isArray(value)) {
    if (depth <= ZERO) return null;
    return value.map((item) => maskDeep(item, masker, depth - ONE));
  }
  if (value && typeof value === 'object') {
    if (depth <= ZERO) return null;
    const out = {};
    for (const [key, item] of Object.entries(value)) {
      out[key] = maskDeep(item, masker, depth - ONE);
    }
    return out;
  }
  return value;
}

function canonicalize(ev, runtime, machine, masker) {
  if (!ev || typeof ev !== 'object') return null;
  const ts = typeof ev.ts === 'string' ? ev.ts : null;
  // An unusable timestamp lands in a visible catch-all partition, not dropped.
  const date = ts && DATE_SHAPE.test(ts) ? ts.slice(ZERO, TEN) : 'unknown';
  const event = {
    ts, runtime, machine,
    session_id: ev.session_id ?? null,
    project: ev.project ?? null,
    event_type: typeof ev.event_type === 'string' ? ev.event_type : 'meta',
    text: clip(masker.mask(typeof ev.text === 'string' ? ev.text : String(ev.text ?? ''))),
    tool_name: ev.tool_name ?? null, model: ev.model ?? null,
    tokens_in: Number.isFinite(ev.tokens_in) ? ev.tokens_in : null,
    tokens_out: Number.isFinite(ev.tokens_out) ? ev.tokens_out : null,
    extra: maskDeep(ev.extra && typeof ev.extra === 'object' ? ev.extra : {}, masker, EXTRA_DEPTH),
  };
  return { event, date };
}

function writeBatch(events, runtime, partName, dataDir, machine, masker, tally, stemHash) {
  const rows = [];
  for (const ev of events) {
    const canon = canonicalize(ev, runtime, machine, masker);
    if (!canon) continue;
    // Oko keys some runtimes by filename. Persist only its one-way digest;
    // signals.sql hashes the Oko key too, so no token-like stem bypasses masking.
    canon.event.extra.source_stem_hash = stemHash;
    const dir = join(dataDir, 'events', 'runtime=' + runtime, 'date=' + canon.date);
    rows.push({ dir, line: JSON.stringify(canon.event) });
    tally.events += ONE;
  }
  for (const dir of new Set(rows.map((row) => row.dir))) {
    mkdirSync(dir, { recursive: true });
    const lines = rows.filter((row) => row.dir === dir).map((row) => row.line);
    appendFileSync(join(dir, partName), lines.join('\n') + '\n');
  }
}

// Streams one source file from its resume offset, byte-accurate so cursor
// checkpoints always land on line boundaries even with multibyte text.
async function ingestFile(job) {
  const { adapter, entry, st, cursors, masker, dataDir, machine, tally, full } = job;
  const cur = full ? null : cursors.get(entry.file);
  if (cur && st.size < cur.size) {
    throw new Error(
      'source shrank after its last checkpoint; select an empty LAKE_DATA and run --full'
    );
  }
  if (
    cur
    && st.size === cur.size
    && cur.mtimeMs !== st.mtimeMs
    && cur.offset >= st.size
  ) {
    throw new Error(
      'source changed without an append; select an empty LAKE_DATA and run --full'
    );
  }
  let offset = ZERO;
  if (cur) offset = Math.min(cur.offset, st.size);
  const parser = adapter.createParser({ file: entry.file, sessionId: entry.sessionId, project: entry.project, machine });
  const digest = createHash('sha' + '256').update(entry.file).digest('hex');
  const partName = 'part-' + digest.slice(ZERO, TWELVE) + '.ndjson';
  const stemHash = createHash('sha' + '256').update(basename(entry.file).replace(new RegExp('\\.[^.]+$'), '')).digest('hex');
  let batch = [];
  let consumed = offset;

  const checkpoint = () => {
    if (batch.length) {
      writeBatch(batch, adapter.runtime, partName, dataDir, machine, masker, tally, stemHash);
      batch = [];
    }
    cursors.set(entry.file, { mtimeMs: st.mtimeMs, size: st.size, offset: consumed });
    cursors.flush();
  };

  // Parsers tolerate malformed lines themselves; a throw is an adapter bug,
  // reported on stderr but not fatal to the rest of the file.
  const feed = (line) => {
    let events = [];
    try {
      events = parser.onLine(line);
    } catch (error) {
      warn('parser threw on ' + entry.file + ': ' + String(error));
    }
    if (Array.isArray(events) && events.length) batch.push(...events);
  };

  await new Promise((resolve, rejectFn) => {
    const stream = createReadStream(entry.file, { start: offset });
    let pending = null;
    stream.on('data', (chunk) => {
      pending = pending ? Buffer.concat([pending, chunk]) : chunk;
      let from = ZERO;
      for (;;) {
        const at = pending.indexOf(NEWLINE, from);
        if (at === NEG_ONE) break;
        let line = pending.subarray(from, at).toString('utf8');
        if (line.endsWith('\r')) line = line.slice(ZERO, NEG_ONE);
        consumed += at - from + ONE;
        from = at + ONE;
        feed(line);
        if (batch.length >= BATCH_EVENTS) checkpoint();
      }
      if (from) pending = pending.subarray(from);
    });
    stream.on('end', () => {
      let tail = [];
      try {
        tail = parser.end();
      } catch (error) {
        warn('parser end() threw on ' + entry.file + ': ' + String(error));
      }
      if (Array.isArray(tail) && tail.length) batch.push(...tail);
      checkpoint();
      resolve();
    });
    stream.on('error', rejectFn);
  });
}

// Inline pseudo-adapter over the adaptive hook decision log. Record shape,
// from the hooks-rotator telemetry writer: { ts (epoch millis), event, id,
// decision, ms, code, tool, timedOut, infra, reason }. Downstream SQL relies
// on extra.decision / extra.event / extra.infra passing through unchanged.
function hooksAdapter() {
  const pick = ['event', 'decision', 'tool', 'code', 'ms', 'timedOut', 'infra'];
  return {
    runtime: HOOKS,
    roots(home) {
      const dir = join(home, '.hooks-adaptive');
      return existsSync(dir) ? [dir] : [];
    },
    listSessions(root) {
      const out = [];
      for (const name of ['telemetry.prev.jsonl', 'telemetry.jsonl']) {
        const file = join(root, name);
        if (existsSync(file)) out.push({ file, sessionId: null, project: null });
      }
      return out;
    },
    createParser(ctx) {
      // The frozen parser interface maps a malformed line to zero events; the
      // drop is still reported on stderr so it is never invisible.
      return {
        onLine(raw) {
          const line = raw.trim();
          if (!line) return [];
          let rec = null;
          try {
            rec = JSON.parse(line);
          } catch (error) {
            warn(ctx.file + ': dropped malformed telemetry line: ' + String(error));
          }
          if (!rec || typeof rec !== 'object') return [];
          const ts = typeof rec.ts === 'number' && Number.isFinite(rec.ts)
            ? new Date(rec.ts).toISOString()
            : typeof rec.ts === 'string' ? rec.ts : null;
          if (!ts) return [];
          const extra = {};
          for (const key of pick) {
            if (rec[key] !== undefined && rec[key] !== null) extra[key] = rec[key];
          }
          return [{
            ts,
            event_type: 'hook_decision',
            session_id: rec.session_id ?? rec.sessionId ?? rec.session ?? null,
            project: rec.project ?? rec.cwd ?? null,
            text: typeof rec.reason === 'string' ? rec.reason : '',
            tool_name: typeof rec.id === 'string' ? rec.id : null,
            model: null, tokens_in: null, tokens_out: null,
            extra,
          }];
        },
        end() { return []; },
      };
    },
  };
}

function sumCounts(counts) {
  let total = ZERO;
  for (const value of Object.values(counts)) total += value;
  return total;
}

async function ingestLocked(opts = {}) {
  const started = Date.now();
  const dataDir = resolveDataDir(opts);
  const machine = hostname();
  const known = SUPPORTED_SOURCES;
  const requested = opts.source ? String(opts.source) : null;
  if (requested && !known.includes(requested)) {
    throw new Error('unknown source "' + requested + '" (expected one of: ' + known.join(', ') + ')');
  }
  const selected = requested ? [requested] : known;
  const cursors = openCursors(dataDir);
  const masker = createMasker();
  const perRuntime = {};
  for (const name of selected) {
    const tally = { files: ZERO, events: ZERO, maskedHits: ZERO, skipped: ZERO, failures: ZERO };
    perRuntime[name] = tally;
    const before = sumCounts(masker.counts());
    let adapter = null;
    if (name === HOOKS) {
      const readyDir = process.env.HOOKS_ADAPTIVE_SEGMENTS_READY
        || join(homedir(), '.hooks-adaptive', 'telemetry-segments', 'ready');
      if (existsSync(readyDir)) {
        const segments = ingestClosedHookSegments({
          readyDir,
          dataDir,
          cursors,
          warn,
          mapRecord: (record, context) => mapCanonicalHookRecord(
            record, canonicalize, machine, masker, context
          ),
        });
        tally.files += segments.files;
        tally.events += segments.events;
        tally.skipped += segments.skipped;
        tally.failures += segments.invalid;
      } else {
        adapter = hooksAdapter();
      }
    } else {
      try {
        adapter = await import('./adapters/' + name + '.mjs');
      } catch (error) {
        warn('adapter "' + name + '" unavailable, runtime skipped: ' + errText(error));
        tally.failures += ONE;
      }
    }
    if (!adapter) continue;
    for (const root of adapter.roots(homedir())) {
      let entries = [];
      try {
        entries = adapter.listSessions(root);
      } catch (error) {
        warn('listSessions failed under ' + root + ': ' + String(error));
        tally.failures += ONE;
      }
      for (const entry of entries) {
        let st = null;
        try {
          st = statSync(entry.file);
        } catch (error) {
          warn('stat failed for ' + entry.file + ': ' + String(error));
          tally.failures += ONE;
        }
        if (!st) continue;
        const cur = opts.full ? null : cursors.get(entry.file);
        if (cur && cur.mtimeMs === st.mtimeMs && cur.size === st.size && cur.offset >= st.size) {
          tally.skipped += ONE;
          continue;
        }
        try {
          await ingestFile({ adapter, entry, st, cursors, masker, dataDir, machine, tally, full: Boolean(opts.full) });
          tally.files += ONE;
        } catch (error) {
          warn(entry.file + ': ' + errText(error));
          tally.failures += ONE;
        }
      }
    }
    tally.maskedHits = sumCounts(masker.counts()) - before;
    cursors.flush();
  }
  const failures = Object.values(perRuntime).reduce((sum, tally) => sum + tally.failures, ZERO);
  return {
    perRuntime,
    maskCounts: masker.counts(),
    durationMs: Date.now() - started,
    partial: failures > ZERO,
    failures,
  };
}

export async function ingest(opts = {}) {
  const dataDir = resolveDataDir(opts);
  if (opts.full && existsSync(dataDir) && readdirSync(dataDir).length) {
    throw new Error(
      '--full requires an empty LAKE_DATA root so replay cannot duplicate or erase existing evidence'
    );
  }
  const lease = openWriterLease(dataDir);
  try {
    return await ingestLocked({ ...opts, dataDir });
  } finally {
    lease.close();
  }
}
