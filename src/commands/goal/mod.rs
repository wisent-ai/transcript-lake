//! Local goal-title inference with the qualified Jeden GGUF.
//!
//! The model consumes only caller-supplied or already masked Lake text. It runs
//! on Ster (`ster generate`), the product that owns local model inference, on
//! this machine; no transcript content is sent to an inference service.

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

pub(super) const MODEL_NAME: &str = "jeden-goal-qwen3-4b-q4_k_m.gguf";
pub(super) const MODEL_SHA256: &str =
    "2512d7a455a50a16742b75d8fe38bf02b46b5d6b607f785be32a6345d999d310";
pub(super) const MODEL_REVISION: &str = "d9ce79f106ead1176b74bb0d9fb875521ca712b1";
const MODEL_SOURCE: &str = "model:jeden-goal-qwen3-4b-2512d7a4";
pub(super) const PROMPT_NAME: &str = "goal-system-prompt.md";
pub(super) const PROMPT_SHA256: &str =
    "6a42afdb497988d0e0281dabe230f2e256423432ffec2eb02f0d570d34ac4621";
pub(super) const REPOSITORY: &str = "lbartoszcze/jeden-goal-qwen3-4b";

#[derive(Deserialize, Serialize)]
pub(super) struct ModelValidationStamp {
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
    let checkpoint = resolve_checkpoint(&data_dir, &model)?;
    // The model's own declared context bounds the answer: it is generated
    // until the model ends it, as no token budget is chosen here.
    let context = declared_context(&checkpoint)?;
    let request = format!("<user>{}</user>", text.replace('\0', ""));
    // Argmax decoding draws nothing, so the seed Ster requires changes no
    // token; it is the request's digest read as a number, so the same text
    // runs the same way.
    let digest = Sha256::digest(request.as_bytes());
    let seed = digest
        .chunks_exact(std::mem::size_of::<u64>())
        .next()
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| Error("a SHA-256 digest is shorter than one u64".into()))?;
    let output = Command::new(&runtime)
        .arg("generate")
        .arg("--model")
        .arg(&checkpoint)
        .arg("--system")
        .arg(&prompt)
        .arg("--prompt")
        .arg(&request)
        .args([
            "--chat-template",
            "auto",
            "--temperature",
            "0",
            "--max-new-tokens",
        ])
        .arg(context.to_string())
        .arg("--seed")
        .arg(seed.to_string())
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Error(format!(
                "failed to start Ster {}: {error}",
                runtime.display()
            ))
        })?;
    if !output.status.success() {
        return Err(Error(format!(
            "Ster could not run the goal model ({} generate exited {}): {}",
            runtime.display(),
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
    Ok(Some(goal.to_string()))
}

mod model;

use model::*;
