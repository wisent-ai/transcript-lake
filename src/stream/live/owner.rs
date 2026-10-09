//! Which harness a changed path belongs to. Roots of different harnesses
//! nest — an editor's global storage holds several extensions' stores, and
//! the editor's own chat database beside them — so the owner is the first
//! adapter whose root holds the path and that recognises it as one of its
//! sessions, not merely the first whose root holds it.
use std::path::Path;

use crate::sources::AdoptedSource;
use crate::types::{Adapter, SessionEntry};

pub(super) fn owner<'a>(
    adapters: &'a [Box<dyn Adapter>],
    selected: Option<&AdoptedSource>,
    home: &Path,
    path: &Path,
) -> Option<(&'a dyn Adapter, SessionEntry)> {
    adapters.iter().find_map(|adapter| {
        let owns = match selected {
            Some(source) => adapter.runtime() == source.runtime && path.starts_with(&source.root),
            None => adapter
                .roots(home)
                .iter()
                .any(|root| path.starts_with(root)),
        };
        if !owns {
            return None;
        }
        adapter
            .entry_for(path)
            .map(|entry| (adapter.as_ref(), entry))
    })
}
