//! Transcript Lake command router: streaming, discovery, inspection,
//! analytics, recovery, derived artifacts, and Oko integration.
mod adapters;
mod args;
mod commands;
mod cursors;
mod duck;
mod hook_segments;
mod labels;
mod oko_export;
mod paths;
mod redact;
mod stream;
mod types;
mod util;

use std::process::ExitCode;

use util::{Error, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = concat!(
    "Transcript Lake creates a privacy-masked local event archive from coding-agent transcripts.\n",
    "\n",
    "Usage: transcript-lake [--data-dir <path>] <command> [flags]\n",
    "\n",
    "Start safely:\n",
    "  transcript-lake onboarding                    walk the first-use journey\n",
    "  transcript-lake paths                         show every local product path\n",
    "  transcript-lake sources                       discover supported transcript stores\n",
    "  transcript-lake status                        inspect Lake and stream state\n",
    "\n",
    "Stream and recover:\n",
    "  stream [--json]                               follow source appends in real time\n",
    "  rebuild --to <empty-path> [--source <runtime>] replay history to a new Lake\n",
    "\n",
    "Discover and inspect:\n",
    "  paths [--json]                                resolved state and integration paths\n",
    "  sources [--json]                              source availability and file counts\n",
    "  doctor [--json]                               state and dependency health checks\n",
    "  status [--json]                               partitions, cursors, stream, Oko\n",
    "\n",
    "Read and analyze:\n",
    "  sessions [--runtime <r>] [--project <text>] [--interrupted] [--limit <n>] [--json]\n",
    "  events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n",
    "  search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n",
    "  show <session-id> [--include <types>] [--limit <n>] [--json]\n",
    "  stats [--days <n>] [--runtime <r>] [--json]   usage summary\n",
    "  hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]\n",
    "  signals [--report <frustration|overlap|daily|freshness>] [--limit <n>] [--json]\n",
    "  query [--json] \"<sql>\"                        arbitrary DuckDB SQL over Lake views\n",
    "\n",
    "Label and annotate:\n",
    "  label add <session-id> --aspect <a> --value <v> [--note <text>] [--source <name[:detail]>] [--json]\n",
    "  label list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json]\n",
    "  label aspects [--json]                       distinct aspects with value counts\n",
    "  goal title (--text <text>|--stdin) [--json]   distill a local goal title\n",
    "  goal label <session-id> [--runtime <r>] [--json]  persist a model goal label\n",
    "\n",
    "Derived data and Oko:\n",
    "  compact [--source <runtime>] [--json]          write Parquet mirrors\n",
    "  rebuild-oko [--reindex]                        reconstruct Oko sessions\n",
    "  oko-refresh                                    reindex current export in Oko\n",
    "  clean [--target <parquet|oko|all>] [--apply] [--json]\n",
    "\n",
    "Guidance:\n",
    "  help [command]                                 command-specific help\n",
    "\n",
    "Global flags:\n",
    "  --data-dir <path>                              select state root for this invocation\n",
    "  -h, --help                                     show general or command help\n",
    "  -V, --version                                  print canonical product version\n",
    "\n",
    "State default: ~/.transcript-lake. Mutation is local; source stores are read-only.\n",
    "Help: https://github.com/wisent-ai/transcript-lake#readme",
);

/// Per-command syntax and safety guidance, printed by `help <command>` and by
/// `<command> --help`.
pub fn command_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "paths" => "paths [--json]\n  Print resolved Lake, derived-data, Tama, DuckDB, and Oko paths.",
        "sources" => "sources [--json]\n  Discover supported runtime roots and count candidate transcript files.",
        "doctor" => "doctor [--json]\n  Check cursor integrity, source discovery, and optional dependency presence.",
        "status" => "status [--json]\n  Show partitions, cursor freshness, live stream state, and Oko freshness.",
        "stream" => "stream [--json]\n  Follow supported source files continuously and project each append directly into the Lake and Oko.",
        "rebuild" => "rebuild --to <empty-path> [--source <runtime>]\n  Recovery replay into a different empty root; never mutates the current Lake.",
        "sessions" => "sessions [--runtime <r>] [--project <text>] [--interrupted] [--limit <n>] [--json]\n  List recent sessions through the canonical DuckDB view.\n  --interrupted keeps only conversations whose last turn was an unanswered user message or a tool call cut off mid-run, and reports stopped_as plus the opening of that final request.",
        "events" => "events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  List recent masked canonical events.",
        "search" => "search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  Case-insensitive literal substring match over masked event text, newest first.",
        "show" => "show <session-id> [--include <types>] [--limit <n>] [--json]\n  Reconstruct one conversation in full: masked event text, oldest turn first, no per-event truncation.\n  --include takes a comma-separated list of event types (user, assistant, thinking, tool_call, tool_result, meta, hook_decision) or \"all\"; the default is user,assistant.\n  The footer reports rendered and matched counts, so a --limit cut is always visible.",
        "stats" => "stats [--days <n>] [--runtime <r>] [--json]\n  Summarize events, sessions, tools, and token counters.",
        "hooks" => "hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]\n  Inspect adaptive-hook decisions.",
        "signals" => "signals [--report <frustration|overlap|daily|freshness>] [--limit <n>] [--json]\n  Query Oko/Lake cross-source signal views.",
        "label" => "label <add|list|aspects> ...\n  add <session-id> --aspect <name> --value <v> [--note <text>] [--source <name[:detail]>] [--json] records a session label with provenance (manual, human, model, or brama, optional :detail).\n  list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json] shows the latest assignment per session and aspect, newest first.\n  aspects [--json] summarizes distinct aspects, values, and labeled sessions.",
        "goal" => "goal <title|label> ...\n  title (--text <text>|--stdin) [--json] runs the SHA-pinned qualified GGUF locally.\n  label <session-id> [--runtime <r>] [--json] labels the first masked user prompt with source model:jeden-goal-qwen3-4b-2512d7a4.",
        "query" => "query [--json] \"<sql>\"\n  Execute operator-supplied SQL after loading canonical Lake views.",
        "compact" => "compact [--source <runtime>] [--json]\n  Rebuild per-runtime Parquet mirrors; NDJSON remains authoritative.",
        "rebuild-oko" => "rebuild-oko [--reindex]\n  Reconstruct every Oko session projection from authoritative Lake partitions.",
        "oko-refresh" => "oko-refresh\n  Invoke the compatible oko-cli transcript reindex command.",
        "clean" => "clean [--target <parquet|oko|all>] [--apply] [--json]\n  Preview by default; --apply removes rebuildable derived data only.",
        "onboarding" => "onboarding [--reset] [--yes] [--json]\n  Walk the published first-use journey this binary ships, one screen at a time, recording progress for this machine outside the Lake.\n  The journey completes when a real query over the Lake returns rows; until then it reports what is still missing and how to resume.\n  --reset discards the recorded attempt and replays the journey from its entry screen; --yes answers the Enter prompts; --json emits the whole walk as one object and never prompts.",
        "help" => "help [command]\n  Show general guidance or the exact syntax for one command.",
        _ => return None,
    })
}

