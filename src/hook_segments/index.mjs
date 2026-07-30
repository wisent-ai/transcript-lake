// Transactional ingest for immutable adaptive-hook telemetry segments.
// New hook outputs are deterministic per segment; acknowledgements publish last.
import {
  closeSync, existsSync, fsyncSync, linkSync, lstatSync, mkdirSync, openSync,
  readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync,
} from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { hostname } from 'node:os';
import { basename, dirname, join } from 'node:path';

const N = (text) => Number(text);
const ZERO = N('0');
const ONE = N('1');
const TWO = N('2');
const PROTOCOL = 'hooks-telemetry-segment-v1';
const ACK_PROTOCOL = 'hooks-telemetry-ack-v1';
const COMMIT_KIND = 'closed-segment';
const DATE = new RegExp('^[0-9]{4}-[0-9]{2}-[0-9]{2}$');
const digest = (value) => createHash('sha256').update(value).digest('hex');
const segmentName = (id) => 'segment-' + id + '.jsonl';
const ackName = (id) => 'segment-' + id + '.ack.json';

function parse(text) {
  try {
    const value = JSON.parse(text);
    return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
  } catch { return null; }
}
export function mapCanonicalHookRecord(record, canonicalize, machine, masker, context = {}) {
  const timestamp = Number(record && record.ts);
  const ts = Number.isFinite(timestamp) ? new Date(timestamp).toISOString() : null;
  return canonicalize({
    ts,
    session_id: record && record.session_id || null,
    project: record && record.project || null,
    event_type: 'hook_decision',
    text: record && (record.reason || record.text) || '',
    tool_name: record && record.id || null,
    extra: {
      source_type: record && record.type || null,
      hook_id: record && record.id || null,
      decision: record && record.decision || null,
      code: record ? record.code ?? null : null,
      timed_out: record && record.timedOut === true,
      infra: record && record.infra || null,
      source: record && record.source || null,
      episode_id: record && record.episode_id || null,
      adaptive_state_persisted: record && record.adaptiveStatePersisted === true,
      causal_episode_persisted: record && record.causalEpisodePersisted === true,
      segment_created_at: Number.isFinite(context.segmentCreatedAt) ? context.segmentCreatedAt : null,
      segment_id: context.segmentId || null,
      sequence: Number.isInteger(context.sequence) ? context.sequence : null,
      payload_ts: record ? record.payloadTs ?? null : null,
      payload: record && record.payload !== undefined ? record.payload : null,
      meta: record && record.meta !== undefined ? record.meta : null,
      label: record && record.label || null,
      repair_kind: record && record.kind || null,
      evidence: record && record.evidence || null,
    },
  }, 'hooks', machine, masker);
}


function syncDirectory(path) {
  const descriptor = openSync(path, 'r');
  try { fsyncSync(descriptor); } finally { closeSync(descriptor); }
}

function durableWrite(path, content) {
  const parent = dirname(path);
  mkdirSync(parent, { recursive: true });
  const temporary = join(parent, '.' + basename(path) + '.' + randomUUID() + '.tmp');
  const descriptor = openSync(temporary, 'wx');
  try {
    writeFileSync(descriptor, content);
    fsyncSync(descriptor);
  } finally { closeSync(descriptor); }
  renameSync(temporary, path);
  syncDirectory(parent);
}

