// Adapter: Kimi Code CLI wire transcripts —
//   ~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl
// Frozen interface: runtime, roots(homeDir), listSessions(root), createParser(ctx).
// Adapters emit UNMASKED text (the ingest driver masks) and never do IO in onLine.
// Shapes verified against the largest live wire files on this machine:
//   metadata {protocol_version, app_version, created_at(epoch ms)}
//   config.update {profileName, systemPrompt}          -> meta only, prompt dropped
//   context.append_message {message:{role, content:[{type:'text', text}],
//     origin:{kind}}, time} — origin.kind 'user' is the human turn; 'injection'
//     and 'background_task' are synthetic context and become meta events.
//   turn.prompt / turn.steer duplicate append_message one-to-one -> skipped.
//   context.append_loop_event {event:{type}, time} carries the assistant loop:
//     content.part {part:{type:'text'|'think'}}         (each part arrives complete)
//     tool.call    {toolCallId, name, args, description}
//     tool.result  {toolCallId, result:{output, isError?}}
//     step.begin / step.end                             -> dropped (usage.record wins)
//   usage.record {model, usage:{input*, output}, usageScope:'turn'|'session'} —
//     'turn' records mirror step.end usage exactly (per-step deltas, summable);
//     'session' records are cumulative snapshots and only refresh the model.
//   context.apply_compaction {summary} and turn.cancel  -> small meta events.
// Wire records carry no session id or cwd; the session id is the session_* dirname
// and ~/.kimi-code/session_index.jsonl maps sessionId -> workDir (the only place the
// absolute project path exists, so listSessions performs that one small read).
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const TEXT_CAP = N('65536');

export const runtime = 'kimi';

export function roots(homeDir) {
  const dir = join(homeDir, '.kimi-code', 'sessions');
  if (existsSync(dir)) return [dir];
  return [];
}

// A root or session directory may vanish between scan and read; that is not an error
// state worth failing ingestion over. Anything else (permissions, IO) propagates.
function readDirents(dir) {
  try {
    return readdirSync(dir, { withFileTypes: true });
  } catch (error) {
    if (error && (error.code === 'ENOENT' || error.code === 'ENOTDIR')) return [];
    throw error;
  }
}

function readWorkDirIndex(root) {
  const map = new Map();
  let text;
  try {
    text = readFileSync(join(dirname(root), 'session_index.jsonl'), 'utf8');
  } catch {
    return map;
  }
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    try {
      const rec = JSON.parse(line);
      if (rec && typeof rec.sessionId === 'string' && typeof rec.workDir === 'string') {
        map.set(rec.sessionId, rec.workDir);
      }
    } catch {
      // A torn index line only costs a project attribution, never the session.
    }
  }
  return map;
}

