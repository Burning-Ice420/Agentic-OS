//! Reactive kernel agents — the kernel-level "AI"
//!
//! Each agent watches blobs in a Memory node and fires actions when
//! conditions are met.  Rules are evaluated on every `tick_all()` call
//! (called ~once per second from the shell loop).
//!
//! Rules are the fast, deterministic "reflex" tier. A rule can also DELEGATE its
//! decision to the model accelerator (COM3): give the action value the form
//! "ai:<prompt>" and, when the rule fires, the node's state is offloaded to the
//! model instead of writing a fixed blob. The model's answer arrives as a blob
//! (via llm::poll_and_apply) that the reflex tier can then react to — the two-tier
//! loop, with no human typing `ai`.
//!
//! Example (fixed reflex action):
//!   agent new TempMonitor 3       ← watches memory node #3
//!   agent rule 2 temperature gt:80 alert HIGH_TEMP
//!   → whenever blob "temperature" > 80, writes blob "alert" = "HIGH_TEMP"
//!
//! Example (auto-offload to the model):
//!   agent rule 2 temperature gt:80 decision ai:is this overheating, decide
//!   → when temperature > 80, offloads the node's state to the model; its answer
//!     is written back as a blob the reflex tier can react to.

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
    /// Scheduling priority (higher first under the `priority` policy).
    pub priority:      i32,
    /// Per rule: tick at which a fire was first deferred by the budget.
    pub deferred_since: Vec<Option<u64>>,
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
            priority:      0,
            deferred_since: Vec::new(),
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
    ag.deferred_since.push(None);
    rt.agents.push(ag);
}

