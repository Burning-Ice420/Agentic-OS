use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

use crate::agent::{Agent, AgentConfig, AgentHandle, AgentSnapshot};
use crate::blob::{Blob, BlobValue};
use crate::memory::{EdgeType, Memory, MemorySnapshot};

/// Maximum events kept in the event log.
const MAX_EVENTS: usize = 200;

/// A signal sent between Memory nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveSignal {
    pub id: Uuid,
    pub source_memory_id: Uuid,
    pub target_memory_id: Option<Uuid>, // None = broadcast to all subscribers
    pub signal_type: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

/// A timestamped event in the hive log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub description: String,
}

impl HiveEvent {
    pub fn new(event_type: &str, description: &str) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            event_type: event_type.to_string(),
            description: description.to_string(),
        }
    }
}

/// Live statistics about the hive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveStats {
    pub total_memories: usize,
    pub total_blobs: usize,
    pub total_agents: usize,
    pub signals_per_second: f64,
    pub llm_calls_total: u64,
    pub llm_calls_openai: u64,
    pub llm_calls_anthropic: u64,
    // Internal tracking for signals/sec calculation
    #[serde(skip)]
    pub signal_timestamps: VecDeque<u64>,
}

impl Default for HiveStats {
    fn default() -> Self {
        Self {
            total_memories: 0,
            total_blobs: 0,
            total_agents: 0,
            signals_per_second: 0.0,
            llm_calls_total: 0,
            llm_calls_openai: 0,
            llm_calls_anthropic: 0,
            signal_timestamps: VecDeque::new(),
        }
    }
}

impl HiveStats {
    /// Record a signal and recalculate signals/sec (rolling 5-second window).
    pub fn record_signal(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.signal_timestamps.push_back(now);
        // Remove timestamps older than 5 seconds
        while let Some(&front) = self.signal_timestamps.front() {
            if now - front > 5 {
                self.signal_timestamps.pop_front();
            } else {
                break;
            }
        }
        self.signals_per_second = self.signal_timestamps.len() as f64 / 5.0;
    }
}

/// Full serializable snapshot of the hive for the observer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveSnapshot {
    pub memories: Vec<MemorySnapshot>,
    pub agents: Vec<AgentSnapshot>,
    pub stats: HiveStatsSnapshot,
    pub events: Vec<HiveEventSnapshot>,
}

/// Stats snapshot for the observer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveStatsSnapshot {
    pub total_memories: usize,
    pub total_blobs: usize,
    pub total_agents: usize,
    pub signals_per_second: f64,
    pub llm_calls_total: u64,
    pub llm_calls_openai: u64,
    pub llm_calls_anthropic: u64,
}

/// Event snapshot for the observer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveEventSnapshot {
    pub timestamp: u64,
    pub event_type: String,
    pub description: String,
}

/// The top-level coordinator of the HiveMind system.
/// Manages the global Memory graph, routes signals, enforces write locks,
/// and provides the observer API.
#[derive(Clone)]
pub struct Hive {
    /// All Memory nodes, keyed by ID.
    pub memories: Arc<RwLock<HashMap<Uuid, Memory>>>,
    /// All Agents, keyed by ID.
    pub agents: Arc<RwLock<HashMap<Uuid, AgentHandle>>>,
    /// Broadcast channel for signals.
    pub signal_tx: broadcast::Sender<HiveSignal>,
    /// Event log for the observer.
    pub event_log: Arc<RwLock<VecDeque<HiveEvent>>>,
    /// Live statistics.
    pub stats: Arc<RwLock<HiveStats>>,
}

impl Hive {
    /// Create a new empty Hive.
    pub fn new() -> Self {
        let (signal_tx, _) = broadcast::channel(1024);
        tracing::info!("HiveMind kernel initialized");
        Self {
            memories: Arc::new(RwLock::new(HashMap::new())),
            agents: Arc::new(RwLock::new(HashMap::new())),
            signal_tx,
            event_log: Arc::new(RwLock::new(VecDeque::new())),
            stats: Arc::new(RwLock::new(HiveStats::default())),
        }
    }

