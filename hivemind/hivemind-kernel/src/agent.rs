use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Roles an agent can fulfill in the hive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentRole {
    Executor,
    Monitor,
    Planner,
    Router,
    Orchestrator,
}

impl AgentRole {
    pub fn as_str(&self) -> &str {
        match self {
            AgentRole::Executor => "Executor",
            AgentRole::Monitor => "Monitor",
            AgentRole::Planner => "Planner",
            AgentRole::Router => "Router",
            AgentRole::Orchestrator => "Orchestrator",
        }
    }
}

/// Current status of an agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentStatus {
    Idle,
    Running,
    Waiting,
    Error,
}

impl AgentStatus {
    pub fn as_str(&self) -> &str {
        match self {
            AgentStatus::Idle => "Idle",
            AgentStatus::Running => "Running",
            AgentStatus::Waiting => "Waiting",
            AgentStatus::Error => "Error",
        }
    }
}

/// Which LLM provider an agent uses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LLMProvider {
    OpenAI,
    Anthropic,
    None, // No LLM — agent runs purely on rules
}

/// A timestamped action log entry for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub timestamp: u64,
    pub action_type: String,
    pub description: String,
    pub success: bool,
}

impl AgentAction {
    pub fn new(action_type: &str, description: &str, success: bool) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            timestamp: now,
            action_type: action_type.to_string(),
            description: description.to_string(),
            success,
        }
    }
}

/// An autonomous process attached to one or more Memories.
/// Agents read blobs from their home Memory, write results back,
/// and can signal other Memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub role: AgentRole,
    pub home_memory_id: Uuid,
    pub system_prompt: String,
    pub llm_provider: LLMProvider,
    pub status: AgentStatus,
    pub action_log: Vec<AgentAction>,
}

impl Agent {
    /// Create a new agent with the given configuration.
    pub fn new(
        name: String,
        role: AgentRole,
        home_memory_id: Uuid,
        system_prompt: String,
        llm_provider: LLMProvider,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            role,
            home_memory_id,
            system_prompt,
            llm_provider,
            status: AgentStatus::Idle,
            action_log: Vec::new(),
        }
    }

    /// Log an action to the agent's action log (keeps last 100 entries).
    pub fn log_action(&mut self, action_type: &str, description: &str, success: bool) {
        self.action_log.push(AgentAction::new(action_type, description, success));
        if self.action_log.len() > 100 {
            self.action_log.remove(0);
        }
    }

    /// Get the last N actions from the log.
    pub fn last_actions(&self, n: usize) -> Vec<&AgentAction> {
        self.action_log.iter().rev().take(n).collect()
    }

    /// Produce a serializable snapshot for the observer.
    pub fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            id: self.id.to_string(),
            name: self.name.clone(),
            role: self.role.as_str().to_string(),
            home_memory_id: self.home_memory_id.to_string(),
            status: self.status.as_str().to_string(),
            last_actions: self
                .last_actions(5)
                .iter()
                .map(|a| format!("[{}] {}: {}", a.action_type, a.description, if a.success { "OK" } else { "FAIL" }))
                .collect(),
        }
    }
}

/// Thread-safe handle to an Agent.
pub type AgentHandle = Arc<RwLock<Agent>>;

/// Serializable snapshot of an Agent for the observer API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub id: String,
    pub name: String,
    pub role: String,
    pub home_memory_id: String,
    pub status: String,
    pub last_actions: Vec<String>,
}

/// Configuration for spawning a new agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub role: AgentRole,
    pub system_prompt: String,
    pub llm_provider: LLMProvider,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_creation() {
        let mem_id = Uuid::new_v4();
        let agent = Agent::new(
            "test-agent".to_string(),
            AgentRole::Monitor,
            mem_id,
            "You are a monitor.".to_string(),
            LLMProvider::None,
        );
        assert_eq!(agent.name, "test-agent");
        assert_eq!(agent.role, AgentRole::Monitor);
        assert_eq!(agent.status, AgentStatus::Idle);
    }

    #[test]
    fn test_agent_action_log() {
        let mem_id = Uuid::new_v4();
        let mut agent = Agent::new(
            "logger".to_string(),
            AgentRole::Executor,
            mem_id,
            String::new(),
            LLMProvider::None,
        );
        agent.log_action("read", "Read blob key1", true);
        agent.log_action("write", "Wrote blob key2", false);
        assert_eq!(agent.action_log.len(), 2);
        let last = agent.last_actions(1);
        assert_eq!(last.len(), 1);
        assert!(!last[0].success);
    }
}
