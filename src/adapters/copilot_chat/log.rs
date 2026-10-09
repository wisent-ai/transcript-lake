//! Replaying VS Code's object mutation log (`objectMutationLog.ts`) to the
//! state it describes. Each entry is `{kind, k?, v?, i?}`; `k` is a path of
//! property names and array indexes. The kinds are VS Code's `EntryKind`,
//! whose numbers are their places in `ENTRY_KINDS`:
//!   Initial {v}       the whole state, replacing what came before;
//!   Set {k, v}        replace the value at `k`;
//!   Push {k, v?, i?}  at the array at `k`, drop everything from index `i`
//!                     when given, then append `v`'s items;
//!   Delete {k}        remove the property at `k`.
//! An entry whose path does not exist in the state is skipped, as a reader
//! of a log written by a newer VS Code should.
use serde_json::Value;

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

/// An entry's kind.
fn kind(entry: &Value) -> Option<EntryKind> {
    let number = usize::try_from(entry.get("kind")?.as_u64()?).ok()?;
    ENTRY_KINDS.get(number).copied()
}

/// The state every entry of a log leads to.
pub(super) fn replay(entries: &[Value]) -> Value {
    let mut state = Value::Null;
    for entry in entries {
        let path: Vec<Value> = entry
            .get("k")
            .and_then(Value::as_array)
            .cloned()
            .into_iter()
            .flatten()
            .collect();
        match kind(entry) {
            Some(EntryKind::Initial) => {
                if let Some(value) = entry.get("v") {
                    state = value.clone();
                }
            }
            Some(EntryKind::Set) => {
                if let (Some(slot), Some(value)) = (resolve(&mut state, &path), entry.get("v")) {
                    *slot = value.clone();
                }
            }
            Some(EntryKind::Push) => {
                if let Some(items) = resolve(&mut state, &path).and_then(Value::as_array_mut) {
                    if let Some(keep) = entry
                        .get("i")
                        .and_then(Value::as_u64)
                        .and_then(|keep| usize::try_from(keep).ok())
                    {
                        items.truncate(keep);
                    }
                    items.extend(
                        entry
                            .get("v")
                            .and_then(Value::as_array)
                            .cloned()
                            .into_iter()
                            .flatten(),
                    );
                }
            }
            Some(EntryKind::Delete) => {
                if let Some((last, parent)) = path.split_last() {
                    if let (Some(container), Some(name)) =
                        (resolve(&mut state, parent), last.as_str())
                    {
                        if let Some(object) = container.as_object_mut() {
                            object.remove(name);
                        }
                    }
                }
            }
            None => {}
        }
    }
    state
}

/// The value at `path` inside `state`; a missing property of an object is
/// created empty, because a Set may introduce one.
fn resolve<'a>(state: &'a mut Value, path: &[Value]) -> Option<&'a mut Value> {
    let mut current = state;
    for step in path {
        current = match step {
            Value::String(name) => current
                .as_object_mut()?
                .entry(name.clone())
                .or_insert(Value::Null),
            Value::Number(index) => {
                let index = usize::try_from(index.as_u64()?).ok()?;
                current.as_array_mut()?.get_mut(index)?
            }
            _ => return None,
        };
    }
    Some(current)
}