export function validateHookSegment(path) {
  let before;
  try { before = lstatSync(path); } catch { return null; }
  if (!before.isFile() || before.isSymbolicLink()) return null;
  let bytes;
  try { bytes = readFileSync(path); } catch { return null; }
  if (!bytes.length || bytes.at(-ONE) !== '\n'.charCodeAt(ZERO)) return null;
  const source = bytes.toString('utf8');
  if (!Buffer.from(source).equals(bytes)) return null;
  const rows = source.slice(ZERO, -ONE).split('\n');
  if (rows.length < TWO) return null;
  const header = parse(rows.at(ZERO));
  const footer = parse(rows.at(-ONE));
  if (!header || header.kind !== 'segment_open' || header.protocol !== PROTOCOL) return null;
  if (typeof header.segmentId !== 'string' || !header.segmentId) return null;
  if (typeof header.createdAt !== 'number' || !Number.isFinite(header.createdAt)) return null;
  if (typeof header.producerId !== 'string' || !header.producerId) return null;
  if (typeof header.invocationId !== 'string' || !header.invocationId) return null;
  if (!header.source || header.source.producer !== 'hooks-rotator') return null;
  if (basename(path) !== segmentName(header.segmentId)) return null;
  const events = [];
  for (const row of rows.slice(ONE, -ONE)) {
    const frame = parse(row);
    if (!frame || frame.kind !== 'event' || frame.sequence !== events.length) return null;
    if (!frame.event || typeof frame.event !== 'object' || Array.isArray(frame.event)) return null;
    events.push(frame.event);
  }
  const payload = rows.slice(ZERO, -ONE).join('\n') + '\n';
  const payloadSha256 = digest(payload);
  if (!footer || footer.kind !== 'segment_close' || footer.protocol !== PROTOCOL) return null;
  if (footer.segmentId !== header.segmentId || footer.eventCount !== events.length) return null;
  if (footer.payloadSha256 !== payloadSha256) return null;
  let after;
  try { after = lstatSync(path); } catch { return null; }
  if (!after.isFile() || after.size !== before.size || after.ino !== before.ino) return null;
  return {
    segmentId: header.segmentId,
    createdAt: header.createdAt,
    sourceSha256: digest(bytes),
    sourceSize: bytes.length,
    payloadSha256,
    eventCount: events.length,
    events,
  };
}

function outputPath(dataDir, date, segmentId) {
  return join(dataDir, 'events', 'runtime=hooks', 'date=' + date, 'segment-' + segmentId + '.ndjson');
}

function publishOutput(path, content) {
  const sha256 = digest(content);
  if (existsSync(path)) {
    if (digest(readFileSync(path)) !== sha256) throw new Error('hook segment output conflict: ' + path);
    return { path, sha256 };
  }
  durableWrite(path, content);
  return { path, sha256 };
}

function outputsValid(outputs) {
  return Array.isArray(outputs) && outputs.length > ZERO && outputs.every((item) => {
    if (!item || !item.path || !item.sha256 || !existsSync(item.path)) return false;
    try { return digest(readFileSync(item.path)) === item.sha256; } catch { return false; }
  });
}

function publishAck(readyDir, segment, commit) {
  const ackedDir = join(dirname(readyDir), 'acked');
  const path = join(ackedDir, ackName(segment.segmentId));
  const ack = {
    protocol: ACK_PROTOCOL,
    segmentId: segment.segmentId,
    sourceSha256: segment.sourceSha256,
    sourceSize: segment.sourceSize,
    eventCount: segment.eventCount,
    payloadSha256: segment.payloadSha256,
    lakeCommitId: commit.commitId,
    outputs: commit.outputs,
  };
  const content = JSON.stringify(ack, null, TWO) + '\n';
  if (existsSync(path)) {
    const prior = parse(readFileSync(path, 'utf8'));
    if (!prior || prior.sourceSha256 !== ack.sourceSha256 || prior.lakeCommitId !== ack.lakeCommitId) {
      throw new Error('hook segment acknowledgement conflict: ' + path);
    }
    return;
  }
  durableWrite(path, content);
}
function processIdentity(pid) {
  const result = spawnSync('/bin/ps', ['-o', 'lstart=', '-p', String(pid)], { encoding: 'utf8' });
  if (result.error || result.status !== ZERO) return null;
  return result.stdout.trim() || null;
}

function ownerAlive(owner, warn, segmentId) {
  if (!owner || !Number.isInteger(owner.pid)) return false;
  if (owner.host && owner.host !== hostname()) return true;
  const currentIdentity = processIdentity(owner.pid);
  if (owner.started && currentIdentity) return owner.started === currentIdentity;
  try {
    process.kill(owner.pid, ZERO);
    warn('hook segment claim identity unavailable; retaining live pid claim: ' + segmentId);
    return true;
  } catch (error) { return error && error.code === 'EPERM'; }
}

