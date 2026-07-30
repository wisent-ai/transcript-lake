// Adapter for Oh My Pi (omp) agent session transcripts.
// Source layout: HOME/.omp/agent/sessions/<encoded-cwd>/<stamp>_<uuid>.jsonl;
// a sibling directory with the same stem holds non-transcript artifacts and
// is skipped. Typed lines verified on real files from this machine:
//   session (id, cwd, version), title / title_change (title, updatedAt),
//   model_change (model), thinking_level_change (thinkingLevel),
//   message ({ id, parentId, timestamp, message: { role, content } }) with
//     roles user | assistant | toolResult | developer and content blocks
//     text { text } | thinking { thinking } | toolCall { id, name, arguments };
//     toolResult messages carry toolName, toolCallId, isError, content blocks;
//     assistant messages carry model plus usage { input, output, ... },
//   custom_message (customType, content string), custom (customType, data),
//   compaction (summary, tokensBefore, firstKeptEntryId).
// Unknown record or block types become meta events tagged in extra.

import fs from 'node:fs';
import path from 'node:path';

export const runtime = 'omp';

const N = (s) => Number(s);
const ZERO = N('0');
const ONE = N('1');
const TEXT_CAP = N('65536');
const PENDING_CAP = N('64');
const JSONL_EXT = '.jsonl';

export function roots(homeDir) {
  const base = path.join(homeDir, '.omp', 'agent', 'sessions');
  let entries;
  try { entries = fs.readdirSync(base, { withFileTypes: true }); } catch { return []; }
  const dirs = [];
  for (const e of entries) if (e.isDirectory()) dirs.push(path.join(base, e.name));
  return dirs;
}

export function listSessions(root) {
  let entries;
  try { entries = fs.readdirSync(root, { withFileTypes: true }); } catch { return []; }
  const sessions = [];
  for (const e of entries) {
    if (!e.isFile() || !e.name.endsWith(JSONL_EXT)) continue;
    const stem = e.name.slice(ZERO, e.name.length - JSONL_EXT.length);
    const us = stem.indexOf('_');
    // Encoded directory names are home-relative and dash-mangled for omp, so
    // the parser recovers the real project from the session line cwd instead.
    sessions.push({
      file: path.join(root, e.name),
      sessionId: us < ZERO ? stem : stem.slice(us + ONE),
      project: null,
    });
  }
  return sessions;
}

function clip(value) {
  if (typeof value !== 'string') return '';
  return value.length > TEXT_CAP ? value.slice(ZERO, TEXT_CAP) : value;
}

function textOf(content) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  const parts = [];
  for (const b of content) {
    if (typeof b === 'string') parts.push(b);
    else if (b && typeof b === 'object' && typeof b.text === 'string') parts.push(b.text);
  }
  return parts.join('\n');
}

function prune(obj) {
  const out = {};
  for (const key of Object.keys(obj)) {
    const v = obj[key];
    if (v !== undefined && v !== null) out[key] = v;
  }
  return out;
}

