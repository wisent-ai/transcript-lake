//! Where this Lake keeps what it has captured: the resolved paths of every
//! local file it owns, the DuckDB queries that read them, the one source
//! this Lake follows, and the whole-file publish every writer of them uses.

pub mod duck;
pub mod durable;
pub mod paths;
pub mod sources;
