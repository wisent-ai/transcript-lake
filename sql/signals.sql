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
ATTACH IF NOT EXISTS
  '~/Library/Application Support/Oko/transcript-index.sqlite'
  AS oko (TYPE sqlite, READ_ONLY);

-- REQUIRES-OKO: frustration leaderboard. Oko tallies frustration terms per
-- session (word, group, severity, count); weight them by severity and join
-- back to the lake view for runtime / project / span / message volume.
-- Oko cwd fills in when the lake has not ingested that conversation yet.
CREATE OR REPLACE VIEW oko_frustration AS
SELECT
  f.sessionId AS session_id,
  sum(f."count" * CASE f.severity
        WHEN 'strong'   THEN CAST('3' AS INTEGER)
        WHEN 'moderate' THEN CAST('2' AS INTEGER)
        ELSE CAST('1' AS INTEGER)
      END) AS frustration_score,
  sum(f."count") AS term_hits,
  count(DISTINCT f.word) AS distinct_terms,
  coalesce(s.runtime, CASE WHEN starts_with(f.sessionId, 'rollout-') THEN 'codex' END) AS runtime,
  coalesce(s.project, o.cwd) AS project,
  s.first_ts,
  s.last_ts,
  s.user_msgs
FROM oko.transcript_frustration_terms AS f
LEFT JOIN oko.sessions AS o ON o.sessionId = f.sessionId
LEFT JOIN sessions AS s ON (s.session_id = f.sessionId OR s.oko_session_hash = sha256(f.sessionId))
GROUP BY f.sessionId, s.runtime, s.project, o.cwd,
         s.first_ts, s.last_ts, s.user_msgs
ORDER BY frustration_score DESC
LIMIT CAST('25' AS INTEGER);

-- REQUIRES-OKO: hook-block vs frustration overlap. How many conversations
-- were blocked by an adaptive hook, how many show frustration terms in the
-- Oko index, and how many are both. A large overlap suggests blocking
-- pressure and user frustration travel together.
CREATE OR REPLACE VIEW hook_frustration_overlap AS
WITH hook_blocked AS (
  SELECT DISTINCT session_id
  FROM hook_decisions
  WHERE decision = 'block' AND session_id IS NOT NULL
),
frustrated AS (
  SELECT DISTINCT sessionId AS session_id
  FROM oko.transcript_frustration_terms
)
SELECT
  (SELECT count(*) FROM hook_blocked) AS hook_blocked_sessions,
  (SELECT count(*) FROM frustrated)   AS frustrated_sessions,
  (SELECT count(*)
     FROM hook_blocked
     JOIN frustrated USING (session_id)) AS overlap_sessions;

-- REQUIRES-OKO: the same correlation as a per-day series. Frustration days
-- come from the term tally last_seen clock (epoch seconds); block days come
-- from lake hook telemetry. FULL JOIN keeps days present on only one side.
CREATE OR REPLACE VIEW hook_frustration_daily AS
WITH blocks_daily AS (
  SELECT
    CAST(ts AS DATE) AS day,
    count(*) AS hook_blocks,
    count(DISTINCT session_id) AS blocked_sessions
  FROM hook_decisions
  WHERE decision = 'block'
  GROUP BY day
),
frustration_daily AS (
  SELECT
    CAST(to_timestamp(last_seen) AS DATE) AS day,
    count(DISTINCT sessionId) AS frustrated_sessions,
    sum("count") AS term_hits
  FROM oko.transcript_frustration_terms
  WHERE last_seen IS NOT NULL
  GROUP BY day
)
SELECT
  day,
  coalesce(hook_blocks, CAST('0' AS BIGINT))         AS hook_blocks,
  coalesce(blocked_sessions, CAST('0' AS BIGINT))    AS blocked_sessions,
  coalesce(frustrated_sessions, CAST('0' AS BIGINT)) AS frustrated_sessions,
  coalesce(term_hits, CAST('0' AS BIGINT))           AS term_hits
FROM blocks_daily
FULL JOIN frustration_daily USING (day)
ORDER BY day DESC
LIMIT CAST('30' AS INTEGER);

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
