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
        self.write_blob_versioned(key, value, 0, false);
    }

    /// Overwrite (or create) a blob, keeping the previous value in its bounded
    /// history together with the event that wrote it and its taint.
    pub fn write_blob_versioned(&mut self, key: &str, value: BlobValue, event: u64, tainted: bool) {
        let tick = crate::interrupts::current_tick();
        if let Some(blob) = self.blobs.get_mut(key) {
            // A paged-out blob holds only a placeholder in RAM; don't version that.
            if blob.paged_sector.is_none() {
                let old = core::mem::replace(&mut blob.value, BlobValue::Text(String::new()));
                crate::apps_core::push_version(&mut blob.history, crate::apps_core::Version {
                    value: old, event: blob.last_event, tainted: blob.tainted });
            }
            blob.value         = value;
            blob.modified_tick = tick;
            blob.paged_sector  = None; // an overwrite makes it resident again
            blob.tainted       = tainted;
            blob.last_event    = event;
        } else {
            let id   = next_id();
            let mut blob = Blob::new(id, key, value, self.id);
            blob.tainted = tainted;
            blob.last_event = event;
            self.blobs.insert(key.to_string(), blob);
        }
    }

    pub fn read_blob(&self, key: &str) -> Option<&Blob> {
        self.blobs.get(key)
    }
}
