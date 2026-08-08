#!/usr/bin/env node
// Transcript Lake command router. Simple commands cover discovery, health,
// ingest, inspection, analytics, recovery, derived artifacts, and Oko.
// DuckDB remains optional and is required only for analytics and compaction.
// No digit characters outside quoted strings and comments.
import { spawn, spawnSync } from 'node:child_process';
import {
  existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, watch, writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ingest, resolveDataDir, SUPPORTED_SOURCES } from './ingest.mjs';
import { openWriterLease } from './cursors.mjs';
import {
  appendLabel, labelRecord, normalizeAspect, normalizeLabelValue, normalizeNote, normalizeSource,
} from './labels/index.mjs';

const N = (s) => Number(s);
const [ZERO, ONE, TWO] = ['0', '1', '2'].map(N);
const [DEFAULT_LIMIT, MAX_LIMIT, DEFAULT_DAYS] = ['20', '500', '7'].map(N);
const [DEFAULT_DEBOUNCE, SECOND_MS] = ['60', '1000'].map(N);
const LAKE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const CLI_ENTRY = join(LAKE_ROOT, 'src', 'cli.mjs');
const PACKAGE = JSON.parse(readFileSync(join(LAKE_ROOT, 'package.json'), 'utf8'));
const VERSION = String(PACKAGE.version);
const SUMMARY_FILE = 'last-ingest.json';
const USAGE = [
  'Transcript Lake creates a privacy-masked local event archive from coding-agent transcripts.',
  '',
  'Usage: transcript-lake [--data-dir <path>] <command> [flags]',
  '',
  'Start safely:',
  '  transcript-lake paths                         show every local product path',
  '  transcript-lake sources                       discover supported transcript stores',
  '  transcript-lake status                        inspect Lake state without ingesting',
  '  transcript-lake ingest                        ingest new local transcript events',
  '',
  'Discover and inspect:',
  '  paths [--json]                                resolved state and integration paths',
  '  sources [--json]                              source availability and file counts',
  '  doctor [--json]                               state and dependency health checks',
  '  status [--json]                               partitions, cursors, masking, Oko',
  '',
  'Ingest and recover:',
  '  ingest [--source <runtime>] [--full]           incremental scan; full needs empty root',
  '  rebuild --to <empty-path> [--source <runtime>] safe full replay to a new Lake',
  '  watch [--debounce <seconds>] [--json]          online refresh when sources change',
  '',
  'Read and analyze:',
  '  sessions [--runtime <r>] [--project <text>] [--limit <n>] [--json]',
  '  events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]',
  '  search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]',
  '  stats [--days <n>] [--runtime <r>] [--json]   usage summary',
  '  hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]',
  '  signals [--report <frustration|overlap|daily|freshness>] [--limit <n>] [--json]',
  '  query [--json] "<sql>"                        arbitrary DuckDB SQL over Lake views',
  '',
  'Label and annotate:',
  '  label add <session-id> --aspect <a> --value <v> [--note <text>] [--source <name[:detail]>] [--json]',
  '  label list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json]',
  '  label aspects [--json]                       distinct aspects with value counts',
  '',
  'Derived data and Oko:',
  '  compact [--source <runtime>] [--json]          write Parquet mirrors',
  '  export-oko [--full] [--reindex]                materialize sessions for Oko',
  '  oko-refresh                                    reindex current export in Oko',
  '  clean [--target <parquet|oko|all>] [--apply] [--json]',
  '',
  'Guidance:',
  '  help [command]                                 command-specific help',
  '',
  'Global flags:',
  '  --data-dir <path>                              select state root for this invocation',
  '  -h, --help                                     show general or command help',
  '  -V, --version                                  print canonical product version',
  '',
  'State default: ~/.transcript-lake. Mutation is local; source stores are read-only.',
  'Help: https://github.com/wisent-ai/transcript-lake#readme',
].join('\n');

const quoteSql = (value) => "'" + String(value).replaceAll("'", "''") + "'";

function runBinary(name, args) {
  const res = spawnSync(name, args, { stdio: 'inherit' });
  if (res.error) throw new Error(name + ' failed to start: ' + res.error.message);
  if (res.status === null) throw new Error(name + ' terminated by signal ' + String(res.signal));
  return res.status;
}
function requireNoArgs(command, rest) {
  if (rest.length) throw new Error(command + ' accepts no arguments or flags');
}

function findOnPath(name) {
  for (const dir of String(process.env.PATH || '').split(delimiter)) {
    if (dir && existsSync(join(dir, name))) return join(dir, name);
  }
  return null;
}

function partitionReport(dataDir) {
  const eventsDir = join(dataDir, 'events');
  const rows = [];
  if (!existsSync(eventsDir)) return rows;
  for (const runtimeEnt of readdirSync(eventsDir, { withFileTypes: true })) {
    if (!runtimeEnt.isDirectory() || !runtimeEnt.name.startsWith('runtime=')) continue;
    const runtimeDir = join(eventsDir, runtimeEnt.name);
    let parts = ZERO;
    let bytes = ZERO;
    for (const dateEnt of readdirSync(runtimeDir, { withFileTypes: true })) {
      if (!dateEnt.isDirectory()) continue;
      const dateDir = join(runtimeDir, dateEnt.name);
      for (const ent of readdirSync(dateDir, { withFileTypes: true })) {
        if (!ent.isFile() || !ent.name.endsWith('.ndjson')) continue;
        bytes += statSync(join(dateDir, ent.name)).size;
        parts += ONE;
      }
    }
    rows.push({ runtime: runtimeEnt.name.slice('runtime='.length), parts, bytes });
  }
  return rows.sort((a, b) => (a.runtime < b.runtime ? -ONE : ONE));
}

