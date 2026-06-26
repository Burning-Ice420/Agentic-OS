// LLM integration — STUB for V1
// Will be implemented after core connectivity is proven.

use serde::{Deserialize, Serialize};

/// Which LLM provider to call.
/// Re-exported from agent.rs for convenience.
pub use crate::agent::LLMProvider;

/// Result of an LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMResponse {
    pub provider: String,
    pub content: String,
    pub tokens_used: u32,
}

/// LLM client — currently a stub that returns a placeholder response.
/// Will be wired up with reqwest calls to OpenAI/Anthropic endpoints.
pub struct LLMClient {
    _http_client: reqwest::Client,
}

impl LLMClient {
    pub fn new() -> Self {
        Self {
            _http_client: reqwest::Client::new(),
        }
    }

    /// Call an LLM provider with the given context.
    /// STUB: Returns a placeholder. Real implementation will POST to provider APIs.
    pub async fn call(
        &self,
        provider: &LLMProvider,
        system_prompt: &str,
        context: &str,
        trigger: &str,
    ) -> Result<LLMResponse, String> {
        tracing::warn!(
            "LLM integration is stubbed — provider={:?}, prompt_len={}, context_len={}, trigger_len={}",
            provider,
            system_prompt.len(),
            context.len(),
            trigger.len()
        );

        match provider {
            LLMProvider::None => Err("Agent has no LLM provider configured".to_string()),
            LLMProvider::OpenAI => {
                // TODO: Implement real OpenAI API call
                // POST https://api.openai.com/v1/chat/completions
                // Model: gpt-4o
                // Timeout: 30s
                Ok(LLMResponse {
                    provider: "OpenAI".to_string(),
                    content: format!("[STUB] OpenAI response to: {}", &trigger[..trigger.len().min(50)]),
                    tokens_used: 0,
                })
            }
            LLMProvider::Anthropic => {
                // TODO: Implement real Anthropic API call
                // POST https://api.anthropic.com/v1/messages
                // Model: claude-sonnet-4-20250514
                // Timeout: 30s
                Ok(LLMResponse {
                    provider: "Anthropic".to_string(),
                    content: format!("[STUB] Anthropic response to: {}", &trigger[..trigger.len().min(50)]),
                    tokens_used: 0,
                })
            }
        }
    }
}
