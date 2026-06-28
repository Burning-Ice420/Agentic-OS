use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::blob::{Blob, BlobValue};
use super::next_id;

/// A named node in the hive memory graph. Owns a set of blobs.
#[derive(Debug)]
pub struct MemoryNode {
    pub id:            u64,
    pub name:          String,
    pub blobs:         BTreeMap<String, Blob>,
    pub parent_id:     Option<u64>,
    pub children:      Vec<u64>,
    /// IDs of MemoryNodes this node subscribes to (receives signals from).
    pub subscriptions: Vec<u64>,
}

impl MemoryNode {
    pub fn new(id: u64, name: &str) -> Self {
        MemoryNode {
            id,
            name:          name.to_string(),
            blobs:         BTreeMap::new(),
            parent_id:     None,
            children:      Vec::new(),
            subscriptions: Vec::new(),
        }
    }

    pub fn write_blob(&mut self, key: &str, value: BlobValue) {
        let tick = crate::interrupts::current_tick();
        if let Some(blob) = self.blobs.get_mut(key) {
            blob.value         = value;
            blob.modified_tick = tick;
        } else {
            let id   = next_id();
            let blob = Blob::new(id, key, value, self.id);
            self.blobs.insert(key.to_string(), blob);
        }
    }

    pub fn read_blob(&self, key: &str) -> Option<&Blob> {
        self.blobs.get(key)
    }
}