function acquireClaim(root, segmentId, warn) {
  mkdirSync(root, { recursive: true });
  const path = join(root, segmentId + '.claim');
  for (const retry of [false, true]) {
    const owner = { host: hostname(), pid: process.pid, started: processIdentity(process.pid), nonce: randomUUID() };
    const temporary = join(root, '.' + segmentId + '.' + owner.nonce + '.claim');
    const descriptor = openSync(temporary, 'wx');
    try {
      writeFileSync(descriptor, JSON.stringify(owner) + '\n');
      fsyncSync(descriptor);
    } finally { closeSync(descriptor); }
    try {
      linkSync(temporary, path);
      unlinkSync(temporary);
      syncDirectory(root);
      return { path, owner };
    } catch (error) {
      try { unlinkSync(temporary); } catch {}
      if (!error || error.code !== 'EEXIST') throw error;
      let incumbent = null;
      try { incumbent = parse(readFileSync(path, 'utf8')); } catch {}
      if (ownerAlive(incumbent, warn, segmentId)) {
        warn('hook segment already claimed: ' + segmentId);
        return null;
      }
      try { unlinkSync(path); syncDirectory(root); }
      catch { return null; }
      if (retry) return null;
    }
  }
  return null;
}

function releaseClaim(claim) {
  if (!claim) return;
  try {
    const owner = parse(readFileSync(claim.path, 'utf8'));
    if (owner && owner.nonce === claim.owner.nonce) {
      unlinkSync(claim.path);
      syncDirectory(dirname(claim.path));
    }
  } catch {}
}


function processSegment(path, options) {
  const { dataDir, cursors, mapRecord, warn } = options;
  const segment = validateHookSegment(path);
  if (!segment) { warn('invalid closed hook segment: ' + path); return { invalid: true }; }
  const key = 'hooks:' + segment.segmentId;
  const existing = cursors.get(key);
  if (existing && existing.kind === COMMIT_KIND
      && existing.sourceSha256 === segment.sourceSha256 && outputsValid(existing.outputs)) {
    publishAck(dirname(path), segment, existing);
    return { skipped: true };
  }
  const claim = acquireClaim(join(dataDir, 'staging', 'hooks', 'claims'), segment.segmentId, warn);
  if (!claim) return { skipped: true };
  try {
    const groups = new Map();
    for (const [sequence, record] of segment.events.entries()) {
      const mapped = mapRecord(record, { segmentId: segment.segmentId, segmentCreatedAt: segment.createdAt, sequence });
      const validMapping = mapped && typeof mapped === 'object'
        && DATE.test(mapped.date)
        && mapped.event && typeof mapped.event === 'object' && !Array.isArray(mapped.event);
      if (!validMapping) {
        warn('closed hook segment produced an invalid canonical mapping: ' + segment.segmentId);
        continue;
      }
      const rows = groups.get(mapped.date) || [];
      rows.push(JSON.stringify(mapped.event));
      groups.set(mapped.date, rows);
    }
    const outputs = [];
    for (const [date, rows] of groups) {
      const content = rows.join('\n') + '\n';
      outputs.push(publishOutput(outputPath(dataDir, date, segment.segmentId), content));
    }
    if (!outputs.length) throw new Error('closed hook segment produced no canonical events');
    const commit = {
      kind: COMMIT_KIND,
      protocol: PROTOCOL,
      state: 'committed',
      segmentId: segment.segmentId,
      sourceSha256: segment.sourceSha256,
      sourceSize: segment.sourceSize,
      eventCount: segment.eventCount,
      payloadSha256: segment.payloadSha256,
      commitId: randomUUID(),
      outputs,
    };
    cursors.set(key, commit);
    cursors.flush();
    publishAck(dirname(path), segment, commit);
    return { files: ONE, events: segment.events.length };
  } finally {
    releaseClaim(claim);
  }
}

export function ingestClosedHookSegments(options) {
  const readyDir = options.readyDir;
  let names = [];
  try { names = readdirSync(readyDir).filter((name) => name.endsWith('.jsonl')).sort(); }
  catch { return { files: ZERO, events: ZERO, invalid: ZERO, skipped: ZERO }; }
  const tally = { files: ZERO, events: ZERO, invalid: ZERO, skipped: ZERO };
  for (const name of names) {
    const result = processSegment(join(readyDir, name), options);
    for (const key of Object.keys(tally)) tally[key] += Number(result[key] || ZERO);
  }
  return tally;
}