/// Evaluate all agents in scheduler order; fire each rule once on its false→true
/// edge (subject to the per-tick budget), check capabilities at the enforcement
/// point, record provenance with parent links, apply actions, and append to the
/// audit log. Lock order: RUNTIME and HIVE are never nested; HIVE -> PROV is ok.
pub fn tick_all() {
    use crate::cap_core::OP_READ;
    use crate::hive::WriteResult;
    use crate::prov::{self, Actor, Kind};
    use core::fmt::Write;

    let tick = crate::interrupts::current_tick();
    let (policy, budget) = {
        let s = SCHED.lock();
        (s.policy, s.budget)
    };

    struct Snap {
        idx: usize,
        agent_id: u64,
        memory_id: u64,
        name: String,
        rules: Vec<Rule>,
        prev: Vec<bool>,
        prio: i32,
    }

    // Phase 1 — snapshot (RUNTIME lock only).
    let snaps: Vec<Snap> = {
        let rt = RUNTIME.lock();
        rt.agents
            .iter()
            .enumerate()
            .map(|(i, a)| Snap {
                idx: i,
                agent_id: a.id,
                memory_id: a.memory_id,
                name: a.name.clone(),
                rules: a.rules.clone(),
                prev: a.prev_match.clone(),
                prio: a.priority,
            })
            .collect()
    };
    let prios: Vec<i32> = snaps.iter().map(|s| s.prio).collect();
    let order = crate::sched_core::order(policy, &prios);

    struct Fired {
        snap: usize,
        cur: Vec<bool>,
        rising: Vec<usize>,
    }

    // Phase 2 — evaluate conditions in scheduler order (HIVE lock only).
    let mut fired: Vec<Fired> = Vec::new();
    for &si in &order {
        let s = &snaps[si];
        let cur: Vec<bool> = hive::with_hive(|h| {
            s.rules
                .iter()
                .map(|r| {
                    h.memories
                        .get(&s.memory_id)
                        .and_then(|m| m.blobs.get(&r.watch_key))
                        .map(|b| r.condition.matches(&b.value))
                        .unwrap_or(false)
                })
                .collect()
        });
        let rising: Vec<usize> = (0..s.rules.len())
            .filter(|&ri| cur[ri] && !s.prev.get(ri).copied().unwrap_or(false))
            .collect();
        fired.push(Fired { snap: si, cur, rising });
    }

    // Scheduling hook — admit up to `budget` actions this tick, defer the rest.
    let wanted: Vec<usize> = fired.iter().map(|f| f.rising.len()).collect();
    let admitted = crate::sched_core::admit(&wanted, budget);

    // Phase 3 — apply admitted actions (HIVE lock per agent). Side effects that
    // need other locks (mesh send, model request) are collected and run after.
    let mut audit_events: Vec<AgentEvent> = Vec::new();
    let mut sends: Vec<(String, String, String, u64, u64, u64)> = Vec::new(); // mem, key, val, mem_id, write_ev, agent
    let mut offloads: Vec<(String, String, String, u64, u64, bool)> = Vec::new(); // mem, task, ctx, offload_ev, agent, tainted
    let mut applied_rules: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut deferred_rules: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut n_applied = 0u64;
    let mut n_deferred = 0u64;
    let mut fired_any = false;

    for (fi, f) in fired.iter().enumerate() {
        if f.rising.is_empty() {
            continue;
        }
        let s = &snaps[f.snap];
        let k = admitted[fi];
        let apply: Vec<usize> = f.rising[..k].to_vec();
        let defer: Vec<usize> = f.rising[k..].to_vec();
        n_deferred += defer.len() as u64;
        deferred_rules.push((s.idx, defer));
        if apply.is_empty() {
            continue;
        }
        let actor = Actor::Agent(s.agent_id);
        let accel = crate::llm::present();
        hive::with_hive(|h| {
            let mem_name = h.memories.get(&s.memory_id).map(|m| m.name.clone()).unwrap_or_default();
            for &ri in &apply {
                let r = &s.rules[ri];
                // The observation that made the rule fire is a read: check it.
                if h.authorize(actor, OP_READ, s.memory_id, &mem_name, &r.watch_key, None).is_err() {
                    continue;
                }
                let mut extra = String::new();
                let _ = write!(extra, "rule={};cond={}", ri, r.condition.describe());
                // Taint flows through the agent: a rule fired by an untrusted value
                // produces an untrusted result (control dependence, conservative).
                let cond_tainted = h.blob_tainted(s.memory_id, &r.watch_key);
                if cond_tainted {
                    extra.push_str(";taint=1");
                }
                let read_ev = h.note_read(actor, s.memory_id, &r.watch_key, None, &extra);
                let mut fa = String::new();
                let _ = write!(fa, "rule={}", ri);
                let fire_ev = prov::emit(Kind::AgentFire, actor, s.memory_id, &mem_name, &r.action_key, Some(read_ev), fa);
                n_applied += 1;
                fired_any = true;

                if let Some(prompt) = r.action_value.strip_prefix("ai:") {
                    // Two-tier offload: needs the "ai" tool capability, and the whole
                    // node leaves the VM as context, so that read is recorded too.
                    let tool_auth = match h.authorize_tool(actor, "ai", Some(fire_ev)) {
                        Ok(a) => a,
                        Err(_) => continue,
                    };
                    // The whole node leaves the VM as context, under the taint policy.
                    let (mctx, withheld) = h.model_context(s.memory_id, None, None, 700);
                    let (ctx, ctx_tainted, redacted, sent) = match mctx {
                        crate::apps_core::Context::Send { ctx, tainted, redacted, included } => (ctx, tainted, redacted, included.join("+")),
                        crate::apps_core::Context::Blocked { tainted_keys } => {
                            let mut da = String::new();
                            let _ = write!(da, "op=i;reason=tainted_context;tainted_keys={}", tainted_keys.len());
                            let dev = prov::emit(Kind::CapDeny, actor, s.memory_id, &mem_name, "tool:ai", Some(fire_ev), da);
                            let mut t = String::new();
                            let _ = write!(t, "event {}: agent {} model call BLOCKED, context held {} tainted key(s)",
                                dev, s.agent_id, tainted_keys.len());
                            prov::record_decision(dev, t);
                            audit_events.push(AgentEvent { tick, agent: s.name.clone(),
                                key: "ai_blocked".to_string(), value: "tainted_context".to_string() });
                            continue;
                        }
                    };
                    let mut ca = String::from("purpose=offload_context;sent=");
                    ca.push_str(&sent);
                    if ctx_tainted { ca.push_str(";taint=1"); }
                    if redacted > 0 { let _ = write!(ca, ";redacted={}", redacted); }
                    if withheld > 0 { let _ = write!(ca, ";private_withheld={}", withheld); }
                    let ctx_ev = h.note_read(actor, s.memory_id, "*", Some(fire_ev), &ca);
                    let task = if prompt.is_empty() { "decide the next action" } else { prompt };
                    let mut oa = String::new();
                    let _ = write!(oa, "tool=ai;prompt_len={};ctx_len={}", task.len(), ctx.len());
                    if let Some(a) = tool_auth {
                        let _ = write!(oa, ";auth={}", a);
                    }
                    if !accel {
                        oa.push_str(";device=absent");
                    }
                    let off_ev = prov::emit(Kind::Offload, actor, s.memory_id, &mem_name, "*", Some(ctx_ev), oa);
                    offloads.push((mem_name.clone(), task.to_string(), ctx, off_ev, s.agent_id, ctx_tainted));
                    audit_events.push(AgentEvent { tick, agent: s.name.clone(),
                        key: "ai_offload".to_string(), value: task.to_string() });
                } else {
                    let val = r.action_value.clone();
                    match h.write_blob_tx(actor, s.memory_id, &r.action_key, BlobValue::parse(&val), Some(fire_ev), cond_tainted) {
                        WriteResult::Written(wev) => {
                            sends.push((mem_name.clone(), r.action_key.clone(), val.clone(), s.memory_id, wev, s.agent_id));
                            audit_events.push(AgentEvent { tick, agent: s.name.clone(),
                                key: r.action_key.clone(), value: val });
                        }
                        WriteResult::Denied(_) | WriteResult::NotFound => {}
                    }
                }
            }
        });
        applied_rules.push((s.idx, apply));
    }

    // Side effects outside the hive lock.
    for (mem, key, val, mem_id, wev, agent) in &sends {
        crate::net::send_blob_tracked(mem, key, val, Actor::Agent(*agent), *mem_id, Some(*wev));
    }
    for (mem, task, ctx, off_ev, agent, tainted) in &offloads {
        let _ = crate::llm::request_tracked(mem, task, ctx, *off_ev, Some(*agent), *tainted);
    }
    if fired_any {
        crate::disk::persist::mark_dirty();
    }

    // Phase 4 — write back match state (deferred rules re-arm so they fire next
    // tick), deferral waits, counters and audit (RUNTIME lock only).
    let mut max_wait = 0u64;
    {
        let mut rt = RUNTIME.lock();
        for f in &fired {
            let s = &snaps[f.snap];
            if let Some(ag) = rt.agents.get_mut(s.idx) {
                let mut cur = f.cur.clone();
                if ag.deferred_since.len() < cur.len() {
                    ag.deferred_since.resize(cur.len(), None);
                }
                for (ri, m) in cur.iter().enumerate() {
                    if !*m {
                        ag.deferred_since[ri] = None; // condition went away; nothing pending
                    }
                }
                if let Some((_, d)) = deferred_rules.iter().find(|(i, _)| *i == s.idx) {
                    for &ri in d {
                        cur[ri] = false; // re-arm: still true next tick -> rising edge again
                        if ag.deferred_since[ri].is_none() {
                            ag.deferred_since[ri] = Some(tick);
                        }
                    }
                }
                if let Some((_, a)) = applied_rules.iter().find(|(i, _)| *i == s.idx) {
                    for &ri in a {
                        if let Some(since) = ag.deferred_since[ri].take() {
                            max_wait = max_wait.max(tick.saturating_sub(since));
                        }
                    }
                    ag.trigger_count += a.len() as u64;
                }
                ag.prev_match = cur;
            }
        }
        for e in audit_events {
            rt.audit.push(e);
        }
        while rt.audit.len() > AUDIT_CAP {
            rt.audit.remove(0);
        }
    }
    let mut st = SCHED_STATS.lock();
    st.ticks += 1;
    st.fired += n_applied;
    st.deferred += n_deferred;
    st.max_wait_ticks = st.max_wait_ticks.max(max_wait);
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
    let id = {
        let mut rt = RUNTIME.lock();
        let id = rt.next_id;
        rt.next_id += 1;
        rt.agents.push(Agent::new(id, name, memory_id));
        id
    };
    let mut a = String::new();
    {
        use core::fmt::Write;
        let _ = write!(a, "agent_id={};name={}", id, crate::prov_core::sanitize(name));
    }
    crate::prov::emit(crate::prov::Kind::AgentSpawn, crate::prov::Actor::Shell, memory_id, "-", "-", None, a);
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
        ag.deferred_since.push(None);
        let mid = ag.memory_id;
        drop(rt);
        let mut a = String::new();
        {
            use core::fmt::Write;
            let _ = write!(a, "agent_id={};action={}", agent_id, crate::prov_core::sanitize(action_val));
        }
        crate::prov::emit(crate::prov::Kind::AgentRule, crate::prov::Actor::Shell, mid, "-", watch_key, None, a);
        true
    } else {
        false
    }
}

