//! Reactive kernel agents — the kernel-level "AI"
//!
//! Each agent watches blobs in a Memory node and fires actions when
//! conditions are met.  Rules are evaluated on every `tick_all()` call
//! (called ~once per second from the shell loop).
//!
//! For LLM-backed reasoning, see hivemind-vos (the Windows-side layer).
//! In the bare-metal kernel, agents are fully rule-based.
//!
//! Example:
//!   agent new TempMonitor 3       ← watches memory node #3
//!   agent rule 2 temperature gt:80 alert HIGH_TEMP
//!   → whenever blob "temperature" > 80, writes blob "alert" = "HIGH_TEMP"

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::hive;
use crate::hive::blob::BlobValue;

// ── Condition ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Condition {
    /// Number blob strictly greater than threshold.
    Gt(i64),
    /// Number blob strictly less than threshold.
    Lt(i64),
    /// Blob's display value equals the given string.
    Eq(String),
    /// Always triggers (use for unconditional side-effects).
    Any,
}

impl Condition {
    pub fn matches(&self, value: &BlobValue) -> bool {
        match (self, value) {
            (Condition::Any,   _)                    => true,
            (Condition::Gt(t), BlobValue::Number(n)) => *n > *t,
            (Condition::Lt(t), BlobValue::Number(n)) => *n < *t,
            (Condition::Eq(s), BlobValue::Text(t))   => s == t,
            (Condition::Eq(s), BlobValue::Bool(b))   => {
                let rep = if *b { "true" } else { "false" };
                s.as_str() == rep
            }
            _ => false,
        }
    }

    pub fn describe(&self) -> String {
        use core::fmt::Write;
        let mut s = String::new();
        match self {
            Condition::Any    => { let _ = write!(s, "any"); }
            Condition::Gt(n)  => { let _ = write!(s, ">{}", n); }
            Condition::Lt(n)  => { let _ = write!(s, "<{}", n); }
            Condition::Eq(v)  => { let _ = write!(s, "=={}", v); }
        }
        s
    }

    /// Parse a condition string:
    ///   "any"       → Any
    ///   "gt:80"     → Gt(80)
    ///   "lt:-10"    → Lt(-10)
    ///   "eq:online" → Eq("online")
    pub fn parse(s: &str) -> Option<Condition> {
        if s == "any" { return Some(Condition::Any); }
        let mut it  = s.splitn(2, ':');
        let op  = it.next()?;
        let val = it.next()?;
        match op {
            "gt" => val.parse::<i64>().ok().map(Condition::Gt),
            "lt" => val.parse::<i64>().ok().map(Condition::Lt),
            "eq" => Some(Condition::Eq(val.to_string())),
            _    => None,
        }
    }
}

// ── Rule ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Rule {
    pub watch_key:    String,
    pub condition:    Condition,
    pub action_key:   String,
    pub action_value: String,
}

// ── Agent ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct Agent {
    pub id:            u64,
    pub name:          String,
    pub memory_id:     u64,
    pub rules:         Vec<Rule>,
    pub trigger_count: u64,
    /// Whether each rule's condition matched on the previous tick — used for
    /// edge-triggered firing (fire once on false→true, not every tick).
    pub prev_match:    Vec<bool>,
}

impl Agent {
    fn new(id: u64, name: &str, memory_id: u64) -> Self {
        Agent {
            id,
            name:          name.to_string(),
            memory_id,
            rules:         Vec::new(),
            trigger_count: 0,
            prev_match:    Vec::new(),
        }
    }
}

// ── Audit ─────────────────────────────────────────────────────────────────────

/// One recorded agent action — the audit trail backing "auditable by construction".
#[derive(Clone, Debug)]
pub struct AgentEvent {
    pub tick:  u64,
    pub agent: String,
    pub key:   String,
    pub value: String,
}

// ── Runtime ───────────────────────────────────────────────────────────────────

struct AgentRuntime {
    agents:  Vec<Agent>,
    next_id: u64,
    /// Rolling log of the last actions any agent took.
    audit:   Vec<AgentEvent>,
}

const AUDIT_CAP: usize = 64;