const flagKey = (flag) => flag.slice(TWO).replaceAll('-', '_');

function parseOptions(command, rest, valueFlags = [], booleanFlags = []) {
  const values = new Set(valueFlags);
  const booleans = new Set(booleanFlags);
  const options = {};
  const positionals = [];
  const queue = [...rest];
  while (queue.length) {
    const token = queue.shift();
    if (!token.startsWith('--')) {
      positionals.push(token);
      continue;
    }
    if (booleans.has(token)) {
      const key = flagKey(token);
      if (options[key]) throw new Error(command + ' received duplicate ' + token);
      options[key] = true;
      continue;
    }
    if (values.has(token)) {
      const key = flagKey(token);
      if (options[key] !== undefined) throw new Error(command + ' received duplicate ' + token);
      const value = queue.shift();
      if (!value || value.startsWith('--')) throw new Error(token + ' requires a value');
      options[key] = value;
      continue;
    }
    throw new Error('unknown ' + command + ' flag: ' + token);
  }
  return { options, positionals };
}

function boundedInteger(value, name, fallback, maximum = MAX_LIMIT) {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < ONE || parsed > maximum) {
    throw new Error(name + ' must be an integer from ' + String(ONE) + ' to ' + String(maximum));
  }
  return parsed;
}

function requireRuntime(value) {
  if (value === undefined) return null;
  if (!SUPPORTED_SOURCES.includes(value)) {
    throw new Error('unknown source "' + value + '" (expected one of: ' + SUPPORTED_SOURCES.join(', ') + ')');
  }
  return value;
}

function writeJson(value) {
  process.stdout.write(JSON.stringify(value, null, TWO) + '\n');
}

function lakePaths(dataDir = resolveDataDir({})) {
  return {
    dataDir,
    events: join(dataDir, 'events'),
    cursors: join(dataDir, 'cursors.json'),
    lastIngest: join(dataDir, SUMMARY_FILE),
    parquet: join(dataDir, 'parquet'),
    okoExport: join(dataDir, 'exports', 'oko'),
    okoStaging: join(dataDir, 'staging', 'oko-export'),
    hookSegments: process.env.HOOKS_ADAPTIVE_SEGMENTS_READY
      || join(homedir(), '.hooks-adaptive', 'telemetry-segments', 'ready'),
    duckdb: findOnPath('duckdb'),
    okoCli: process.env.OKO_CLI || findOnPath('oko-cli'),
  };
}

function readCursorStatus(path) {
  if (!existsSync(path)) return { state: 'absent', files: ZERO, newestSourceMtime: null };
  try {
    const store = JSON.parse(readFileSync(path, 'utf8'));
    if (!store || typeof store !== 'object' || Array.isArray(store)) {
      return { state: 'invalid', files: ZERO, newestSourceMtime: null, error: 'cursor store is not an object' };
    }
    const stamps = Object.values(store)
      .map((record) => Number(record && record.mtimeMs))
      .filter(Number.isFinite);
    return {
      state: 'healthy',
      files: Object.keys(store).length,
      newestSourceMtime: stamps.length ? new Date(Math.max(...stamps)).toISOString() : null,
    };
  } catch (error) {
    return { state: 'invalid', files: ZERO, newestSourceMtime: null, error: String(error) };
  }
}

function readLastIngest(path) {
  if (!existsSync(path)) return { state: 'absent', summary: null };
  try {
    return { state: 'healthy', summary: JSON.parse(readFileSync(path, 'utf8')) };
  } catch (error) {
    return { state: 'invalid', summary: null, error: String(error) };
  }
}

function hookSourceRoots() {
  const ready = lakePaths().hookSegments;
  const legacy = join(homedir(), '.hooks-adaptive');
  const segmentMode = existsSync(ready);
  return {
    ready,
    legacy,
    segmentMode,
    available: segmentMode || existsSync(legacy),
    roots: segmentMode ? [ready] : (existsSync(legacy) ? [legacy] : []),
  };
}

async function sourceReport() {
  const rows = [];
  for (const runtime of SUPPORTED_SOURCES) {
    if (runtime === 'hooks') {
      const hooks = hookSourceRoots();
      try {
        let files = ZERO;
        if (hooks.segmentMode) {
          files = readdirSync(hooks.ready, { withFileTypes: true })
            .filter((entry) => entry.isFile() && entry.name.endsWith('.jsonl')).length;
        } else if (existsSync(hooks.legacy)) {
          files = ['telemetry.prev.jsonl', 'telemetry.jsonl']
            .filter((name) => existsSync(join(hooks.legacy, name))).length;
        }
        rows.push({
          runtime,
          available: hooks.available,
          mode: hooks.segmentMode ? 'closed-segments' : 'legacy-log',
          roots: hooks.roots,
          files,
        });
      } catch (error) {
        rows.push({ runtime, available: false, mode: 'error', roots: [], files: ZERO, error: String(error) });
      }
      continue;
    }
    try {
      const adapter = await import('./adapters/' + runtime + '.mjs');
      const roots = adapter.roots(homedir());
      let files = ZERO;
      for (const root of roots) files += adapter.listSessions(root).length;
      rows.push({ runtime, available: roots.length > ZERO, mode: 'transcripts', roots, files });
    } catch (error) {
      rows.push({ runtime, available: false, mode: 'error', roots: [], files: ZERO, error: String(error) });
    }
  }
  return rows;
}

