//! Runtime adapter registry. One module per supported local transcript store;
//! `hooks` is not here because adaptive-hook telemetry arrives either as closed
//! segments (`crate::hook_segments`) or as the legacy mutable log.
pub mod claude;
pub mod codex;
pub mod droid;
pub mod kimi;
pub mod omp;

use crate::types::Adapter;

/// Every transcript adapter, in the order ingest walks them.
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::Claude),
        Box::new(codex::Codex),
        Box::new(omp::Omp),
        Box::new(droid::Droid),
        Box::new(kimi::Kimi),
    ]
}

/// One adapter by runtime name, or `None` for an unknown or non-adapter name.
pub fn by_name(name: &str) -> Option<Box<dyn Adapter>> {
    match name {
        "claude" => Some(Box::new(claude::Claude)),
        "codex" => Some(Box::new(codex::Codex)),
        "omp" => Some(Box::new(omp::Omp)),
        "droid" => Some(Box::new(droid::Droid)),
        "kimi" => Some(Box::new(kimi::Kimi)),
        _ => None,
    }
}