fn cmd_help(rest: &[String]) -> Result<i32> {
    if rest.len() > 1 {
        return Err(Error("help accepts at most one command name".into()));
    }
    let Some(topic) = rest.first() else {
        println!("{USAGE}");
        return Ok(0);
    };
    let Some(text) = command_help(topic) else {
        return Err(Error(format!("unknown help topic: {topic}")));
    };
    println!("Usage: transcript-lake [--data-dir <path>] {text}");
    Ok(0)
}

fn dispatch(command: &str, rest: &[String]) -> Result<i32> {
    match command {
        "paths" => commands::inspect::paths(rest),
        "sources" => commands::inspect::sources(rest),
        "doctor" => commands::inspect::doctor(rest),
        "status" => commands::inspect::status(rest),
        "stream" => commands::stream::stream(rest),
        "rebuild" => commands::stream::rebuild(rest),
        "sessions" => commands::read::sessions(rest),
        "events" => commands::read::events(rest),
        "search" => commands::read::search(rest),
        "show" => commands::read::show(rest),
        "stats" => commands::read::stats(rest),
        "hooks" => commands::read::hooks(rest),
        "signals" => commands::read::signals(rest),
        "label" => commands::label::label(rest),
        "goal" => commands::goal::goal(rest),
        "query" => commands::read::query(rest),
        "compact" => commands::derived::compact(rest),
        "rebuild-oko" => commands::derived::rebuild_oko(rest),
        "oko-refresh" => commands::derived::oko_refresh(rest),
        "clean" => commands::derived::clean(rest),
        "onboarding" => commands::onboarding::onboarding(rest),
        "help" => cmd_help(rest),
        _ => unreachable!("dispatch called with an unrouted command"),
    }
}

fn run() -> Result<i32> {
    let input: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<String> = Vec::new();
    let mut selected_data_dir: Option<String> = None;
    let mut index = 0;
    while index < input.len() {
        let token = &input[index];
        if token != "--data-dir" {
            args.push(token.clone());
            index += 1;
            continue;
        }
        if selected_data_dir.is_some() {
            return Err(Error("duplicate global --data-dir".into()));
        }
        match input.get(index + 1) {
            Some(value) if !value.is_empty() && !value.starts_with("--") => {
                selected_data_dir = Some(util::absolute(value).to_string_lossy().to_string());
            }
            _ => return Err(Error("--data-dir requires a path".into())),
        }
        index += 2;
    }
    if let Some(selected) = selected_data_dir {
        // Downstream resolution reads LAKE_DATA, so one selection covers the
        // command, the DuckDB views, and any child process this run spawns.
        std::env::set_var("LAKE_DATA", selected);
    }
    let Some((command, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(0);
    };
    if command == "--help" || command == "-h" {
        println!("{USAGE}");
        return Ok(0);
    }
    if command == "--version" || command == "-V" {
        println!("{VERSION}");
        return Ok(0);
    }
    if rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        return cmd_help(&[command.clone()]);
    }
    if command_help(command).is_none() && command != "help" {
        eprintln!("error: unknown command: {command}\n\n{USAGE}");
        return Ok(1);
    }
    dispatch(command, rest)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
