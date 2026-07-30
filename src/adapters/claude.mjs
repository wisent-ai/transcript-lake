// Adapter: Claude Code transcripts — ~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl
// Frozen interface: runtime, roots(homeDir), listSessions(root), createParser(ctx).
// Adapters emit UNMASKED text (the ingest driver masks) and never do IO in onLine.
// Contract: malformed lines are tolerated silently (empty array); the parser keeps an
// honest tally of them in its state. Verified against live files on this machine:
// record types user | assistant | system | summary carry messages; permission-mode,
// file-history-snapshot, attachment, ai-title, last-prompt, queue-operation, progress
// and mode records are bookkeeping noise and are dropped.
import { existsSync, readdirSync } from 'node:fs';
import { join, basename } from 'node:path';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const TEXT_CAP = N('65536');

export const runtime = 'claude';

export function roots(homeDir) {
  const dir = join(homeDir, '.claude', 'projects');
  if (existsSync(dir)) return [dir];
  return [];
}

// A root or project directory may vanish between scan and read; that is not an error
// state worth failing ingestion over. Anything else (permissions, IO) propagates.
function readDirents(dir) {
  try {
    return readdirSync(dir, { withFileTypes: true });
  } catch (error) {
    if (error && (error.code === 'ENOENT' || error.code === 'ENOTDIR')) return [];
    throw error;
  }
}

export function listSessions(root) {
  const sessions = [];
  for (const entry of readDirents(root)) {
    if (!entry.isDirectory()) continue;
    const projectDir = join(root, entry.name);
    const project = decodeProjectDir(entry.name);
    for (const child of readDirents(projectDir)) {
      if (!child.isFile() || !child.name.endsWith('.jsonl')) continue;
      sessions.push({
        file: join(projectDir, child.name),
        sessionId: basename(child.name, '.jsonl'),
        project,
      });
    }
  }
  return sessions;
}

// Directory names encode the cwd with '/' turned into '-'; the reverse mapping is best
// effort (dashes that belonged to the real path are indistinguishable). The parser
// prefers the per-record cwd field over this value whenever one is present.
function decodeProjectDir(name) {
  if (!name.startsWith('-')) return null;
  return name.replaceAll('-', '/');
}

function cap(value) {
  if (typeof value === 'string') return value.slice(ZERO, TEXT_CAP);
  return '';
}

function num(value) {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  return null;
}

function str(value) {
  if (typeof value === 'string' && value) return value;
  return null;
}

function noteMalformed(st, error) {
  st.malformed += ONE;
  st.lastError = String((error && error.message) || error);
}

export function createParser(ctx) {
  const st = {
    sessionId: str(ctx && ctx.sessionId),
    project: str(ctx && ctx.project),
    machine: str(ctx && ctx.machine),
    lastTs: null,
    malformed: ZERO,
    lastError: null,
  };
  const onLine = (raw) => {
    if (typeof raw !== 'string' || !raw.trim()) return [];
    let rec;
    try {
      rec = JSON.parse(raw);
    } catch (error) {
      noteMalformed(st, error);
      return [];
    }
    if (!rec || typeof rec !== 'object' || Array.isArray(rec)) return [];
    try {
      return mapRecord(rec, st);
    } catch (error) {
      noteMalformed(st, error);
      return [];
    }
  };
  return { onLine, end: () => [] };
}

function mapRecord(rec, st) {
  if (str(rec.sessionId)) st.sessionId = rec.sessionId;
  if (str(rec.cwd)) st.project = rec.cwd;
  if (str(rec.timestamp)) st.lastTs = rec.timestamp;
  const ts = str(rec.timestamp) || st.lastTs;
  if (!ts) return [];
  const base = (eventType, text, patch) => ({
    ts,
    runtime,
    machine: st.machine,
    session_id: st.sessionId,
    project: st.project,
    event_type: eventType,
    text: cap(text),
    tool_name: null,
    model: null,
    tokens_in: null,
    tokens_out: null,
    extra: {},
    ...patch,
  });
  if (rec.type === 'user') return mapUser(rec, base);
  if (rec.type === 'assistant') return mapAssistant(rec, base);
  if (rec.type === 'system') {
    const text = str(rec.content) || str(rec.stopReason) || '';
    const extra = { kind: 'system' };
    if (str(rec.subtype)) extra.subtype = rec.subtype;
    return [base('meta', text, { extra })];
  }
  if (rec.type === 'summary' && str(rec.summary)) {
    return [base('meta', rec.summary, { extra: { kind: 'summary' } })];
  }
  return [];
}

