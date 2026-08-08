-- transcript-lake / sql/views.sql
-- Canonical DuckDB views over the masked NDJSON event partitions.
-- The caller (src/cli.mjs query) MUST run
--   SET VARIABLE lake_data = '<data dir>';
-- before sourcing this file. Views read the partition glob
--   <lake_data>/events/runtime=<r>/date=<day>/part-<hash>.ndjson
--
-- Empty-lake bootstrap: DuckDB binds a view at CREATE time and a glob that
-- matches zero files is an IO error, so this script first materialises a
-- zero-row stub file at a fixed scratch path, then points the reader at the
-- live glob whenever at least one partition file exists and at the stub
-- otherwise. Once any partition exists the views read the glob directly, so
-- partition files created later are visible without reloading this script.

SET VARIABLE lake_glob =
  coalesce(getvariable('lake_data'), '.') || '/events/*/*/*.ndjson';

COPY (SELECT NULL AS placeholder WHERE false)
  TO '/tmp/transcript-lake-empty-stub.ndjson' (FORMAT json);

SET VARIABLE lake_events_src = (
  SELECT CASE
           WHEN count(*) >= CAST('1' AS BIGINT) THEN getvariable('lake_glob')
           ELSE '/tmp/transcript-lake-empty-stub.ndjson'
         END
  FROM glob(getvariable('lake_glob'))
);

-- The canonical event schema is frozen, so inference is disabled via an
-- explicit column list: cross-partition drift can never re-type a column.
-- filename=true exposes the source partition file for every row and
-- ignore_errors=true skips a torn final line while an ingest is appending.
CREATE OR REPLACE VIEW events AS
SELECT
  ts, runtime, machine, session_id, project, event_type, text,
  tool_name, model, tokens_in, tokens_out, extra, filename
FROM read_ndjson_auto(
  getvariable('lake_events_src'),
  filename = true,
  ignore_errors = true,
  columns = {
    ts: 'TIMESTAMP',
    runtime: 'VARCHAR',
    machine: 'VARCHAR',
    session_id: 'VARCHAR',
    project: 'VARCHAR',
    event_type: 'VARCHAR',
    text: 'VARCHAR',
    tool_name: 'VARCHAR',
    model: 'VARCHAR',
    tokens_in: 'BIGINT',
    tokens_out: 'BIGINT',
    extra: 'JSON'
  }
);

-- One row per session: identity, span, message mix, summed usage counters.
CREATE OR REPLACE VIEW sessions AS
SELECT
  runtime,
  session_id,
  max(project)  AS project,
  min(ts)       AS first_ts,
  max(ts)       AS last_ts,
  count(*) FILTER (WHERE event_type = 'user')      AS user_msgs,
  count(*) FILTER (WHERE event_type = 'assistant') AS assistant_msgs,
  count(*) FILTER (WHERE event_type = 'tool_call') AS tool_calls,
  sum(tokens_in)  AS tokens_in,
  sum(tokens_out) AS tokens_out,
  -- One-way alias for Oko runtimes keyed by source filename rather than the
  -- source runtime's native session id.
  max(extra ->> '$.source_stem_hash') AS oko_session_hash
FROM events
WHERE session_id IS NOT NULL
GROUP BY runtime, session_id;

-- Tool usage per day / runtime / tool.
CREATE OR REPLACE VIEW tools_daily AS
SELECT
  CAST(ts AS DATE) AS day,
  runtime,
  tool_name,
  count(*) AS calls,
  count(DISTINCT session_id) AS sessions
FROM events
WHERE event_type = 'tool_call' AND tool_name IS NOT NULL
GROUP BY day, runtime, tool_name;

-- Usage counters per day / runtime / model.
CREATE OR REPLACE VIEW tokens_daily AS
SELECT
  CAST(ts AS DATE) AS day,
  runtime,
  model,
  sum(tokens_in)  AS tokens_in,
  sum(tokens_out) AS tokens_out,
  count(DISTINCT session_id) AS sessions
FROM events
WHERE tokens_in IS NOT NULL OR tokens_out IS NOT NULL
GROUP BY day, runtime, model;

