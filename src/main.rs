//! Transcript Lake command router: streaming, discovery, inspection,
//! analytics, recovery, derived artifacts, and Oko integration.
mod adapters;
mod args;
mod commands;
mod cursors;
mod hook_segments;
mod lake;
mod oko_export;
mod record;
mod stream;
mod util;

/// The Lake's own modules keep their familiar paths: `crate::paths`,
/// `crate::duck`, `crate::sources`, `crate::types`, `crate::redact` and
/// `crate::labels` are where every caller already looks for them.
pub use lake::{duck, paths, sources};
pub use record::{labels, redact, types};

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
    "  transcript-lake adopt --source <runtime> --root <path>  ingest and select one discovered source\n",
    "  transcript-lake status                        inspect Lake and stream state\n",
    "\n",
    "Stream and recover:\n",
    "  stream [--json]                               follow source appends in real time\n",
    "  rebuild --to <empty-path> [--source <runtime>] replay history to a new Lake\n",
    "  reconcile --source <runtime> [--apply] [--reindex] supersede rows the current adapter projects differently\n",
    "\n",
    "Discover and inspect:\n",
    "  paths [--json]                                resolved state and integration paths\n",
    "  sources [--json]                              source availability and file counts\n",
    "  adopt --source <runtime> --root <path> [--json] validate, ingest, and select a source\n",
    "  doctor [--json]                               state and dependency health checks\n",
    "  status [--json]                               partitions, cursors, stream, Oko\n",
    "\n",
    "Read and analyze:\n",
    "  sessions [--runtime <r>] [--project <text>] [--interrupted] [--limit <n>] [--json]\n",
    "  events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n",
    "  search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n",
    "  show <session-id> [--include <types>] [--limit <n>] [--out <path>] [--json]\n",
    "  stats [--days <n>] [--runtime <r>] [--json]   usage summary\n",
    "  hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]\n",
    "  signals [--report freshness] [--limit <n>] [--json]\n",
    "  query [--json] \"<sql>\"                        arbitrary DuckDB SQL over Lake views\n",
    "\n",
    "Label and annotate:\n",
    "  label add <session-id> --aspect <a> --value <v> [--note <text>] [--source <name[:detail]>] [--json]\n",
    "  label remove <session-id> --aspect <a> [--note <text>] [--source <name[:detail]>] [--runtime <r>] [--json]\n",
    "  label list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json]\n",
    "  label aspects [--json]                       distinct aspects with value counts\n",
    "  goal title (--text <text>|--stdin) [--json]   distill a local goal title\n",
    "  goal label <session-id> [--runtime <r>] [--json]  persist a model goal label\n",
    "\n",
    "Derived data and projections:\n",
    "  compact [--source <runtime>] [--json]          write Parquet mirrors\n",
    "  projection rebuild --target <oko> [--reindex]  reconstruct the per-session projection one consumer imports\n",
    "  projection refresh --target <oko>              hand the current projection to that consumer's own CLI\n",
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
        "adopt" => "adopt --source <claude|codex|omp|droid|kimi> --root <discovered-root> [--json]\n  Validate every complete source record, ingest through the canonical masking/cursor/Oko boundary, then persist this root as the selected source. Repeating the same source resumes by native file identity and reports unchanged files.",
        "doctor" => "doctor [--json]\n  Check cursor integrity, source discovery, and optional dependency presence.",
        "status" => "status [--json]\n  Show partitions, cursor freshness, live stream state, and Oko freshness.",
        "stream" => "stream [--json]\n  Follow supported source files continuously and project each append directly into the Lake and Oko.",
        "rebuild" => "rebuild --to <empty-path> [--source <runtime>]\n  Recovery replay into a different empty root; never mutates the current Lake.",
        "reconcile" => "reconcile --source <runtime> [--apply] [--reindex]\n  Replay every retained original of one runtime through the current adapter into a staging root and compare each canonical source partition with it. Prints one JSON report: unchanged, changed (rows superseded and replayed per partition, with its sessions), unprovable (no original on this machine reproduces the partition; kept and never reclassified) and refused (rows recorded on another machine).\n  --apply replaces every changed partition whole under the writer lease, moves the stream cursors to the replayed ones, removes the runtime's Parquet mirror and rebuilds the Oko projection; it is refused when the replay could not read an original or a partition is refused. --reindex (with --apply) then runs `oko-cli transcripts reindex`.\n  Exits 1 when any partition is unprovable or refused or the replay was partial.",
        "sessions" => "sessions [--runtime <r>] [--project <text>] [--interrupted] [--limit <n>] [--json]\n  List sessions, newest first, through the canonical DuckDB view; every matching session unless --limit keeps the newest n (zero is refused).\n  --interrupted keeps only conversations whose last turn was an unanswered user message or a tool call cut off mid-run, and reports stopped_as plus the opening of that final request.",
        "events" => "events [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  List masked canonical events with their whole text, newest first; every matching event unless --limit keeps the newest n (zero is refused).",
        "search" => "search <text> [--runtime <r>] [--session <id>] [--type <type>] [--limit <n>] [--json]\n  Case-insensitive literal substring match over masked event text, newest first, each match with its whole text; every match unless --limit keeps the newest n (zero is refused).",
        "show" => "show <session-id> [--include <types>] [--limit <n>] [--out <path>] [--json]\n  Reconstruct one conversation in full: masked event text, oldest turn first, no per-event truncation, every event unless --limit keeps the oldest n (zero is refused).\n  --include takes a comma-separated list of event types (user, assistant, thinking, tool_call, tool_result, meta, hook_decision) or \"all\"; the default is user,assistant.\n  The footer reports rendered and matched counts, so a --limit cut is always visible.\n  --out writes the same record to a file instead of the terminal, through a sibling .partial file renamed into place, and prints the path, the byte count and the same footer. A missing directory, a directory as the target and an empty value are refused before the read runs.",
        "stats" => "stats [--days <n>] [--runtime <r>] [--json]\n  Summarize events, sessions, tools, and token counters over the whole Lake, or over the last n days with --days (zero is refused).",
        "hooks" => "hooks [--decision <value>] [--tool <name>] [--limit <n>] [--json]\n  Inspect adaptive-hook decisions, newest first; every decision unless --limit keeps the newest n (zero is refused).",
        "signals" => "signals [--report freshness] [--limit <n>] [--json]\n  Compare the Oko index's freshness with Lake's; every row unless --limit keeps n (zero is refused). Frustration is Oko's model reading: `oko stats snapshot`.",
        "label" => "label <add|remove|list|aspects> ...\n  add <session-id> --aspect <name> --value <v> [--note <text>] [--source <name[:detail]>] [--json] records a session label with provenance (manual, human, model, or brama, optional :detail).\n  remove <session-id> --aspect <name> [--note <text>] [--source <name[:detail]>] [--runtime <r>] [--json] withdraws the assignment in force for that aspect by appending a record with no value; the history stays whole, and a session with no such label is refused.\n  list [--session <id>] [--aspect <a>] [--runtime <r>] [--limit <n>] [--json] shows the latest assignment per session and aspect, newest first; every one unless --limit keeps the newest n (zero is refused).\n  aspects [--json] summarizes distinct aspects, values, and labeled sessions.",
        "goal" => "goal <title|label> ...\n  title (--text <text>|--stdin) [--json] runs the SHA-pinned qualified GGUF locally.\n  label <session-id> [--runtime <r>] [--json] labels the first masked user prompt, recording the pinned goal model as the label's source.",
        "query" => "query [--json] \"<sql>\"\n  Execute operator-supplied SQL after loading canonical Lake views.",
        "compact" => "compact [--source <runtime>] [--json]\n  Rebuild per-runtime Parquet mirrors; NDJSON remains authoritative.",
        "projection" => "projection <rebuild|refresh> --target <oko> ...\n  rebuild --target <consumer> [--reindex] reconstructs every per-session projection that consumer imports from authoritative Lake partitions; --reindex asks the consumer to reindex afterwards.\n  refresh --target <consumer> hands the current projection to the consumer through its own CLI (oko: `oko-cli transcripts reindex`).\n  The consumer is an adapter behind the operation; a --target without one is refused with the targets that exist.",
        "clean" => "clean [--target <parquet|oko|all>] [--apply] [--json]\n  Preview by default; --apply removes rebuildable derived data only.",
        "onboarding" => "onboarding [--source <runtime> --root <discovered-root>] [--skip-source] [--reset] [--yes] [--json]\n  Discover supported roots and optionally adopt one through the canonical ingestion boundary before querying the resulting Lake.\n  Without --source/--root the source step reports the exact adopt command; --skip-source leaves an empty usable Lake and does not claim first success.\n  --reset discards the recorded attempt and replays the journey from its entry screen; --yes answers the Enter prompts; --json emits the whole walk as one object and never prompts.",
        "help" => "help [command]\n  Show general guidance or the exact syntax for one command.",
        _ => return None,
    })
}