    /// Log an event to the event log.
    async fn log_event(&self, event_type: &str, description: &str) {
        let event = HiveEvent::new(event_type, description);
        tracing::info!("[HIVE EVENT] {}: {}", event_type, description);
        let mut log = self.event_log.write().await;
        log.push_back(event);
        if log.len() > MAX_EVENTS {
            log.pop_front();
        }
    }

    /// Update computed stats.
    async fn refresh_stats(&self) {
        let memories = self.memories.read().await;
        let agents = self.agents.read().await;
        let mut stats = self.stats.write().await;
        stats.total_memories = memories.len();
        stats.total_blobs = memories.values().map(|m| m.blobs.len()).sum();
        stats.total_agents = agents.len();
    }

    // ──────────────────────────────────────────────
    // MEMORY OPERATIONS
    // ──────────────────────────────────────────────

    /// Create a new Memory node, optionally as a child of an existing Memory.
    /// If `parent_id` is provided, a Dependency edge is added from parent → child.
    pub async fn create_memory(&self, name: String, parent_id: Option<Uuid>) -> Result<Memory, String> {
        let memory = Memory::new(name.clone());
        let memory_id = memory.id;

        {
            let mut memories = self.memories.write().await;

            // If parent is specified, verify it exists and add edge
            if let Some(pid) = parent_id {
                if let Some(parent) = memories.get_mut(&pid) {
                    parent.add_edge(memory_id, EdgeType::Dependency);
                } else {
                    return Err(format!("Parent memory {} not found", pid));
                }
            }

            memories.insert(memory_id, memory.clone());
        }

        self.refresh_stats().await;
        self.log_event(
            "memory_created",
            &format!("Memory '{}' created (id={})", name, memory_id),
        )
        .await;

        Ok(memory)
    }

    /// Write or update a blob inside a Memory. Triggers subscriptions.
    pub async fn write_blob(
        &self,
        memory_id: Uuid,
        key: String,
        value: BlobValue,
    ) -> Result<Blob, String> {
        let blob;
        let subscribers;

        {
            let mut memories = self.memories.write().await;
            let memory = memories
                .get_mut(&memory_id)
                .ok_or_else(|| format!("Memory {} not found", memory_id))?;

            if let Some(existing) = memory.blobs.get_mut(&key) {
                // Update existing blob
                existing.value = value;
                existing.touch();
                blob = existing.clone();
            } else {
                // Create new blob
                let new_blob = Blob::new(key.clone(), value, memory_id);
                memory.blobs.insert(key.clone(), new_blob.clone());
                blob = new_blob;
            }

            memory.touch_sync();
            subscribers = memory.subscriptions.clone();
        }

        // Notify subscribers via broadcast
        if !subscribers.is_empty() {
            let signal = HiveSignal {
                id: Uuid::new_v4(),
                source_memory_id: memory_id,
                target_memory_id: None,
                signal_type: "blob_updated".to_string(),
                payload: serde_json::json!({
                    "key": key,
                    "blob_id": blob.id.to_string(),
                }),
                timestamp: blob.modified_at,
            };
            let _ = self.signal_tx.send(signal);
        }

        self.refresh_stats().await;
        self.log_event(
            "blob_written",
            &format!("Blob '{}' written in memory {}", key, memory_id),
        )
        .await;

        Ok(blob)
    }

    /// Read a blob from a Memory. Optionally registers a read-ref for an agent.
    pub async fn read_blob(
        &self,
        memory_id: Uuid,
        key: &str,
        reader_agent_id: Option<Uuid>,
    ) -> Result<Option<Blob>, String> {
        let mut memories = self.memories.write().await;
        let memory = memories
            .get_mut(&memory_id)
            .ok_or_else(|| format!("Memory {} not found", memory_id))?;

        if let Some(blob) = memory.blobs.get_mut(key) {
            if let Some(agent_id) = reader_agent_id {
                blob.add_read_ref(agent_id);
            }
            Ok(Some(blob.clone()))
        } else {
            Ok(None)
        }
    }

    /// Get all blobs from a Memory.
    pub async fn get_all_blobs(&self, memory_id: Uuid) -> Result<Vec<Blob>, String> {
        let memories = self.memories.read().await;
        let memory = memories
            .get(&memory_id)
            .ok_or_else(|| format!("Memory {} not found", memory_id))?;
        Ok(memory.blobs.values().cloned().collect())
    }

