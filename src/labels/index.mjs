// Operator label store: aspect/value annotations over Lake sessions. One
// append-only NDJSON file beneath LAKE_DATA/labels; each assignment is one
// complete line written in a single append call and fsynced before return.
// Readers (the labels view in sql/views.sql) tolerate a torn final line, so
// a crash mid-write loses at most the record being appended. Labels are
// derived operator data, not masked Lake events: the events writer lease
// deliberately does not cover this store, so labeling neither blocks nor is
// blocked by an active ingest, and deleting labels/ discards only labels.
// No digit characters outside quoted strings and comments.
import { closeSync, fsyncSync, mkdirSync, openSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const STORE_DIR = 'labels';
const STORE_FILE = 'labels.ndjson';
const MANUAL = 'manual';
// Namespaced provenance: bare manual/human/model/brama, or with a detail
// suffix (brama:claude-opus-4.6, model:hf-distilbert-topic). Kept as a
// quoted pattern so the digit convention above holds.
const SOURCE_PATTERN = '^(manual|human|model|brama)(:[A-Za-z0-9._/-]+)?$';

export function normalizeSource(value) {
  if (value === undefined) return MANUAL;
  const source = String(value).trim();
  if (!new RegExp(SOURCE_PATTERN).test(source)) {
    throw new Error(
      '--source must match ' + SOURCE_PATTERN
      + ' (manual, human, model, or brama, with an optional :detail suffix)'
    );
  }
  return source;
}

export function labelsPath(dataDir) {
  return join(dataDir, STORE_DIR, STORE_FILE);
}

export function normalizeAspect(value) {
  const aspect = String(value === undefined ? '' : value).trim().toLowerCase();
  if (!aspect) throw new Error('--aspect must be a non-empty string');
  return aspect;
}

export function normalizeLabelValue(value) {
  const text = String(value === undefined ? '' : value).trim();
  if (!text) throw new Error('--value must be a non-empty string');
  return text;
}

export function normalizeNote(value) {
  if (value === undefined) return null;
  const text = String(value).trim();
  return text || null;
}

export function labelRecord({ sessionId, runtime, aspect, value, note, source }) {
  return {
    ts: new Date().toISOString(),
    session_id: sessionId,
    runtime,
    aspect,
    value,
    note,
    source: source || MANUAL,
  };
}

export function appendLabel(dataDir, record) {
  mkdirSync(join(dataDir, STORE_DIR), { recursive: true });
  const fd = openSync(labelsPath(dataDir), 'a');
  try {
    writeFileSync(fd, JSON.stringify(record) + '\n');
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  return record;
}
