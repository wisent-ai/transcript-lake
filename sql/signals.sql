-- transcript-lake / sql/signals.sql
-- Cross-source signal queries: lake views joined to the Oko transcript index.
-- Load sql/views.sql first (after SET VARIABLE lake_data): the queries below
-- reference the lake views (sessions, hook_decisions, events).
-- The preamble and named view definitions load as one installed command asset;
-- `transcript-lake signals --report <name>` selects one view afterward.
--
-- REQUIRES-OKO: statements tagged like this read the attached Oko index and
-- fail gracefully when the sqlite database is absent on this machine; the
-- lake-only views keep working regardless.

INSTALL sqlite;
LOAD sqlite;

-- REQUIRES-OKO: attach the Oko transcript index read-only. When the database
-- file is missing this ATTACH is the statement that fails; nothing is
-- created or modified either way.
--
-- ATTACH takes a string literal, not getvariable(), and DuckDB does not
-- expand a leading tilde, so the CLI substitutes __OKO_DB__ with the resolved
-- absolute path before handing this script to DuckDB.
ATTACH IF NOT EXISTS
  '__OKO_DB__'
  AS oko (TYPE sqlite, READ_ONLY);

-- REQUIRES-OKO: freshness comparison. Newest activity the Oko index knows
-- about (session mtime, epoch seconds) against the newest event per lake
-- runtime, so drift between the two pipelines is visible at a glance.
-- items = indexed conversation count on the Oko row, distinct ingested
-- conversation count on each lake row.
CREATE OR REPLACE VIEW oko_lake_freshness AS
SELECT
  'oko-index' AS source,
  CAST(to_timestamp(max(mtime)) AS TIMESTAMP) AS newest_activity,
  count(*) AS items
FROM oko.sessions
UNION ALL
SELECT
  'lake:' || runtime AS source,
  max(ts) AS newest_activity,
  count(DISTINCT session_id) AS items
FROM events
GROUP BY runtime
ORDER BY newest_activity DESC NULLS LAST;
