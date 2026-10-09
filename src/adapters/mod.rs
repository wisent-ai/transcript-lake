//! Runtime adapter registry: every harness whose local transcript store the
//! Lake reads, one adapter per runtime id. `hooks` is not here because
//! adaptive-hook telemetry arrives either as closed segments
//! (`crate::hook_segments`) or as the legacy mutable log.
//!
//! The runtime ids are shared with Oko's harness catalogue and Tama's hook
//! adapters, so one harness has one name in every product.
pub mod aider;
pub mod amazon_q;
pub mod amp;
pub mod antigravity;
pub mod claude;
pub mod cline_family;
pub mod codebuddy;
pub mod codewhale;
pub mod codex;
pub mod commandcode;
pub(crate) mod common;
pub mod continue_dev;
pub mod copilot_chat;
pub mod copilot_cli;
pub mod crush;
pub mod cursor;
pub mod cursor_agent;
pub mod devin;
pub mod droid;
pub mod gemini_family;
pub mod goose;
pub mod gptme;
pub mod grok;
pub mod hermes;
pub mod jeden;
pub mod junie;
pub mod kimi;
pub mod kiro;
pub mod muse;
pub mod omp;
pub mod opencode;
pub mod openhands;
pub mod vibe;
pub mod zed;

use crate::types::Adapter;

/// Every transcript adapter, in stable discovery order. A path two roots
/// hold goes to the first adapter that recognises it as a session.
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::CLAUDE),
        Box::new(codex::CODEX),
        Box::new(omp::OMP),
        Box::new(droid::Droid),
        Box::new(kimi::Kimi),
        Box::new(jeden::Jeden),
        Box::new(omp::PI),
        Box::new(omp::SENPI),
        Box::new(omp::GJC),
        Box::new(omp::PRIME),
        Box::new(omp::KIMCHI),
        Box::new(omp::OPENCLAW),
        Box::new(claude::CHERRY_STUDIO),
        Box::new(codex::TRAE),
        Box::new(gemini_family::Gemini),
        Box::new(gemini_family::Qwen),
        Box::new(copilot_cli::CopilotCli),
        Box::new(copilot_chat::CopilotChat),
        Box::new(cline_family::CLINE),
        Box::new(cline_family::ROO),
        Box::new(cline_family::KILOCODE),
        Box::new(continue_dev::Continue),
        Box::new(cursor::Cursor),
        Box::new(cursor_agent::CursorAgent),
        Box::new(opencode::OPENCODE),
        Box::new(opencode::KILO),
        Box::new(opencode::ZCODE),
        Box::new(crush::Crush),
        Box::new(goose::Goose),
        Box::new(zed::Zed),
        Box::new(amazon_q::AMAZON_Q),
        Box::new(amazon_q::KIRO_CLI),
        Box::new(kiro::Kiro),
        Box::new(antigravity::Antigravity),
        Box::new(grok::Grok),
        Box::new(codebuddy::CodeBuddy),
        Box::new(commandcode::CommandCode),
        Box::new(openhands::OpenHands),
        Box::new(gptme::Gptme),
        Box::new(vibe::Vibe),
        Box::new(muse::Muse),
        Box::new(junie::Junie),
        Box::new(amp::Amp),
        Box::new(codewhale::CodeWhale),
        Box::new(hermes::Hermes),
        Box::new(devin::Devin),
        Box::new(aider::Aider),
    ]
}

/// One adapter by runtime name, or `None` for an unknown or non-adapter name.
pub fn by_name(name: &str) -> Option<Box<dyn Adapter>> {
    all().into_iter().find(|adapter| adapter.runtime() == name)
}