async function statusSnapshot() {
  const paths = lakePaths();
  let oko;
  try {
    const exporter = await import('./oko_export.mjs');
    oko = exporter.freshness();
  } catch (error) {
    oko = { state: 'unavailable', error: String(error && error.message ? error.message : error) };
  }
  return {
    dataDir: paths.dataDir,
    partitions: partitionReport(paths.dataDir),
    cursors: readCursorStatus(paths.cursors),
    lastIngest: readLastIngest(paths.lastIngest),
    oko,
  };
}

function viewsScript(sql, includeSignals = false) {
  const viewsPath = join(LAKE_ROOT, 'sql', 'views.sql');
  if (!existsSync(viewsPath)) throw new Error('missing ' + viewsPath + ' (installation is incomplete)');
  let setup = readFileSync(viewsPath, 'utf8');
  if (includeSignals) {
    const signalsPath = join(LAKE_ROOT, 'sql', 'signals.sql');
    if (!existsSync(signalsPath)) throw new Error('missing ' + signalsPath + ' (installation is incomplete)');
    setup += '\n' + readFileSync(signalsPath, 'utf8');
  }
  const dataDir = resolveDataDir({});
  return 'SET VARIABLE lake_data = ' + quoteSql(dataDir) + ';\n' + setup + '\n' + sql;
}

function runDuckQuery(sql, json, includeSignals = false) {
  const args = [];
  if (json) args.push('-json');
  args.push('-c', viewsScript(sql, includeSignals));
  process.exitCode = runBinary('duckdb', args);
}

function queryDuckJson(sql) {
  const res = spawnSync('duckdb', ['-json', '-c', viewsScript(sql)], { encoding: 'utf8' });
  if (res.error) throw new Error('duckdb failed to start: ' + res.error.message);
  if (res.status === null) throw new Error('duckdb terminated by signal ' + String(res.signal));
  if (res.status !== ZERO) {
    throw new Error('duckdb exited with status ' + String(res.status) + ': ' + String(res.stderr || '').trim());
  }
  const out = String(res.stdout || '').trim();
  return out ? JSON.parse(out) : [];
}

function pathSize(path) {
  if (!existsSync(path)) return ZERO;
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.isSymbolicLink()) return stat.size;
  let total = ZERO;
  for (const entry of readdirSync(path)) total += pathSize(join(path, entry));
  return total;
}

async function performIngest({ source = null, full = false, dataDir = resolveDataDir({}) }) {
  const summary = await ingest({ source, full, dataDir });
  const exporter = await import('./oko_export.mjs');
  const okoExport = await exporter.exportOko({ full, dataDir });
  const record = { finishedAt: new Date().toISOString(), dataDir, source, full, okoExport, ...summary };
  mkdirSync(dataDir, { recursive: true });
  writeFileSync(join(dataDir, SUMMARY_FILE), JSON.stringify(record, null, TWO) + '\n');
  writeJson(record);
  if (summary.partial) process.exitCode = ONE;
  return record;
}

async function cmdIngest(rest) {
  const parsed = parseOptions('ingest', rest, ['--source'], ['--full']);
  if (parsed.positionals.length) throw new Error('ingest accepts flags only');
  return performIngest({
    source: requireRuntime(parsed.options.source),
    full: Boolean(parsed.options.full),
  });
}

const COMMAND_HELP = {
  paths: 'paths [--json]\n  Print resolved Lake, derived-data, Tama, DuckDB, and Oko paths.',
  sources: 'sources [--json]\n  Discover supported runtime roots and count candidate transcript files.',
  doctor: 'doctor [--json]\n  Check cursor integrity, source discovery, and optional dependency presence.',
  status: 'status [--json]\n  Show partitions, cursor freshness, last ingest, and Oko export freshness.',
  ingest: 'ingest [--source <runtime>] [--full]\n  Incremental by default. --full is allowed only for an empty selected root.',
  rebuild: 'rebuild --to <empty-path> [--source <runtime>]\n  Full replay into a different empty root; never mutates the current Lake.',
  watch: 'watch [--debounce <seconds>] [--json]\n  Watch supported source roots and run the ingest/export refresh after a quiet interval. Long-running foreground process; launchd or systemd is expected to KeepAlive it.',
  sessions: 'sessions [--runtime <r>] [--project <text>] [--limit <n>] [--json]\n  List recent sessions through the canonical DuckDB view.',
  events: 'events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  List recent masked canonical events.',
  search: 'search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  Case-insensitive literal substring match over masked event text, newest first.',
  stats: 'stats [--days <n>] [--runtime <r>] [--json]\n  Summarize events, sessions, tools, and token counters.',
  hooks: 'hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]\n  Inspect adaptive-hook decisions.',
  signals: 'signals [--report <frustration|overlap|daily|freshness>] [--limit <n>] [--json]\n  Query Oko/Lake cross-source signal views.',
  label: 'label <add|list|aspects> ...\n  add <session-id> --aspect <name> --value <v> [--note <text>] [--source <name[:detail]>] [--json] records a session label with provenance (manual, human, model, or brama, optional :detail).\n  list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json] shows the latest assignment per session and aspect, newest first.\n  aspects [--json] summarizes distinct aspects, values, and labeled sessions.',
  query: 'query [--json] "<sql>"\n  Execute operator-supplied SQL after loading canonical Lake views.',
  compact: 'compact [--source <runtime>] [--json]\n  Rebuild per-runtime Parquet mirrors; NDJSON remains authoritative.',
  'export-oko': 'export-oko [--full] [--reindex]\n  Materialize canonical sessions and optionally invoke Oko reindex.',
  'oko-refresh': 'oko-refresh\n  Invoke the compatible oko-cli transcript reindex command.',
  clean: 'clean [--target <parquet|oko|all>] [--apply] [--json]\n  Preview by default; --apply removes rebuildable derived data only.',
  help: 'help [command]\n  Show general guidance or the exact syntax for one command.',
};

