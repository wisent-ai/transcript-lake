//! Replay accounts for canonical occurrences instead of appending them twice.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use super::READ_BUFFER;
use crate::oko_export::fingerprint;
use crate::util::{Error, Result};

pub(super) struct Retained {
    occurrences: HashMap<String, usize>,
}

impl Retained {
    pub(super) fn load(data_dir: &Path, runtime: &str, part_name: &str) -> Result<Self> {
        let mut retained = Self {
            occurrences: HashMap::new(),
        };
        let root = data_dir.join("events").join(format!("runtime={runtime}"));
        let dates = match fs::read_dir(&root) {
            Ok(dates) => dates,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(retained),
            Err(error) => {
                return Err(Error(format!(
                    "cannot read canonical source partitions at {}: {error}",
                    root.display()
                )))
            }
        };
        for date in dates {
            let date = date?;
            if !date.file_type()?.is_dir() {
                continue;
            }
            let path = date.path().join(part_name);
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(Error(format!(
                        "cannot open canonical source partition {}: {error}",
                        path.display()
                    )))
                }
            };
            let reader = BufReader::with_capacity(READ_BUFFER, file);
            for (index, line) in reader.lines().enumerate() {
                let line = line
                    .map_err(|error| Error(format!("cannot read {}: {error}", path.display())))?;
                if line.trim().is_empty() {
                    continue;
                }
                let event: Value = serde_json::from_str(&line).map_err(|error| Error(format!(
                    "cannot replay source against corrupt canonical partition {} at line {}: {error}", path.display(), index + 1)))?;
                *retained
                    .occurrences
                    .entry(fingerprint(&event, runtime))
                    .or_default() += 1;
            }
        }
        Ok(retained)
    }

    pub(super) fn already_recorded(&mut self, event: &Value, runtime: &str) -> bool {
        if let Some(remaining) = self.occurrences.get_mut(&fingerprint(event, runtime)) {
            if *remaining > 0 {
                *remaining -= 1;
                return true;
            }
        }
        false
    }
}
