//! Building canonical events from what harnesses record: one event of a
//! session, tool calls and results, metadata, usage, and the two message
//! shapes most harnesses share (chat completions and Anthropic Messages).
use serde_json::{Map, Value};

use super::{as_text, iso_millis, text_field, text_of};
use crate::types::{ParserCtx, RawEvent};

/// One event of `ctx`'s session at `ts`.
pub(crate) fn event(
    ctx: &ParserCtx,
    ts: Option<String>,
    event_type: &str,
    text: String,
) -> RawEvent {
    RawEvent {
        ts,
        session_id: ctx.session_id.clone(),
        project: ctx.project.clone(),
        event_type: event_type.to_string(),
        text,
        ..RawEvent::default()
    }
}

/// A tool call event; `name` is the tool the harness recorded, when it did.
pub(crate) fn tool_call(
    ctx: &ParserCtx,
    ts: Option<String>,
    name: Option<&str>,
    input: String,
    id: Option<&str>,
) -> RawEvent {
    let mut call = event(ctx, ts, "tool_call", input);
    call.tool_name = name.map(str::to_string);
    if let Some(id) = id {
        call.extra
            .insert("tool_call_id".into(), Value::String(id.to_string()));
    }
    call
}

/// A tool result event; `failed` is what the harness recorded, when it did.
pub(crate) fn tool_result(
    ctx: &ParserCtx,
    ts: Option<String>,
    name: Option<&str>,
    output: String,
    id: Option<&str>,
    failed: Option<bool>,
) -> RawEvent {
    let mut result = event(ctx, ts, "tool_result", output);
    result.tool_name = name.map(str::to_string);
    if let Some(id) = id {
        result
            .extra
            .insert("tool_call_id".into(), Value::String(id.to_string()));
    }
    if let Some(failed) = failed {
        result.extra.insert("is_error".into(), Value::Bool(failed));
    }
    result
}

/// A metadata event naming what the harness recorded (`kind`), with the
/// record's own fields kept in `extra`.
pub(crate) fn meta(
    ctx: &ParserCtx,
    ts: Option<String>,
    kind: &str,
    text: String,
    fields: Map<String, Value>,
) -> RawEvent {
    let mut note = event(ctx, ts, "meta", text);
    note.extra = fields;
    note.extra
        .insert("kind".into(), Value::String(kind.to_string()));
    note
}

/// Usage a harness reported for one model call, as a metadata event carrying
/// the model and token counts.
pub(crate) fn usage(
    ctx: &ParserCtx,
    ts: Option<String>,
    model: Option<String>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
) -> RawEvent {
    let mut note = event(ctx, ts, "meta", String::new());
    note.model = model;
    note.tokens_in = tokens_in;
    note.tokens_out = tokens_out;
    note.extra
        .insert("kind".into(), Value::String("usage".into()));
    note
}

/// The events of messages in the chat-completions shape many harnesses keep
/// (`{role, content, tool_calls?, tool_call_id?, name?}`): user and assistant
/// words, assistant reasoning when recorded, tool calls and tool results.
/// `ts` is the time the harness recorded for the conversation, used for a
/// message that carries none of its own. A tool call is either OpenAI's
/// `{id, function: {name, arguments}}` or the flat `{id, name, arguments}`
/// some harnesses write.
pub(crate) fn chat_messages(
    ctx: &ParserCtx,
    messages: &[Value],
    ts: Option<String>,
) -> Vec<RawEvent> {
    let mut events = Vec::new();
    for message in messages {
        let at = text_field(message, "timestamp").or_else(|| ts.clone());
        let content = message.get("content");
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let words = text_of(content);
                if !words.is_empty() {
                    events.push(event(ctx, at.clone(), "user", words));
                }
            }
            Some("assistant") => {
                if let Some(reasoning) = text_field(message, "reasoning_content")
                    .or_else(|| text_field(message, "reasoning"))
                {
                    events.push(event(ctx, at.clone(), "thinking", reasoning));
                }
                let words = text_of(content);
                if !words.is_empty() {
                    events.push(event(ctx, at.clone(), "assistant", words));
                }
                for call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let function = match call.get("function") {
                        Some(function) => function,
                        None => call,
                    };
                    events.push(tool_call(
                        ctx,
                        at.clone(),
                        function.get("name").and_then(Value::as_str),
                        as_text(function.get("arguments")),
                        call.get("id").and_then(Value::as_str),
                    ));
                }
            }
            Some("tool") | Some("function") => events.push(tool_result(
                ctx,
                at.clone(),
                message.get("name").and_then(Value::as_str),
                text_of(content),
                message.get("tool_call_id").and_then(Value::as_str),
                None,
            )),
            Some("system") => {
                let mut fields = Map::new();
                fields.insert("role".into(), Value::String("system".into()));
                events.push(meta(
                    ctx,
                    at.clone(),
                    "system_prompt",
                    String::new(),
                    fields,
                ));
            }
            _ => {}
        }
    }
    events
}

/// The events of one message's content blocks in Anthropic's Messages shape
/// (text, thinking, tool_use, tool_result), which Cline-family extensions and
/// Claude-compatible harnesses keep. `role` decides whose words a text block
/// is.
pub(crate) fn anthropic_blocks(
    ctx: &ParserCtx,
    role: &str,
    content: Option<&Value>,
    at: Option<String>,
) -> Vec<RawEvent> {
    let blocks = match content {
        Some(Value::String(text)) => vec![serde_json::json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    };
    let mut events = Vec::new();
    for block in &blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(words) = text_field(block, "text") {
                    // Words under a role that is neither side of the
                    // conversation (a tool's, a command's) are tool output.
                    match role {
                        "assistant" | "user" => events.push(event(ctx, at.clone(), role, words)),
                        _ => {
                            events.push(tool_result(ctx, at.clone(), Some(role), words, None, None))
                        }
                    }
                }
            }
            Some("thinking") => {
                if let Some(thought) = text_field(block, "thinking") {
                    events.push(event(ctx, at.clone(), "thinking", thought));
                }
            }
            Some("tool_use") => events.push(tool_call(
                ctx,
                at.clone(),
                block.get("name").and_then(Value::as_str),
                as_text(block.get("input")),
                block.get("id").and_then(Value::as_str),
            )),
            Some("tool_result") => events.push(tool_result(
                ctx,
                at.clone(),
                None,
                match block.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    other => text_of(other),
                },
                block.get("tool_use_id").and_then(Value::as_str),
                block.get("is_error").and_then(Value::as_bool),
            )),
            Some(other) => {
                let mut fields = Map::new();
                fields.insert("block_type".into(), Value::String(other.to_string()));
                events.push(meta(
                    ctx,
                    at.clone(),
                    "unknown_block",
                    String::new(),
                    fields,
                ));
            }
            None => {}
        }
    }
    events
}

/// The time an Anthropic-shaped message carries: Cline's epoch-millisecond
/// `ts` or an ISO `timestamp`.
pub(crate) fn message_time(message: &Value) -> Option<String> {
    message
        .get("ts")
        .and_then(Value::as_i64)
        .and_then(iso_millis)
        .or_else(|| text_field(message, "timestamp"))
}
