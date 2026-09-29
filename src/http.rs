//! Shared HTTP plumbing for the model clients behind features (`jev`, `llm`).
//!
//! `ureq::post` (the module function) builds a throwaway agent per call, so
//! every request paid a fresh TCP(+TLS) handshake and connection pools were
//! never reused. Routing every model call through one process-wide agent
//! gives keep-alive pooling for free.

use std::time::Duration;

/// One process-wide shared agent. Connection timeout lives here; read/total
/// timeouts stay per request, where they differ per path (jev: 180 s, chat:
/// 900 s, embeddings: 60 s).
static AGENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .build()
});

pub fn agent() -> &'static ureq::Agent {
    &AGENT
}
