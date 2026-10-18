//! Which harness a changed path belongs to. Roots of different harnesses
//! nest — an editor's global storage holds several extensions' stores, and
//! the editor's own chat database beside them — so the owner is the first
//! adapter whose root holds the path and that recognises it as one of its
//! sessions, not merely the first whose root holds it.
use std::fs;
use std::path::Path;

use crate::sources::AdoptedSource;
use crate::types::{Adapter, SessionEntry};

/// What a notified path is to the Lake.
pub(super) enum Resolved<'a> {
    /// A session file of this adapter.
    Session(&'a dyn Adapter, SessionEntry),
    /// A path no adapter's root holds, one that vanished, or one a root
    /// holds that is not a session.
    Unowned,
    /// A path a runtime's root holds whose type cannot be read, so no
    /// adapter could tell whether it is a session: a failure of that runtime.
    Unreadable(&'static str, std::io::Error),
}

pub(super) fn owner<'a>(
    adapters: &'a [Box<dyn Adapter>],
    selected: Option<&AdoptedSource>,
    home: &Path,
    path: &Path,
) -> Resolved<'a> {
    let mut holder = None;
    for adapter in adapters {
        let owns = match selected {
            Some(source) => adapter.runtime() == source.runtime && path.starts_with(&source.root),
            None => adapter
                .roots(home)
                .iter()
                .any(|root| path.starts_with(root)),
        };
        if !owns {
            continue;
        }
        if let Some(entry) = adapter.entry_for(path) {
            return Resolved::Session(adapter.as_ref(), entry);
        }
        holder.get_or_insert(adapter.runtime());
    }
    let Some(runtime) = holder else {
        return Resolved::Unowned;
    };
    match fs::symlink_metadata(path) {
        Ok(_) => Resolved::Unowned,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Resolved::Unowned,
        Err(error) => Resolved::Unreadable(runtime, error),
    }
}
