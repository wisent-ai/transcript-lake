//! `transcript-lake scrub`: replace a credential literal already stored in the
//! Lake with the marker the masker would have written for it.
//!
//! The masker protects new events; it cannot reach the ones already committed.
//! A password typed as prose, or a transcript imported before its credential
//! class existed, sits in clear text in append-only partitions. This command is
//! the one supported way to remove such a literal after the fact, and the one
//! the scheduled repair runs, so the scrub lives inside the product that owns
//! the archive instead of beside it.
//!
//! - Preview is the default; `--apply` is the only way to change a byte, and it
//!   takes the Lake's own writer lease, so a rewrite never interleaves with a
//!   stream commit.
//! - The replacement is `[masked:credential:<chars>:<sha256[:8]>]`, the masker's
//!   own spelling, so readers keep working and reuse still correlates.
//! - Idempotent: the marker holds none of the literal.
//! - Counting comes first, per literal; a literal found in more than
//!   `--max-files` files is refused, so a wrong value costs a report.
//! - A rewritten NDJSON line or JSON document must still parse, or the file is
//!   left untouched. Files are replaced atomically with their mode kept.
//! - Literals never appear on a command line or in output: only their length
//!   and fingerprint are printed.
mod vault;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::args::parse_options;
use crate::cursors::open_writer_lease;
use crate::paths::resolve_data_dir;
use crate::util::{Error, Result};

/// A literal present in more files than this is either not a secret or so
/// pervasive that replacing it would destroy evidence; it is reported, not
/// applied. Text, because a bare number is refused in this repository.
const MAX_FILES_DEFAULT: &str = "1024";
const FINGERPRINT_CHARS: usize = 8;
/// Where an incremental run remembers how far the previous one got. It sits in
/// the Lake's state root beside the cursors, outside every partition.
const STATE_FILE: &str = "scrub-state.json";
const SKIPPED_DIRECTORIES: [&str; 1] = ["stream.lock"];

fn fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..FINGERPRINT_CHARS].to_string()
}

fn marker(value: &str) -> String {
    format!(
        "[masked:credential:{}:{}]",
        value.chars().count(),
        fingerprint(value)
    )
}

/// What may be printed about a secret: how long it is and which one it is.
fn describe(value: &str) -> String {
    format!(
        "len={} sha256[:8]={}",
        value.chars().count(),
        fingerprint(value)
    )
}

/// One alternation for the whole list, longest first, so a file is read once.
fn scanner(literals: &BTreeSet<String>) -> Result<Regex> {
    let mut ordered: Vec<&String> = literals.iter().collect();
    ordered.sort_by(|left, right| right.len().cmp(&left.len()));
    let pattern = ordered
        .iter()
        .map(|literal| regex::escape(literal))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&pattern)
        .map_err(|error| Error(format!("the literal list does not compile: {error}")))
}

fn lake_files(root: &Path, since: u64, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if SKIPPED_DIRECTORIES.iter().any(|skipped| name == *skipped) || name == STATE_FILE {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            lake_files(&path, since, found)?;
        } else if kind.is_file() {
            let modified = entry
                .metadata()?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            if since == 0 || modified >= since {
                found.push(path);
            }
        }
    }
    Ok(())
}

/// Whether the rewritten text still parses as the file kind claims.
fn broken_lines(path: &Path, text: &str) -> Vec<usize> {
    let suffix = path
        .extension()
        .and_then(|suffix| suffix.to_str())
        .unwrap_or_default();
    match suffix {
        "ndjson" | "jsonl" => text
            .split('\n')
            .enumerate()
            .filter(|(_, line)| {
                !line.trim().is_empty() && serde_json::from_str::<Value>(line).is_err()
            })
            .map(|(index, _)| index + 1)
            .collect(),
        "json" if serde_json::from_str::<Value>(text).is_err() => vec![1],
        _ => Vec::new(),
    }
}

