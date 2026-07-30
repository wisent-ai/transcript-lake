// Adapter: Codex CLI rollouts — ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
// Frozen interface: runtime, roots(homeDir), listSessions(root), createParser(ctx).
// Adapters emit UNMASKED text (the ingest driver masks) and never do IO in onLine;
// malformed lines are tolerated silently (empty array) with an honest tally in state.
// Envelope per line: { timestamp, type, payload }. Verified on live files, old and
// current CLI versions: content is duplicated between the response_item stream and
// event_msg (user_message / agent_message / agent_reasoning). To avoid double counting
// we take user turns from event_msg/user_message (response_item role=user also carries
// injected environment context) and everything else from response_item records.
import { existsSync, readdirSync } from 'node:fs';
import { join, basename } from 'node:path';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const TEXT_CAP = N('65536');

export const runtime = 'codex';

export function roots(homeDir) {
  const dir = join(homeDir, '.codex', 'sessions');
  if (existsSync(dir)) return [dir];
  return [];
}

// A directory may vanish between scan and read; that is not an error state worth
// failing ingestion over. Anything else (permissions, IO) propagates.
function readDirents(dir) {
  try {
    return readdirSync(dir, { withFileTypes: true });
  } catch (error) {
    if (error && (error.code === 'ENOENT' || error.code === 'ENOTDIR')) return [];
    throw error;
  }
}

function subdirs(dir) {
  const out = [];
  for (const entry of readDirents(dir)) {
    if (entry.isDirectory()) out.push(join(dir, entry.name));
  }
  return out;
}

// Filenames look like rollout-<ISO-stamp>-<uuid>.jsonl; the uuid is the session id.
// Built via strings so the source stays free of bare digit characters.
const HEX = '[0-9a-f]';
function rep(count) {
  return '{' + count + '}';
}
const UUID_RE = new RegExp(
  HEX + rep('8') + '(?:-' + HEX + rep('4') + ')' + rep('3') + '-' + HEX + rep('12'),
  'i',
);

function sessionIdFromName(name) {
  const stem = basename(name, '.jsonl');
  const match = stem.match(UUID_RE);
  if (match) {
    const [id] = match;
    return id;
  }
  return stem;
}

export function listSessions(root) {
  const sessions = [];
  for (const year of subdirs(root)) {
    for (const month of subdirs(year)) {
      for (const day of subdirs(month)) {
        for (const entry of readDirents(day)) {
          if (!entry.isFile()) continue;
          if (!entry.name.startsWith('rollout-') || !entry.name.endsWith('.jsonl')) continue;
          sessions.push({
            file: join(day, entry.name),
            sessionId: sessionIdFromName(entry.name),
            project: null,
          });
        }
      }
    }
  }
  return sessions;
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
    model: null,
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
  if (str(rec.timestamp)) st.lastTs = rec.timestamp;
  const ts = str(rec.timestamp) || st.lastTs;
  const payload = rec.payload;
  if (!ts || !payload || typeof payload !== 'object') return [];
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
  if (rec.type === 'session_meta') return mapSessionMeta(payload, st, base);
  if (rec.type === 'turn_context') {
    // Pure configuration: refresh session state, emit nothing.
    if (str(payload.model)) st.model = payload.model;
    if (str(payload.cwd)) st.project = payload.cwd;
    return [];
  }
  if (rec.type === 'event_msg') return mapEventMsg(payload, st, base);
  if (rec.type === 'response_item') return mapResponseItem(payload, st, base);
  if (rec.type === 'compacted') {
    return [base('meta', str(payload.message) || '', { extra: { kind: 'compacted' } })];
  }
  return [];
}

function mapSessionMeta(payload, st, base) {
  if (str(payload.id)) st.sessionId = payload.id;
  if (str(payload.cwd)) st.project = payload.cwd;
  const extra = { kind: 'session_meta' };
  if (str(payload.originator)) extra.originator = payload.originator;
  if (str(payload.cli_version)) extra.cli_version = payload.cli_version;
  if (str(payload.source)) extra.source = payload.source;
  return [base('meta', '', { extra })];
}

