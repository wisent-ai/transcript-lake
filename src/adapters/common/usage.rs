//! Token usage as a harness reports it, carried into the lake one way by every
//! adapter: the fresh input, the cache written and the cache read travel in
//! `extra` under the same names, `tokens_in` is the whole of the input parts
//! that were reported, and `tokens_out` is the output. A part the harness does
//! not report is left out rather than written as zero, so a reader can tell a
//! reported zero from a number nobody stated.

use serde_json::Value;

use crate::types::RawEvent;

/// What one harness record says it spent; `None` is a part it did not report.
pub(crate) struct Usage {
    pub(crate) fresh: Option<i64>,
    pub(crate) cache_write: Option<i64>,
    pub(crate) cache_read: Option<i64>,
    pub(crate) output: Option<i64>,
}

impl Usage {
    /// Whether the record reported any part at all.
    pub(crate) fn is_reported(&self) -> bool {
        self.fresh.is_some()
            || self.cache_write.is_some()
            || self.cache_read.is_some()
            || self.output.is_some()
    }

    /// Write the reported parts onto `event`.
    pub(crate) fn record(&self, event: &mut RawEvent) {
        let input = [self.fresh, self.cache_write, self.cache_read];
        if input.iter().any(Option::is_some) {
            event.tokens_in = Some(input.into_iter().flatten().sum());
        }
        if self.output.is_some() {
            event.tokens_out = self.output;
        }
        for (name, tokens) in [
            ("input_non_cached_tokens", self.fresh),
            ("cache_creation_tokens", self.cache_write),
            ("cache_read_tokens", self.cache_read),
        ] {
            if let Some(tokens) = tokens {
                event.extra.insert(name.into(), Value::from(tokens));
            }
        }
    }
}
