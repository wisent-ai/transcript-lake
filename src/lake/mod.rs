//! Where this Lake keeps what it has captured: the resolved paths of every
//! local file it owns, the DuckDB queries that read them, and the one source
//! this Lake follows.

pub mod duck;
pub mod paths;
pub mod sources;
