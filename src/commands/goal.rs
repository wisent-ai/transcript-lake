//! Local goal-title inference with the qualified Jeden GGUF.
//!
//! The model consumes only caller-supplied or already masked Lake text. It runs
//! through a local llama.cpp executable; no transcript content is sent to an
//! inference service.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::args::{parse_options, require_flags_only};
use crate::duck::query_duck_json;
use crate::labels::{append_label, label_record};
use crate::paths::resolve_data_dir;
use crate::util::{find_on_path, home_dir, quote_sql, write_json, Error, Result};

const MODEL_NAME: &str = "jeden-goal-qwen3-4b-q4_k_m.gguf";
const MODEL_SHA256: &str = "2512d7a455a50a16742b75d8fe38bf02b46b5d6b607f785be32a6345d999d310";
const MODEL_BYTES: u64 = 2_497_280_320;
const MODEL_REVISION: &str = "d9ce79f106ead1176b74bb0d9fb875521ca712b1";
const MODEL_SOURCE: &str = "model:jeden-goal-qwen3-4b-2512d7a4";
const PROMPT_NAME: &str = "goal-system-prompt.md";
const PROMPT_SHA256: &str = "6a42afdb497988d0e0281dabe230f2e256423432ffec2eb02f0d570d34ac4621";
const REPOSITORY: &str = "lbartoszcze/jeden-goal-qwen3-4b";

#[derive(Deserialize, Serialize)]
struct ModelValidationStamp {
    path: String,
    bytes: u64,
    modified_secs: u64,
    modified_nanos: u32,
    sha256: String,
}

#[derive(Serialize)]
struct GoalOutput<'a> {
    goal: Option<&'a str>,
    model: &'static str,
    sha256: &'static str,
}

#[derive(Serialize)]
struct GoalLabelOutput<'a> {
    session_id: &'a str,
    runtime: &'a str,
    goal: Option<&'a str>,
    applied: bool,
    source: &'static str,
    sha256: &'static str,
}

pub fn goal(rest: &[String]) -> Result<i32> {
    match rest.split_first() {
        Some((subcommand, subrest)) if subcommand == "title" => title(subrest),
        Some((subcommand, subrest)) if subcommand == "label" => label(subrest),
        _ => Err(Error(
            "usage: transcript-lake goal <title|label> (see: transcript-lake help goal)".into(),
        )),
    }
}

fn title(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("goal title", rest, &["text"], &["stdin", "json"])?;
    require_flags_only("goal title", &parsed)?;
    let text = input_text(parsed.value("text"), parsed.flag("stdin"))?;
    let goal = infer_goal(&text)?;
    if parsed.flag("json") {
        write_json(&GoalOutput {
            goal: goal.as_deref(),
            model: MODEL_NAME,
            sha256: MODEL_SHA256,
        })?;
    } else if let Some(goal) = goal {
        println!("{goal}");
    } else {
        println!("<goal/>");
    }
    Ok(0)
}

fn label(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("goal label", rest, &["runtime"], &["json"])?;
    if parsed.positionals.len() != 1 {
        return Err(Error(
            "usage: transcript-lake goal label <session-id> [--runtime <r>] [--json]".into(),
        ));
    }
    let session_id = parsed.positionals[0].trim();
    if session_id.is_empty() {
        return Err(Error("goal label requires a session id".into()));
    }
    let runtime_filter = parsed
        .value("runtime")
        .map(|runtime| format!(" AND runtime = {}", quote_sql(runtime)))
        .unwrap_or_default();
    let rows = query_duck_json(&format!(
        "SELECT runtime, text FROM events WHERE session_id = {}{runtime_filter} \
         AND event_type = 'user' AND text IS NOT NULL AND length(trim(text)) > 0 \
         ORDER BY ts LIMIT 2",
        quote_sql(session_id)
    ))?;
    let Some(first) = rows.first() else {
        return Err(Error(format!(
            "session {session_id} has no masked user prompt in this Lake"
        )));
    };
    let runtime = first
        .get("runtime")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if rows
        .iter()
        .any(|row| row.get("runtime").and_then(Value::as_str) != Some(runtime))
    {
        return Err(Error(format!(
            "session {session_id} exists in multiple runtimes; pass --runtime"
        )));
    }
    let text = first
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let goal = infer_goal(text)?;
    let applied = if let Some(goal) = goal.as_deref() {
        let record = label_record(
            session_id,
            runtime,
            "goal",
            goal,
            Some(format!("qualified GGUF sha256:{MODEL_SHA256}")),
            MODEL_SOURCE,
        );
        append_label(&resolve_data_dir(None), &record)?;
        true
    } else {
        false
    };
    if parsed.flag("json") {
        write_json(&GoalLabelOutput {
            session_id,
            runtime,
            goal: goal.as_deref(),
            applied,
            source: MODEL_SOURCE,
            sha256: MODEL_SHA256,
        })?;
    } else if let Some(goal) = goal {
        println!("{goal}");
    } else {
        println!("<goal/>");
    }
    Ok(0)
}

