//! The hive cluster: several HiveMind instances working at once.
//!
//!  * The **coordinator** kernel owns the job table and decides placement
//!    (`cluster_core::pick`: round robin, least loaded, or speed-aware). It
//!    watches worker heartbeats, re-routes the jobs of a worker that goes quiet,
//!    re-runs a sample of results itself, and quarantines a worker whose result
//!    does not match.
//!  * A **worker** kernel runs jobs in short slices from the main loop (so its
//!    shell stays usable), sends results, and reports its load every second.
//!  * Results land in the coordinator's shared memory graph (`results` node) as
//!    peer writes: untrusted until verified, like everything from the mesh.
//!
//! Wire (COM2, via the host hub): `HMSG|jobs|<id>|<worker>|<kind>`,
//! `HMSG|results|<id>|<worker>|<result>|<ms>`, `HMSG|load|<worker>|<queued>|<done>|<busy‰>`.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::cluster_core::{ai_prompt, ema, pick, run_to_end, sampled, split_range, Exec, Kind, Policy, WorkerView};
use crate::hive::{self, blob::BlobValue};
use crate::prov::{self, Actor, Kind as EvKind};

const SLICE_MS: u64 = 4;
const HEARTBEAT_MS: u64 = 1000;
const DEAD_MS: u64 = 3500;
pub const RESULTS: &str = "results";
/// Prime jobs start here so every job costs about the same.
const BASE: u64 = 50_000_000;
const AI_NODE: &str = "cluster_ai";

fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Role { Off, Coord, Worker(String) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JState { Waiting, Queued, Assigned, Done }

pub struct Job {
    pub id: String,
    pub kind: Kind,
    pub seq: u64,
    pub state: JState,
    pub worker: Option<String>,
    pub result: Option<String>,
    pub t_assign: u64,
    pub t_done: u64,
    pub compute_ms: u32,
    pub attempts: u32,
    pub verified: bool,
    /// For a check job: the job it re-runs on a different worker.
    pub check_of: Option<usize>,
    /// Worker that must not run this job (the one being checked).
    pub exclude: Option<String>,
}

pub struct Worker {
    pub view: WorkerView,
    pub last_seen: u64,
    pub done: u64,
    pub busy_permille: u32,
    pub queued: u32,
    pub verified_ok: u32,
    pub verified_bad: u32,
    /// Left out of runs by `cluster use N` (still alive, gets no jobs).
    pub parked: bool,
}

pub struct Run {
    pub name: String,
    pub t_start: u64,
    pub t_end: u64,
    pub reassigned: u32,
    pub mismatches: u32,
}

struct WorkerState {
    queue: VecDeque<(String, Kind)>,
    cur: Option<(String, Exec, u64)>,
    ai: Vec<(String, u64)>,
    done: u64,
    busy_cycles: u64,
    win_start: u64,
    last_hb: u64,
    evil: bool,
}

pub struct Cluster {
    pub role: Role,
    tsc_per_ms: u64,
    // coordinator
    pub policy: Policy,
    pub verify_pct: u32,
    pub jobs: Vec<Job>,
    pub workers: Vec<Worker>,
    pub run: Option<Run>,
    pub runs: u32,
    pub use_n: Option<usize>,
    seq: u64,
    /// Disputes the coordinator settles itself: (original job, check job).
    verify_q: VecDeque<(usize, Option<usize>)>,
    verifier: Option<(usize, Option<usize>, Exec)>,
    // worker
    w: WorkerState,
}

static CL: Mutex<Cluster> = Mutex::new(Cluster {
    role: Role::Off,
    tsc_per_ms: 0,
    policy: Policy::Least,
    verify_pct: 0,
    jobs: Vec::new(),
    workers: Vec::new(),
    run: None,
    runs: 0,
    use_n: None,
    seq: 0,
    verify_q: VecDeque::new(),
    verifier: None,
    w: WorkerState { queue: VecDeque::new(), cur: None, ai: Vec::new(), done: 0, busy_cycles: 0,
                     win_start: 0, last_hb: 0, evil: false },
});

fn with<R>(f: impl FnOnce(&mut Cluster) -> R) -> R {
    without_interrupts(|| f(&mut CL.lock()))
}

impl Cluster {
    fn now(&self) -> u64 {
        if self.tsc_per_ms == 0 { 0 } else { tsc() / self.tsc_per_ms }
    }
    fn my_name(&self) -> Option<&str> {
        match &self.role { Role::Worker(n) => Some(n), _ => None }
    }
}

pub fn now_ms() -> u64 { with(|c| c.now()) }

fn calibrate() -> u64 {
    // TSC ticks per PIT tick (54.925 ms) -> per ms
    (crate::bench::calibrate(4) * 1000 / 54_925).max(1)
}

fn line(s: String) {
    crate::sysinfo::with_uuid(|u| crate::serial_println!("CLUSTER|1|{}|{}", u, s));
}

// ── Roles ─────────────────────────────────────────────────────────────────────

pub fn set_role(role: Role) {
    let cal = calibrate();
    crate::net::enable_rx_wakeup();
    with(|c| {
        c.tsc_per_ms = cal;
        c.role = role.clone();
        let now = c.now();
        c.w.win_start = now;
        c.w.last_hb = 0;
    });
    match &role {
        Role::Coord => line(String::from("role=coord")),
        Role::Worker(n) => line(alloc::format!("role=worker;name={}", n)),
        Role::Off => {}
    }
}

pub fn role() -> Role { with(|c| c.role.clone()) }

pub fn set_evil(on: bool) { with(|c| c.w.evil = on); }
pub fn set_policy(p: Policy) { with(|c| c.policy = p); }
pub fn set_verify(p: u32) { with(|c| c.verify_pct = p.min(100)); }

/// Forgive quarantined and lost workers (they rejoin on their next heartbeat).
pub fn reset_workers() {
    with(|c| for w in c.workers.iter_mut() { w.view.quarantined = false; w.view.in_flight = 0; });
}

/// Use only the first `n` workers (by name) for the next runs; 0 = all.
pub fn use_workers(n: usize) {
    with(|c| {
        c.workers.sort_by(|a, b| a.view.name.cmp(&b.view.name));
        c.use_n = if n == 0 { None } else { Some(n) };
        for (i, w) in c.workers.iter_mut().enumerate() { w.parked = n != 0 && i >= n; }
    });
}

/// Should the main loop skip `hlt`? (work in progress, or a run being coordinated)
pub fn busy() -> bool {
    with(|c| match c.role {
        Role::Worker(_) => c.w.cur.is_some() || !c.w.queue.is_empty(),
        // Mesh input wakes the coordinator by interrupt, so it only stays awake
        // while it is recomputing a disputed result.
        Role::Coord => c.verifier.is_some() || !c.verify_q.is_empty(),
        Role::Off => false,
    })
}

// ── Mesh input ────────────────────────────────────────────────────────────────

/// Called by the mesh driver for every incoming message. Returns true if the
/// message was cluster traffic (and so must not be written to the hive as is).
pub fn on_mesh(memory: &str, key: &str, value: &str) -> bool {
    match memory {
        "jobs" => { worker_receive(key, value); true }
        "results" => { coord_result(key, value); true }
        "load" => { coord_load(key, value); true }
        _ => false,
    }
}

fn worker_receive(id: &str, value: &str) {
    let mut it = value.splitn(2, '|');
    let to = it.next().unwrap_or("");
    let spec = it.next().unwrap_or("");
    let refused = with(|c| {
        if c.my_name() != Some(to) { return None; }
        match Kind::decode(spec) {
            Some(k) if !k.is_local() => { c.w.queue.push_back((id.to_string(), k)); Some(false) }
            _ => Some(true),
        }
    });
    if refused == Some(true) {
        // out of bounds or unknown kind: refuse rather than run it
        let me = with(|c| c.my_name().unwrap_or("?").to_string());
        crate::net::send_blob("results", id, &alloc::format!("{}|refused|0", me));
    }
}

fn coord_load(worker: &str, value: &str) {
    let mut it = value.split('|');
    let q: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let done: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let busy: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    with(|c| {
        if c.role != Role::Coord { return; }
        let now = c.now();
        match c.workers.iter_mut().find(|w| w.view.name == worker) {
            Some(w) => { w.last_seen = now; w.view.alive = true; w.queued = q; w.done = done; w.busy_permille = busy; }
            None => c.workers.push(Worker {
                view: WorkerView { name: worker.to_string(), in_flight: 0, alive: true, quarantined: false, ema_ms: 0 },
                last_seen: now, done, busy_permille: busy, queued: q, verified_ok: 0, verified_bad: 0,
                parked: c.use_n.map_or(false, |n| c.workers.len() >= n),
            }),
        }
    });
}

fn coord_result(id: &str, value: &str) {
    let mut it = value.splitn(3, '|');
    let worker = it.next().unwrap_or("").to_string();
    let result = it.next().unwrap_or("").to_string();
    let ms: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let accepted = with(|c| {
        if c.role != Role::Coord { return None; }
        let now = c.now();
        let idx = c.jobs.iter().position(|j| j.id == id)?;
        let j = &mut c.jobs[idx];
        // Only the current assignee's answer counts (a worker that was declared
        // dead and comes back late does not overwrite the re-run).
        if j.state != JState::Assigned || j.worker.as_deref() != Some(worker.as_str()) { return None; }
        if result == "refused" {
            j.state = JState::Queued;
            j.worker = None;
        } else {
            j.state = JState::Done;
            j.result = Some(result.clone());
            j.t_done = now;
            j.compute_ms = ms;
        }
        let seq = j.seq;
        let compute = !j.kind.is_ai() && !j.kind.is_local();
        let wall = (now - j.t_assign) as u32;
        let check_of = j.check_of;
        let kind = j.kind.clone();
        let (jid, attempts) = (j.id.clone(), j.attempts);
        if let Some(w) = c.workers.iter_mut().find(|w| w.view.name == worker) {
            w.view.in_flight = w.view.in_flight.saturating_sub(1);
            if result != "refused" { w.view.ema_ms = ema(w.view.ema_ms, ms.max(1)); }
        }
        if result == "refused" { return Some((wall, seq)); }
        match check_of {
            // A check came back: compare with the original.
            Some(o) => {
                let orig = c.jobs[o].result.clone();
                let ow = c.jobs[o].worker.clone().unwrap_or_default();
                if c.jobs[o].state == JState::Done && orig.as_deref() == Some(result.as_str()) {
                    c.jobs[o].verified = true;
                    for n in [ow.as_str(), worker.as_str()] {
                        if let Some(x) = c.workers.iter_mut().find(|x| x.view.name == n) { x.verified_ok += 1; }
                    }
                } else if c.jobs[o].state == JState::Done {
                    // Two workers disagree: the coordinator settles it.
                    c.verify_q.push_back((o, Some(idx)));
                }
            }
            None if compute && sampled(seq, c.verify_pct) => {
                let usable = c.workers.iter().filter(|w| w.view.usable() && !w.parked && w.view.name != worker).count();
                if usable == 0 {
                    c.verify_q.push_back((idx, None));
                } else {
                    c.seq += 1;
                    let s2 = c.seq;
                    c.jobs.push(Job { id: alloc::format!("{}c{}", jid, attempts), kind, seq: s2, state: JState::Queued,
                        worker: None, result: None, t_assign: 0, t_done: 0, compute_ms: 0, attempts: 0,
                        verified: false, check_of: Some(idx), exclude: Some(worker.clone()) });
                }
            }
            None => {}
        }
        Some((wall, seq))
    });
    if let Some((wall, _)) = accepted {
        if result != "refused" {
            // The result enters the shared memory graph as a peer write: untrusted.
            hive::with_hive(|h| {
                let nid = h.node_named(Actor::Kernel, RESULTS);
                let _ = h.write_blob_as(Actor::Peer, nid, id, BlobValue::Text(result.clone()), None);
            });
        }
        line(alloc::format!("job={};worker={};result={};compute_ms={};wall_ms={}", id, worker, result, ms, wall));
    }
}

/// The answer to a worker's AI job has been written to `cluster_ai/<id>`.
pub fn on_ai_answer(key: &str, value: &str) {
    let msg = with(|c| {
        let me = c.my_name()?.to_string();
        let pos = c.w.ai.iter().position(|(id, _)| id == key)?;
        let (id, t0) = c.w.ai.remove(pos);
        c.w.done += 1;
        let ms = c.now() - t0;
        let short: String = value.chars().filter(|ch| *ch != '|').take(48).collect();
        Some((id, alloc::format!("{}|{}|{}", me, short.trim(), ms)))
    });
    if let Some((id, v)) = msg {
        crate::net::send_blob("results", &id, &v);
    }
}

// ── Worker loop ───────────────────────────────────────────────────────────────

fn worker_tick() {
    // start the next job if idle
    let start: Option<(String, Kind)> = with(|c| {
        if c.w.cur.is_none() { c.w.queue.pop_front() } else { None }
    });
    if let Some((id, k)) = start {
        if k.is_ai() {
            let prompt = match &k { Kind::Ai { i } => ai_prompt(*i), Kind::AiText { text } => text.clone(), _ => String::new() };
            start_ai(&id, &prompt);
        } else if let Some(e) = Exec::new(k) {
            with(|c| { let t = c.now(); c.w.cur = Some((id, e, t)); });
        }
    }
    // run one slice
    let finished = with(|c| {
        let slice = c.tsc_per_ms * SLICE_MS;
        let t0 = tsc();
        let mut out = None;
        if let Some((_, e, _)) = c.w.cur.as_mut() {
            while tsc() - t0 < slice {
                if let Some(r) = e.step(20_000) { out = Some(r); break; }
            }
            c.w.busy_cycles += tsc() - t0;
        }
        let r = out?;
        let (id, _, t_start) = c.w.cur.take()?;
        c.w.done += 1;
        let ms = c.now() - t_start;
        // A deliberately faulty worker (for the verification demo) corrupts one
        // result in five.
        let r = if c.w.evil && c.w.done % 5 == 0 { r ^ 1 } else { r };
        Some((id, alloc::format!("{}|{}|{}", c.my_name().unwrap_or("?"), r, ms)))
    });
    if let Some((id, v)) = finished {
        crate::net::send_blob("results", &id, &v);
    }
    // heartbeat
    let hb = with(|c| {
        let now = c.now();
        if now - c.w.last_hb < HEARTBEAT_MS && c.w.last_hb != 0 { return None; }
        let win = (now - c.w.win_start).max(1);
        let busy = (c.w.busy_cycles / c.tsc_per_ms.max(1)).min(win);
        let permille = (busy * 1000 / win) as u32;
        c.w.busy_cycles = 0;
        c.w.win_start = now;
        c.w.last_hb = now;
        let q = c.w.queue.len() + c.w.cur.is_some() as usize + c.w.ai.len();
        Some((c.my_name()?.to_string(), alloc::format!("{}|{}|{}", q, c.w.done, permille)))
    });
    if let Some((me, v)) = hb {
        crate::net::send_blob("load", &me, &v);
    }
}

fn start_ai(id: &str, prompt: &str) {
    let mem = hive::with_hive(|h| {
        let nid = h.node_named(Actor::Kernel, AI_NODE);
        let mut k = String::from(id);
        k.push_str("_in");
        // The job came over the mesh, so its text is untrusted.
        let _ = h.write_blob_tx(Actor::Kernel, nid, &k, BlobValue::Text(String::from(prompt)), None, true);
        (nid, k)
    });
    let mut p = alloc::format!("@key:{} ", id);
    p.push_str("Answer in at most eight words.");
    let only = [mem.1.as_str()];
    let o = crate::apps::offload(Actor::Kernel, mem.0, &p, Some(&only));
    let sent = matches!(o, crate::apps::Offload::Sent { .. });
    let me = with(|c| {
        if sent { let t = c.now(); c.w.ai.push((id.to_string(), t)); }
        c.my_name().unwrap_or("?").to_string()
    });
    if !sent {
        let (m, _) = crate::apps::offload_msg(&o);
        let short: String = m.chars().filter(|ch| *ch != '|').take(40).collect();
        crate::net::send_blob("results", id, &alloc::format!("{}|no-ai: {}|0", me, short));
    }
}

// ── Coordinator ───────────────────────────────────────────────────────────────

/// Queue a new run. Returns the number of jobs.
pub fn submit(name: &str, kinds: Vec<Kind>) -> usize {
    // fresh results node for the run
    hive::with_hive(|h| {
        if let Some(id) = h.find_by_name(RESULTS) {
            let keys: Vec<String> = h.memories[&id].blobs.keys().cloned().collect();
            for k in keys { let _ = h.remove_blob(Actor::Kernel, id, &k, None); }
        }
    });
    let n = kinds.len();
    let (run_no, wn, pol) = with(|c| {
        c.runs += 1;
        let r = c.runs;
        c.jobs.clear();
        c.verify_q.clear();
        c.verifier = None;
        for w in c.workers.iter_mut() { w.view.in_flight = 0; w.view.ema_ms = 0; w.verified_ok = 0; w.verified_bad = 0; }
        for (i, k) in kinds.into_iter().enumerate() {
            let local = k.is_local();
            c.seq += 1;
            let seq = c.seq;
            c.jobs.push(Job { id: alloc::format!("r{}j{}", r, i + 1), kind: k, seq,
                state: if local { JState::Waiting } else { JState::Queued }, worker: None, result: None,
                t_assign: 0, t_done: 0, compute_ms: 0, attempts: 0, verified: false,
                check_of: None, exclude: None });
        }
        let now = c.now();
        c.run = Some(Run { name: name.to_string(), t_start: now, t_end: 0, reassigned: 0, mismatches: 0 });
        (r, c.workers.iter().filter(|w| w.view.usable() && !w.parked).count(), c.policy)
    });
    let a = alloc::format!("cluster_run={};name={};jobs={};workers={};policy={}", run_no, name, n, wn, pol.name());
    prov::emit(EvKind::Marker, Actor::Shell, 0, "-", "cluster", None, a.clone());
    line(alloc::format!("start;{}", a));
    n
}

/// Build the jobs of a named workload. `size` 0 = default.
pub fn workload(kind: &str, jobs: u64, size: u64) -> Option<Vec<Kind>> {
    let jobs = jobs.clamp(1, 512);
    Some(match kind {
        "prime" => {
            let span = if size == 0 { 1_000_000 } else { size };
            (0..jobs).map(|i| Kind::Prime { lo: BASE + i * span, hi: BASE + (i + 1) * span }).collect()
        }
        "hash" => (0..jobs).map(|i| Kind::Hash { n: if size == 0 { 60_000_000 } else { size }, seed: i }).collect(),
        "matmul" => (0..jobs).map(|i| Kind::Matmul { n: if size == 0 { 96 } else { size as u32 }, seed: i }).collect(),
        "collatz" => {
            let span = if size == 0 { 400_000 } else { size };
            (0..jobs).map(|i| Kind::Collatz { lo: 1 + i * span, hi: 1 + (i + 1) * span }).collect()
        }
        "ai" => (0..jobs).map(|i| Kind::Ai { i: i as u32 }).collect(),
        "mixed" => (0..jobs).map(|i| match i % 3 {
            0 => Kind::Prime { lo: BASE + i * 1_000_000, hi: BASE + (i + 1) * 1_000_000 },
            1 => Kind::Hash { n: 60_000_000, seed: i },
            _ => Kind::Collatz { lo: 1 + i * 400_000, hi: 1 + (i + 1) * 400_000 },
        }).collect(),
        // map (prime counts over slices) -> reduce (sum, on the coordinator) -> AI report on a worker
        "pipeline" => {
            let hi = if size == 0 { BASE + jobs * 1_000_000 } else { size };
            let mut v: Vec<Kind> = split_range(BASE, hi, jobs).into_iter()
                .map(|(a, b)| Kind::Prime { lo: a, hi: b }).collect();
            let r = with(|c| c.runs + 1);
            let deps: Vec<String> = (1..=jobs).map(|i| alloc::format!("r{}j{}", r, i)).collect();
            v.push(Kind::Sum { deps });
            let mut what = alloc::format!("primes between {} and {}", BASE, hi);
            what.truncate(80);
            v.push(Kind::Report { dep: alloc::format!("r{}j{}", r, jobs + 1), what });
            v
        }
        _ => return None,
    })
}

fn coord_tick() {
    let now = now_ms();
    // 1. dead workers: re-route what they held
    let rerouted = with(|c| {
        let mut out = Vec::new();
        let names: Vec<String> = c.workers.iter()
            .filter(|w| w.view.alive && now.saturating_sub(w.last_seen) > DEAD_MS)
            .map(|w| w.view.name.clone()).collect();
        for n in &names {
            if let Some(w) = c.workers.iter_mut().find(|w| &w.view.name == n) {
                w.view.alive = false;
                w.view.in_flight = 0;
            }
            let mut moved = 0;
            for j in c.jobs.iter_mut().filter(|j| j.state == JState::Assigned && j.worker.as_deref() == Some(n.as_str())) {
                j.state = JState::Queued;
                j.worker = None;
                j.attempts += 1;
                moved += 1;
            }
            if let Some(r) = c.run.as_mut() { r.reassigned += moved; }
            out.push((n.clone(), moved));
        }
        out
    });
    for (n, moved) in rerouted {
        let a = alloc::format!("cluster=worker_lost;worker={};rerouted={}", n, moved);
        prov::emit(EvKind::Marker, Actor::Kernel, 0, "-", "cluster", None, a.clone());
        line(a);
        crate::apps::notify(alloc::format!("worker {} went quiet: {} job(s) re-routed", n, moved));
    }

    // 2. local jobs whose inputs are ready (reduce, report)
    resolve_local();

    // 3. placement
    let sends: Vec<(String, String, String)> = with(|c| {
        let mut out = Vec::new();
        let policy = c.policy;
        let now = c.now();
        let mut views: Vec<WorkerView> = c.workers.iter().map(|w| {
            let mut v = w.view.clone();
            if w.parked { v.alive = false; }
            v
        }).collect();
        for j in c.jobs.iter_mut().filter(|j| j.state == JState::Queued) {
            let i = match &j.exclude {
                Some(x) => {
                    let mut v2 = views.clone();
                    for v in v2.iter_mut() { if &v.name == x { v.alive = false; } }
                    match pick(policy, &v2, j.seq) { Some(i) => i, None => continue }
                }
                None => match pick(policy, &views, j.seq) { Some(i) => i, None => break },
            };
            views[i].in_flight += 1;
            j.state = JState::Assigned;
            j.worker = Some(views[i].name.clone());
            j.t_assign = now;
            out.push((j.id.clone(), views[i].name.clone(), j.kind.encode()));
        }
        for (w, v) in c.workers.iter_mut().zip(views.into_iter()) { w.view.in_flight = v.in_flight; }
        out
    });
    for (id, w, spec) in sends {
        let mut v = w.clone();
        v.push('|');
        v.push_str(&spec);
        crate::net::send_blob("jobs", &id, &v);
    }

    // 4. verification: re-run sampled results here, a slice at a time
    verify_tick();

    // 5. completion
    let fin = with(|c| {
        let all_done = !c.jobs.is_empty() && c.jobs.iter().all(|j| j.state == JState::Done);
        let pending_verify = c.verifier.is_some() || !c.verify_q.is_empty();
        let now = c.now();
        let run = c.run.as_mut()?;
        if run.t_end != 0 || !all_done || pending_verify { return None; }
        run.t_end = now.max(run.t_start + 1);
        let ms = run.t_end - run.t_start;
        let mut per = String::new();
        for w in &c.workers {
            let n = c.jobs.iter().filter(|j| j.worker.as_deref() == Some(w.view.name.as_str())).count();
            let _ = write!(per, "{}:{}+", w.view.name, n);
        }
        per.pop();
        let verified = c.jobs.iter().filter(|j| j.verified).count();
        let checks = c.jobs.iter().filter(|j| j.check_of.is_some()).count();
        let originals = c.jobs.len() - checks;
        Some(alloc::format!("done;name={};policy={};jobs={};makespan_ms={};per_worker={};reassigned={};verified={};checks={};mismatches={}",
            run.name, c.policy.name(), originals, ms, per, run.reassigned, verified, checks, run.mismatches))
    });
    if let Some(s) = fin {
        prov::emit(EvKind::Marker, Actor::Kernel, 0, "-", "cluster", None, s.replace("done;", "cluster_done=1;"));
        line(s.clone());
        let ms = s.split(';').find_map(|p| p.strip_prefix("makespan_ms=")).unwrap_or("?").to_string();
        crate::apps::notify(alloc::format!("cluster run finished in {} ms", ms));
    }
}

fn resolve_local() {
    // Sum: all deps done -> done here. Report: dep done -> becomes an AI job.
    let ready: Vec<(usize, Kind)> = with(|c| {
        let mut out = Vec::new();
        for (i, j) in c.jobs.iter().enumerate() {
            if j.state != JState::Waiting { continue; }
            let deps: Vec<&str> = match &j.kind {
                Kind::Sum { deps } => deps.iter().map(|d| d.as_str()).collect(),
                Kind::Report { dep, .. } => alloc::vec![dep.as_str()],
                _ => continue,
            };
            if deps.iter().all(|d| c.jobs.iter().any(|x| x.id == *d && x.state == JState::Done)) {
                out.push((i, j.kind.clone()));
            }
        }
        out
    });
    for (i, k) in ready {
        match k {
            Kind::Sum { deps } => {
                // read the inputs from the shared memory graph
                let (sum, tainted) = hive::with_hive(|h| {
                    let id = match h.find_by_name(RESULTS) { Some(i) => i, None => return (0u64, false) };
                    let mut s = 0u64;
                    let mut t = false;
                    for d in &deps {
                        if let Some(b) = h.memories[&id].blobs.get(d) {
                            s += b.value.display().parse::<u64>().unwrap_or(0);
                            t |= b.tainted;
                        }
                    }
                    (s, t)
                });
                let id = with(|c| {
                    let now = c.now();
                    let j = &mut c.jobs[i];
                    j.state = JState::Done;
                    j.result = Some(alloc::format!("{}", sum));
                    j.worker = Some(String::from("coord"));
                    j.t_assign = now;
                    j.t_done = now;
                    j.id.clone()
                });
                hive::with_hive(|h| {
                    let nid = h.node_named(Actor::Kernel, RESULTS);
                    let _ = h.write_blob_tx(Actor::Kernel, nid, &id, BlobValue::Text(alloc::format!("{}", sum)), None, tainted);
                });
                line(alloc::format!("job={};worker=coord;result={};compute_ms=0;wall_ms=0", id, sum));
            }
            Kind::Report { dep, what } => {
                let v = hive::with_hive(|h| h.find_by_name(RESULTS)
                    .and_then(|id| h.memories[&id].blobs.get(&dep).map(|b| b.value.display())))
                    .unwrap_or_default();
                let mut text = alloc::format!("In one sentence, report that there are {} {}.", v, what);
                text.truncate(190);
                with(|c| { c.jobs[i].kind = Kind::AiText { text }; c.jobs[i].state = JState::Queued; });
            }
            _ => {}
        }
    }
}

fn verify_tick() {
    with(|c| {
        if c.verifier.is_some() { return; }
        while let Some((o, chk)) = c.verify_q.pop_front() {
            if let Some(e) = Exec::new(c.jobs[o].kind.clone()) { c.verifier = Some((o, chk, e)); return; }
        }
    });
    let outcome = with(|c| {
        let slice = c.tsc_per_ms * 2;
        let t0 = tsc();
        let (o, chk, e) = c.verifier.as_mut()?;
        let (o, chk) = (*o, *chk);
        let mut r = None;
        while tsc() - t0 < slice {
            if let Some(x) = e.step(20_000) { r = Some(x); break; }
        }
        let r = r?;
        c.verifier = None;
        let truth = alloc::format!("{}", r);
        let mut wrong: Vec<String> = Vec::new();
        let mut right: Vec<String> = Vec::new();
        for i in core::iter::once(o).chain(chk) {
            let w = c.jobs[i].worker.clone().unwrap_or_default();
            if c.jobs[i].result.as_deref() == Some(truth.as_str()) { right.push(w); } else { wrong.push(w); }
        }
        if wrong.is_empty() { c.jobs[o].verified = true; }
        for w in &right {
            if let Some(x) = c.workers.iter_mut().find(|x| &x.view.name == w) { x.verified_ok += 1; }
        }
        Some((wrong, c.jobs[o].id.clone(), r))
    });
    let (wrong, id, expect) = match outcome { Some(x) => x, None => return };
    for w in wrong {
        quarantine(&w, &id, expect);
    }
}

/// Quarantine a worker that returned a wrong result, and re-run everything it
/// produced in this run on other workers.
fn quarantine(w: &str, id: &str, expect: u64) {
    let requeued = with(|c| {
        if let Some(x) = c.workers.iter_mut().find(|x| x.view.name == w) {
            x.verified_bad += 1;
            x.view.quarantined = true;
            x.view.in_flight = 0;
        }
        let mut redo: Vec<usize> = Vec::new();
        for (i, j) in c.jobs.iter_mut().enumerate() {
            if j.worker.as_deref() == Some(w) && j.check_of.is_none() && !j.kind.is_local() {
                j.state = JState::Queued;
                j.worker = None;
                j.result = None;
                j.verified = false;
                j.attempts += 1;
                redo.push(i);
            }
        }
        // Checks done by the bad worker, or checking a job that will be re-run,
        // are dropped (the re-run is sampled again).
        let mut freed: Vec<String> = Vec::new();
        for j in c.jobs.iter_mut() {
            let drop = j.check_of.map_or(false, |o| redo.contains(&o)) || (j.check_of.is_some() && j.worker.as_deref() == Some(w));
            if drop && j.state != JState::Done {
                if j.state == JState::Assigned {
                    if let Some(x) = j.worker.clone() { if x != w { freed.push(x); } }
                }
                j.state = JState::Done;
                j.result = None;
            }
        }
        for x in freed {
            if let Some(v) = c.workers.iter_mut().find(|v| v.view.name == x) { v.view.in_flight = v.view.in_flight.saturating_sub(1); }
        }
        c.verify_q.retain(|(o, _)| !redo.contains(o));
        let n = redo.len() as u32;
        if let Some(r) = c.run.as_mut() { r.mismatches += 1; r.reassigned += n; }
        n
    });
    let a = alloc::format!("op=w;reason=result_mismatch;worker={};job={};expected={};requeued={}", w, id, expect, requeued);
    let ev = prov::emit(EvKind::CapDeny, Actor::Peer, 0, "-", "cluster", None, a);
    prov::record_decision(ev, alloc::format!("event {}: worker {} returned a wrong result for {}; quarantined, {} job(s) re-run elsewhere", ev, w, id, requeued));
    line(alloc::format!("quarantine;worker={};job={};requeued={}", w, id, requeued));
    crate::apps::notify(alloc::format!("worker {} returned a wrong result: quarantined, {} job(s) re-run", w, requeued));
}

/// Called from the shared background loop.
pub fn tick() {
    match role() {
        Role::Worker(_) => worker_tick(),
        Role::Coord => coord_tick(),
        Role::Off => {}
    }
}

// ── Views for the shell ───────────────────────────────────────────────────────

pub struct Status {
    pub role: Role,
    pub policy: Policy,
    pub verify: u32,
    pub workers: Vec<(String, bool, bool, u32, u64, u32, u32, u32, u32)>,
    pub run: Option<(String, usize, usize, usize, u64, bool, u32, u32)>,
    pub worker_self: Option<(usize, u64, bool)>,
}

pub fn status() -> Status {
    with(|c| {
        let now = c.now();
        Status {
            role: c.role.clone(),
            policy: c.policy,
            verify: c.verify_pct,
            workers: c.workers.iter().map(|w| (w.view.name.clone(), w.view.alive && !w.parked, w.view.quarantined,
                w.view.in_flight, w.done, w.view.ema_ms, w.busy_permille, w.verified_ok, w.verified_bad)).collect(),
            run: c.run.as_ref().map(|r| {
                let done = c.jobs.iter().filter(|j| j.state == JState::Done).count();
                let inflight = c.jobs.iter().filter(|j| j.state == JState::Assigned).count();
                let el = if r.t_end != 0 { r.t_end - r.t_start } else { now - r.t_start };
                (r.name.clone(), c.jobs.len(), done, inflight, el, r.t_end != 0, r.reassigned, r.mismatches)
            }),
            worker_self: match c.role {
                Role::Worker(_) => Some((c.w.queue.len() + c.w.cur.is_some() as usize + c.w.ai.len(), c.w.done, c.w.evil)),
                _ => None,
            },
        }
    })
}

/// Results of the current run, for `cluster results`.
pub fn results(n: usize) -> Vec<(String, String, String, String, u32)> {
    with(|c| c.jobs.iter().filter(|j| j.state == JState::Done).rev().take(n)
        .map(|j| (j.id.clone(), j.kind.name().to_string(), j.worker.clone().unwrap_or_default(),
                  j.result.clone().unwrap_or_default(), j.compute_ms)).collect())
}

/// Check one job's result locally (exposed for the verification test).
pub fn local_result(spec: &str) -> Option<u64> {
    run_to_end(&Kind::decode(spec)?)
}
