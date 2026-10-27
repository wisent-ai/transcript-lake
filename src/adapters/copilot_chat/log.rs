//! Replaying VS Code's object mutation log (`objectMutationLog.ts`) to the
//! state it describes. Each entry is `{kind, k?, v?, i?}`; `k` is a path of
//! property names and array indexes. The kinds are VS Code's `EntryKind`,
//! whose numbers are their places in `ENTRY_KINDS`:
//!   Initial {v}       the whole state, replacing what came before;
//!   Set {k, v}        replace the value at `k`;
//!   Push {k, v?, i?}  at the array at `k`, drop everything from index `i`
//!                     when given, then append `v`'s items;
//!   Delete {k}        remove the property at `k`.
//! An entry of a kind this reader does not know, or whose path does not
//! exist in the state, is skipped, as a reader of a log written by a newer
//! VS Code should. An entry that is malformed for its own kind (no numeric
//! kind, a path that is not a list, an Initial or Set without a value, a Push
//! whose `i` is not an index or whose `v` is not a list, a path step that is
//! neither a name nor an index) refuses the log naming the entry, so a
//! transcript is never imported with turns silently missing.
use serde_json::Value;

use crate::util::{Error, Result};

/// One kind of log entry.
#[derive(Clone, Copy)]
enum EntryKind {
    Initial,
    Set,
    Push,
    Delete,
}

/// VS Code's `EntryKind` members in declaration order: each one's number is
/// its index here.
const ENTRY_KINDS: &[EntryKind] = &[
    EntryKind::Initial,
    EntryKind::Set,
    EntryKind::Push,
    EntryKind::Delete,
];

/// An entry's kind; `None` for a number this reader does not know.
fn kind(position: usize, entry: &Value) -> Result<Option<EntryKind>> {
    let number = entry.get("kind").and_then(Value::as_u64).ok_or_else(|| {
        Error(format!(
            "log entry at index {position} has no numeric kind: {entry}"
        ))
    })?;
    let number = usize::try_from(number).map_err(|cause| {
        Error(format!(
            "log entry at index {position} has kind {number}, past any index: {cause}"
        ))
    })?;
    Ok(ENTRY_KINDS.get(number).copied())
}

/// The value an Initial or Set entry states.
fn stated_value(position: usize, entry: &Value) -> Result<&Value> {
    entry.get("v").ok_or_else(|| {
        Error(format!(
            "log entry at index {position} states no value: {entry}"
        ))
    })
}

/// An array index a log states: `i` of a Push, or a numeric path step.
fn index_of(position: usize, what: &str, value: &Value) -> Result<usize> {
    let index = value.as_u64().ok_or_else(|| {
        Error(format!(
            "log entry at index {position} has {what} {value}, which is not an array index"
        ))
    })?;
    usize::try_from(index).map_err(|cause| {
        Error(format!(
            "log entry at index {position} has {what} {index}, past any array index: {cause}"
        ))
    })
}

/// The state every entry of a log leads to.
pub(super) fn replay(entries: &[Value]) -> Result<Value> {
    let mut state = Value::Null;
    for (position, entry) in entries.iter().enumerate() {
        let path: &[Value] = match entry.get("k") {
            None => &[],
            Some(Value::Array(path)) => path,
            Some(other) => {
                return Err(Error(format!(
                    "log entry at index {position} has a path that is not a list: {other}"
                )))
            }
        };
        match kind(position, entry)? {
            Some(EntryKind::Initial) => state = stated_value(position, entry)?.clone(),
            Some(EntryKind::Set) => {
                let value = stated_value(position, entry)?;
                if let Some(slot) = resolve(position, &mut state, path)? {
                    *slot = value.clone();
                }
            }
            Some(EntryKind::Push) => {
                let keep = match entry.get("i") {
                    Some(keep) => Some(index_of(position, "i", keep)?),
                    None => None,
                };
                let added: &[Value] = match entry.get("v") {
                    None => &[],
                    Some(Value::Array(added)) => added,
                    Some(other) => {
                        return Err(Error(format!(
                            "log entry at index {position} pushes {other}, which is not a list"
                        )))
                    }
                };
                if let Some(items) =
                    resolve(position, &mut state, path)?.and_then(Value::as_array_mut)
                {
                    if let Some(keep) = keep {
                        items.truncate(keep);
                    }
                    items.extend(added.iter().cloned());
                }
            }
            Some(EntryKind::Delete) => {
                if let Some((last, parent)) = path.split_last() {
                    let name = last.as_str().ok_or_else(|| {
                        Error(format!(
                            "log entry at index {position} deletes {last}, which is not a property name"
                        ))
                    })?;
                    if let Some(object) =
                        resolve(position, &mut state, parent)?.and_then(Value::as_object_mut)
                    {
                        object.remove(name);
                    }
                }
            }
            None => {}
        }
    }
    Ok(state)
}

/// The value at `path` inside `state`, or `None` when the path does not
/// exist in it; a missing property of an object is created empty, because a
/// Set may introduce one.
fn resolve<'a>(
    position: usize,
    state: &'a mut Value,
    path: &[Value],
) -> Result<Option<&'a mut Value>> {
    let mut current = state;
    for step in path {
        let next = match step {
            Value::String(name) => match current.as_object_mut() {
                Some(object) => object.entry(name.clone()).or_insert(Value::Null),
                None => return Ok(None),
            },
            Value::Number(_) => {
                let index = index_of(position, "path step", step)?;
                match current.as_array_mut().and_then(|items| items.get_mut(index)) {
                    Some(item) => item,
                    None => return Ok(None),
                }
            }
            other => {
                return Err(Error(format!(
                    "log entry at index {position} has path step {other}, neither a property name nor an array index"
                )))
            }
        };
        current = next;
    }
    Ok(Some(current))
}