function mapUser(rec, base) {
  const msg = rec.message;
  if (!msg || typeof msg !== 'object' || msg.role !== 'user') return [];
  // isMeta marks injected content (hook feedback, command wrappers), not a human turn.
  let eventType = 'user';
  const flag = {};
  if (rec.isSidechain) flag.sidechain = true;
  if (rec.isMeta) {
    eventType = 'meta';
    flag.kind = 'injected';
  }
  const events = [];
  if (typeof msg.content === 'string') {
    if (msg.content.trim()) events.push(base(eventType, msg.content, { extra: { ...flag } }));
    return events;
  }
  if (!Array.isArray(msg.content)) return [];
  const textParts = [];
  for (const block of msg.content) {
    if (!block || typeof block !== 'object') continue;
    if (block.type === 'text' && typeof block.text === 'string') textParts.push(block.text);
    else if (block.type === 'image') textParts.push('[image]');
    else if (block.type === 'tool_result') events.push(toolResultEvent(block, base, flag));
  }
  const text = textParts.join('\n').trim();
  if (text) events.push(base(eventType, text, { extra: { ...flag } }));
  return events;
}

function toolResultEvent(block, base, flag) {
  let text = '';
  if (typeof block.content === 'string') text = block.content;
  else if (Array.isArray(block.content)) {
    const parts = [];
    for (const item of block.content) {
      if (item && item.type === 'text' && typeof item.text === 'string') parts.push(item.text);
      else if (item && item.type === 'image') parts.push('[image]');
    }
    text = parts.join('\n');
  }
  const extra = { ...flag };
  if (str(block.tool_use_id)) extra.tool_use_id = block.tool_use_id;
  if (block.is_error === true) extra.is_error = true;
  const ref = persistedOutputPath(text);
  if (ref) extra.result_file = ref;
  return base('tool_result', text, { extra });
}

// Large tool results are persisted beside the transcript and referenced inline as
// "<persisted-output>\nOutput too large (…). Full output saved to: /abs/path.txt\n…".
// We record the reference path only and never follow it.
function persistedOutputPath(text) {
  if (!text.includes('<persisted-output>')) return null;
  const marker = 'saved to: ';
  const at = text.indexOf(marker);
  if (at < ZERO) return null;
  const start = at + marker.length;
  let stop = text.indexOf('\n', start);
  if (stop < ZERO) stop = text.length;
  return text.slice(start, stop).trim() || null;
}

function mapAssistant(rec, base) {
  const msg = rec.message;
  if (!msg || typeof msg !== 'object') return [];
  const model = str(msg.model);
  let usage = null;
  if (msg.usage && typeof msg.usage === 'object') usage = msg.usage;
  const flag = {};
  if (rec.isSidechain) flag.sidechain = true;
  let blocks = [];
  if (Array.isArray(msg.content)) blocks = msg.content;
  else if (typeof msg.content === 'string') blocks = [{ type: 'text', text: msg.content }];
  const events = [];
  for (const block of blocks) {
    if (!block || typeof block !== 'object') continue;
    if (block.type === 'text') {
      if (typeof block.text === 'string' && block.text.trim()) {
        events.push(base('assistant', block.text, { model, extra: { ...flag } }));
      }
    } else if (block.type === 'thinking') {
      // Encrypted-only thinking blocks carry an empty string plus a signature; skip those.
      if (typeof block.thinking === 'string' && block.thinking.trim()) {
        events.push(base('thinking', block.thinking, { model, extra: { ...flag } }));
      }
    } else if (block.type === 'tool_use') {
      // block.input came out of JSON.parse, so stringify cannot throw here.
      let args = '';
      if (block.input !== undefined) args = JSON.stringify(block.input);
      const extra = { ...flag };
      if (str(block.id)) extra.tool_use_id = block.id;
      events.push(base('tool_call', args, { tool_name: str(block.name), model, extra }));
    }
  }
  attachUsage(events, usage, model, base);
  return events;
}

// Usage is reported once per assistant record; attach it to the first emitted event so
// downstream aggregation never double counts. Records whose only content is encrypted
// thinking still surface their token spend through a small meta event.
function attachUsage(events, usage, model, base) {
  if (!usage) return;
  const rawInputTokens = num(usage.input_tokens);
  const rawCacheCreationTokens = num(usage.cache_creation_input_tokens);
  const rawCacheReadTokens = num(usage.cache_read_input_tokens);
  const tokensOut = num(usage.output_tokens);
  if (
    rawInputTokens === null
    && rawCacheCreationTokens === null
    && rawCacheReadTokens === null
    && tokensOut === null
  ) return;
  const inputTokens = rawInputTokens || ZERO;
  const cacheCreationTokens = rawCacheCreationTokens || ZERO;
  const cacheReadTokens = rawCacheReadTokens || ZERO;
  const tokensIn = inputTokens + cacheCreationTokens + cacheReadTokens;
  const extra = {
    input_non_cached_tokens: inputTokens,
    cache_creation_tokens: cacheCreationTokens,
    cache_read_tokens: cacheReadTokens,
  };
  if (events.length) {
    const [first] = events;
    first.tokens_in = tokensIn;
    first.tokens_out = tokensOut;
    first.extra = { ...(first.extra || {}), ...extra };
    return;
  }
  events.push(base('meta', '', {
    model,
    tokens_in: tokensIn,
    tokens_out: tokensOut,
    extra: { kind: 'usage', ...extra },
  }));
}
