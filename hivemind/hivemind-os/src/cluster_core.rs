//! Pure logic for the hive cluster: job kinds that run in slices, and the
//! scheduling policies the coordinator kernel uses to place jobs on workers.
//! Only `core` + `alloc`, so the host crate unit-tests this exact file.
//!
//! Jobs are a fixed set of kinds with bounded parameters. No code travels over
//! the mesh: a worker can only be asked to run one of these functions.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

// ── Job kinds ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Count primes in [lo, hi) by trial division.
    Prime { lo: u64, hi: u64 },
    /// `n` rounds of a 64-bit mixing function from `seed`.
    Hash { n: u64, seed: u64 },
    /// n x n integer matrix product (wrapping u32), checksum of the result.
    Matmul { n: u32, seed: u64 },
    /// Longest Collatz chain start in [lo, hi), encoded as start*1000+steps.
    Collatz { lo: u64, hi: u64 },
    /// Ask the model a short question (the text is generated from `i`).
    Ai { i: u32 },
    /// Ask the model with a prompt built by the coordinator.
    AiText { text: String },
    /// Reduce on the coordinator: sum the numeric results of other jobs.
    Sum { deps: Vec<String> },
    /// AI report on another job's result (resolved by the coordinator into AiText).
    Report { dep: String, what: String },
}

pub const MAX_HASH: u64 = 200_000_000;
pub const MAX_RANGE: u64 = 50_000_000;
pub const MAX_MAT: u32 = 96;

fn num(s: Option<&str>) -> Option<u64> {
    s?.parse().ok()
}

impl Kind {
    pub fn encode(&self) -> String {
        let mut s = String::new();
        let _ = match self {
            Kind::Prime { lo, hi } => write!(s, "prime:{}:{}", lo, hi),
            Kind::Hash { n, seed } => write!(s, "hash:{}:{}", n, seed),
            Kind::Matmul { n, seed } => write!(s, "matmul:{}:{}", n, seed),
            Kind::Collatz { lo, hi } => write!(s, "collatz:{}:{}", lo, hi),
            Kind::Ai { i } => write!(s, "ai:{}", i),
            Kind::AiText { text } => write!(s, "aitext:{}", clean(text)),
            Kind::Sum { deps } => write!(s, "sum:{}", deps.join("+")),
            Kind::Report { dep, what } => write!(s, "report:{}:{}", dep, clean(what)),
        };
        s
    }

    /// Parse and bound-check. Anything out of range is refused, so a peer
    /// cannot ask a worker for unbounded work.
    pub fn decode(s: &str) -> Option<Kind> {
        let mut it = s.splitn(3, ':');
        let k = it.next()?;
        match k {
            "prime" | "collatz" => {
                let lo = num(it.next())?;
                let hi = num(it.next())?;
                if hi <= lo || hi - lo > MAX_RANGE { return None; }
                Some(if k == "prime" { Kind::Prime { lo, hi } } else { Kind::Collatz { lo: lo.max(1), hi } })
            }
            "hash" => {
                let n = num(it.next())?;
                let seed = num(it.next())?;
                if n == 0 || n > MAX_HASH { return None; }
                Some(Kind::Hash { n, seed })
            }
            "matmul" => {
                let n = num(it.next())? as u32;
                let seed = num(it.next())?;
                if n == 0 || n > MAX_MAT { return None; }
                Some(Kind::Matmul { n, seed })
            }
            "ai" => Some(Kind::Ai { i: num(it.next())? as u32 }),
            "aitext" => {
                let mut rest = String::from(it.next().unwrap_or(""));
                if let Some(r) = it.next() { rest.push(':'); rest.push_str(r); }
                if rest.is_empty() || rest.len() > 200 { return None; }
                Some(Kind::AiText { text: rest })
            }
            "sum" => {
                let deps: Vec<String> = it.next()?.split('+').filter(|d| !d.is_empty()).map(|d| d.to_string()).collect();
                if deps.is_empty() { None } else { Some(Kind::Sum { deps }) }
            }
            "report" => Some(Kind::Report { dep: it.next()?.to_string(), what: it.next().unwrap_or("").to_string() }),
            _ => None,
        }
    }

    pub fn is_ai(&self) -> bool {
        matches!(self, Kind::Ai { .. } | Kind::AiText { .. })
    }