function cmdHelp(rest) {
  if (rest.length > ONE) throw new Error('help accepts at most one command name');
  if (!rest.length) {
    process.stdout.write(USAGE + '\n');
    return;
  }
  const text = COMMAND_HELP[rest[ZERO]];
  if (!text) throw new Error('unknown help topic: ' + rest[ZERO]);
  process.stdout.write('Usage: transcript-lake [--data-dir <path>] ' + text + '\n');
}

function cmdPaths(rest) {
  const parsed = parseOptions('paths', rest, [], ['--json']);
  if (parsed.positionals.length) throw new Error('paths accepts flags only');
  const report = lakePaths();
  if (parsed.options.json) {
    writeJson(report);
    return;
  }
  for (const [name, value] of Object.entries(report)) {
    process.stdout.write(name + ': ' + String(value || 'not found') + '\n');
  }
}

async function cmdSources(rest) {
  const parsed = parseOptions('sources', rest, [], ['--json']);
  if (parsed.positionals.length) throw new Error('sources accepts flags only');
  const rows = await sourceReport();
  if (rows.some((row) => row.error)) process.exitCode = ONE;
  if (parsed.options.json) {
    writeJson(rows);
    return;
  }
  for (const row of rows) {
    const suffix = row.error ? ' error=' + row.error : '';
    process.stdout.write(
      row.runtime + ': ' + (row.available ? row.mode : 'not found')
      + ', ' + String(row.files) + ' files' + suffix + '\n'
    );
    for (const root of row.roots) process.stdout.write('  ' + root + '\n');
  }
}

async function cmdStatus(rest) {
  const parsed = parseOptions('status', rest, [], ['--json']);
  if (parsed.positionals.length) throw new Error('status accepts flags only');
  const report = await statusSnapshot();
  if (report.cursors.state === 'invalid' || report.lastIngest.state === 'invalid') {
    process.exitCode = ONE;
  }
  if (parsed.options.json) {
    writeJson(report);
    return;
  }
  process.stdout.write('data dir: ' + report.dataDir + '\n');
  if (!report.partitions.length) process.stdout.write('partitions: none (run ingest first)\n');
  for (const row of report.partitions) {
    process.stdout.write(
      '  ' + row.runtime + ': ' + String(row.parts) + ' partition files, '
      + String(row.bytes) + ' bytes\n'
    );
  }
  process.stdout.write(
    'cursors: ' + report.cursors.state + ', ' + String(report.cursors.files) + ' tracked files'
    + (report.cursors.newestSourceMtime ? ', newest ' + report.cursors.newestSourceMtime : '') + '\n'
  );
  if (report.cursors.error) process.stdout.write('  cursor error: ' + report.cursors.error + '\n');
  const last = report.lastIngest.summary;
  process.stdout.write(
    'last ingest: ' + (last
      ? String(last.finishedAt) + ', failures ' + String(last.failures ?? ZERO)
      : report.lastIngest.state)
    + '\n'
  );
  if (report.lastIngest.error) process.stdout.write('  summary error: ' + report.lastIngest.error + '\n');
  process.stdout.write('oko: ' + (typeof report.oko === 'string' ? report.oko : JSON.stringify(report.oko)) + '\n');
}

async function cmdDoctor(rest) {
  const parsed = parseOptions('doctor', rest, [], ['--json']);
  if (parsed.positionals.length) throw new Error('doctor accepts flags only');
  const paths = lakePaths();
  const cursors = readCursorStatus(paths.cursors);
  const sources = await sourceReport();
  const checks = [
    {
      name: 'state-root',
      status: existsSync(paths.dataDir) ? 'ok' : 'ok',
      detail: existsSync(paths.dataDir) ? paths.dataDir : 'absent zero-state: ' + paths.dataDir,
    },
    {
      name: 'cursors',
      status: cursors.state === 'invalid' ? 'error' : 'ok',
      detail: cursors.state + (cursors.error ? ': ' + cursors.error : ''),
    },
    {
      name: 'sources',
      status: sources.some((row) => row.available) ? 'ok' : 'warning',
      detail: String(sources.filter((row) => row.available).length) + ' supported runtimes found',
    },
    {
      name: 'source-integrity',
      status: sources.some((row) => row.error) ? 'error' : 'ok',
      detail: sources.some((row) => row.error)
        ? sources.filter((row) => row.error).map((row) => row.runtime + ': ' + row.error).join('; ')
        : 'all installed adapters loaded',
    },
    {
      name: 'duckdb',
      status: paths.duckdb ? 'ok' : 'warning',
      detail: paths.duckdb || 'optional dependency not found; analytics and compact unavailable',
    },
    {
      name: 'oko-cli',
      status: paths.okoCli ? 'ok' : 'warning',
      detail: paths.okoCli || 'optional dependency not found; reindex unavailable',
    },
  ];
  const report = { dataDir: paths.dataDir, healthy: !checks.some((check) => check.status === 'error'), checks };
  if (parsed.options.json) writeJson(report);
  else {
    for (const check of checks) {
      process.stdout.write(check.status.toUpperCase() + ' ' + check.name + ': ' + check.detail + '\n');
    }
  }
  if (!report.healthy) process.exitCode = ONE;
}

