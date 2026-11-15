//! Pure logic for the everyday apps and for taint tracking. Only `core` +
//! `alloc`, so the host crate `hivemind-os-logic` unit-tests this exact file.
//!
//!  * time:    minutes since 2000-01-01 from an RTC date, due-time parsing
//!  * tasks:   the `due|status|text` blob encoding used by the `tasks` node
//!  * taint:   the policy applied when a node's content is sent to the model,
//!             and the bounded version history that makes rewind possible

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

// ── Time ──────────────────────────────────────────────────────────────────────

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Absolute minutes (since 1970) for a wall-clock reading.
pub fn abs_minutes(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> i64 {
    days_from_civil(y, mo, d) * 1440 + h * 60 + mi
}

/// Parse a due time: `+N` (minutes from now), `+Nh` (hours), or `HH:MM`
/// (today, or tomorrow if that time has already passed).
pub fn parse_due(spec: &str, now_abs: i64) -> Option<i64> {
    if let Some(rest) = spec.strip_prefix('+') {
        if let Some(h) = rest.strip_suffix('h') {
            let n: i64 = h.parse().ok()?;
            return if n > 0 && n <= 24 * 30 { Some(now_abs + n * 60) } else { None };
        }
        let n: i64 = rest.parse().ok()?;
        return if n > 0 && n <= 1440 * 30 { Some(now_abs + n) } else { None };
    }
    let mut it = spec.splitn(2, ':');
    let h: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    let today = now_abs - now_abs.rem_euclid(1440);
    let mut due = today + h * 60 + m;
    if due <= now_abs {
        due += 1440;
    }
    Some(due)
}

/// `HH:MM` of an absolute minute value.
pub fn fmt_hhmm(abs: i64) -> String {
    let m = abs.rem_euclid(1440);
    let mut s = String::new();
    let _ = write!(s, "{:02}:{:02}", m / 60, m % 60);
    s
}

// ── Tasks ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    /// The reminder has gone off.
    Fired,
    Done,
}

impl Status {
    pub fn name(&self) -> &'static str {
        match self { Status::Open => "open", Status::Fired => "fired", Status::Done => "done" }
    }
    pub fn parse(s: &str) -> Option<Status> {
        match s { "open" => Some(Status::Open), "fired" => Some(Status::Fired), "done" => Some(Status::Done), _ => None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub due: i64,
    pub status: Status,
    pub text: String,
}

impl Task {
    pub fn encode(&self) -> String {
        let mut s = String::new();
        let _ = write!(s, "{}|{}|{}", self.due, self.status.name(), clean_text(&self.text));
        s
    }
    pub fn decode(s: &str) -> Option<Task> {
        let mut it = s.splitn(3, '|');
        let due = it.next()?.parse().ok()?;
        let status = Status::parse(it.next()?)?;
        let text = it.next()?.to_string();
        Some(Task { due, status, text })
    }
    /// Should the reminder fire now?
    pub fn is_due(&self, now_abs: i64) -> bool {
        self.status == Status::Open && now_abs >= self.due
    }
}

/// Strip characters that break the blob, mesh and model-bridge line formats.
pub fn clean_text(s: &str) -> String {
    s.chars().map(|c| match c { '|' | '\n' | '\r' => ' ', c => c }).collect()
}

/// Next free numbered key with a prefix (`t1`, `t2`, ... or `n1`, ...).
pub fn next_key<'a, I: Iterator<Item = &'a str>>(keys: I, prefix: &str) -> String {
    let mut max = 0u64;
    for k in keys {
        if let Some(n) = k.strip_prefix(prefix).and_then(|r| r.parse::<u64>().ok()) {
            if n > max { max = n; }
        }
    }
    let mut s = String::from(prefix);
    let _ = write!(s, "{}", max + 1);
    s
}

// ── Taint ─────────────────────────────────────────────────────────────────────

/// What happens when content derived from an untrusted origin would be sent to
/// the model as context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaintPolicy {
    /// Send it anyway; the answer is marked tainted (detection only).
    Off,
    /// Refuse the model call.
    Block,
    /// Leave tainted entries out of the context and send the rest.
    Redact,
}

impl TaintPolicy {
    pub fn name(&self) -> &'static str {
        match self { TaintPolicy::Off => "off", TaintPolicy::Block => "block", TaintPolicy::Redact => "redact" }
    }
    pub fn parse(s: &str) -> Option<TaintPolicy> {
        match s { "off" => Some(TaintPolicy::Off), "block" => Some(TaintPolicy::Block),
                  "redact" => Some(TaintPolicy::Redact), _ => None }
    }
}

/// Result of building a model context under a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    /// `ctx` to send; `tainted` = untrusted content is included; `redacted` = entries left out.
    Send { ctx: String, tainted: bool, redacted: usize, included: Vec<String> },
    /// Refused under `Block`; carries the tainted keys.
    Blocked { tainted_keys: Vec<String> },
}

/// Build `key=value,key=value` context from `(key, value, tainted)` entries,
/// optionally restricted to `only` keys, capped at `max_len` characters.
pub fn build_context(entries: &[(String, String, bool)], only: Option<&[&str]>, policy: TaintPolicy,
                     max_len: usize) -> Context {
    let picked: Vec<&(String, String, bool)> = entries.iter()
        .filter(|(k, _, _)| only.map_or(true, |o| o.iter().any(|x| x == k)))
        .collect();
    let tainted_keys: Vec<String> = picked.iter().filter(|e| e.2).map(|e| e.0.clone()).collect();
    if policy == TaintPolicy::Block && !tainted_keys.is_empty() {
        return Context::Blocked { tainted_keys };
    }
    let mut ctx = String::new();
    let mut redacted = 0;
    let mut tainted = false;
    let mut included = Vec::new();
    for (k, v, t) in picked {
        if *t && policy == TaintPolicy::Redact {
            redacted += 1;
            continue;
        }
        let mut item = String::new();
        let _ = write!(item, "{}={}", k, clean_text(v));
        if ctx.len() + item.len() + 1 > max_len {
            break;
        }
        if !ctx.is_empty() { ctx.push(','); }
        ctx.push_str(&item);
        tainted |= *t;
        included.push(k.clone());
    }
    Context::Send { ctx, tainted, redacted, included }
}

/// Bounded per-blob version history: the values a blob held before its current
/// one, oldest first. Generic over the value type so the kernel stores its own.
#[derive(Clone, Debug)]
pub struct Version<V> {
    pub value: V,
    /// Provenance event of the write that produced this value (0 = unknown).
    pub event: u64,
    pub tainted: bool,
}

pub const HISTORY_CAP: usize = 8;

pub fn push_version<V>(hist: &mut Vec<Version<V>>, v: Version<V>) {
    if hist.len() >= HISTORY_CAP {
        hist.remove(0);
    }
    hist.push(v);
}

/// Index of the newest clean (untainted) version, if any.
pub fn last_clean<V>(hist: &[Version<V>]) -> Option<usize> {
    hist.iter().rposition(|v| !v.tainted)
}