export function createParser(ctx) {
  const state = {
    project: ctx.project ?? null,
    sessionId: ctx.sessionId,
    lastTs: null,
    model: null,
    pending: [],
    ready: false,
  };

  function make(ts, eventType, text, patch) {
    return {
      ts,
      runtime,
      machine: ctx.machine,
      session_id: state.sessionId,
      project: state.project,
      event_type: eventType,
      text: clip(text),
      tool_name: null,
      model: null,
      tokens_in: null,
      tokens_out: null,
      extra: {},
      ...patch,
    };
  }

  function stamp(rec) {
    let ts = null;
    if (typeof rec.timestamp === 'string') ts = rec.timestamp;
    else if (typeof rec.timestamp === 'number') ts = new Date(rec.timestamp).toISOString();
    else if (typeof rec.updatedAt === 'string') ts = rec.updatedAt;
    if (ts) state.lastTs = ts;
    return ts ?? state.lastTs;
  }

  function messageEvents(rec, ts) {
    const msg = rec.message;
    if (!msg || typeof msg !== 'object') {
      return [make(ts, 'meta', '', { extra: { kind: 'message' } })];
    }
    const role = typeof msg.role === 'string' ? msg.role : 'unknown';
    if (role === 'toolResult') {
      return [make(ts, 'tool_result', textOf(msg.content), {
        tool_name: typeof msg.toolName === 'string' ? msg.toolName : null,
        extra: prune({
          call_id: msg.toolCallId,
          is_error: msg.isError === true || undefined,
        }),
      })];
    }
    const textType = role === 'user' ? 'user' : role === 'assistant' ? 'assistant' : 'meta';
    const events = [];
    const buf = [];
    const flushText = () => {
      if (!buf.length) return;
      const patch = textType === 'meta' ? { extra: { kind: 'message', role } } : {};
      events.push(make(ts, textType, buf.join('\n'), patch));
      buf.length = ZERO;
    };
    const blocks = Array.isArray(msg.content)
      ? msg.content
      : [{ type: 'text', text: textOf(msg.content) }];
    for (const b of blocks) {
      if (!b || typeof b !== 'object') continue;
      if (b.type === 'text') {
        buf.push(typeof b.text === 'string' ? b.text : '');
      } else if (b.type === 'thinking') {
        flushText();
        events.push(make(ts, 'thinking', typeof b.thinking === 'string' ? b.thinking : ''));
      } else if (b.type === 'toolCall') {
        flushText();
        events.push(make(ts, 'tool_call', '', {
          tool_name: typeof b.name === 'string' ? b.name : null,
          extra: prune({ call_id: b.id }),
        }));
      } else {
        flushText();
        events.push(make(ts, 'meta', '', { extra: { kind: 'block', omp_block: String(b.type) } }));
      }
    }
    flushText();
    if (role === 'assistant') {
      const model = typeof msg.model === 'string' ? msg.model : state.model;
      for (const e of events) e.model = model;
      const [head] = events;
      const usage = msg.usage;
      if (head && usage && typeof usage === 'object') {
        if (typeof usage.input === 'number') head.tokens_in = usage.input;
        if (typeof usage.output === 'number') head.tokens_out = usage.output;
      }
    }
    return events;
  }

  function handle(rec) {
    const ts = stamp(rec);
    const type = rec.type;
    if (type === 'session') {
      if (typeof rec.cwd === 'string' && rec.cwd) state.project = rec.cwd;
      if (typeof rec.id === 'string' && rec.id) state.sessionId = rec.id;
      return [make(ts, 'meta', '', { extra: prune({ kind: type, version: rec.version }) })];
    }
    if (type === 'message') return messageEvents(rec, ts);
    if (type === 'title' || type === 'title_change') {
      const title = typeof rec.title === 'string' ? rec.title : '';
      return [make(ts, 'meta', title, { extra: { kind: type } })];
    }
    if (type === 'model_change') {
      if (typeof rec.model === 'string') state.model = rec.model;
      return [make(ts, 'meta', '', { extra: { kind: type }, model: state.model })];
    }
    if (type === 'thinking_level_change') {
      return [make(ts, 'meta', '', { extra: prune({ kind: type, level: rec.thinkingLevel }) })];
    }
    if (type === 'compaction') {
      const summary = typeof rec.summary === 'string' ? rec.summary : '';
      return [make(ts, 'meta', summary, {
        extra: prune({
          kind: type,
          tokens_before: rec.tokensBefore,
          first_kept_entry: rec.firstKeptEntryId,
        }),
      })];
    }
    if (type === 'custom_message' || type === 'custom') {
      return [make(ts, 'meta', textOf(rec.content), {
        extra: prune({ kind: type, custom_type: rec.customType }),
      })];
    }
    return [make(ts, 'meta', '', { extra: { kind: 'unknown', omp_type: String(type) } })];
  }

  function flushPending() {
    const flushed = state.pending;
    state.pending = [];
    for (const e of flushed) {
      if (e.project == null) e.project = state.project;
      if (e.session_id !== state.sessionId) e.session_id = state.sessionId;
      if (e.ts == null) e.ts = state.lastTs;
    }
    return flushed.filter((e) => e.ts != null);
  }

  return {
    onLine(raw) {
      if (typeof raw !== 'string' || !raw.trim()) return [];
      let rec;
      try { rec = JSON.parse(raw); } catch { return []; }
      if (!rec || typeof rec !== 'object' || Array.isArray(rec)) return [];
      let events;
      try { events = handle(rec); } catch { return []; }
      if (state.ready) return events;
      state.pending.push(...events);
      if (rec.type === 'session' || state.pending.length > PENDING_CAP) {
        state.ready = true;
        return flushPending();
      }
      return [];
    },
    end() {
      state.ready = true;
      return flushPending();
    },
  };
}
