-- Devin CLI keeps every local session in one SQLite store,
-- <data>/devin/cli/sessions.db: a `sessions` row per conversation and its
-- nodes in `message_nodes`. The node table in order is not the conversation —
-- the agent rebuilds its context chain whenever the system prefix changes,
-- so several copies share their message_id and sessions.main_chain_id names
-- the head of the live one. Every timestamp is unix seconds.
--
-- Row 1 (registry-devin) is one full conversation: a rebuilt prefix pair,
-- a user turn, an exec tool call and its tool result with an exit code, an
-- edit call, one unmarked system message (injected hook context), and a
-- compacted replay of the user turn under the same message_id — the dedupe
-- the chain walk exists for.
-- Row 2 (registry-devin-sub) has the main chain plus two side chains the
-- main one cannot reach: the run_subagent run subagent_heads declares, and
-- an orphan chain a rewind left behind that no head row names.
-- Row 3 (registry-devin-hidden) is hidden=1 and must not be read.

create table sessions (
    id text primary key,
    working_directory text,
    backend_type text,
    model text,
    agent_mode text,
    created_at integer,
    last_activity_at integer,
    title text,
    main_chain_id integer,
    shell_last_seen_index integer,
    cogs_json text,
    workspace_dirs text,
    hidden integer not null default 0,
    metadata text
);

create table message_nodes (
    row_id integer primary key autoincrement,
    session_id text not null,
    node_id integer not null,
    parent_node_id integer,
    chat_message text not null,
    created_at integer,
    metadata text
);

create table subagent_heads (
    session_id text not null,
    agent_id text not null,
    chain_node_id integer not null,
    updated_at integer not null,
    primary key (session_id, agent_id)
);

create table prompt_history (
    id integer primary key autoincrement,
    content text,
    timestamp integer,
    session_id text,
    is_shell integer not null default 0
);

insert into sessions values (
    'registry-devin', '/w/api', 'cli', 'swe-2', 'normal',
    1785600000, 1785600090, 'registry devin session', 8, 0, null, '["/w/api"]', 0, null
);
insert into sessions values (
    'registry-devin-sub', '/w/api', 'cli', 'swe-2', 'normal',
    1785600100, 1785600150, 'registry devin parent', 13, 0, null, '["/w/api"]', 0, null
);
insert into sessions values (
    'registry-devin-hidden', '/w/api', 'cli', 'swe-2', 'normal',
    1785600200, 1785600250, 'hidden session', 16, 0, null, null, 1, null
);

-- registry-devin: nodes 1-2 are the first prefix pair (is_system_prefix), 3 the
-- user turn, 4-5 the exec call and its tool result, 6 an edit call, 7 injected
-- hook context (a system row with no flag), 8 the rebuilt chain's head — a
-- replay of node 3 under the same message_id after compaction. main_chain_id
-- names 8, and the chain it heads holds 3's copy — which is why the walk
-- dedupes message_id rather than trusting the table order.
insert into message_nodes (session_id, node_id, parent_node_id, chat_message, created_at, metadata) values
('registry-devin', 1, null,
 '{"message_id":"sys-1","role":"system","content":"<system_info>registry</system_info>"}',
 1785600001, '{"is_system_prefix":true}'),
('registry-devin', 2, 1,
 '{"message_id":"sys-2","role":"system","content":"<rules>registry</rules>"}',
 1785600002, '{"is_system_prefix":true}'),
('registry-devin', 3, 2,
 '{"message_id":"u-1","role":"user","content":"the retry loop drops the last attempt"}',
 1785600003, null),
('registry-devin', 4, 3,
 '{"message_id":"a-1","role":"assistant","content":"checking","tool_calls":[{"id":"call_1","name":"exec","arguments":{"command":"go test ./internal/loop"},"index":0,"kind":"function"}]}',
 1785600004, null),
('registry-devin', 5, 4,
 '{"message_id":"t-1","role":"tool","tool_call_id":"call_1","content":"Output from command in shell abc:\nFAIL attempt three\n\n\nExit code: 1","metadata":{"extensions":{"chisel/terminal_output":{"exit":{"exit_code":1}}}}}',
 1785600005, null),
('registry-devin', 6, 5,
 '{"message_id":"a-2","role":"assistant","content":"","tool_calls":[{"id":"call_2","name":"edit","arguments":{"file_path":"/w/api/loop.go","old_string":"if i < n {","new_string":"if i <= n {"},"index":0,"kind":"function"}]}',
 1785600006, null),
('registry-devin', 7, 6,
 '{"message_id":"inj-1","role":"system","content":"DEJA-REGISTRY-INJECTED-CONTEXT"}',
 1785600007, null),
('registry-devin', 8, 7,
 '{"message_id":"u-1","role":"user","content":"the retry loop drops the last attempt"}',
 1785600008, '{"extensions":{"compact/prior_node_ids":[3]}}');

-- registry-devin-sub: 9-13 the main chain, 14-15 the subagent's own chain —
-- unreachable from the main head, rooted on a null parent the way every
-- chain in the table starts, and declared by its subagent_heads row. Nodes
-- 16-17 are a chain a rewind orphaned: new message_ids, no head row, still
-- a conversation the harness wrote.
insert into message_nodes (session_id, node_id, parent_node_id, chat_message, created_at, metadata) values
('registry-devin-sub', 9, null,
 '{"message_id":"p-sys","role":"system","content":"<system_info>parent</system_info>"}',
 1785600101, '{"is_system_prefix":true}'),
('registry-devin-sub', 10, 9,
 '{"message_id":"p-u","role":"user","content":"run the worker"}',
 1785600102, null),
('registry-devin-sub', 11, 10,
 '{"message_id":"p-a","role":"assistant","content":"","tool_calls":[{"id":"call_9","name":"run_subagent","arguments":{"instructions":"echo registry-worker"},"index":0,"kind":"function"}]}',
 1785600103, null),
('registry-devin-sub', 12, 11,
 '{"message_id":"p-t","role":"tool","tool_call_id":"call_9","content":"Background subagent started"}',
 1785600104, null),
('registry-devin-sub', 13, 12,
 '{"message_id":"p-a2","role":"assistant","content":"worker done"}',
 1785600105, null),
('registry-devin-sub', 14, null,
 '{"message_id":"w-u","role":"user","content":"echo registry-worker"}',
 1785600106, null),
('registry-devin-sub', 15, 14,
 '{"message_id":"w-a","role":"assistant","content":"registry-worker printed"}',
 1785600107, null),
('registry-devin-sub', 16, null,
 '{"message_id":"rw-u","role":"user","content":"earlier draft of the ask"}',
 1785600108, null),
('registry-devin-sub', 17, 16,
 '{"message_id":"rw-a","role":"assistant","content":"earlier draft of the answer"}',
 1785600109, null);

insert into subagent_heads values ('registry-devin-sub', 'ag-worker', 15, 1785600107);

-- registry-devin-hidden: one ordinary chain under a hidden row.
insert into message_nodes (session_id, node_id, parent_node_id, chat_message, created_at, metadata) values
('registry-devin-hidden', 16, null,
 '{"message_id":"h-u","role":"user","content":"must not be read"}',
 1785600201, null);

-- prompt_history carries the inline shell commands the user ran between
-- turns: is_shell=1 rows index as commands when the line is worth keeping.
insert into prompt_history (content, timestamp, session_id, is_shell) values
('the retry loop drops the last attempt', 1785600003, 'registry-devin', 0),
('git log --oneline -5', 1785600040, 'registry-devin', 1),
('make build', 1785600041, 'registry-devin', 1),
('ls -la', 1785600042, 'registry-devin', 1);
