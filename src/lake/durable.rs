//! Publishing a whole file so a reader sees the old bytes or the new ones and
//! never a part: a fresh sibling is written and flushed, renamed over the
//! target, and the directory is flushed after it.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::util::{Error, Result};

/// One step of a publish, named with the path it acted on when it fails.
fn step<T>(result: std::io::Result<T>, action: &str, path: &Path) -> Result<T> {
    result.map_err(|error| Error(format!("could not {action} {}: {error}", path.display())))
}

/// Flush `directory` so the names it holds survive a crash; a failure names
/// the step and the directory.
pub fn sync_directory(directory: &Path) -> Result<()> {
    let opened = step(File::open(directory), "open the directory", directory)?;
    step(opened.sync_all(), "flush the directory", directory)
}

/// Publish `bytes` as the whole content of `path`. A path without a directory
/// or without a file name is refused, because the sibling and the directory
/// flush need both. A failed step names itself and its path; a sibling left
/// behind after a failure is said with why it could not be removed.
pub fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(directory) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(Error(format!(
            "{} names no directory to publish into",
            path.display()
        )));
    };
    let Some(name) = path.file_name() else {
        return Err(Error(format!(
            "{} names no file to publish",
            path.display()
        )));
    };
    step(
        fs::create_dir_all(directory),
        "create the directory",
        directory,
    )?;
    let sibling = directory.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let written = (|| -> Result<()> {
        let mut file = step(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&sibling),
            "create",
            &sibling,
        )?;
        step(file.write_all(bytes), "write", &sibling)?;
        step(file.sync_all(), "flush", &sibling)?;
        drop(file);
        step(
            fs::rename(&sibling, path),
            "rename the new content over",
            path,
        )?;
        sync_directory(directory)
    })();
    if written.is_err() {
        match fs::remove_file(&sibling) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                crate::stream::warn(&format!("{} was left behind: {error}", sibling.display()))
            }
        }
    }
    written
}
