//! Provenance core: actors, event kinds, the bounded event ring and the wire
//! format. Pure logic (only `core` + `alloc`), so the same file is compiled into
//! the kernel and into the host test crate `hivemind-os-logic`.
//!
//! Wire format, one line per event (schema v1), emitted on COM1:
//!   PEV|1|<runtime_uuid>|<event_id>|<tick>|<kind>|<actor>|<mem_id>|<mem_name>|<key>|<parent|->|<attrs>
//! `attrs` is `k=v;k=v`. Values never appear on the wire, only their length and
//! a 32-bit FNV-1a hash (`vh`) so cross-instance writes can be correlated.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// How many events the kernel keeps after shipping them, so in-OS tools
/// (`why`, the Activity window) can answer questions without the host.
pub const RECENT_CAP: usize = 256;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Kernel,
    Shell,
    Desktop,
    Agent(u64),
    /// The model accelerator, acting on behalf of the agent that offloaded (if known).
    Model(Option<u64>),
    Peer,
    Restore,
}

impl Actor {
    pub fn tag(&self, out: &mut String) {
        match self {
            Actor::Kernel => out.push_str("kernel"),
            Actor::Shell => out.push_str("shell"),
            Actor::Desktop => out.push_str("desktop"),
            Actor::Agent(id) => { let _ = write!(out, "agent:{}", id); }
            Actor::Model(Some(id)) => { let _ = write!(out, "model:agent:{}", id); }
            Actor::Model(None) => out.push_str("model:shell"),
            Actor::Peer => out.push_str("peer"),
            Actor::Restore => out.push_str("restore"),
        }
    }

    /// Trusted principals are not subject to capability checks (see THREAT_MODEL).
    pub fn is_trusted(&self) -> bool {
        matches!(self, Actor::Kernel | Actor::Shell | Actor::Desktop | Actor::Restore | Actor::Model(None))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Create,
    Write,
    Read,
    Link,
    AgentFire,
    Offload,
    MeshSend,
    PageOut,
    PageIn,
    RestoreSummary,
    CapAllow,
    CapDeny,
    CapChange,
    AgentSpawn,
    AgentRule,
    Marker,
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Create => "create",
            Kind::Write => "write",
            Kind::Read => "read",
            Kind::Link => "link",
            Kind::AgentFire => "agent_fire",
            Kind::Offload => "offload",
            Kind::MeshSend => "mesh_send",
            Kind::PageOut => "page_out",
            Kind::PageIn => "page_in",
            Kind::RestoreSummary => "restore",
            Kind::CapAllow => "cap_allow",
            Kind::CapDeny => "cap_deny",
            Kind::CapChange => "cap_change",
            Kind::AgentSpawn => "agent_spawned",
            Kind::AgentRule => "agent_rule",
            Kind::Marker => "marker",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u64,
    pub tick: u64,
    pub kind: Kind,
    pub actor: Actor,
    pub mem_id: u64,
    pub mem_name: String,
    pub key: String,
    pub parent: Option<u64>,
    pub attrs: String,
}

/// Bounded FIFO of events awaiting drain. When full, the NEW event is dropped and
/// counted: ids stay monotonic, so the collector also sees the gap.
pub struct Ring {
    buf: VecDeque<Event>,
    cap: usize,
    /// Copies of the most recent events, kept after drain (bounded).
    pub recent: VecDeque<Event>,
    next_id: u64,
    pub emitted: u64,
    pub dropped: u64,
    pub drained: u64,
    pub enabled: bool,
}

impl Ring {
    pub const fn new(cap: usize) -> Self {
        Ring { buf: VecDeque::new(), cap, recent: VecDeque::new(), next_id: 1, emitted: 0, dropped: 0, drained: 0, enabled: true }
    }

    /// Record an event. Returns its id (also when dropped, so parents stay consistent),
    /// or 0 when provenance is disabled.
    #[allow(clippy::too_many_arguments)]
    pub fn emit(&mut self, tick: u64, kind: Kind, actor: Actor, mem_id: u64, mem_name: &str,
                key: &str, parent: Option<u64>, attrs: String) -> u64 {
        if !self.enabled {
            return 0;
        }
        let id = self.next_id;
        self.next_id += 1;
        let ev = Event {
            id, tick, kind, actor, mem_id,
            mem_name: sanitize(mem_name),
            key: sanitize(key),
            parent, attrs,
        };
        if self.recent.len() >= RECENT_CAP {
            self.recent.pop_front();
        }
        self.recent.push_back(ev.clone());
        if self.buf.len() >= self.cap {
            self.dropped += 1;
            return id;
        }
        self.emitted += 1;
        self.buf.push_back(ev);
        id
    }