export function listSessions(root) {
  const index = readWorkDirIndex(root);
  const sessions = [];
  for (const wd of readDirents(root)) {
    if (!wd.isDirectory() || !wd.name.startsWith('wd_')) continue;
    const wdDir = join(root, wd.name);
    for (const entry of readDirents(wdDir)) {
      if (!entry.isDirectory() || !entry.name.startsWith('session_')) continue;
      const file = join(wdDir, entry.name, 'agents', 'main', 'wire.jsonl');
      if (!existsSync(file)) continue;
      let project = null;
      if (index.has(entry.name)) project = index.get(entry.name);
      sessions.push({ file, sessionId: entry.name, project });
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
  if (error && error.message) st.lastError = String(error.message);
  else st.lastError = String(error);
}

// Wire times are epoch milliseconds; an out-of-range value must not kill the line.
function isoFrom(value) {
  const ms = num(value);
  if (ms === null) return null;
  try {
    return new Date(ms).toISOString();
  } catch {
    return null;
  }
}

export function createParser(ctx) {
  const st = {
    sessionId: str(ctx && ctx.sessionId),
    project: str(ctx && ctx.project),
    machine: str(ctx && ctx.machine),
    lastTs: null,
    lastModel: null,
    toolNames: new Map(),
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
  let ts = isoFrom(rec.time);
  if (ts === null) ts = isoFrom(rec.created_at);
  if (ts === null) ts = st.lastTs;
  if (!ts) return [];
  st.lastTs = ts;
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
  if (rec.type === 'context.append_loop_event') return mapLoopEvent(rec.event, base, st);
  if (rec.type === 'context.append_message') return mapMessage(rec.message, base);
  if (rec.type === 'usage.record') return mapUsage(rec, base, st);
  if (rec.type === 'metadata') {
    const extra = { kind: 'metadata' };
    if (str(rec.protocol_version)) extra.protocol = rec.protocol_version;
    if (str(rec.app_version)) extra.app = rec.app_version;
    return [base('meta', '', { extra })];
  }
  if (rec.type === 'config.update') {
    // The systemPrompt payload is deliberately dropped: meta at most, no text.
    const extra = { kind: 'config' };
    if (str(rec.profileName)) extra.profile = rec.profileName;
    return [base('meta', '', { extra })];
  }
  if (rec.type === 'context.apply_compaction' && str(rec.summary)) {
    return [base('meta', rec.summary, { extra: { kind: 'compaction' } })];
  }
  if (rec.type === 'turn.cancel') {
    return [base('meta', '', { extra: { kind: 'turn.cancel' } })];
  }
  // turn.prompt / turn.steer echo append_message; tools.*, permission.*, *_mode.*,
  // and compaction bookkeeping records carry no conversational content.
  return [];
}

function mapMessage(msg, base) {
  if (!msg || typeof msg !== 'object') return [];
  const role = str(msg.role);
  if (!role) return [];
  const parts = [];
  if (Array.isArray(msg.content)) {
    for (const block of msg.content) {
      if (block && block.type === 'text' && typeof block.text === 'string') parts.push(block.text);
    }
  } else if (typeof msg.content === 'string') parts.push(msg.content);
  const text = parts.join('\n').trim();
  if (!text) return [];
  let origin = null;
  if (msg.origin && typeof msg.origin === 'object') origin = str(msg.origin.kind);
  if (role === 'user' && origin && origin !== 'user') {
    // Injections and background-task notifications are synthetic context, not turns.
    return [base('meta', text, { extra: { kind: 'injected', origin } })];
  }
  if (role === 'user') return [base('user', text, {})];
  if (role === 'assistant') return [base('assistant', text, {})];
  return [];
}

function mapLoopEvent(ev, base, st) {
  if (!ev || typeof ev !== 'object') return [];
  if (ev.type === 'content.part') {
    const part = ev.part;
    if (!part || typeof part !== 'object') return [];
    const model = st.lastModel;
    if (part.type === 'text' && str(part.text)) return [base('assistant', part.text, { model })];
    if (part.type === 'think' && str(part.think)) return [base('thinking', part.think, { model })];
    return [];
  }
  if (ev.type === 'tool.call') {
    let callId = str(ev.toolCallId);
    if (callId === null) callId = str(ev.uuid);
    const name = str(ev.name);
    if (callId && name) st.toolNames.set(callId, name);
    // Mirror the claude adapter: the argument JSON is the searchable call text.
    let text = '';
    if (ev.args !== undefined) text = JSON.stringify(ev.args);
    if (!text) {
      const desc = str(ev.description);
      if (desc) text = desc;
    }
    const extra = {};
    if (callId) extra.tool_use_id = callId;
    return [base('tool_call', text, { tool_name: name, model: st.lastModel, extra })];
  }
  if (ev.type === 'tool.result') {
    let callId = str(ev.toolCallId);
    if (callId === null) callId = str(ev.parentUuid);
    const result = ev.result && typeof ev.result === 'object' ? ev.result : {};
    let text = '';
    if (typeof result.output === 'string') text = result.output;
    else if (typeof ev.result === 'string') text = ev.result;
    let toolName = null;
    if (callId && st.toolNames.has(callId)) toolName = st.toolNames.get(callId);
    const extra = {};
    if (callId) extra.tool_use_id = callId;
    if (result.isError) extra.is_error = true;
    return [base('tool_result', text, { tool_name: toolName, extra })];
  }
  // step.begin/step.end carry loop bookkeeping only; usage.record owns the tokens.
  return [];
}

// 'turn'-scoped usage records are per-step deltas (verified identical to step.end
// usage), so summing them downstream yields honest session totals. 'session'-scoped
// snapshots would double count and only refresh the current model name.
function mapUsage(rec, base, st) {
  if (str(rec.model)) st.lastModel = rec.model;
  if (rec.usageScope !== 'turn') return [];
  const usage = rec.usage && typeof rec.usage === 'object' ? rec.usage : {};
  const inputOther = num(usage.inputOther) || ZERO;
  const cacheRead = num(usage.inputCacheRead) || ZERO;
  const cacheCreation = num(usage.inputCacheCreation) || ZERO;
  const tokensIn = inputOther + cacheRead + cacheCreation;
  const tokensOut = num(usage.output) || ZERO;
  if (tokensIn + tokensOut === ZERO) return [];
  return [base('meta', '', {
    model: st.lastModel,
    tokens_in: tokensIn,
    tokens_out: tokensOut,
    extra: {
      kind: 'usage',
      input_non_cached_tokens: inputOther,
      cache_creation_tokens: cacheCreation,
      cache_read_tokens: cacheRead,
    },
  })];
}
