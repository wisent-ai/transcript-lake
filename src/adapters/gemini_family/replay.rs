//! Replaying a Gemini CLI session file into the messages it holds: a
//! whole-document `session-*.json` lists them, and a JSONL session records
//! them line by line with `$set`, `$patch` and `$rewindTo` records that
//! rewrite earlier ones. A rewind hides messages from the harness's own
//! history, but what was said stays in the archive, so it removes nothing.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::adapters::common::{read_json, read_json_lines, text_field};
use crate::util::Result;

/// Every message of a session file, in order, with its patches applied, and
/// the session id its metadata names.
pub(super) fn messages(file: &Path) -> Result<(Option<String>, Vec<Value>)> {
    if file
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let document = read_json(file)?;
        let messages = document
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .into_iter()
            .flatten()
            .collect();
        return Ok((text_field(&document, "sessionId"), messages));
    }
    let mut session = None;
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    for record in read_json_lines(file)? {
        if let Some(set) = record.get("$set") {
            for message in set
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                put(message, &mut order, &mut by_id);
            }
        } else if let Some(patch) = record.get("$patch") {
            apply_patch(patch, &mut by_id);
            for update in patch
                .get("updates")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                apply_patch(update, &mut by_id);
            }
        } else if record.get("$rewindTo").is_some() {
            continue;
        } else if record.get("projectHash").is_some() {
            session = text_field(&record, "sessionId");
        } else {
            put(&record, &mut order, &mut by_id);
        }
    }
    Ok((
        session,
        order
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect(),
    ))
}

/// Record `message` under its id, keeping the place it first had.
fn put(message: &Value, order: &mut Vec<String>, by_id: &mut HashMap<String, Value>) {
    if let Some(id) = text_field(message, "id") {
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, message.clone());
    }
}

/// Apply one message patch: new content, and results for its tool calls.
fn apply_patch(patch: &Value, by_id: &mut HashMap<String, Value>) {
    let Some(id) = text_field(patch, "id") else {
        return;
    };
    let Some(message) = by_id.get_mut(&id) else {
        return;
    };
    if let Some(content) = patch.get("content") {
        message["content"] = content.clone();
    }
    for update in patch
        .get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = update.get("id") else { continue };
        let Some(calls) = message.get_mut("toolCalls").and_then(Value::as_array_mut) else {
            continue;
        };
        for call in calls.iter_mut().filter(|call| call.get("id") == Some(id)) {
            if let Some(result) = update.get("result") {
                call["result"] = result.clone();
            }
        }
    }
}
