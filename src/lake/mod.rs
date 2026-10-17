//! Where this Lake keeps what it has captured: the resolved paths of every
//! local file it owns, the DuckDB queries that read them, the one source
//! this Lake follows, the whole-file publish every writer of them uses, and
//! the process check that keeps two writers apart.

pub mod duck;
pub mod durable;
pub mod owner;
pub mod paths;
pub mod sources;