fn cmd_help(rest: &[String]) -> Result<()> {
    let topic = match rest {
        [] => {
            println!("{USAGE}");
            return Ok(());
        }
        [topic] => topic,
        _ => return Err(Error("help accepts at most one command name".into())),
    };
    let Some(text) = command_help(topic) else {
        return Err(Error(format!("unknown help topic: {topic}")));
    };
    println!("Usage: transcript-lake [--data-dir <path>] {text}");
    Ok(())
}

fn dispatch(command: &str, rest: &[String]) -> Result<i32> {
    match command {
        "paths" => commands::inspect::paths(rest),
        "sources" => commands::inspect::sources(rest),
        "adopt" => commands::adopt::adopt(rest),
        "doctor" => commands::inspect::doctor(rest),
        "status" => commands::inspect::status(rest),
        "stream" => commands::stream::stream(rest),
        "rebuild" => commands::stream::rebuild(rest),
        "reconcile" => commands::reconcile::reconcile(rest),
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
        "projection" => commands::derived::projection(rest),
        "clean" => commands::derived::clean(rest),
        "onboarding" => commands::onboarding::onboarding(rest),
        _ => unreachable!("dispatch called with an unrouted command"),
    }
}

/// The exit status of an invocation that is itself wrong (an unknown command,
/// a repeated or pathless global `--data-dir`), as the product documents it:
/// https://github.com/wisent-ai/transcript-lake#readme
const EXIT_USAGE: u8 = 2;