async function cmdRebuild(rest) {
  const parsed = parseOptions('rebuild', rest, ['--to', '--source']);
  if (parsed.positionals.length) throw new Error('rebuild accepts flags only');
  if (!parsed.options.to) throw new Error('rebuild requires --to <empty-path>');
  const current = resolveDataDir({});
  const target = resolve(parsed.options.to);
  if (target === current) throw new Error('rebuild target must differ from the current Lake');
  return performIngest({
    source: requireRuntime(parsed.options.source),
    full: true,
    dataDir: target,
  });
}

// Watch roots come from the same adapter and hooks discovery that ingest and
// sources use, so a new runtime store is watched the moment it is supported.
async function watchRoots() {
  const roots = [];
  for (const runtime of SUPPORTED_SOURCES) {
    if (runtime === 'hooks') {
      roots.push(...hookSourceRoots().roots);
      continue;
    }
    try {
      const adapter = await import('./adapters/' + runtime + '.mjs');
      roots.push(...adapter.roots(homedir()));
    } catch {
      // An adapter that fails to load contributes no watch roots; doctor
      // remains the place that surfaces the breakage.
    }
  }
  return roots;
}

// Online freshness: recursively watch every supported source root, coalesce
// changes over a quiet interval, then run the same refresh the external
// timer runs (ingest, then export-oko) as child processes of this CLI. At
// most one refresh runs and one more is queued; the writer lease inside
// ingest remains the backstop against any other writer. This is a
// long-running foreground process: launchd or systemd should KeepAlive it.
async function cmdWatch(rest) {
  const parsed = parseOptions('watch', rest, ['--debounce'], ['--json']);
  if (parsed.positionals.length) throw new Error('watch accepts flags only');
  const debounceSeconds = boundedInteger(parsed.options.debounce, '--debounce', DEFAULT_DEBOUNCE);
  const json = Boolean(parsed.options.json);
  const roots = await watchRoots();
  if (!roots.length) throw new Error('watch found no supported source roots on this machine');
  const log = (kind, details) => {
    const ts = new Date().toISOString();
    if (json) {
      process.stdout.write(JSON.stringify({ ts, kind, ...details }) + '\n');
      return;
    }
    const text = Object.entries(details).map(([key, value]) => key + '=' + String(value)).join(' ');
    process.stdout.write(ts + ' watch ' + kind + (text ? ' ' + text : '') + '\n');
  };
  let pendingEvents = ZERO;
  let timer = null;
  let running = false;
  let queued = false;
  const runStep = (command) => new Promise((done) => {
    log('run-start', { command });
    const child = spawn(process.execPath, [CLI_ENTRY, command], { stdio: 'inherit' });
    child.on('error', (error) => {
      log('run-finish', { command, error: String(error && error.message ? error.message : error) });
      done(ONE);
    });
    child.on('close', (status) => {
      log('run-finish', { command, status });
      done(status);
    });
  });
  const fire = async () => {
    timer = null;
    const batch = pendingEvents;
    pendingEvents = ZERO;
    log('batch', { events: batch });
    if (running) {
      if (!queued) log('queued', { events: batch });
      queued = true;
      return;
    }
    running = true;
    for (;;) {
      const status = await runStep('ingest');
      if (status === ZERO) await runStep('export-oko');
      if (!queued) break;
      queued = false;
    }
    running = false;
  };
  const schedule = () => {
    pendingEvents += ONE;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => { fire(); }, debounceSeconds * SECOND_MS);
  };
  const watchers = roots.map((root) => watch(root, { recursive: true }, () => schedule()));
  const stop = () => {
    for (const watcher of watchers) watcher.close();
    process.exit(ZERO);
  };
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
  log('start', { roots: roots.length, debounceSeconds });
}

function cmdSessions(rest) {
  const parsed = parseOptions(
    'sessions', rest, ['--runtime', '--project', '--limit'], ['--json']
  );
  if (parsed.positionals.length) throw new Error('sessions accepts flags only');
  const runtime = requireRuntime(parsed.options.runtime);
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  const where = [];
  if (runtime) where.push('runtime = ' + quoteSql(runtime));
  if (parsed.options.project) {
    where.push('lower(coalesce(project, \'\')) LIKE lower(' + quoteSql('%' + parsed.options.project + '%') + ')');
  }
  const clause = where.length ? ' WHERE ' + where.join(' AND ') : '';
  runDuckQuery(
    'SELECT runtime, session_id, project, first_ts, last_ts, user_msgs, assistant_msgs, '
    + 'tool_calls, tokens_in, tokens_out FROM sessions' + clause
    + ' ORDER BY last_ts DESC LIMIT ' + String(limit),
    Boolean(parsed.options.json)
  );
}