fn input_text(value: Option<&str>, stdin: bool) -> Result<String> {
    if value.is_some() == stdin {
        return Err(Error(
            "goal title requires exactly one of --text <text> or --stdin".into(),
        ));
    }
    let text = if stdin {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        text
    } else {
        value.unwrap_or_default().to_string()
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(Error("goal title input must not be empty".into()));
    }
    Ok(text.chars().take(6_000).collect())
}

fn infer_goal(text: &str) -> Result<Option<String>> {
    let data_dir = resolve_data_dir(None);
    let model = resolve_model(&data_dir)?;
    let prompt = resolve_prompt(&data_dir)?;
    let runtime = resolve_runtime()?;
    let input = temporary_input(&data_dir, text)?;
    let output = Command::new(&runtime)
        .args([
            "--model",
            model.to_string_lossy().as_ref(),
            "--system-prompt-file",
            prompt.to_string_lossy().as_ref(),
            "--file",
            input.to_string_lossy().as_ref(),
            "--single-turn",
            "--reasoning",
            "off",
            "--ctx-size",
            "2048",
            "--n-predict",
            "40",
            "--temp",
            "0",
            "--gpu-layers",
            "all",
            "--no-display-prompt",
            "--no-show-timings",
            "--simple-io",
            "--log-disable",
        ])
        .stdin(Stdio::null())
        .output();
    let _ = fs::remove_file(&input);
    let output = output.map_err(|error| {
        Error(format!(
            "failed to start local goal model runtime {}: {error}",
            runtime.display()
        ))
    })?;
    if !output.status.success() {
        return Err(Error(format!(
            "local goal model failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_goal(&String::from_utf8_lossy(&output.stdout))
}

fn parse_goal(output: &str) -> Result<Option<String>> {
    let empty = output.rfind("<goal/>");
    let closing = output.rfind("</goal>");
    if empty.is_some() && closing.is_none_or(|closing| empty.unwrap_or_default() > closing) {
        return Ok(None);
    }
    let closing = closing.ok_or_else(|| Error("local goal model returned no goal tag".into()))?;
    let opening = output[..closing]
        .rfind("<goal>")
        .ok_or_else(|| Error("local goal model returned no opening goal tag".into()))?;
    let goal = output[opening + "<goal>".len()..closing].trim();
    if goal.is_empty() {
        return Ok(None);
    }
    let words = goal.split_whitespace().count();
    if !(3..=12).contains(&words) || goal.chars().count() > 100 {
        return Err(Error(format!(
            "local goal model returned an invalid {words}-word title"
        )));
    }
    Ok(Some(goal.to_string()))
}

fn resolve_runtime() -> Result<PathBuf> {
    for key in ["TRANSCRIPT_LAKE_GOAL_LLAMA_CLI", "JEDEN_GOAL_LLAMA_CLI"] {
        if let Some(path) = env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
        {
            if path.is_file() {
                return Ok(path);
            }
            return Err(Error(format!(
                "{key} does not name a file: {}",
                path.display()
            )));
        }
    }
    find_on_path("llama-cli").ok_or_else(|| {
        Error(
            "local goal model requires llama-cli on PATH or TRANSCRIPT_LAKE_GOAL_LLAMA_CLI".into(),
        )
    })
}

fn resolve_model(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("TRANSCRIPT_LAKE_GOAL_MODEL")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        validate_model(data_dir, &path)?;
        return Ok(path);
    }
    let shared = home_dir()
        .join("Library/Caches/ai.wisent.jeden.desktop/goal-model")
        .join(MODEL_NAME);
    if validate_model(data_dir, &shared).is_ok() {
        return Ok(shared);
    }
    let cached = artifact_dir(data_dir).join(MODEL_NAME);
    if validate_model(data_dir, &cached).is_ok() {
        return Ok(cached);
    }
    download(
        &format!("https://huggingface.co/{REPOSITORY}/resolve/{MODEL_REVISION}/{MODEL_NAME}"),
        &cached,
    )?;
    validate_model(data_dir, &cached)?;
    Ok(cached)
}

fn resolve_prompt(data_dir: &Path) -> Result<PathBuf> {
    let stado = home_dir()
        .join(".stado/local-storage/ecosystem/releases/jeden-desktop/models/goal-qwen3-4b")
        .join(MODEL_SHA256)
        .join(PROMPT_NAME);
    if validate_digest(&stado, PROMPT_SHA256, "system prompt").is_ok() {
        return Ok(stado);
    }
    let cached = artifact_dir(data_dir).join(PROMPT_NAME);
    if validate_digest(&cached, PROMPT_SHA256, "system prompt").is_ok() {
        return Ok(cached);
    }
    download(
        &format!("https://huggingface.co/{REPOSITORY}/resolve/{MODEL_REVISION}/{PROMPT_NAME}"),
        &cached,
    )?;
    validate_digest(&cached, PROMPT_SHA256, "system prompt")?;
    Ok(cached)
}

fn artifact_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("models/jeden-goal-qwen3-4b")
        .join(MODEL_SHA256)
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        Error(format!(
            "invalid model destination: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension(format!("download-{}", std::process::id()));
    let status = Command::new("curl")
        .args(["--fail", "--location", "--retry", "3", "--output"])
        .arg(&temporary)
        .arg(url)
        .status()
        .map_err(|error| Error(format!("failed to start curl for model artifact: {error}")))?;
    if !status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(Error(format!(
            "model artifact download failed with status {status}"
        )));
    }
    fs::rename(&temporary, destination)?;
    Ok(())
}

fn validate_model(data_dir: &Path, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| Error(format!("model {} is unavailable: {error}", path.display())))?;
    if !metadata.is_file() || metadata.len() != MODEL_BYTES {
        return Err(Error(format!(
            "model {} has the wrong size",
            path.display()
        )));
    }
    let modified = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let expected = ModelValidationStamp {
        path: path.to_string_lossy().to_string(),
        bytes: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        sha256: MODEL_SHA256.to_string(),
    };
    let stamp_path = artifact_dir(data_dir).join("model-validation.json");
    if let Ok(bytes) = fs::read(&stamp_path) {
        if let Ok(stored) = serde_json::from_slice::<ModelValidationStamp>(&bytes) {
            if stored.path == expected.path
                && stored.bytes == expected.bytes
                && stored.modified_secs == expected.modified_secs
                && stored.modified_nanos == expected.modified_nanos
                && stored.sha256 == expected.sha256
            {
                return Ok(());
            }
        }
    }
    validate_digest(path, MODEL_SHA256, "model")?;
    fs::create_dir_all(artifact_dir(data_dir))?;
    let temporary = stamp_path.with_extension(format!("json-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(&expected)?)?;
    fs::rename(temporary, stamp_path)?;
    Ok(())
}

fn validate_digest(path: &Path, expected: &str, name: &str) -> Result<()> {
    let mut file = File::open(path)
        .map_err(|error| Error(format!("{name} {} is unavailable: {error}", path.display())))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 4 * 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(Error(format!("{name} {} failed SHA-256", path.display())));
    }
    Ok(())
}

fn temporary_input(data_dir: &Path, text: &str) -> Result<PathBuf> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("goal-input-{}-{nonce}.txt", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    write!(file, "<user>{}</user>", text.replace('\0', ""))?;
    file.sync_all()?;
    Ok(path)
}
