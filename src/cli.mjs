#!/usr/bin/env node
// Transcript-lake command router (frozen interface, see the build contract).
// Commands: ingest [--source r] [--full] | status | query "<sql>" | compact |
// export-oko [--full] [--reindex] | oko-refresh. Query and compact shell out
// to DuckDB; every ingest also refreshes Oko's per-session import view.
// No digit characters outside quoted strings and comments.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ingest, resolveDataDir } from './ingest.mjs';

const N = (s) => Number(s);
const [ZERO, ONE, TWO] = ['0', '1', '2'].map(N);
const LAKE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const PACKAGE = JSON.parse(readFileSync(join(LAKE_ROOT, 'package.json'), 'utf8'));
const VERSION = String(PACKAGE.version);
const SUMMARY_FILE = 'last-ingest.json';
const USAGE = [
  'Transcript Lake creates a privacy-masked local event archive from coding-agent transcripts.',
  '',
  'Usage: transcript-lake <command> [flags]',
  '',
  'Start safely:',
  '  transcript-lake status                inspect configuration without ingesting',
  '  transcript-lake ingest                incrementally ingest supported local stores',
  '',
  'Commands:',
  '  ingest [--source <runtime>] [--full]  incremental scan into the Lake',
  '  status                                partition, cursor, masking report',
  '  query "<sql>"                         run SQL over Lake views via DuckDB',
  '  compact                               write Parquet copies per runtime',
  '  export-oko [--full] [--reindex]       materialize every runtime for Oko',
  '  oko-refresh                           ask oko-cli to reindex transcripts',
  '',
  'Global flags:',
  '  -h, --help                            show this guidance',
  '  -V, --version                         print the canonical product version',
  '',
  'State: LAKE_DATA selects the mutable root; default ~/.transcript-lake.',
  'Help:  https://github.com/wisent-ai/transcript-lake#readme',
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

async function cmdIngest(rest) {
  const queue = [...rest];
  let source = null;
  let full = false;
  while (queue.length) {
    const flag = queue.shift();
    if (flag === '--source') {
      const value = queue.shift();
      if (!value) throw new Error('--source requires a runtime name');
      source = value;
    } else if (flag === '--full') {
      full = true;
    } else {
      throw new Error('unknown ingest flag: ' + flag);
    }
  }
  const summary = await ingest({ source, full });
  const exporter = await import('./oko_export.mjs');
  const okoExport = await exporter.exportOko({ full });
  const record = { finishedAt: new Date().toISOString(), source, full, okoExport, ...summary };
  const dataDir = resolveDataDir({});
  mkdirSync(dataDir, { recursive: true });
  writeFileSync(join(dataDir, SUMMARY_FILE), JSON.stringify(record, null, TWO) + '\n');
  process.stdout.write(JSON.stringify(record, null, TWO) + '\n');
  if (summary.partial) process.exitCode = ONE;
}

async function cmdStatus(rest) {
  requireNoArgs('status', rest);
  const dataDir = resolveDataDir({});
  process.stdout.write('data dir: ' + dataDir + '\n');
  const rows = partitionReport(dataDir);
  if (!rows.length) process.stdout.write('partitions: none (run ingest first)\n');
  for (const row of rows) {
    process.stdout.write('  ' + row.runtime + ': ' + String(row.parts) + ' partition files, ' + String(row.bytes) + ' bytes\n');
  }
  const cursorsPath = join(dataDir, 'cursors.json');
  if (!existsSync(cursorsPath)) {
    process.stdout.write('cursors: none\n');
  } else {
    try {
      const store = JSON.parse(readFileSync(cursorsPath, 'utf8'));
      const stamps = Object.values(store).map((rec) => Number(rec && rec.mtimeMs)).filter(Number.isFinite);
      const newest = stamps.length ? new Date(Math.max(...stamps)).toISOString() : 'n/a';
      process.stdout.write('cursors: ' + String(Object.keys(store).length) + ' tracked files, newest source mtime ' + newest + '\n');
    } catch (error) {
      process.stdout.write('cursors: unreadable (' + String(error) + ')\n');
    }
  }
  const summaryPath = join(dataDir, SUMMARY_FILE);
  if (!existsSync(summaryPath)) {
    process.stdout.write('last ingest: none recorded\n');
  } else {
    try {
      const summary = JSON.parse(readFileSync(summaryPath, 'utf8'));
      process.stdout.write('last ingest: ' + String(summary.finishedAt) + ', mask counts ' + JSON.stringify(summary.maskCounts) + '\n');
    } catch (error) {
      process.stdout.write('last ingest: unreadable (' + String(error) + ')\n');
    }
  }
  try {
    const exporter = await import('./oko_export.mjs');
    const report = exporter.freshness();
    process.stdout.write('oko: ' + (typeof report === 'string' ? report : JSON.stringify(report)) + '\n');
  } catch (error) {
    process.stdout.write('oko: freshness unavailable (' + String(error && error.message ? error.message : error) + ')\n');
  }
}

function cmdQuery(rest) {
  const sql = rest.join(' ').trim();
  if (!sql) throw new Error('usage: node src/cli.mjs query "<sql>"');
  const viewsPath = join(LAKE_ROOT, 'sql', 'views.sql');
  if (!existsSync(viewsPath)) throw new Error('missing ' + viewsPath + ' (views are built separately)');
  const views = readFileSync(viewsPath, 'utf8');
  const dataDir = resolveDataDir({});
  const script = 'SET VARIABLE lake_data = ' + quoteSql(dataDir) + ';\n' + views + '\n' + sql;
  process.exitCode = runBinary('duckdb', ['-c', script]);
}

function cmdCompact(rest) {
  requireNoArgs('compact', rest);
  const dataDir = resolveDataDir({});
  const rows = partitionReport(dataDir);
  if (!rows.length) throw new Error('no partitions under ' + join(dataDir, 'events') + ' (run ingest first)');
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
      continue;
    }
    const parquetBytes = statSync(outFile).size;
    process.stdout.write(row.runtime + ': ndjson ' + String(row.bytes) + ' bytes -> parquet ' + String(parquetBytes) + ' bytes (' + outFile + ')\n');
  }
}

async function cmdExportOko(rest) {
  for (const flag of rest) {
    if (flag !== '--full' && flag !== '--reindex') {
      throw new Error('unknown export-oko flag: ' + flag);
    }
  }
  const exporter = await import('./oko_export.mjs');
  const summary = await exporter.exportOko({
    full: rest.includes('--full'),
    reindex: rest.includes('--reindex'),
  });
  process.stdout.write(JSON.stringify(summary, null, TWO) + '\n');
  if (
    rest.includes('--reindex')
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
  ingest: cmdIngest,
  status: cmdStatus,
  query: cmdQuery,
  compact: cmdCompact,
  'export-oko': cmdExportOko,
  'oko-refresh': cmdOkoRefresh,
};

async function main() {
  const [command, ...rest] = process.argv.slice(TWO);
  if (!command || command === 'help' || command === '--help' || command === '-h') {
    process.stdout.write(USAGE + '\n');
    return;
  }
  if (command === '--version' || command === '-V') {
    process.stdout.write(VERSION + '\n');
    return;
  }
  const handler = COMMANDS[command];
  if (!handler) {
    process.stderr.write('error: unknown command: ' + command + '\n\n' + USAGE + '\n');
    process.exitCode = ONE;
    return;
  }
  try {
    await handler(rest);
  } catch (error) {
    process.stderr.write('error: ' + String(error && error.message ? error.message : error) + '\n');
    process.exitCode = ONE;
  }
}

await main();