    pub fn pop(&mut self) -> Option<Event> {
        let e = self.buf.pop_front();
        if e.is_some() {
            self.drained += 1;
        }
        e
    }

    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }
}

/// Replace characters that would break the line format.
pub fn sanitize(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        o.push(match c { '|' | ';' | '\n' | '\r' => '_', c => c });
    }
    o
}

/// 32-bit FNV-1a, used to correlate a value across instances without sending it.
pub fn fnv32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Attributes for a value-carrying event: length + hash, never the value.
pub fn value_attrs(value_display: &str) -> String {
    let mut a = String::new();
    let _ = write!(a, "len={};vh={:08x}", value_display.len(), fnv32(value_display.as_bytes()));
    a
}

/// Render one event as a schema-v1 line (without trailing newline).
pub fn format_line(e: &Event, runtime: &str) -> String {
    let mut s = String::new();
    let _ = write!(s, "PEV|{}|{}|{}|{}|{}|", SCHEMA_VERSION, runtime, e.id, e.tick, e.kind.name());
    e.actor.tag(&mut s);
    let _ = write!(s, "|{}|{}|{}|", e.mem_id, e.mem_name, e.key);
    match e.parent {
        Some(p) => { let _ = write!(s, "{}", p); }
        None => s.push('-'),
    }
    s.push('|');
    s.push_str(&e.attrs);
    s
}

/// Value of one `k=v` attribute.
pub fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    attrs.split(';').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        if it.next()? == key { it.next() } else { None }
    })
}

/// How a step in a why-chain was linked to the step before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Start,
    /// Recorded by the kernel at the time (the event's parent).
    Explicit,
    /// A read linked to the latest earlier write of the same blob, hash-checked.
    ReadFrom,
}

impl Link {
    pub fn name(&self) -> &'static str {
        match self { Link::Start => "", Link::Explicit => "caused by", Link::ReadFrom => "read value from" }
    }
}

/// The latest write to `mem_id/key` with id < `before`.
pub fn latest_write_before<'a>(events: &'a [Event], mem_id: u64, key: &str, before: u64) -> Option<&'a Event> {
    events.iter().rev().find(|e| e.kind == Kind::Write && e.mem_id == mem_id && e.key == key && e.id < before)
}

/// Walk back from event `start` through explicit parents and single-key
/// read-from links, using only events still held in memory. A whole-node read
/// (`*`) stops the walk; its inputs are listed by `node_inputs`.
pub fn why_chain(events: &[Event], start: u64, max: usize) -> Vec<(Event, Link)> {
    let mut out = Vec::new();
    let mut cur = events.iter().find(|e| e.id == start).cloned();
    let mut link = Link::Start;
    while let Some(e) = cur {
        if out.len() >= max || out.iter().any(|(x, _): &(Event, Link)| x.id == e.id) {
            break;
        }
        let next = match e.parent {
            Some(p) => events.iter().find(|x| x.id == p).cloned().map(|x| (x, Link::Explicit)),
            None if e.kind == Kind::Read && e.key != "*" => {
                latest_write_before(events, e.mem_id, &e.key, e.id)
                    .filter(|w| attr(&w.attrs, "vh") == attr(&e.attrs, "vh"))
                    .cloned()
                    .map(|w| (w, Link::ReadFrom))
            }
            None => None,
        };
        out.push((e, link));
        match next {
            Some((n, l)) => { cur = Some(n); link = l; }
            None => cur = None,
        }
    }
    out
}

/// For a whole-node read (offload context), the latest write of every key in
/// that node before it: what the model was actually shown.
pub fn node_inputs(events: &[Event], read: &Event) -> Vec<Event> {
    // `sent=k1+k2` (when present) names exactly the keys that left the VM.
    let sent: Option<Vec<&str>> = attr(&read.attrs, "sent").map(|s| s.split('+').filter(|k| !k.is_empty()).collect());
    let mut keys: Vec<&str> = Vec::new();
    for e in events.iter() {
        if e.kind == Kind::Write && e.mem_id == read.mem_id && e.id < read.id && !keys.contains(&e.key.as_str())
            && sent.as_ref().map_or(true, |s| s.contains(&e.key.as_str())) {
            keys.push(&e.key);
        }
    }
    keys.iter().filter_map(|k| latest_write_before(events, read.mem_id, k, read.id).cloned()).collect()
}
