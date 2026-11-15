pub mod blob;
pub mod memory_node;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use blob::BlobValue;
use core::fmt::Write;
use memory_node::MemoryNode;

use crate::cap_core::{ops_string, CapTable, Decision, Subject, OP_INVOKE, OP_READ, OP_WRITE};
use crate::prov::{self, value_attrs, Actor, Kind};
use crate::apps_core::{build_context, last_clean, Context, TaintPolicy};

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
    /// Capability table, enforced at the write/read/invoke boundary below.
    pub caps:       CapTable,
    /// What happens when tainted content would be sent to the model.
    pub taint_policy: TaintPolicy,
}

/// Outcome of an attributed write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    /// Written; carries the provenance event id.
    Written(u64),
    /// Refused by capability policy; carries the `cap_deny` event id.
    Denied(u64),
    NotFound,
}

/// Map a principal to the capability subject it is checked as. Trusted
/// principals (kernel, shell, desktop, restore, shell-initiated model calls)
/// return None and are not checked.
pub fn subject_of(actor: Actor) -> Option<Subject> {
    match actor {
        Actor::Agent(id) | Actor::Model(Some(id)) => Some(Subject::Agent(id)),
        Actor::Peer => Some(Subject::Peer),
        _ => None,
    }
}

fn op_char(op: u8) -> &'static str {
    match op {
        OP_READ => "r",
        OP_WRITE => "w",
        OP_INVOKE => "i",
        _ => "?",
    }
}

impl Hive {
    pub fn new() -> Self {
        Hive {
            memories:   BTreeMap::new(),
            edges:      Vec::new(),
            signal_log: Vec::new(),
            caps:       CapTable::new(),
            taint_policy: TaintPolicy::Off,
        }
    }

    // ── Enforcement point ────────────────────────────────────────────────────

    /// Record a policy decision as a provenance event and in the explain log.
    fn record(&self, actor: Actor, op: u8, mem_id: u64, target: &str, key: &str,
              parent: Option<u64>, d: Decision) -> Result<Option<u64>, u64> {
        let mut who = String::new();
        actor.tag(&mut who);
        match d {
            Decision::NotEnforced => Ok(None),
            Decision::Allow { cap, policy_version } => {
                let mut a = String::new();
                let _ = write!(a, "op={};cap={};pv={}", op_char(op), cap, policy_version);
                let ev = prov::emit(Kind::CapAllow, actor, mem_id, target, key, parent, a);
                let mut t = String::new();
                let rule = self.caps.caps.iter().find(|c| c.id == cap);
                let _ = write!(t, "event {}: {} requested '{}' on {} -> ALLOW, matched capability #{}",
                    ev, who, op_char(op), target, cap);
                if let Some(c) = rule {
                    let _ = write!(t, " (resource {:?}, ops {}, issuer {:?})", c.resource, ops_string(c.ops), c.issuer);
                }
                let _ = write!(t, ", policy v{}", policy_version);
                prov::record_decision(ev, t);
                Ok(Some(ev))
            }
            Decision::Deny { reason, policy_version } => {
                let mut a = String::new();
                let _ = write!(a, "op={};reason={};pv={}", op_char(op), reason.name(), policy_version);
                let ev = prov::emit(Kind::CapDeny, actor, mem_id, target, key, parent, a);
                let mut t = String::new();
                let _ = write!(t, "event {}: {} requested '{}' on {} -> DENY ({}), policy v{}",
                    ev, who, op_char(op), target, reason.name(), policy_version);
                prov::record_decision(ev, t);
                Err(ev)
            }
        }
    }

    /// Check `actor` may perform `op` on a memory node. Ok(Some(allow_event)),
    /// Ok(None) when trusted or not enforced, Err(deny_event) when refused.
    pub fn authorize(&self, actor: Actor, op: u8, mem_id: u64, mem_name: &str, key: &str,
                     parent: Option<u64>) -> Result<Option<u64>, u64> {
        let subj = match subject_of(actor) { Some(s) => s, None => return Ok(None) };
        let d = self.caps.check_node(subj, op, mem_id, mem_name, crate::interrupts::current_tick());
        self.record(actor, op, mem_id, mem_name, key, parent, d)
    }

    /// Check `actor` may invoke a tool (e.g. "ai").
    pub fn authorize_tool(&self, actor: Actor, tool: &str, parent: Option<u64>) -> Result<Option<u64>, u64> {
        let subj = match subject_of(actor) { Some(s) => s, None => return Ok(None) };
        let d = self.caps.check_tool(subj, tool, crate::interrupts::current_tick());
        let mut target = String::from("tool:");
        target.push_str(tool);
        self.record(actor, OP_INVOKE, 0, &target, "-", parent, d)
    }