// ── Scheduling hook ───────────────────────────────────────────────────────────

static SCHED: Mutex<crate::sched_core::SchedConfig> = Mutex::new(crate::sched_core::SchedConfig::new());
static SCHED_STATS: Mutex<crate::sched_core::SchedStats> = Mutex::new(crate::sched_core::SchedStats {
    ticks: 0, fired: 0, deferred: 0, max_wait_ticks: 0,
});

pub fn set_policy(p: crate::sched_core::Policy) { SCHED.lock().policy = p; }
pub fn set_budget(b: usize) { SCHED.lock().budget = b; }
pub fn policy() -> (crate::sched_core::Policy, usize) { let s = SCHED.lock(); (s.policy, s.budget) }

/// (ticks, fired, deferred, max_wait_ticks)
pub fn sched_stats() -> (u64, u64, u64, u64) {
    let s = SCHED_STATS.lock();
    (s.ticks, s.fired, s.deferred, s.max_wait_ticks)
}

pub fn reset_sched_stats() {
    let mut s = SCHED_STATS.lock();
    s.ticks = 0; s.fired = 0; s.deferred = 0; s.max_wait_ticks = 0;
}

pub fn set_priority(agent_id: u64, prio: i32) -> bool {
    let mut rt = RUNTIME.lock();
    match rt.agents.iter_mut().find(|a| a.id == agent_id) {
        Some(a) => { a.priority = prio; true }
        None => false,
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