lazy_static! {
    static ref RUNTIME: Mutex<AgentRuntime> = Mutex::new(AgentRuntime {
        agents:  Vec::new(),
        next_id: 1,
        audit:   Vec::new(),
    });
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Register the built-in watchdog agent (monitors kernel-root memory node).
pub fn init() {
    let mut rt = RUNTIME.lock();
    let id     = rt.next_id;
    rt.next_id += 1;
    let mut ag = Agent::new(id, "kernel-watchdog", 1);
    // Rule: if kernel-root's "status" == "error" → write "alert" = "kernel_error"
    ag.rules.push(Rule {
        watch_key:    "status".to_string(),
        condition:    Condition::Eq("error".to_string()),
        action_key:   "alert".to_string(),
        action_value: "kernel_error".to_string(),
    });
    ag.prev_match.push(false);
    rt.agents.push(ag);
}

/// Evaluate all agents; fire each rule once on its false→true edge, apply the
/// action blobs, and append to the audit log. Locks are never nested (RUNTIME
/// and HIVE are taken in separate phases).
pub fn tick_all() {
    let tick = crate::interrupts::current_tick();

    // Phase 1 — snapshot each agent's rules + previous match state.
    let snapshots: Vec<(usize, u64, String, Vec<Rule>, Vec<bool>)> = {
        let rt = RUNTIME.lock();
        rt.agents
            .iter()
            .enumerate()
            .map(|(i, a)| (i, a.memory_id, a.name.clone(), a.rules.clone(), a.prev_match.clone()))
            .collect()
    };

    struct Fired {
        idx:        usize,
        cur:        Vec<bool>,             // this tick's match state per rule
        actions:    Vec<(String, String)>, // rising-edge actions only
        name:       String,
        memory_id:  u64,
    }

    // Phase 2 — evaluate conditions (HIVE lock only).
    let mut results: Vec<Fired> = Vec::new();
    for (idx, memory_id, name, rules, prev) in snapshots {
        let cur: Vec<bool> = hive::with_hive(|h| {
            rules
                .iter()
                .map(|r| {
                    h.memories
                        .get(&memory_id)
                        .and_then(|m| m.blobs.get(&r.watch_key))
                        .map(|b| r.condition.matches(&b.value))
                        .unwrap_or(false)
                })
                .collect()
        });

        let mut actions = Vec::new();
        for (ri, r) in rules.iter().enumerate() {
            let was = prev.get(ri).copied().unwrap_or(false);
            if cur[ri] && !was {
                actions.push((r.action_key.clone(), r.action_value.clone()));
            }
        }
        results.push(Fired { idx, cur, actions, name, memory_id });
    }

    // Phase 3 — apply the fired actions to the hive (HIVE lock only).
    let mut audit_events: Vec<AgentEvent> = Vec::new();
    for r in &results {
        if r.actions.is_empty() {
            continue;
        }
        hive::with_hive(|h| {
            for (key, val) in &r.actions {
                h.write_blob(r.memory_id, key, BlobValue::parse(val));
            }
        });
        for (key, val) in &r.actions {
            audit_events.push(AgentEvent {
                tick,
                agent: r.name.clone(),
                key:   key.clone(),
                value: val.clone(),
            });
        }
    }

    // Phase 4 — write back match state, trigger counts, and audit (RUNTIME lock only).
    let mut rt = RUNTIME.lock();
    for r in &results {
        if let Some(ag) = rt.agents.get_mut(r.idx) {
            ag.prev_match = r.cur.clone();
            ag.trigger_count += r.actions.len() as u64;
        }
    }
    for e in audit_events {
        rt.audit.push(e);
    }
    while rt.audit.len() > AUDIT_CAP {
        rt.audit.remove(0);
    }
}

/// Call `f` with the agent audit log (most recent last).
pub fn with_audit<F>(f: F)
where
    F: FnOnce(&[AgentEvent]),
{
    let rt = RUNTIME.lock();
    f(&rt.audit);
}

/// Create a new agent watching `memory_id`. Returns the new agent's ID.
pub fn create_agent(name: &str, memory_id: u64) -> u64 {
    let mut rt = RUNTIME.lock();
    let id     = rt.next_id;
    rt.next_id += 1;
    rt.agents.push(Agent::new(id, name, memory_id));
    id
}

/// Attach a rule to an existing agent. Returns false if `agent_id` is not found.
pub fn add_rule(
    agent_id:   u64,
    watch_key:  &str,
    cond:       Condition,
    action_key: &str,
    action_val: &str,
) -> bool {
    let mut rt = RUNTIME.lock();
    if let Some(ag) = rt.agents.iter_mut().find(|a| a.id == agent_id) {
        ag.rules.push(Rule {
            watch_key:    watch_key.to_string(),
            condition:    cond,
            action_key:   action_key.to_string(),
            action_value: action_val.to_string(),
        });
        ag.prev_match.push(false);
        true
    } else {
        false
    }
}

/// Call `f` with an immutable slice of all agents (RUNTIME lock held during call).
pub fn with_agents<F>(f: F)
where
    F: FnOnce(&[Agent]),
{
    let rt = RUNTIME.lock();
    f(&rt.agents);
}