    /// Attributed, authorized blob write. Every graph write should go through here.
    pub fn write_blob_as(&mut self, actor: Actor, memory_id: u64, key: &str, value: BlobValue,
                         parent: Option<u64>) -> WriteResult {
        self.write_blob_tx(actor, memory_id, key, value, parent, false)
    }

    /// Attributed, authorized write with taint tracking. The new value is tainted
    /// if the writer is a mesh peer or `derived_taint` says it was computed from
    /// tainted content (an agent fired by a tainted value, a model answer whose
    /// context included tainted content).
    pub fn write_blob_tx(&mut self, actor: Actor, memory_id: u64, key: &str, value: BlobValue,
                         parent: Option<u64>, derived_taint: bool) -> WriteResult {
        let name = match self.memories.get(&memory_id) {
            Some(m) => m.name.clone(),
            None => return WriteResult::NotFound,
        };
        let auth = match self.authorize(actor, OP_WRITE, memory_id, &name, key, parent) {
            Ok(a) => a,
            Err(ev) => return WriteResult::Denied(ev),
        };
        let tainted = derived_taint || actor == Actor::Peer;
        let mut a = value_attrs(&value.display());
        if let Some(av) = auth {
            let _ = write!(a, ";auth={}", av);
        }
        if tainted {
            a.push_str(";taint=1");
        }
        let ev = prov::emit(Kind::Write, actor, memory_id, &name, key, parent, a);
        if let Some(mem) = self.memories.get_mut(&memory_id) {
            mem.write_blob_versioned(key, value, ev, tainted);
        }
        WriteResult::Written(ev)
    }

    // ── Taint ──────────────────────────────────────────────────────────────────

    pub fn blob_tainted(&self, memory_id: u64, key: &str) -> bool {
        self.memories.get(&memory_id).and_then(|m| m.blobs.get(key)).map_or(false, |b| b.tainted)
    }

    /// Every tainted blob as (memory_id, node name, key).
    pub fn tainted_blobs(&self) -> Vec<(u64, String, String)> {
        let mut out = Vec::new();
        for (id, m) in &self.memories {
            for (k, b) in &m.blobs {
                if b.tainted {
                    out.push((*id, m.name.clone(), k.clone()));
                }
            }
        }
        out
    }

    /// Build the context sent to the model from a node, under the taint policy.
    /// `only` restricts it to some keys; `skip_suffix` drops derived keys
    /// (e.g. `_tags`) so the model sees the source text, not its own output.
    pub fn model_context(&self, memory_id: u64, only: Option<&[&str]>, skip_suffix: Option<&str>,
                         max_len: usize) -> Context {
        let mut entries = Vec::new();
        if let Some(m) = self.memories.get(&memory_id) {
            for (k, b) in &m.blobs {
                if skip_suffix.map_or(false, |sfx| k.ends_with(sfx)) {
                    continue;
                }
                entries.push((k.clone(), b.value.display(), b.tainted));
            }
        }
        build_context(&entries, only, self.taint_policy, max_len)
    }

    /// Restore one blob to an earlier version, or remove it if `idx` is None.
    /// Recorded as a write by `actor` with `rollback` attributes.
    fn restore_blob(&mut self, actor: Actor, memory_id: u64, key: &str, idx: Option<usize>,
                    parent: Option<u64>) -> Option<u64> {
        let name = self.memories.get(&memory_id)?.name.clone();
        let undone = self.memories.get(&memory_id)?.blobs.get(key)?.last_event;
        match idx {
            Some(i) => {
                let blob = self.memories.get_mut(&memory_id)?.blobs.get_mut(key)?;
                let v = blob.history.remove(i);
                blob.history.truncate(i); // later versions belonged to the undone line
                let mut a = value_attrs(&v.value.display());
                let _ = write!(a, ";rollback_of={};restored_from={}", undone, v.event);
                if v.tainted {
                    a.push_str(";taint=1");
                }
                let ev = prov::emit(Kind::Write, actor, memory_id, &name, key, parent, a);
                blob.value = v.value;
                blob.tainted = v.tainted;
                blob.last_event = ev;
                blob.paged_sector = None;
                blob.modified_tick = crate::interrupts::current_tick();
                Some(ev)
            }
            None => {
                let mut a = String::from("len=0;removed=1");
                let _ = write!(a, ";rollback_of={}", undone);
                let ev = prov::emit(Kind::Write, actor, memory_id, &name, key, parent, a);
                self.memories.get_mut(&memory_id)?.blobs.remove(key);
                Some(ev)
            }
        }
    }

