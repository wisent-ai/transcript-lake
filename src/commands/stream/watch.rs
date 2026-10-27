//! The source watcher that wakes the stream's foreground loop.
//!
//! Every notification becomes a [`Wake`]: a changed path, or the watcher's
//! own error. An error stands for changes the watcher may have lost (an
//! event queue that overflowed, a root it stopped following), so it is sent
//! to the loop rather than dropped, and the loop catches every root up again.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::util::{Error, Result};

/// What wakes the foreground loop. All arrive on one channel, so the loop
/// blocks on it with no tick.
pub(super) enum Wake {
    /// A source path changed.
    Path(PathBuf),
    /// The watcher reported an error instead of an event; which changes it
    /// stood for is unknown.
    Failed(String),
    /// A stop signal arrived.
    Stop,
}

/// Watch every root recursively, sending each changed path and each watcher
/// error to `wake`. The returned watcher keeps the watches alive.
pub(super) fn watch(roots: &[PathBuf], wake: Sender<Wake>) -> Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let wakes: Vec<Wake> = match event {
            Ok(event) => event.paths.into_iter().map(Wake::Path).collect(),
            Err(error) => vec![Wake::Failed(error.to_string())],
        };
        for one in wakes {
            // A send fails only once the loop has stopped and dropped its
            // receiver: nothing is left to wake.
            if wake.send(one).is_err() {
                return;
            }
        }
    })
    .map_err(|error| Error(format!("stream could not start: {error}")))?;
    for root in roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| {
                Error(format!(
                    "stream could not watch {}: {error}",
                    root.display()
                ))
            })?;
    }
    Ok(watcher)
}

/// Stop watching every root, naming each root the watcher could not release.
pub(super) fn unwatch(watcher: &mut RecommendedWatcher, roots: &[PathBuf]) -> Vec<String> {
    roots
        .iter()
        .filter_map(|root| match watcher.unwatch(root) {
            Ok(()) => None,
            Err(error) => Some(format!("{}: {error}", root.display())),
        })
        .collect()
}
