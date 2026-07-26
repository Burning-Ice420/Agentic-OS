pub mod blob;
pub mod memory_node;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use blob::BlobValue;
use memory_node::MemoryNode;

// ── Global ID counter ─────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Ensure the global counter is at least `id + 1` (used when restoring from disk).
pub fn bump_id(id: u64) {
    let mut cur = NEXT_ID.load(Ordering::Relaxed);
    while cur <= id {
        match NEXT_ID.compare_exchange_weak(cur, id + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(v) => cur = v,
        }
    }
}

// ── Signal ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Signal {
    pub from_id:     u64,
    pub signal_type: String,
    pub payload:     String,
    pub tick:        u64,
}

// ── Edge between memory nodes ─────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum EdgeType {
    Sync,
    Signal,
    Mirror,
    Dependency,
}

impl EdgeType {
    pub fn name(&self) -> &str {
        match self {
            EdgeType::Sync       => "Sync",
            EdgeType::Signal     => "Signal",
            EdgeType::Mirror     => "Mirror",
            EdgeType::Dependency => "Dependency",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryEdge {
    pub from_id:   u64,
    pub to_id:     u64,
    pub edge_type: EdgeType,
}

// ── Hive ──────────────────────────────────────────────────────────────────────

pub struct Hive {
    pub memories:   BTreeMap<u64, MemoryNode>,
    pub edges:      Vec<MemoryEdge>,
    pub signal_log: Vec<Signal>,
}

impl Hive {
    pub fn new() -> Self {
        Hive {
            memories:   BTreeMap::new(),
            edges:      Vec::new(),
            signal_log: Vec::new(),
        }
    }

    /// Create a new MemoryNode, optionally as a child of `parent_id`.
    pub fn create_memory(&mut self, name: &str, parent_id: Option<u64>) -> u64 {
        let id   = next_id();
        let mut node = MemoryNode::new(id, name);
        if let Some(pid) = parent_id {
            node.parent_id = Some(pid);
            // Auto-create a Sync edge from parent → child
            self.edges.push(MemoryEdge {
                from_id:   pid,
                to_id:     id,
                edge_type: EdgeType::Sync,
            });
            if let Some(parent) = self.memories.get_mut(&pid) {
                parent.children.push(id);
            }
        }
        self.memories.insert(id, node);
        id
    }

    /// Write a blob into a memory node. Returns false if the node doesn't exist.
    pub fn write_blob(&mut self, memory_id: u64, key: &str, value: BlobValue) -> bool {
        if let Some(mem) = self.memories.get_mut(&memory_id) {
            mem.write_blob(key, value);
            true
        } else {
            false
        }
    }

    /// Read a blob from a memory node.
    pub fn read_blob(&self, memory_id: u64, key: &str) -> Option<&blob::Blob> {
        self.memories.get(&memory_id)?.read_blob(key)
    }

    /// Create a directed edge between two memory nodes.
    pub fn link_memories(&mut self, from_id: u64, to_id: u64, edge_type: EdgeType) -> bool {
        if !self.memories.contains_key(&from_id) || !self.memories.contains_key(&to_id) {
            return false;
        }
        // Add subscription: to_id subscribes to from_id's signals
        if let Some(to_node) = self.memories.get_mut(&to_id) {
            if !to_node.subscriptions.contains(&from_id) {
                to_node.subscriptions.push(from_id);
            }
        }
        self.edges.push(MemoryEdge { from_id, to_id, edge_type });
        true
    }

    // ── Tiered memory (MemGPT-style hot/cold paging) ───────────────────────────

    /// Page out cold blobs of a node whose last write is older than `age_ticks`,
    /// freeing their in-RAM value to disk. Returns (blobs_paged, bytes_freed).
    pub fn page_out_cold(&mut self, id: u64, now: u64, age_ticks: u64) -> (usize, usize) {
        let mut count = 0;
        let mut freed = 0;
        if let Some(mem) = self.memories.get_mut(&id) {
            for blob in mem.blobs.values_mut() {
                if blob.paged_sector.is_some() {
                    continue; // already paged
                }
                if now.wrapping_sub(blob.modified_tick) < age_ticks {
                    continue; // still hot
                }
                let bytes = blob.value.encode();
                if bytes.len() <= 4 {
                    continue; // not worth paging a tiny value
                }
                if let Some(sector) = crate::disk::page_out(&bytes) {
                    freed += match &blob.value {
                        BlobValue::Text(s)   => s.len(),
                        BlobValue::Binary(b) => b.len(),
                        _ => 0,
                    };
                    blob.value = BlobValue::Text(String::new()); // drop the payload from RAM
                    blob.paged_sector = Some(sector);
                    count += 1;
                }
            }
        }
        (count, freed)
    }

    /// Page a blob back in from disk if it was paged out. Returns Some(true) if a
    /// page-in happened, Some(false) if already resident, None if not found.
    pub fn page_in_blob(&mut self, id: u64, key: &str) -> Option<bool> {
        let mem  = self.memories.get_mut(&id)?;
        let blob = mem.blobs.get_mut(key)?;
        match blob.paged_sector {
            None => Some(false),
            Some(sector) => {
                if let Some(bytes) = crate::disk::page_in(sector) {
                    blob.value = BlobValue::decode(&bytes);
                    blob.paged_sector = None;
                    Some(true)
                } else {
                    Some(false)
                }
            }
        }
    }

    /// (resident, paged) blob counts across the whole hive.
    pub fn tier_counts(&self) -> (usize, usize) {
        let mut resident = 0;
        let mut paged = 0;
        for m in self.memories.values() {
            for b in m.blobs.values() {
                if b.paged_sector.is_some() { paged += 1; } else { resident += 1; }
            }
        }
        (resident, paged)
    }

    // ── Mirror replication (resilient shared state, Chord/RON lineage) ──────────

    /// True if this node has an outgoing Mirror edge, i.e. its writes should be
    /// replicated to peer VMs so a dead node's state survives on a peer.
    pub fn is_mirrored(&self, id: u64) -> bool {
        self.edges.iter().any(|e| e.from_id == id && e.edge_type == EdgeType::Mirror)
    }

    /// The name of a memory node, if it exists.
    pub fn memory_name(&self, id: u64) -> Option<&str> {
        self.memories.get(&id).map(|m| m.name.as_str())
    }

    /// Mark a node as mirrored by adding a Mirror self-edge. Writes to it will then
    /// replicate to any peer VM holding a node of the same name.
    pub fn set_mirror(&mut self, id: u64) -> bool {
        if !self.memories.contains_key(&id) {
            return false;
        }
        if !self.is_mirrored(id) {
            self.edges.push(MemoryEdge { from_id: id, to_id: id, edge_type: EdgeType::Mirror });
        }
        true
    }

    /// Broadcast a signal from a memory node to all subscribers.
    pub fn broadcast_signal(&mut self, from_id: u64, signal_type: &str, payload: &str) {
        let tick = crate::interrupts::current_tick();
        self.signal_log.push(Signal {
            from_id,
            signal_type: signal_type.to_string(),
            payload:     payload.to_string(),
            tick,
        });
        if self.signal_log.len() > 64 {
            self.signal_log.remove(0);
        }
    }

    // ── Stats helpers ─────────────────────────────────────────────────────────

    pub fn total_blobs(&self) -> usize {
        self.memories.values().map(|m| m.blobs.len()).sum()
    }

    pub fn total_edges(&self) -> usize {
        self.edges.len()
    }
}

// ── Global hive singleton ─────────────────────────────────────────────────────

static HIVE: Mutex<Option<Hive>> = Mutex::new(None);

pub fn init() {
    crate::println!("  [hive] new");
    crate::serial_println!("[hive] new");
    let mut h = Hive::new();
    crate::println!("  [hive] create root");
    crate::serial_println!("[hive] create root");
    // Boot memory: the root node that represents the kernel itself
    let root = h.create_memory("kernel-root", None);
    crate::println!("  [hive] write version");
    crate::serial_println!("[hive] write version");
    h.write_blob(root, "version",  BlobValue::Text("0.1".to_string()));
    crate::println!("  [hive] write status");
    crate::serial_println!("[hive] write status");
    h.write_blob(root, "status",   BlobValue::Text("running".to_string()));
    crate::println!("  [hive] store global");
    crate::serial_println!("[hive] store global");
    // Store the hive — lock scope ends here
    {
        *HIVE.lock() = Some(h);
    }
    crate::println!("  [hive] done");
    crate::serial_println!("[hive] done");
}

/// Run a closure with exclusive access to the global hive.
pub fn with_hive<F, R>(f: F) -> R
where
    F: FnOnce(&mut Hive) -> R,
{
    let mut guard = HIVE.lock();
    f(guard.as_mut().expect("Hive not initialized"))
}