function cmdEvents(rest) {
  const parsed = parseOptions(
    'events', rest, ['--runtime', '--session', '--type', '--limit'], ['--json']
  );
  if (parsed.positionals.length) throw new Error('events accepts flags only');
  const runtime = requireRuntime(parsed.options.runtime);
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  const where = [];
  if (runtime) where.push('runtime = ' + quoteSql(runtime));
  if (parsed.options.session) where.push('session_id = ' + quoteSql(parsed.options.session));
  if (parsed.options.type) where.push('event_type = ' + quoteSql(parsed.options.type));
  const clause = where.length ? ' WHERE ' + where.join(' AND ') : '';
  runDuckQuery(
    'SELECT ts, runtime, session_id, project, event_type, tool_name, model, tokens_in, tokens_out, '
    + 'substr(text, CAST(\'1\' AS INTEGER), CAST(\'240\' AS INTEGER)) AS text FROM events'
    + clause + ' ORDER BY ts DESC LIMIT ' + String(limit),
    Boolean(parsed.options.json)
  );
}

function cmdSearch(rest) {
  const parsed = parseOptions(
    'search', rest, ['--runtime', '--session', '--type', '--limit'], ['--json']
  );
  const term = parsed.positionals.join(' ').trim();
  if (!term) throw new Error('usage: transcript-lake search [--json] <text>');
  const runtime = requireRuntime(parsed.options.runtime);
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  const literal = String(term)
    .replaceAll('!', '!!')
    .replaceAll('%', '!%')
    .replaceAll('_', '!_');
  const where = ['lower(text) LIKE lower(' + quoteSql('%' + literal + '%') + ") ESCAPE '!'"];
  if (runtime) where.push('runtime = ' + quoteSql(runtime));
  if (parsed.options.session) where.push('session_id = ' + quoteSql(parsed.options.session));
  if (parsed.options.type) where.push('event_type = ' + quoteSql(parsed.options.type));
  runDuckQuery(
    'SELECT ts, runtime, session_id, event_type, '
    + 'substr(text, CAST(\'1\' AS INTEGER), CAST(\'240\' AS INTEGER)) AS text FROM events WHERE '
    + where.join(' AND ') + ' ORDER BY ts DESC LIMIT ' + String(limit),
    Boolean(parsed.options.json)
  );
}

function cmdLabelAdd(rest) {
  const parsed = parseOptions(
    'label add', rest, ['--aspect', '--value', '--note', '--runtime', '--source'], ['--json']
  );
  if (parsed.positionals.length !== ONE) {
    throw new Error(
      'usage: transcript-lake label add <session-id> --aspect <name> --value <v>'
      + ' [--note <text>] [--source <name[:detail]>] [--json]'
    );
  }
  const sessionId = String(parsed.positionals[ZERO]).trim();
  if (!sessionId) throw new Error('label add requires a session id');
  const aspect = normalizeAspect(parsed.options.aspect);
  const value = normalizeLabelValue(parsed.options.value);
  const note = normalizeNote(parsed.options.note);
  const source = normalizeSource(parsed.options.source);
  const rows = queryDuckJson(
    'SELECT DISTINCT runtime FROM sessions WHERE session_id = ' + quoteSql(sessionId)
  );
  if (!rows.length) {
    throw new Error(
      'unknown session "' + sessionId + '": not present in the selected Lake'
      + ' (check the id or run ingest first)'
    );
  }
  const runtimes = rows.map((row) => row.runtime).sort();
  let runtime = requireRuntime(parsed.options.runtime);
  if (runtime && !runtimes.includes(runtime)) {
    throw new Error(
      'session "' + sessionId + '" exists under ' + runtimes.join(', ') + ', not ' + runtime
    );
  }
  if (!runtime) {
    if (runtimes.length > ONE) {
      throw new Error(
        'session id "' + sessionId + '" is ambiguous across runtimes ('
        + runtimes.join(', ') + '); repeat with --runtime'
      );
    }
    runtime = runtimes[ZERO];
  }
  const record = appendLabel(
    resolveDataDir({}),
    labelRecord({ sessionId, runtime, aspect, value, note, source })
  );
  if (parsed.options.json) {
    writeJson(record);
    return;
  }
  process.stdout.write(
    'labeled ' + record.session_id + ' (' + record.runtime + '): '
    + record.aspect + ' = ' + record.value
    + (record.note ? ' (note: ' + record.note + ')' : '') + '\n'
  );
}

function latestLabelsInner(where) {
  return 'SELECT ts, session_id, runtime, aspect, value, note, source, '
    + 'row_number() OVER (PARTITION BY session_id, aspect ORDER BY ts DESC) AS rn FROM labels'
    + (where.length ? ' WHERE ' + where.join(' AND ') : '');
}

function cmdLabelList(rest) {
  const parsed = parseOptions(
    'label list', rest, ['--session', '--aspect', '--runtime', '--limit'], ['--json']
  );
  if (parsed.positionals.length) throw new Error('label list accepts flags only');
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  const where = [];
  if (parsed.options.session) where.push('session_id = ' + quoteSql(parsed.options.session));
  if (parsed.options.aspect) where.push('aspect = ' + quoteSql(normalizeAspect(parsed.options.aspect)));
  const runtime = requireRuntime(parsed.options.runtime);
  if (runtime) where.push('runtime = ' + quoteSql(runtime));
  runDuckQuery(
    'SELECT ts, session_id, runtime, aspect, value, note, source FROM ('
    + latestLabelsInner(where) + ') WHERE rn = CAST(\'1\' AS BIGINT)'
    + ' ORDER BY ts DESC LIMIT ' + String(limit),
    Boolean(parsed.options.json)
  );
}

