use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;


use crate::blob::{Blob, BlobSnapshot};

/// Directed edge between two Memory nodes in the hive graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEdge {
    pub from_id: Uuid,
    pub to_id: Uuid,
    pub edge_type: EdgeType,
}

/// Types of directed connections between Memories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EdgeType {
    /// Real-time synchronization of blobs
    Sync,
    /// One-way signal/event channel
    Signal,
    /// Mirror/replica relationship
    Mirror,
    /// Dependency relationship (to_id depends on from_id)
    Dependency,
}

impl EdgeType {
    pub fn as_str(&self) -> &str {
        match self {
            EdgeType::Sync => "Sync",
            EdgeType::Signal => "Signal",
            EdgeType::Mirror => "Mirror",
            EdgeType::Dependency => "Dependency",
        }
    }
}

/// Metadata about a Memory node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMeta {
    pub created_at: u64,
    pub last_sync: u64,
    pub description: String,
}

impl Default for MemoryMeta {
    fn default() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            created_at: now,
            last_sync: now,
            description: String::new(),
        }
    }
}

/// A named, structured node in the hive graph.
/// Contains multiple blobs, spawns sub-agents, and connects to other Memories.
#[derive(Debug, Clone)]
pub struct Memory {
    pub id: Uuid,
    pub name: String,
    pub blobs: HashMap<String, Blob>,
    pub sub_agents: Vec<Uuid>,
    /// IDs of peer Memories this Memory watches for changes
    pub subscriptions: Vec<Uuid>,
    /// Directed connections to other Memories
    pub edges: Vec<MemoryEdge>,
    pub metadata: MemoryMeta,
}

impl Memory {
    /// Create a new empty Memory node with the given name.
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            blobs: HashMap::new(),
            sub_agents: Vec::new(),
            subscriptions: Vec::new(),
            edges: Vec::new(),
            metadata: MemoryMeta::default(),
        }
    }

    /// Subscribe this Memory to changes in another Memory.
    pub fn subscribe_to(&mut self, peer_id: Uuid) {
        if !self.subscriptions.contains(&peer_id) {
            self.subscriptions.push(peer_id);
        }
    }

    /// Add an agent to this Memory.
    pub fn attach_agent(&mut self, agent_id: Uuid) {
        if !self.sub_agents.contains(&agent_id) {
            self.sub_agents.push(agent_id);
        }
    }

    /// Add a directed edge from this Memory to another.
    pub fn add_edge(&mut self, to_id: Uuid, edge_type: EdgeType) {
        // Avoid duplicate edges
        if !self.edges.iter().any(|e| e.to_id == to_id && e.edge_type == edge_type) {
            self.edges.push(MemoryEdge {
                from_id: self.id,
                to_id,
                edge_type,
            });
        }
    }

    /// Update the last_sync timestamp.
    pub fn touch_sync(&mut self) {
        self.metadata.last_sync = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }

    /// Produce a serializable snapshot of this Memory.
    pub fn snapshot(&self) -> MemorySnapshot {
        MemorySnapshot {
            id: self.id.to_string(),
            name: self.name.clone(),
            blobs: self.blobs.values().map(BlobSnapshot::from).collect(),
            edges: self
                .edges
                .iter()
                .map(|e| EdgeSnapshot {
                    from_id: e.from_id.to_string(),
                    to_id: e.to_id.to_string(),
                    edge_type: e.edge_type.as_str().to_string(),
                })
                .collect(),
            subscriptions: self.subscriptions.iter().map(|id| id.to_string()).collect(),
        }
    }
}

/// Serializable snapshot of a Memory for the observer API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub id: String,
    pub name: String,
    pub blobs: Vec<BlobSnapshot>,
    pub edges: Vec<EdgeSnapshot>,
    pub subscriptions: Vec<String>,
}

/// Serializable snapshot of an edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSnapshot {
    pub from_id: String,
    pub to_id: String,
    pub edge_type: String,
}

// We need a Serialize impl for Memory to enable JSON snapshot of the hive.
// Since Memory contains AgentHandles (Arc<RwLock<Agent>>), we use the snapshot method instead.
impl Serialize for Memory {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.snapshot().serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobValue;

    #[test]
    fn test_memory_creation() {
        let mem = Memory::new("test-memory".to_string());
        assert_eq!(mem.name, "test-memory");
        assert!(mem.blobs.is_empty());
        assert!(mem.sub_agents.is_empty());
    }

    #[test]
    fn test_memory_subscription() {
        let mut mem = Memory::new("subscriber".to_string());
        let peer_id = Uuid::new_v4();
        mem.subscribe_to(peer_id);
        mem.subscribe_to(peer_id); // duplicate
        assert_eq!(mem.subscriptions.len(), 1);
    }

    #[test]
    fn test_memory_edge() {
        let mut mem = Memory::new("source".to_string());
        let target_id = Uuid::new_v4();
        mem.add_edge(target_id, EdgeType::Sync);
        mem.add_edge(target_id, EdgeType::Sync); // duplicate
        mem.add_edge(target_id, EdgeType::Signal); // different type, allowed
        assert_eq!(mem.edges.len(), 2);
    }

    #[test]
    fn test_memory_snapshot() {
        let mut mem = Memory::new("snap-test".to_string());
        let blob = crate::blob::Blob::new(
            "key1".to_string(),
            BlobValue::Text("value1".to_string()),
            mem.id,
        );
        mem.blobs.insert("key1".to_string(), blob);
        let snap = mem.snapshot();
        assert_eq!(snap.name, "snap-test");
        assert_eq!(snap.blobs.len(), 1);
    }
}