    /// Create a directed edge between two Memories.
    /// Performs a DAG cycle check to prevent circular dependencies.
    pub async fn link_memories(
        &self,
        from_id: Uuid,
        to_id: Uuid,
        edge_type: EdgeType,
    ) -> Result<(), String> {
        if from_id == to_id {
            return Err("Cannot link a memory to itself".to_string());
        }

        let mut memories = self.memories.write().await;

        // Verify both memories exist
        if !memories.contains_key(&from_id) {
            return Err(format!("Source memory {} not found", from_id));
        }
        if !memories.contains_key(&to_id) {
            return Err(format!("Target memory {} not found", to_id));
        }

        // DAG cycle check: ensure to_id cannot reach from_id through existing edges
        if self.would_create_cycle(&memories, to_id, from_id) {
            return Err(format!(
                "Linking {} -> {} would create a cycle in the memory graph",
                from_id, to_id
            ));
        }

        let from_memory = memories.get_mut(&from_id).unwrap();
        from_memory.add_edge(to_id, edge_type.clone());

        // Auto-subscribe: if edge is Sync or Signal, the target subscribes to the source
        if edge_type == EdgeType::Sync || edge_type == EdgeType::Signal {
            let to_memory = memories.get_mut(&to_id).unwrap();
            to_memory.subscribe_to(from_id);
        }

        drop(memories);

        self.log_event(
            "memories_linked",
            &format!("Edge {:?}: {} -> {}", edge_type, from_id, to_id),
        )
        .await;

        Ok(())
    }

    /// Check if adding an edge from `from` to `to` would create a cycle.
    /// Uses BFS from `from` following existing outgoing edges to see if `to` is reachable.
    fn would_create_cycle(
        &self,
        memories: &HashMap<Uuid, Memory>,
        from: Uuid,
        target: Uuid,
    ) -> bool {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            if current == target {
                return true;
            }
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current);

