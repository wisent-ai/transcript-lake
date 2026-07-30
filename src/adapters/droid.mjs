// Adapter for Factory Droid session transcripts.
// Source layout: HOME/.factory/sessions/<uuid>.jsonl (legacy, flat) and
// HOME/.factory/sessions/<encoded-cwd>/<uuid>.jsonl, each with an optional
// <uuid>.settings.json sidecar (multi-line JSON: providerLock, tokenUsage).
// Record types verified across a wide sample of real files on this machine:
//   session_start — first line, no timestamp; legacy files carry only
//     { id, title, owner }, newer ones add cwd / version / sessionTitle,
//   message — { id, timestamp, parentId, message: { role, content } } with
//     roles user | assistant and content blocks text { text },
//     tool_use { id, name, input }, tool_result { tool_use_id, content },
//     thinking { thinking, signature }; tool results ride inside user-role
//     records; no per-message usage or model fields exist,
//   todo_state — { timestamp, todos: { todos: [...] } },
//   compaction_state — { timestamp, summaryText }.
// Unknown record or block types map to meta events with a type tag in extra
// rather than being dropped. Sidecars contribute at most one meta event.

import fs from 'node:fs';
import path from 'node:path';

export const runtime = 'droid';

const N = (s) => Number(s);
const ZERO = N('0');
const TEXT_CAP = N('65536');
const PENDING_CAP = N('64');
const JSONL_EXT = '.jsonl';
const SETTINGS_EXT = '.settings.json';

export function roots(homeDir) {
  const base = path.join(homeDir, '.factory', 'sessions');
  let entries;
  try { entries = fs.readdirSync(base, { withFileTypes: true }); } catch { return []; }
  const dirs = [base];
  for (const e of entries) if (e.isDirectory()) dirs.push(path.join(base, e.name));
  return dirs;
}

// Best-effort decode of '-Users-name-...' directory names; lossy for real
// dashes in path segments, so session_start cwd overrides it when present.
function decodeProject(name) {
  if (!name.startsWith('-')) return null;
  return name.replaceAll('-', '/');
}

export function listSessions(root) {
  let entries;
  try { entries = fs.readdirSync(root, { withFileTypes: true }); } catch { return []; }
  const project = decodeProject(path.basename(root));
  const sessions = [];
  for (const e of entries) {
    if (!e.isFile()) continue;
    let ext = null;
    if (e.name.endsWith(SETTINGS_EXT)) ext = SETTINGS_EXT;
    else if (e.name.endsWith(JSONL_EXT)) ext = JSONL_EXT;
    if (!ext) continue;
    sessions.push({
      file: path.join(root, e.name),
      sessionId: e.name.slice(ZERO, e.name.length - ext.length),
      project,
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
  if (typeof ctx.file === 'string' && ctx.file.endsWith(SETTINGS_EXT)) {
    return settingsParser(ctx);
  }
  return transcriptParser(ctx);
}

// Sidecar settings files are whole-file JSON, accumulated line by line and
// parsed once at end(); they yield at most one meta event with token totals.
function settingsParser(ctx) {
  const lines = [];
  return {
    onLine(raw) {
      if (typeof raw === 'string') lines.push(raw);
      return [];
    },
    end() {
      let rec;
      try { rec = JSON.parse(lines.join('\n')); } catch { return []; }
      if (!rec || typeof rec !== 'object') return [];
      if (typeof rec.providerLockTimestamp !== 'string') return [];
      const rawUsage = rec.tokenUsage;
      const usage = rawUsage && typeof rawUsage === 'object' ? rawUsage : {};
      return [{
        ts: rec.providerLockTimestamp,
        runtime,
        machine: ctx.machine,
        session_id: ctx.sessionId,
        project: ctx.project ?? null,
        event_type: 'meta',
        text: '',
        tool_name: null,
        model: null,
        tokens_in: typeof usage.inputTokens === 'number' ? usage.inputTokens : null,
        tokens_out: typeof usage.outputTokens === 'number' ? usage.outputTokens : null,
        extra: prune({
          kind: 'settings',
          provider: rec.apiProviderLock ?? rec.providerLock,
        }),
      }];
    },
  };
}

function transcriptParser(ctx) {
  const state = {
    project: ctx.project ?? null,
    sessionId: ctx.sessionId,
    lastTs: null,
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
    if (ts) state.lastTs = ts;
    return ts ?? state.lastTs;
  }

  function messageEvents(rec, ts) {
    const msg = rec.message;
    if (!msg || typeof msg !== 'object') {
      return [make(ts, 'meta', '', { extra: { kind: 'message' } })];
    }
    const role = typeof msg.role === 'string' ? msg.role : 'unknown';
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
      } else if (b.type === 'tool_use') {
        flushText();
        events.push(make(ts, 'tool_call', '', {
          tool_name: typeof b.name === 'string' ? b.name : null,
          extra: prune({ call_id: b.id }),
        }));
      } else if (b.type === 'tool_result') {
        flushText();
        events.push(make(ts, 'tool_result', textOf(b.content), {
          extra: prune({ call_id: b.tool_use_id }),
        }));
      } else {
        flushText();
        events.push(make(ts, 'meta', '', { extra: { kind: 'block', droid_block: String(b.type) } }));
      }
    }
    flushText();
    return events;
  }

  function handle(rec) {
    const ts = stamp(rec);
    const type = rec.type;
    if (type === 'session_start') {
      if (typeof rec.cwd === 'string' && rec.cwd) state.project = rec.cwd;
      if (typeof rec.id === 'string' && rec.id) state.sessionId = rec.id;
      let title = '';
      if (typeof rec.sessionTitle === 'string' && rec.sessionTitle) title = rec.sessionTitle;
      else if (typeof rec.title === 'string') title = rec.title;
      return [make(ts, 'meta', title, { extra: prune({ kind: type, version: rec.version }) })];
    }
    if (type === 'message') return messageEvents(rec, ts);
    if (type === 'todo_state') {
      const box = rec.todos;
      const todos = box && Array.isArray(box.todos) ? box.todos : [];
      return [make(ts, 'meta', '', { extra: { kind: type, todo_count: todos.length } })];
    }
    if (type === 'compaction_state') {
      const summary = typeof rec.summaryText === 'string' ? rec.summaryText : '';
      return [make(ts, 'meta', summary, { extra: { kind: type } })];
    }
    return [make(ts, 'meta', '', { extra: { kind: 'unknown', droid_type: String(type) } })];
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
      // session_start carries no timestamp; hold events until the first
      // stamped record so its ts (and any late cwd) can be backfilled.
      if (state.lastTs != null || state.pending.length > PENDING_CAP) {
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