/// A usage error: the error and the usage on stderr, then `EXIT_USAGE`.
fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

/// The process status a command's status becomes. One no process can report
/// is said and reported as a failure.
fn process_status(code: i32) -> ExitCode {
    match u8::try_from(code) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: command status {code} is not a process status: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut input = std::env::args();
    // The first argument is this program's own path.
    input.next();
    let mut args: Vec<String> = Vec::new();
    let mut selected_data_dir: Option<String> = None;
    while let Some(token) = input.next() {
        if token != "--data-dir" {
            args.push(token);
            continue;
        }
        // A malformed global flag is a usage error.
        if selected_data_dir.is_some() {
            return Ok(usage_error("duplicate global --data-dir"));
        }
        match input.next() {
            Some(value) if !value.is_empty() && !value.starts_with("--") => {
                selected_data_dir = Some(util::absolute(&value).to_string_lossy().to_string());
            }
            _ => return Ok(usage_error("--data-dir requires a path")),
        }
    }
    if let Some(selected) = selected_data_dir {
        // Downstream resolution reads LAKE_DATA, so one selection covers the
        // command, the DuckDB views, and any child process this run spawns.
        std::env::set_var("LAKE_DATA", selected);
    }
    let Some((command, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    if command == "--help" || command == "-h" {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if command == "--version" || command == "-V" {
        println!("{VERSION}");
        return Ok(ExitCode::SUCCESS);
    }
    if rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        cmd_help(std::slice::from_ref(command))?;
        return Ok(ExitCode::SUCCESS);
    }
    if command_help(command).is_none() {
        return Ok(usage_error(&format!("unknown command: {command}")));
    }
    if command == "help" {
        cmd_help(rest)?;
        return Ok(ExitCode::SUCCESS);
    }
    Ok(process_status(dispatch(command, rest)?))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