-- Hook telemetry stream: one row per adaptive-hook decision.
-- extra carries decision / event / infra passed through by the ingest driver;
-- tool_name is the hook id and text is the (masked) reason.
CREATE OR REPLACE VIEW hook_decisions AS
SELECT
  ts,
  session_id,
  project,
  tool_name AS hook_id,
  extra ->> '$.decision' AS decision,
  extra ->> '$.event'    AS hook_event,
  extra ->> '$.infra'    AS infra,
  text AS reason
FROM events
WHERE runtime = 'hooks' AND event_type = 'hook_decision';

-- Blocking pressure per hook id.
CREATE OR REPLACE VIEW blocks_by_hook AS
SELECT
  hook_id,
  count(*) AS blocks,
  count(DISTINCT session_id) AS sessions,
  min(ts) AS first_block_ts,
  max(ts) AS last_block_ts,
  arg_max(reason, ts) AS last_reason
FROM hook_decisions
WHERE decision = 'block'
GROUP BY hook_id;

-- Conversations that stopped without an answer. The last recorded turn is
-- either a user message the agent never replied to, or a tool call whose run
-- was cut off before the agent spoke again. Newest first, so the top row is
-- the conversation most recently left unfinished. last_user_text carries the
-- masked opening of that final request, which is what identifies the thread.
CREATE OR REPLACE VIEW interrupted_sessions AS
WITH turns AS (
  SELECT
    runtime, session_id, ts, event_type, text,
    row_number() OVER (
      PARTITION BY runtime, session_id ORDER BY ts DESC
    ) AS rn_session,
    row_number() OVER (
      PARTITION BY runtime, session_id, event_type ORDER BY ts DESC
    ) AS rn_kind
  FROM events
  WHERE session_id IS NOT NULL
    AND event_type IN ('user', 'assistant', 'tool_call')
),
tail AS (
  SELECT runtime, session_id, event_type
  FROM turns
  WHERE rn_session = CAST('1' AS BIGINT) AND event_type <> 'assistant'
),
final_request AS (
  SELECT runtime, session_id, text
  FROM turns
  WHERE event_type = 'user' AND rn_kind = CAST('1' AS BIGINT)
)
SELECT
  s.runtime,
  s.session_id,
  s.project,
  CASE t.event_type
    WHEN 'user' THEN 'unanswered'
    ELSE 'cut_off_mid_tool'
  END AS stopped_as,
  s.first_ts,
  s.last_ts,
  s.user_msgs,
  s.assistant_msgs,
  s.tool_calls,
  substr(r.text, CAST('1' AS INTEGER), CAST('240' AS INTEGER)) AS last_user_text
FROM tail AS t
JOIN sessions AS s ON s.runtime = t.runtime AND s.session_id = t.session_id
LEFT JOIN final_request AS r ON r.runtime = t.runtime AND r.session_id = t.session_id
ORDER BY s.last_ts DESC;

-- Operator label store: one row per aspect/value assignment over a session,
-- appended by transcript-lake label add beneath <lake_data>/labels/.
-- The store is append-only; re-labeling a session and aspect adds a row and
-- the latest assignment wins in CLI reads, while this view exposes the full
-- history. Same empty-store stub and torn-final-line tolerance as events.
SET VARIABLE lake_labels_glob =
  coalesce(getvariable('lake_data'), '.') || '/labels/*.ndjson';

SET VARIABLE lake_labels_src = (
  SELECT CASE
           WHEN count(*) >= CAST('1' AS BIGINT) THEN getvariable('lake_labels_glob')
           ELSE '/tmp/transcript-lake-empty-stub.ndjson'
         END
  FROM glob(getvariable('lake_labels_glob'))
);

CREATE OR REPLACE VIEW labels AS
SELECT
  ts, session_id, runtime, aspect, value, note, source, filename
FROM read_ndjson_auto(
  getvariable('lake_labels_src'),
  filename = true,
  ignore_errors = true,
  columns = {
    ts: 'TIMESTAMP',
    session_id: 'VARCHAR',
    runtime: 'VARCHAR',
    aspect: 'VARCHAR',
    value: 'VARCHAR',
    note: 'VARCHAR',
    source: 'VARCHAR'
  }
);