    /// Undo the latest write to a blob (go back one version; remove the blob if
    /// it has no earlier version). Returns the rollback event.
    pub fn rewind(&mut self, actor: Actor, memory_id: u64, key: &str) -> Option<u64> {
        let n = self.memories.get(&memory_id)?.blobs.get(key)?.history.len();
        let idx = if n == 0 { None } else { Some(n - 1) };
        self.restore_blob(actor, memory_id, key, idx, None)
    }

    /// Roll every tainted blob back to its newest clean version, removing blobs
    /// that never had one. Returns (restored, removed, summary_event).
    pub fn quarantine(&mut self, actor: Actor) -> (usize, usize, u64) {
        let targets = self.tainted_blobs();
        let mut a = String::new();
        let _ = write!(a, "tainted={};policy={}", targets.len(), self.taint_policy.name());
        let summary = prov::emit(Kind::Marker, actor, 0, "-", "quarantine", None, a);
        let (mut restored, mut removed) = (0, 0);
        for (id, _, key) in targets {
            let idx = self.memories.get(&id).and_then(|m| m.blobs.get(&key)).and_then(|b| last_clean(&b.history));
            if self.restore_blob(actor, id, &key, idx, Some(summary)).is_some() {
                if idx.is_some() { restored += 1; } else { removed += 1; }
            }
        }
        (restored, removed, summary)
    }

    /// Delete a blob, recorded as a write of nothing by `actor`.
    pub fn remove_blob(&mut self, actor: Actor, memory_id: u64, key: &str, parent: Option<u64>) -> Option<u64> {
        let name = self.memories.get(&memory_id)?.name.clone();
        self.memories.get(&memory_id)?.blobs.get(key)?;
        let ev = prov::emit(Kind::Write, actor, memory_id, &name, key, parent, String::from("len=0;removed=1"));
        self.memories.get_mut(&memory_id)?.blobs.remove(key);
        Some(ev)
    }

    /// Find a node by name, creating it (attributed to `actor`) if missing.
    pub fn node_named(&mut self, actor: Actor, name: &str) -> u64 {
        match self.find_by_name(name) {
            Some(id) => id,
            None => self.create_memory_as(actor, name, None),
        }
    }

    /// Mark a blob tainted without writing it (used when restoring from disk).
    pub fn set_tainted(&mut self, memory_id: u64, key: &str) {
        if let Some(b) = self.memories.get_mut(&memory_id).and_then(|m| m.blobs.get_mut(key)) {
            b.tainted = true;
        }
    }

    /// Record that `actor` observed `key` of a node (`*` = the whole node).
    pub fn note_read(&self, actor: Actor, memory_id: u64, key: &str, parent: Option<u64>, extra: &str) -> u64 {
        let (name, val) = match self.memories.get(&memory_id) {
            Some(m) => (m.name.clone(), m.blobs.get(key).map(|b| b.value.display())),
            None => (String::from("?"), None),
        };
        let mut a = match val {
            Some(v) => value_attrs(&v),
            None => String::from("len=0"),
        };
        if !extra.is_empty() {
            a.push(';');
            a.push_str(extra);
        }
        prov::emit(Kind::Read, actor, memory_id, &name, key, parent, a)
    }

    /// Find a node by name.
    pub fn find_by_name(&self, name: &str) -> Option<u64> {
        self.memories.iter().find(|(_, n)| n.name == name).map(|(&id, _)| id)
    }

    /// Attributed node creation.
    pub fn create_memory_as(&mut self, actor: Actor, name: &str, parent_id: Option<u64>) -> u64 {
        let id = self.create_memory_inner(name, parent_id);
        prov::emit(Kind::Create, actor, id, name, "-", None, String::new());
        id
    }

    /// Create a new MemoryNode (attributed to the kernel), optionally as a child of `parent_id`.
    pub fn create_memory(&mut self, name: &str, parent_id: Option<u64>) -> u64 {
        self.create_memory_as(Actor::Kernel, name, parent_id)
    }

    fn create_memory_inner(&mut self, name: &str, parent_id: Option<u64>) -> u64 {
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

    /// Write a blob attributed to the kernel. Kept for backward compatibility;
    /// prefer `write_blob_as` so the writer is recorded.
    pub fn write_blob(&mut self, memory_id: u64, key: &str, value: BlobValue) -> bool {
        matches!(self.write_blob_as(Actor::Kernel, memory_id, key, value, None), WriteResult::Written(_))
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
