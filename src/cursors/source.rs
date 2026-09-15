//! The exact source generation and consumed-prefix digest behind a cursor.
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCheckpoint {
    pub sha256: [u8; 32],
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanos: i64,
}

impl SourceCheckpoint {
    pub fn new(meta: &Metadata, sha256: [u8; 32]) -> Self {
        Self {
            sha256,
            device: meta.dev(),
            inode: meta.ino(),
            changed_seconds: meta.ctime(),
            changed_nanos: meta.ctime_nsec(),
        }
    }

    pub fn matches(&self, meta: &Metadata) -> bool {
        self.device == meta.dev()
            && self.inode == meta.ino()
            && self.changed_seconds == meta.ctime()
            && self.changed_nanos == meta.ctime_nsec()
    }
}