function cmdLabelAspects(rest) {
  const parsed = parseOptions('label aspects', rest, [], ['--json']);
  if (parsed.positionals.length) throw new Error('label aspects accepts flags only');
  runDuckQuery(
    'SELECT aspect, count(DISTINCT value) AS values, count(*) AS labels, '
    + 'count(DISTINCT session_id) AS sessions FROM ('
    + latestLabelsInner([]) + ') WHERE rn = CAST(\'1\' AS BIGINT)'
    + ' GROUP BY aspect ORDER BY labels DESC, aspect',
    Boolean(parsed.options.json)
  );
}

function cmdLabel(rest) {
  const [subcommand, ...subrest] = rest;
  if (subcommand === 'add') return cmdLabelAdd(subrest);
  if (subcommand === 'list') return cmdLabelList(subrest);
  if (subcommand === 'aspects') return cmdLabelAspects(subrest);
  throw new Error('usage: transcript-lake label <add|list|aspects> (see: transcript-lake help label)');
}

function cmdStats(rest) {
  const parsed = parseOptions('stats', rest, ['--days', '--runtime'], ['--json']);
  if (parsed.positionals.length) throw new Error('stats accepts flags only');
  const runtime = requireRuntime(parsed.options.runtime);
  const days = boundedInteger(parsed.options.days, '--days', DEFAULT_DAYS, MAX_LIMIT);
  const where = [
    'ts >= current_timestamp - CAST(' + quoteSql(String(days) + ' days') + ' AS INTERVAL)',
  ];
  if (runtime) where.push('runtime = ' + quoteSql(runtime));
  runDuckQuery(
    'SELECT runtime, count(*) AS events, count(DISTINCT session_id) AS sessions, '
    + 'count(*) FILTER (WHERE event_type = \'user\') AS user_msgs, '
    + 'count(*) FILTER (WHERE event_type = \'assistant\') AS assistant_msgs, '
    + 'count(*) FILTER (WHERE event_type = \'tool_call\') AS tool_calls, '
    + 'sum(tokens_in) AS tokens_in, sum(tokens_out) AS tokens_out, min(ts) AS first_ts, max(ts) AS last_ts '
    + 'FROM events WHERE ' + where.join(' AND ') + ' GROUP BY runtime ORDER BY events DESC',
    Boolean(parsed.options.json)
  );
}

function cmdHooks(rest) {
  const parsed = parseOptions(
    'hooks', rest, ['--decision', '--tool', '--limit'], ['--json']
  );
  if (parsed.positionals.length) throw new Error('hooks accepts flags only');
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  const where = [];
  if (parsed.options.decision) where.push('decision = ' + quoteSql(parsed.options.decision));
  if (parsed.options.tool) where.push('hook_id = ' + quoteSql(parsed.options.tool));
  const clause = where.length ? ' WHERE ' + where.join(' AND ') : '';
  runDuckQuery(
    'SELECT ts, session_id, project, hook_id, decision, hook_event, infra, reason '
    + 'FROM hook_decisions' + clause + ' ORDER BY ts DESC LIMIT ' + String(limit),
    Boolean(parsed.options.json)
  );
}

function cmdSignals(rest) {
  const parsed = parseOptions('signals', rest, ['--report', '--limit'], ['--json']);
  if (parsed.positionals.length) throw new Error('signals accepts flags only');
  const reports = {
    frustration: 'oko_frustration',
    overlap: 'hook_frustration_overlap',
    daily: 'hook_frustration_daily',
    freshness: 'oko_lake_freshness',
  };
  const name = parsed.options.report || 'freshness';
  const view = reports[name];
  if (!view) throw new Error('--report must be frustration, overlap, daily, or freshness');
  const limit = boundedInteger(parsed.options.limit, '--limit', DEFAULT_LIMIT);
  runDuckQuery(
    'SELECT * FROM ' + view + ' LIMIT ' + String(limit),
    Boolean(parsed.options.json),
    true
  );
}

function cmdClean(rest) {
  const parsed = parseOptions('clean', rest, ['--target'], ['--apply', '--json']);
  if (parsed.positionals.length) throw new Error('clean accepts flags only');
  const target = parsed.options.target || 'all';
  if (!['parquet', 'oko', 'all'].includes(target)) {
    throw new Error('--target must be parquet, oko, or all');
  }
  const paths = lakePaths();
  const selected = [];
  if (target === 'parquet' || target === 'all') {
    selected.push({ target: 'parquet', path: paths.parquet });
  }
  if (target === 'oko' || target === 'all') {
    selected.push({ target: 'oko', path: paths.okoExport });
    selected.push({ target: 'oko-staging', path: paths.okoStaging });
  }
  const lease = parsed.options.apply && selected.some((entry) => existsSync(entry.path))
    ? openWriterLease(paths.dataDir)
    : null;
  let report;
  try {
    report = selected.map((entry) => ({
      ...entry,
      exists: existsSync(entry.path),
      bytes: pathSize(entry.path),
      applied: Boolean(parsed.options.apply),
    }));
    if (parsed.options.apply) {
      for (const entry of selected) rmSync(entry.path, { recursive: true, force: true });
    }
  } finally {
    if (lease) lease.close();
  }
  if (parsed.options.json) writeJson(report);
  else {
    for (const entry of report) {
      process.stdout.write(
        (entry.applied ? 'removed' : 'would remove') + ' ' + entry.target + ': '
        + entry.path + ' (' + String(entry.bytes) + ' bytes)\n'
      );
    }
    if (!parsed.options.apply) process.stdout.write('preview only; add --apply to remove derived data\n');
  }
}