function mapEventMsg(payload, st, base) {
  if (payload.type === 'user_message') {
    return [base('user', str(payload.message) || '', {})];
  }
  if (payload.type === 'token_count') {
    let info = null;
    if (payload.info && typeof payload.info === 'object') info = payload.info;
    if (!info) return [];
    const usage = info.last_token_usage && typeof info.last_token_usage === 'object'
      ? info.last_token_usage
      : null;
    if (!usage) return [];
    const totalInput = num(usage.input_tokens) || ZERO;
    const cachedInput = num(usage.cached_input_tokens) || ZERO;
    const output = num(usage.output_tokens) || ZERO;
    const reasoningOutput = num(usage.reasoning_output_tokens) || ZERO;
    if (totalInput + output + reasoningOutput === ZERO) return [];
    return [base('meta', '', {
      model: st.model,
      tokens_in: totalInput,
      tokens_out: output + reasoningOutput,
      extra: {
        kind: 'token_count',
        input_non_cached_tokens: Math.max(ZERO, totalInput - cachedInput),
        cache_creation_tokens: ZERO,
        cache_read_tokens: cachedInput,
      },
    })];
  }
  // agent_message / agent_reasoning duplicate response_item content; task_started,
  // task_complete and the rest are turn bookkeeping. All dropped.
  return [];
}

function mapResponseItem(payload, st, base) {
  const sub = str(payload.type);
  if (!sub) return [];
  if (sub === 'message') return mapMessageItem(payload, st, base);
  if (sub === 'reasoning') {
    // summary carries the readable text; encrypted_content is opaque and skipped.
    const parts = [];
    if (Array.isArray(payload.summary)) {
      for (const item of payload.summary) {
        if (item && typeof item.text === 'string' && item.text.trim()) parts.push(item.text);
      }
    }
    const text = parts.join('\n');
    if (!text) return [];
    return [base('thinking', text, { model: st.model })];
  }
  if (sub.endsWith('_call_output')) {
    const extra = { kind: sub };
    if (str(payload.call_id)) extra.call_id = payload.call_id;
    return [base('tool_result', outputText(payload.output, st), { extra })];
  }
  if (sub.endsWith('_call')) {
    let args = str(payload.arguments) || str(payload.input) || '';
    if (!args && payload.action !== undefined) args = JSON.stringify(payload.action);
    const extra = { kind: sub };
    if (str(payload.call_id)) extra.call_id = payload.call_id;
    const toolName = str(payload.name) || sub;
    return [base('tool_call', args, { tool_name: toolName, model: st.model, extra })];
  }
  return [];
}

function mapMessageItem(payload, st, base) {
  const role = str(payload.role);
  if (!role) return [];
  // Role user duplicates event_msg/user_message and additionally carries injected
  // environment/permission context — dropped here to keep user turns clean.
  if (role === 'user') return [];
  const parts = [];
  if (Array.isArray(payload.content)) {
    for (const block of payload.content) {
      if (!block || typeof block !== 'object') continue;
      if (typeof block.text === 'string' && block.text.trim()) parts.push(block.text);
    }
  }
  const text = parts.join('\n');
  if (role === 'assistant') {
    if (!text) return [];
    return [base('assistant', text, { model: st.model })];
  }
  // developer / system prompts: record presence only, never the text.
  return [base('meta', '', { extra: { kind: 'system_prompt', role } })];
}

// function_call_output.output is usually a plain string; some CLI versions wrap it as
// a JSON string or object { output | content, metadata }. Unwrap the readable part.
function outputText(value, st) {
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (trimmed.startsWith('{') && trimmed.includes('"output"')) {
      let parsed;
      try {
        parsed = JSON.parse(trimmed);
      } catch (error) {
        // The wrapper is optional: a leading '{' does not guarantee JSON. The raw
        // string IS the tool output in that case; remember why unwrapping failed.
        st.lastError = String((error && error.message) || error);
        return value;
      }
      if (parsed && typeof parsed.output === 'string') return parsed.output;
    }
    return value;
  }
  if (value && typeof value === 'object') {
    if (typeof value.output === 'string') return value.output;
    if (typeof value.content === 'string') return value.content;
    return JSON.stringify(value);
  }
  return '';
}
