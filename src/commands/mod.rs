//! One module per command family. Every command takes the argument tail after
//! the command name and returns the exit status the process should adopt.
pub mod derived;
pub mod inspect;
pub mod label;
pub mod read;
pub mod stream;