            if let Some(memory) = memories.get(&current) {
                for edge in &memory.edges {
                    queue.push_back(edge.to_id);
                }
            }
        }

        false
    }

    /// Broadcast a signal from a Memory to all its subscribers.
    pub async fn broadcast_signal(
        &self,
        from_memory_id: Uuid,
        signal_type: String,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        {
            let memories = self.memories.read().await;
            if !memories.contains_key(&from_memory_id) {
                return Err(format!("Source memory {} not found", from_memory_id));
            }
        }

        let signal = HiveSignal {
            id: Uuid::new_v4(),
            source_memory_id: from_memory_id,
            target_memory_id: None,
            signal_type: signal_type.clone(),
            payload,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        let _ = self.signal_tx.send(signal);

        {
            let mut stats = self.stats.write().await;
            stats.record_signal();
        }

        self.log_event(
            "signal_broadcast",
            &format!("Signal '{}' from memory {}", signal_type, from_memory_id),
        )
        .await;

        Ok(())
    }

    // ──────────────────────────────────────────────
    // AGENT OPERATIONS
    // ──────────────────────────────────────────────

    /// Spawn a new agent attached to a Memory.
    pub async fn spawn_agent(
        &self,
        memory_id: Uuid,
        config: AgentConfig,
    ) -> Result<Agent, String> {
        {
            let memories = self.memories.read().await;
            if !memories.contains_key(&memory_id) {
                return Err(format!("Memory {} not found", memory_id));
            }
        }

        let agent = Agent::new(
            config.name.clone(),
            config.role,
            memory_id,
            config.system_prompt,
            config.llm_provider,
        );

        let agent_id = agent.id;
        let handle: AgentHandle = Arc::new(RwLock::new(agent.clone()));

        // Register agent globally
        {
            let mut agents = self.agents.write().await;
            agents.insert(agent_id, handle);
        }

        // Attach agent to its home memory
        {
            let mut memories = self.memories.write().await;
            if let Some(memory) = memories.get_mut(&memory_id) {
                memory.attach_agent(agent_id);
            }
        }

        self.refresh_stats().await;
        self.log_event(
            "agent_spawned",
            &format!("Agent '{}' spawned on memory {}", config.name, memory_id),
        )
        .await;

        Ok(agent)
    }

    /// Get a snapshot of a specific agent.
    pub async fn get_agent(&self, agent_id: Uuid) -> Result<AgentSnapshot, String> {
        let agents = self.agents.read().await;
        let handle = agents
            .get(&agent_id)
            .ok_or_else(|| format!("Agent {} not found", agent_id))?;
        let agent = handle.read().await;
        Ok(agent.snapshot())
    }

    // ──────────────────────────────────────────────
    // MEMORY QUERY
    // ──────────────────────────────────────────────

    /// Get details of a specific Memory.
    pub async fn get_memory(&self, memory_id: Uuid) -> Result<MemorySnapshot, String> {
        let memories = self.memories.read().await;
        let memory = memories
            .get(&memory_id)
            .ok_or_else(|| format!("Memory {} not found", memory_id))?;
        Ok(memory.snapshot())
    }

    /// List all memory IDs and names.
    pub async fn list_memories(&self) -> Vec<(Uuid, String)> {
        let memories = self.memories.read().await;
        memories.values().map(|m| (m.id, m.name.clone())).collect()
    }

    // ──────────────────────────────────────────────
    // SNAPSHOT
    // ──────────────────────────────────────────────

    /// Serialize the full hive graph state to a HiveSnapshot.
    pub async fn snapshot(&self) -> HiveSnapshot {
        self.refresh_stats().await;

        let memories = self.memories.read().await;
        let agents = self.agents.read().await;
        let stats = self.stats.read().await;
        let events = self.event_log.read().await;

        let memory_snapshots: Vec<MemorySnapshot> = memories.values().map(|m| m.snapshot()).collect();

        let mut agent_snapshots = Vec::new();
        for handle in agents.values() {
            let agent = handle.read().await;
            agent_snapshots.push(agent.snapshot());
        }

        let stats_snapshot = HiveStatsSnapshot {
            total_memories: stats.total_memories,
            total_blobs: stats.total_blobs,
            total_agents: stats.total_agents,
            signals_per_second: stats.signals_per_second,
            llm_calls_total: stats.llm_calls_total,
            llm_calls_openai: stats.llm_calls_openai,
            llm_calls_anthropic: stats.llm_calls_anthropic,
        };

        let event_snapshots: Vec<HiveEventSnapshot> = events
            .iter()
            .rev()
            .take(20)
            .map(|e| HiveEventSnapshot {
                timestamp: e.timestamp,
                event_type: e.event_type.clone(),
                description: e.description.clone(),
            })
            .collect();

        HiveSnapshot {
            memories: memory_snapshots,
            agents: agent_snapshots,
            stats: stats_snapshot,
            events: event_snapshots,
        }
    }

    /// Get the last N events.
    pub async fn get_events(&self, n: usize) -> Vec<HiveEvent> {
        let events = self.event_log.read().await;
        events.iter().rev().take(n).cloned().collect()
    }

    /// Subscribe to the signal broadcast channel.
    pub fn subscribe_signals(&self) -> broadcast::Receiver<HiveSignal> {
        self.signal_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentRole;
    use crate::agent::LLMProvider;

    #[tokio::test]
    async fn test_create_memory() {
        let hive = Hive::new();
        let mem = hive.create_memory("test".to_string(), None).await.unwrap();
        assert_eq!(mem.name, "test");

        let snapshot = hive.snapshot().await;
        assert_eq!(snapshot.memories.len(), 1);
    }

    #[tokio::test]
    async fn test_create_memory_with_parent() {
        let hive = Hive::new();
        let parent = hive.create_memory("parent".to_string(), None).await.unwrap();
        let child = hive
            .create_memory("child".to_string(), Some(parent.id))
            .await
            .unwrap();

        let memories = hive.memories.read().await;
        let parent_mem = memories.get(&parent.id).unwrap();
        assert_eq!(parent_mem.edges.len(), 1);
        assert_eq!(parent_mem.edges[0].to_id, child.id);
    }

    #[tokio::test]
    async fn test_write_and_read_blob() {
        let hive = Hive::new();
        let mem = hive.create_memory("blobtest".to_string(), None).await.unwrap();

        let blob = hive
            .write_blob(mem.id, "key1".to_string(), BlobValue::Text("hello".to_string()))
            .await
            .unwrap();
        assert_eq!(blob.key, "key1");

        let read = hive.read_blob(mem.id, "key1", None).await.unwrap();
        assert!(read.is_some());
        assert_eq!(read.unwrap().key, "key1");

        let missing = hive.read_blob(mem.id, "nonexistent", None).await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_link_memories() {
        let hive = Hive::new();
        let m1 = hive.create_memory("m1".to_string(), None).await.unwrap();
        let m2 = hive.create_memory("m2".to_string(), None).await.unwrap();

        hive.link_memories(m1.id, m2.id, EdgeType::Sync).await.unwrap();

        let memories = hive.memories.read().await;
        let m1_mem = memories.get(&m1.id).unwrap();
        assert_eq!(m1_mem.edges.len(), 1);

        // m2 should be auto-subscribed to m1
        let m2_mem = memories.get(&m2.id).unwrap();
        assert!(m2_mem.subscriptions.contains(&m1.id));
    }

    #[tokio::test]
    async fn test_cycle_detection() {
        let hive = Hive::new();
        let m1 = hive.create_memory("m1".to_string(), None).await.unwrap();
        let m2 = hive.create_memory("m2".to_string(), None).await.unwrap();
        let m3 = hive.create_memory("m3".to_string(), None).await.unwrap();

        hive.link_memories(m1.id, m2.id, EdgeType::Dependency).await.unwrap();
        hive.link_memories(m2.id, m3.id, EdgeType::Dependency).await.unwrap();

        // This should fail — would create m1 -> m2 -> m3 -> m1 cycle
        let result = hive.link_memories(m3.id, m1.id, EdgeType::Dependency).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cycle"));
    }

    #[tokio::test]
    async fn test_self_link_rejected() {
        let hive = Hive::new();
        let m1 = hive.create_memory("m1".to_string(), None).await.unwrap();
        let result = hive.link_memories(m1.id, m1.id, EdgeType::Sync).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_spawn_agent() {
        let hive = Hive::new();
        let mem = hive.create_memory("agent-home".to_string(), None).await.unwrap();

        let agent = hive
            .spawn_agent(
                mem.id,
                AgentConfig {
                    name: "test-agent".to_string(),
                    role: AgentRole::Monitor,
                    system_prompt: "You monitor things.".to_string(),
                    llm_provider: LLMProvider::None,
                },
            )
            .await
            .unwrap();

        assert_eq!(agent.name, "test-agent");

        let snapshot = hive.snapshot().await;
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.stats.total_agents, 1);
    }

    #[tokio::test]
    async fn test_broadcast_signal() {
        let hive = Hive::new();
        let mem = hive.create_memory("signaler".to_string(), None).await.unwrap();

        let mut rx = hive.subscribe_signals();

        hive.broadcast_signal(mem.id, "test_signal".to_string(), serde_json::json!({"data": "hello"}))
            .await
            .unwrap();

        let signal = rx.recv().await.unwrap();
        assert_eq!(signal.signal_type, "test_signal");
        assert_eq!(signal.source_memory_id, mem.id);
    }

    #[tokio::test]
    async fn test_full_snapshot() {
        let hive = Hive::new();
        let m1 = hive.create_memory("mem1".to_string(), None).await.unwrap();
        let m2 = hive.create_memory("mem2".to_string(), None).await.unwrap();

        hive.write_blob(m1.id, "k1".to_string(), BlobValue::Number(3.14)).await.unwrap();
        hive.write_blob(m2.id, "k2".to_string(), BlobValue::Bool(true)).await.unwrap();

        hive.link_memories(m1.id, m2.id, EdgeType::Signal).await.unwrap();

        hive.spawn_agent(
            m1.id,
            AgentConfig {
                name: "observer".to_string(),
                role: AgentRole::Monitor,
                system_prompt: String::new(),
                llm_provider: LLMProvider::None,
            },
        )
        .await
        .unwrap();

        let snapshot = hive.snapshot().await;
        assert_eq!(snapshot.memories.len(), 2);
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.stats.total_blobs, 2);

        // Ensure snapshot serializes to JSON without errors
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        assert!(json.contains("mem1"));
        assert!(json.contains("mem2"));
    }
}