function cmdQuery(rest) {
  const parsed = parseOptions('query', rest, [], ['--json']);
  const sql = parsed.positionals.join(' ').trim();
  if (!sql) throw new Error('usage: transcript-lake query [--json] "<sql>"');
  runDuckQuery(sql, Boolean(parsed.options.json));
}

function cmdCompact(rest) {
  const parsed = parseOptions('compact', rest, ['--source'], ['--json']);
  if (parsed.positionals.length) throw new Error('compact accepts flags only');
  const source = requireRuntime(parsed.options.source);
  const dataDir = resolveDataDir({});
  let rows = partitionReport(dataDir);
  if (source) rows = rows.filter((row) => row.runtime === source);
  if (!rows.length) {
    throw new Error('no matching partitions under ' + join(dataDir, 'events') + ' (run ingest first)');
  }
  const lease = openWriterLease(dataDir);
  const report = [];
  try {
    for (const row of rows) {
      const runtimeName = 'runtime=' + row.runtime;
      const srcGlob = join(dataDir, 'events', runtimeName, '*', '*.ndjson');
      const outDir = join(dataDir, 'parquet', runtimeName);
      mkdirSync(outDir, { recursive: true });
      const outFile = join(outDir, 'events.parquet');
      const script = 'COPY (SELECT * FROM read_ndjson_auto(' + quoteSql(srcGlob)
        + ', filename=true)) TO ' + quoteSql(outFile) + ' (FORMAT PARQUET);';
      const status = runBinary('duckdb', ['-c', script]);
      if (status !== ZERO) {
        process.stderr.write('compact: duckdb failed for ' + runtimeName + '\n');
        process.exitCode = status;
        report.push({ runtime: row.runtime, sourceBytes: row.bytes, output: outFile, status });
        continue;
      }
      const result = {
        runtime: row.runtime,
        sourceBytes: row.bytes,
        parquetBytes: statSync(outFile).size,
        output: outFile,
        status,
      };
      report.push(result);
      if (!parsed.options.json) {
        process.stdout.write(
          result.runtime + ': ndjson ' + String(result.sourceBytes) + ' bytes -> parquet '
          + String(result.parquetBytes) + ' bytes (' + result.output + ')\n'
        );
      }
    }
  } finally {
    lease.close();
  }
  if (parsed.options.json) writeJson(report);
}

async function cmdExportOko(rest) {
  const parsed = parseOptions('export-oko', rest, [], ['--full', '--reindex']);
  if (parsed.positionals.length) throw new Error('export-oko accepts flags only');
  const exporter = await import('./oko_export.mjs');
  const summary = await exporter.exportOko({
    full: Boolean(parsed.options.full),
    reindex: Boolean(parsed.options.reindex),
    dataDir: resolveDataDir({}),
  });
  writeJson(summary);
  if (
    parsed.options.reindex
    && (!summary.reindex || !summary.reindex.ran || summary.reindex.status !== ZERO)
  ) {
    process.exitCode = ONE;
  }
}

function cmdOkoRefresh(rest) {
  requireNoArgs('oko-refresh', rest);
  const bin = process.env.OKO_CLI || findOnPath('oko-cli');
  if (!bin) {
    process.stderr.write(
      'oko-cli is not on PATH; install Oko or set OKO_CLI, then run: oko-cli transcripts reindex\n'
    );
    process.exitCode = ONE;
    return;
  }
  process.exitCode = runBinary(bin, ['transcripts', 'reindex']);
}

const COMMANDS = {
  paths: cmdPaths,
  sources: cmdSources,
  doctor: cmdDoctor,
  status: cmdStatus,
  ingest: cmdIngest,
  rebuild: cmdRebuild,
  watch: cmdWatch,
  sessions: cmdSessions,
  events: cmdEvents,
  search: cmdSearch,
  stats: cmdStats,
  hooks: cmdHooks,
  signals: cmdSignals,
  label: cmdLabel,
  query: cmdQuery,
  compact: cmdCompact,
  'export-oko': cmdExportOko,
  'oko-refresh': cmdOkoRefresh,
  clean: cmdClean,
  help: cmdHelp,
};

async function main() {
  try {
    const input = process.argv.slice(TWO);
    const args = [];
    let selectedDataDir = null;
    for (let index = ZERO; index < input.length; index += ONE) {
      const token = input[index];
      if (token !== '--data-dir') {
        args.push(token);
        continue;
      }
      if (selectedDataDir !== null) throw new Error('duplicate global --data-dir');
      const value = input[index + ONE];
      if (!value || value.startsWith('--')) throw new Error('--data-dir requires a path');
      selectedDataDir = resolve(value);
      index += ONE;
    }
    if (selectedDataDir !== null) process.env.LAKE_DATA = selectedDataDir;
    const [command, ...rest] = args;
    if (!command || command === '--help' || command === '-h') {
      process.stdout.write(USAGE + '\n');
      return;
    }
    if (command === '--version' || command === '-V') {
      process.stdout.write(VERSION + '\n');
      return;
    }
    if (rest.includes('--help') || rest.includes('-h')) {
      cmdHelp([command]);
      return;
    }
    const handler = COMMANDS[command];
    if (!handler) {
      process.stderr.write('error: unknown command: ' + command + '\n\n' + USAGE + '\n');
      process.exitCode = ONE;
      return;
    }
    await handler(rest);
  } catch (error) {
    process.stderr.write('error: ' + String(error && error.message ? error.message : error) + '\n');
    process.exitCode = ONE;
  }
}

await main();