    /// Runs on the coordinator itself, not on a worker.
    pub fn is_local(&self) -> bool {
        matches!(self, Kind::Sum { .. } | Kind::Report { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Kind::Prime { .. } => "prime", Kind::Hash { .. } => "hash", Kind::Matmul { .. } => "matmul",
            Kind::Collatz { .. } => "collatz", Kind::Ai { .. } => "ai", Kind::AiText { .. } => "ai",
            Kind::Sum { .. } => "sum", Kind::Report { .. } => "report",
        }
    }
}

fn clean(s: &str) -> String {
    s.chars().map(|c| match c { '|' | '\n' | '\r' => ' ', c => c }).collect()
}

/// Short texts for AI jobs (sentiment classification), picked by index.
pub const AI_TEXTS: [&str; 12] = [
    "The update fixed my battery drain, great job.",
    "The app crashes every time I open the camera.",
    "Delivery was on time and the box was intact.",
    "Support never answered my three emails.",
    "Setup took two minutes, very smooth.",
    "The screen flickers after the latest patch.",
    "Love the new dark mode.",
    "I was charged twice for one order.",
    "Fast, quiet and it just works.",
    "The manual is missing half the steps.",
    "Best purchase I made this year.",
    "It stopped charging after one week.",
];

pub fn ai_prompt(i: u32) -> String {
    let mut s = String::from("Classify the sentiment as positive or negative, one word: ");
    s.push_str(AI_TEXTS[i as usize % AI_TEXTS.len()]);
    s
}

// ── Execution in slices ───────────────────────────────────────────────────────

fn mix(mut x: u64) -> u64 {
    // splitmix64 finaliser
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn is_prime(n: u64) -> bool {
    if n < 2 { return false; }
    if n < 4 { return true; }
    if n % 2 == 0 { return false; }
    let mut d = 3;
    while d * d <= n {
        if n % d == 0 { return false; }
        d += 2;
    }
    true
}

fn collatz_steps(mut n: u64) -> u64 {
    let mut s = 0;
    while n != 1 && s < 10_000 {
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        s += 1;
    }
    s
}

/// A compute job in progress. `step` does a bounded amount of work and returns
/// the result when finished.
pub struct Exec {
    kind: Kind,
    cur: u64,
    acc: u64,
    best: u64,
    a: Vec<u32>,
    b: Vec<u32>,
}

impl Exec {
    /// None for kinds that are not local compute (AI, reduce).
    pub fn new(kind: Kind) -> Option<Exec> {
        let mut e = Exec { kind: kind.clone(), cur: 0, acc: 0, best: 0, a: Vec::new(), b: Vec::new() };
        match kind {
            Kind::Prime { lo, .. } | Kind::Collatz { lo, .. } => e.cur = lo,
            Kind::Hash { seed, .. } => e.acc = seed,
            Kind::Matmul { n, seed } => {
                let n = n as usize;
                let mut x = seed;
                for _ in 0..n * n { x = mix(x); e.a.push(x as u32); }
                for _ in 0..n * n { x = mix(x); e.b.push(x as u32); }
            }
            _ => return None,
        }
        Some(e)
    }

    /// Do about `budget` units of work. Returns Some(result) when done.
    pub fn step(&mut self, budget: u64) -> Option<u64> {
        match self.kind {
            Kind::Prime { hi, .. } => {
                let mut spent = 0;
                while self.cur < hi && spent < budget {
                    if is_prime(self.cur) { self.acc += 1; }
                    // cost of a trial division test grows with sqrt(n)
                    spent += 1 + (self.cur >> 16).min(64);
                    self.cur += 1;
                }
                if self.cur >= hi { Some(self.acc) } else { None }
            }
            Kind::Collatz { hi, .. } => {
                let mut spent = 0;
                while self.cur < hi && spent < budget {
                    let s = collatz_steps(self.cur);
                    if s > self.acc || self.best == 0 { self.acc = s; self.best = self.cur * 1000 + s.min(999); }
                    spent += 1 + s / 8;
                    self.cur += 1;
                }
                if self.cur >= hi { Some(self.best) } else { None }
            }
            Kind::Hash { n, .. } => {
                let end = (self.cur + budget * 4).min(n);
                while self.cur < end {
                    self.acc = mix(self.acc ^ self.cur);
                    self.cur += 1;
                }
                if self.cur >= n { Some(self.acc) } else { None }
            }
            Kind::Matmul { n, .. } => {
                let n = n as usize;
                let rows = ((budget as usize) / (n * n)).max(1);
                let mut r = self.cur as usize;
                let end = (r + rows).min(n);
                while r < end {
                    for c in 0..n {
                        let mut s: u32 = 0;
                        for k in 0..n {
                            s = s.wrapping_add(self.a[r * n + k].wrapping_mul(self.b[k * n + c]));
                        }
                        self.acc = mix(self.acc ^ s as u64);
                    }
                    r += 1;
                }
                self.cur = r as u64;
                if r >= n { Some(self.acc) } else { None }
            }
            _ => Some(0),
        }
    }
}

/// Run a job to completion (used by verification and tests).
pub fn run_to_end(kind: &Kind) -> Option<u64> {
    let mut e = Exec::new(kind.clone())?;
    loop {
        if let Some(r) = e.step(1 << 20) { return Some(r); }
    }
}

// ── Scheduling ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Static: job i goes to worker i mod N, decided up front, ignoring load.
    RoundRobin,
    /// Dynamic: the worker with the fewest jobs in flight (join the shortest queue).
    Least,
    /// Dynamic and speed-aware: the worker expected to finish a new job first,
    /// from its measured time per job.
    Adaptive,
}

impl Policy {
    pub fn name(&self) -> &'static str {
        match self { Policy::RoundRobin => "rr", Policy::Least => "least", Policy::Adaptive => "adaptive" }
    }
    pub fn parse(s: &str) -> Option<Policy> {
        match s { "rr" => Some(Policy::RoundRobin), "least" => Some(Policy::Least),
                  "adaptive" | "adapt" => Some(Policy::Adaptive), _ => None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkerView {
    pub name: String,
    pub in_flight: u32,
    pub alive: bool,
    pub quarantined: bool,
    /// Exponential moving average of milliseconds per job (0 = unknown).
    pub ema_ms: u32,
    /// Jobs the worker can run at once (its compute cores; 1 for a single-core VM).
    pub slots: u32,
}

impl WorkerView {
    pub fn usable(&self) -> bool { self.alive && !self.quarantined }
}

/// Jobs a dynamic policy keeps in flight per worker slot (core): two hides the
/// mesh latency without building queues that cannot be rebalanced.
pub const DEPTH: u32 = 2;

/// Pick a worker for the next job, or None to wait. `seq` is the job's sequence
/// number (used by round robin, which ignores load and never waits).
pub fn pick(policy: Policy, workers: &[WorkerView], seq: u64) -> Option<usize> {
    let usable: Vec<usize> = (0..workers.len()).filter(|&i| workers[i].usable()).collect();
    if usable.is_empty() { return None; }
    match policy {
        Policy::RoundRobin => Some(usable[(seq % usable.len() as u64) as usize]),
        // fewest jobs per core; a worker with 4 cores holds up to 4x as many
        Policy::Least => usable.iter().copied()
            .filter(|&i| workers[i].in_flight < DEPTH * workers[i].slots.max(1))
            .min_by_key(|&i| (workers[i].in_flight as u64 * 1000 / workers[i].slots.max(1) as u64, i)),
        Policy::Adaptive => {
            let known: Vec<u32> = usable.iter().map(|&i| workers[i].ema_ms).filter(|&m| m > 0).collect();
            let default = if known.is_empty() { 1 } else { known.iter().sum::<u32>() / known.len() as u32 };
            usable.iter().copied()
                .filter(|&i| workers[i].in_flight < (DEPTH + 1) * workers[i].slots.max(1))
                .min_by_key(|&i| {
                    let ms = if workers[i].ema_ms == 0 { default } else { workers[i].ema_ms } as u64;
                    // a new job waits for in_flight/slots jobs ahead of it on each core
                    let rounds = workers[i].in_flight as u64 / workers[i].slots.max(1) as u64 + 1;
                    (rounds * ms.max(1), i)
                })
        }
    }
}

pub fn ema(prev: u32, sample: u32) -> u32 {
    if prev == 0 { sample } else { ((prev as u64 * 3 + sample as u64) / 4) as u32 }
}

/// Should job `seq` be re-run on the coordinator to check the worker? Spread
/// evenly: `percent` out of every 100 jobs.
pub fn sampled(seq: u64, percent: u32) -> bool {
    percent > 0 && (mix(seq ^ 0x5EED) % 100) < percent as u64
}

/// Split [lo, hi) into `parts` equal ranges.
pub fn split_range(lo: u64, hi: u64, parts: u64) -> Vec<(u64, u64)> {
    let parts = parts.max(1);
    let w = (hi - lo) / parts;
    (0..parts).map(|i| {
        let a = lo + i * w;
        let b = if i + 1 == parts { hi } else { a + w };
        (a, b)
    }).collect()
}
