use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The atomic unit of memory in the HiveMind system.
/// A blob is a key-value store entry owned by exactly one Memory node,
/// but can be observed (read-linked) by agents across the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blob {
    pub id: Uuid,
    pub key: String,
    pub value: BlobValue,
    pub created_at: u64,
    pub modified_at: u64,
    pub owner_memory_id: Uuid,
    /// Agent IDs that have read-linked this blob
    pub read_refs: Vec<Uuid>,
}

/// Possible value types stored in a Blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BlobValue {
    Text(String),
    Number(f64),
    Bool(bool),
    Binary(Vec<u8>),
    Json(serde_json::Value),
}

impl Blob {
    /// Create a new blob with the given key and value, owned by the specified memory.
    pub fn new(key: String, value: BlobValue, owner_memory_id: Uuid) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            id: Uuid::new_v4(),
            key,
            value,
            created_at: now,
            modified_at: now,
            owner_memory_id,
            read_refs: Vec::new(),
        }
    }

    /// Touch the blob, updating its modified_at timestamp.
    pub fn touch(&mut self) {
        self.modified_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }

    /// Register a read reference from an agent.
    pub fn add_read_ref(&mut self, agent_id: Uuid) {
        if !self.read_refs.contains(&agent_id) {
            self.read_refs.push(agent_id);
        }
    }
}

/// Snapshot of a blob for the observer API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobSnapshot {
    pub id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub modified_at: u64,
    pub read_refs: Vec<String>,
}

impl From<&Blob> for BlobSnapshot {
    fn from(blob: &Blob) -> Self {
        Self {
            id: blob.id.to_string(),
            key: blob.key.clone(),
            value: serde_json::to_value(&blob.value).unwrap_or_default(),
            modified_at: blob.modified_at,
            read_refs: blob.read_refs.iter().map(|id| id.to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blob_creation() {
        let mem_id = Uuid::new_v4();
        let blob = Blob::new("test_key".to_string(), BlobValue::Text("hello".to_string()), mem_id);
        assert_eq!(blob.key, "test_key");
        assert_eq!(blob.owner_memory_id, mem_id);
        assert!(blob.read_refs.is_empty());
    }

    #[test]
    fn test_blob_read_ref() {
        let mem_id = Uuid::new_v4();
        let mut blob = Blob::new("key".to_string(), BlobValue::Number(42.0), mem_id);
        let agent_id = Uuid::new_v4();
        blob.add_read_ref(agent_id);
        blob.add_read_ref(agent_id); // duplicate
        assert_eq!(blob.read_refs.len(), 1);
    }

    #[test]
    fn test_blob_serialization() {
        let mem_id = Uuid::new_v4();
        let blob = Blob::new("key".to_string(), BlobValue::Json(serde_json::json!({"a": 1})), mem_id);
        let json = serde_json::to_string(&blob).unwrap();
        let deserialized: Blob = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.key, "key");
    }
}