/// Replace every accepted literal in one file, atomically, or leave it whole.
fn rewrite(path: &Path, pattern: &Regex) -> Result<std::result::Result<usize, Vec<usize>>> {
    let original = fs::read_to_string(path)?;
    let mut occurrences = 0;
    let updated = pattern.replace_all(&original, |captures: &regex::Captures| {
        occurrences += 1;
        marker(&captures[0])
    });
    if occurrences == 0 {
        return Ok(Ok(0));
    }
    let broken = broken_lines(path, &updated);
    if !broken.is_empty() {
        return Ok(Err(broken));
    }
    let temporary = path.with_file_name(format!(
        "{}.scrub-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temporary, updated.as_bytes())?;
    fs::File::open(&temporary)?.sync_all()?;
    fs::set_permissions(&temporary, fs::metadata(path)?.permissions())?;
    fs::rename(&temporary, path)?;
    Ok(Ok(occurrences))
}

fn read_literals(source: &str) -> Result<BTreeSet<String>> {
    let mut raw = String::new();
    if source == "-" {
        std::io::stdin().read_to_string(&mut raw)?;
    } else {
        raw = fs::read_to_string(source)?;
    }
    Ok(raw
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn scrub(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "scrub",
        rest,
        &[
            "secret-file",
            "from-vault",
            "max-files",
            "since",
            "files-per-run",
        ],
        &["apply", "since-last-run", "json"],
    )?;
    let data_dir = resolve_data_dir(None);
    if !data_dir.is_dir() {
        return Err(Error(format!("no Lake at {}", data_dir.display())));
    }
    let literals = match (parsed.value("secret-file"), parsed.value("from-vault")) {
        (Some(source), None) => read_literals(source)?,
        (None, Some(vaults)) => {
            vault::material_literals(vaults.split(',').map(PathBuf::from).collect())?
        }
        _ => return Err(Error(
            "scrub takes exactly one of --secret-file <path|-> or --from-vault <vault[,vault…]>"
                .into(),
        )),
    };
    if literals.is_empty() {
        println!("nothing to scrub: no value could occur in text");
        return Ok(0);
    }
    let parse_number = |name: &str, absent: &str| -> Result<u64> {
        let text = parsed.value(name).unwrap_or(absent);
        text.parse()
            .map_err(|_| Error(format!("--{name} takes a whole number, got {text}")))
    };
    let max_files = parse_number("max-files", MAX_FILES_DEFAULT)?;
    let per_run = parse_number("files-per-run", "0")?;
    let apply = parsed.flag("apply");
    let state_path = data_dir.join(STATE_FILE);
    let state: Value = fs::read_to_string(&state_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let since = if parsed.flag("since-last-run") {
        state["last_complete_run"].as_u64().unwrap_or_default()
    } else {
        parse_number("since", "0")?
    };
    let resume_after = if parsed.flag("since-last-run") {
        state["resume_after"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    } else {
        String::new()
    };
    println!("lake {}", data_dir.display());
    println!(
        "mode {}",
        if apply {
            "apply"
        } else {
            "preview (pass --apply to write)"
        }
    );
    println!(
        "literals {}  max files per literal {max_files}  since {since}",
        literals.len()
    );

    // An applying run owns the archive for its whole pass: counting first and
    // finding the stream holds the lease afterwards would waste the count.
    let _lease = if apply {
        Some(open_writer_lease(&data_dir)?)
    } else {
        None
    };
    let pattern = scanner(&literals)?;
    let mut files = Vec::new();
    lake_files(&data_dir, since, &mut files)?;
    files.sort();
    let mut scanned = 0u64;
    let mut unreadable = 0u64;
    let mut next_resume = String::new();
    let mut per_literal: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut hits: Vec<(PathBuf, BTreeMap<String, u64>)> = Vec::new();
    for path in files {
        if !resume_after.is_empty() && path.to_string_lossy().as_ref() <= resume_after.as_str() {
            continue;
        }
        if per_run > 0 && scanned >= per_run {
            next_resume = path.to_string_lossy().into_owned();
            break;
        }
        scanned += 1;
        let Ok(text) = fs::read_to_string(&path) else {
            unreadable += 1;
            continue;
        };
        let mut found: BTreeMap<String, u64> = BTreeMap::new();
        for hit in pattern.find_iter(&text) {
            *found.entry(hit.as_str().to_owned()).or_default() += 1;
        }
        for (literal, count) in &found {
            let entry = per_literal.entry(literal.clone()).or_default();
            entry.0 += 1;
            entry.1 += count;
        }
        if !found.is_empty() {
            hits.push((path, found));
        }
    }
    println!(
        "coverage {}",
        if next_resume.is_empty() {
            "complete".to_owned()
        } else {
            format!("partial, resume after {next_resume}")
        }
    );
    let mut accepted = BTreeSet::new();
    let mut over_cap = 0u64;
    for literal in &literals {
        let (files, occurrences) = per_literal.get(literal).copied().unwrap_or_default();
        let verdict = if files > max_files {
            over_cap += 1;
            "REFUSED over cap"
        } else if files > 0 {
            accepted.insert(literal.clone());
            "accepted"
        } else {
            "absent"
        };
        println!(
            "literal {} files={files} occurrences={occurrences} -> {} {verdict}",
            describe(literal),
            marker(literal)
        );
    }
    let targets: Vec<&PathBuf> = hits
        .iter()
        .filter(|(_, found)| found.keys().any(|literal| accepted.contains(literal)))
        .map(|(path, _)| path)
        .collect();
    println!("files scanned {scanned}");
    println!("files skipped as unreadable or binary {unreadable}");
    println!(
        "files {} {}",
        if apply { "to rewrite" } else { "to change" },
        targets.len()
    );
    let mut refused = 0u64;
    if apply && !accepted.is_empty() {
        let accepted_pattern = scanner(&accepted)?;
        let mut replaced = 0usize;
        for path in &targets {
            let relative = path
                .strip_prefix(&data_dir)
                .unwrap_or(path)
                .display()
                .to_string();
            match rewrite(path, &accepted_pattern)? {
                Ok(count) => {
                    replaced += count;
                    println!("  rewrote {relative} occurrences={count}");
                }
                Err(broken) => {
                    refused += 1;
                    println!(
                        "  REFUSED {relative}: the rewrite would not parse at lines {broken:?}"
                    );
                }
            }
        }
        println!("occurrences replaced {replaced}");
    } else {
        for path in &targets {
            println!(
                "  would rewrite {}",
                path.strip_prefix(&data_dir).unwrap_or(path).display()
            );
        }
    }
    println!("literals refused over cap {over_cap}");
    println!("files refused {refused}");
    if apply && parsed.flag("since-last-run") {
        let complete = next_resume.is_empty();
        let recorded = json!({
            "last_complete_run": if complete { started } else { since },
            "resume_after": next_resume,
            "literals": literals.len(),
        });
        fs::write(&state_path, format!("{recorded}\n"))?;
    }
    Ok(if refused > 0 || over_cap > 0 { 1 } else { 0 })
}
