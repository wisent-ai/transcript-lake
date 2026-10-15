//! Harnesses built on Gemini CLI's recording of Google GenAI content: Gemini
//! CLI itself and Qwen Code, its fork, which share the part shapes but keep
//! their sessions differently.
mod gemini;
mod parts;
mod qwen;
mod replay;

pub use gemini::Gemini;
pub use qwen::Qwen;
