//! Everyday apps built on the memory graph, plus the in-OS views of provenance.
//!
//!  * Tasks:   to-dos in the `tasks` node; the kernel fires a reminder when one
//!             is due (works in the shell and on the desktop).
//!  * Notes:   notes in the `notes` node; the local model tags each note, and
//!             `ask` answers a question from the notes only.
//!  * Why:     walks the kernel's recent events to explain a value.
//!  * Taint:   every model call goes through `offload`, which applies the taint
//!             policy (off / block / redact) before anything leaves the VM.
//!
//! Lock order: HIVE -> PROV, never the reverse. NOTIFY is a leaf lock.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;
use spin::Mutex;

use crate::apps_core::{abs_minutes, clean_text, fmt_hhmm, next_key, parse_due, Context, Status, Task};
use crate::hive::{self, blob::BlobValue, WriteResult};
use crate::prov::{self, Actor, Kind};
use crate::prov_core::{attr, node_inputs, why_chain, Event, Link};

pub const TASKS: &str = "tasks";
pub const NOTES: &str = "notes";

// ── Notifications ─────────────────────────────────────────────────────────────

static NOTIFY: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn notify(msg: String) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut q = NOTIFY.lock();
        if q.len() >= 16 {
            q.pop_front();
        }
        q.push_back(msg);
    });
}

pub fn take_notification() -> Option<String> {
    x86_64::instructions::interrupts::without_interrupts(|| NOTIFY.lock().pop_front())
}

// ── Clock ─────────────────────────────────────────────────────────────────────

pub fn now_abs() -> i64 {
    let d = crate::rtc::read();
    abs_minutes(d.year as i64, d.month as i64, d.day as i64, d.hour as i64, d.minute as i64)
}

// ── Model calls under the taint policy ────────────────────────────────────────

pub enum Offload {
    /// `private` = items left out because they are in a private thread.
    Sent { event: u64, tainted: bool, redacted: usize, included: usize, private: usize },
    /// Refused because the context held tainted content (policy = block).
    Blocked { event: u64, keys: usize },
    /// Nothing sent: every item asked for is in a private thread.
    Private { event: u64 },
    NoDevice,
    NotFound,
}

/// Send part of a node to the model. `only` limits the keys sent; `prompt` may
/// start with `@key:<name> ` to make the bridge write the answer to that key.
pub fn offload(actor: Actor, mem_id: u64, prompt: &str, only: Option<&[&str]>) -> Offload {
    let prepared = hive::with_hive(|h| {
        let name = h.memory_name(mem_id)?.to_string();
        let (mctx, withheld) = h.model_context(mem_id, only, None, 700);
        match mctx {
            Context::Blocked { tainted_keys } => {
                let mut a = String::new();
                let _ = write!(a, "op=i;reason=tainted_context;tainted_keys={}", tainted_keys.len());
                let ev = prov::emit(Kind::CapDeny, actor, mem_id, &name, "tool:ai", None, a);
                let mut t = String::new();
                let mut who = String::new();
                actor.tag(&mut who);
                let _ = write!(t, "event {}: {} model call on '{}' BLOCKED by taint policy: context held {} untrusted key(s): {}",
                    ev, who, name, tainted_keys.len(), tainted_keys.join(" "));
                prov::record_decision(ev, t);
                Some(Err((ev, tainted_keys.len())))
            }
            Context::Send { included, .. } if included.is_empty() && withheld > 0 => {
                // Everything asked for is in a private thread: nothing leaves the VM.
                let a = alloc::format!("op=i;reason=private_thread;private_withheld={}", withheld);
                let ev = prov::emit(Kind::CapDeny, actor, mem_id, &name, "tool:ai", None, a);
                Some(Err((ev, 0)))
            }
            Context::Send { ctx, tainted, redacted, included } => {
                let mut ra = String::from("purpose=offload_context;sent=");
                ra.push_str(&included.join("+"));
                if tainted { ra.push_str(";taint=1"); }
                if redacted > 0 { let _ = write!(ra, ";redacted={}", redacted); }
                if withheld > 0 { let _ = write!(ra, ";private_withheld={}", withheld); }
                let rd = h.note_read(actor, mem_id, "*", None, &ra);
                let mut oa = String::new();
                let _ = write!(oa, "tool=ai;prompt_len={};ctx_len={};keys={}", prompt.len(), ctx.len(), included.len());
                if tainted { oa.push_str(";taint=1"); }
                if !crate::llm::present() { oa.push_str(";device=absent"); }
                let ev = prov::emit(Kind::Offload, actor, mem_id, &name, "*", Some(rd), oa);
                Some(Ok((name, ctx, ev, tainted, redacted, included.len(), withheld)))
            }
        }
    });
    match prepared {
        None => Offload::NotFound,
        Some(Err((event, 0))) => Offload::Private { event },
        Some(Err((event, keys))) => Offload::Blocked { event, keys },
        Some(Ok((name, ctx, event, tainted, redacted, included, private))) => {
            let agent = match actor { Actor::Agent(id) => Some(id), _ => None };
            if crate::llm::request_tracked(&name, prompt, &ctx, event, agent, tainted) {
                Offload::Sent { event, tainted, redacted, included, private }
            } else {
                Offload::NoDevice
            }
        }
    }
}

/// Called by the model driver after an answer is applied.
pub fn on_model_answer(memory: &str, key: &str, value: &str, tainted: bool) {
    if memory == "cluster_ai" {
        crate::cluster::on_ai_answer(key, value);
        return;
    }
    if (memory == TASKS || memory == NOTES) && key.ends_with("_thread") {
        crate::threads::on_model_label(memory, key, value);
        return;
    }
    if memory == NOTES && key == "answer" {
        let mut m = String::from("Answer: ");
        m.push_str(value);
        if tainted { m.push_str("  [UNTRUSTED SOURCE]"); }
        notify(m);
    }
}

// ── Tasks and reminders ───────────────────────────────────────────────────────

/// Add a task. `spec` is `+N`, `+Nh` or `HH:MM`. Returns (key, due, routing).
pub fn task_add(actor: Actor, spec: &str, text: &str) -> Result<(String, i64, crate::threads::Routed), &'static str> {
    let due = parse_due(spec, now_abs()).ok_or("due must be +MIN, +Nh or HH:MM")?;
    if text.trim().is_empty() {
        return Err("empty task");
    }
    let t = Task { due, status: Status::Open, text: clean_text(text.trim()) };
    let key = hive::with_hive(|h| {
        let id = h.node_named(actor, TASKS);
        let key = next_key(h.memories[&id].blobs.keys().map(|k| k.as_str()), "t");
        let _ = h.write_blob_as(actor, id, &key, BlobValue::Text(t.encode()), None);
        key
    });
    crate::disk::persist::mark_dirty();
    let r = crate::threads::route_new(actor, TASKS, &key);
    Ok((key, due, r))
}

/// (key, task, tainted), ordered by due time.
pub fn task_list() -> Vec<(String, Task, bool)> {
    let mut out: Vec<(String, Task, bool)> = hive::with_hive(|h| {
        let id = match h.find_by_name(TASKS) { Some(i) => i, None => return Vec::new() };
        h.memories[&id].blobs.iter().filter_map(|(k, b)| {
            Task::decode(&b.value.display()).map(|t| (k.clone(), t, b.tainted))
        }).collect()
    });
    out.sort_by_key(|(_, t, _)| (t.status == Status::Done, t.due));
    out
}

fn task_update(actor: Actor, key: &str, f: impl FnOnce(&mut Task)) -> bool {
    let ok = hive::with_hive(|h| {
        let id = match h.find_by_name(TASKS) { Some(i) => i, None => return false };
        let (mut t, tainted, last) = match h.memories[&id].blobs.get(key) {
            Some(b) => match Task::decode(&b.value.display()) {
                Some(t) => (t, b.tainted, b.last_event),
                None => return false,
            },
            None => return false,
        };
        f(&mut t);
        let parent = if last == 0 { None } else { Some(last) };
        matches!(h.write_blob_tx(actor, id, key, BlobValue::Text(t.encode()), parent, tainted), WriteResult::Written(_))
    });
    if ok { crate::disk::persist::mark_dirty(); }
    ok
}

pub fn task_done(actor: Actor, key: &str) -> bool {
    task_update(actor, key, |t| t.status = Status::Done)
}

pub fn task_snooze(actor: Actor, key: &str, minutes: i64) -> bool {
    let now = now_abs();
    task_update(actor, key, |t| { t.due = now + minutes; t.status = Status::Open; })
}

pub fn task_del(actor: Actor, key: &str) -> bool {
    let ok = hive::with_hive(|h| {
        let id = match h.find_by_name(TASKS) { Some(i) => i, None => return false };
        h.remove_blob(actor, id, key, None).is_some()
    });
    if ok { crate::disk::persist::mark_dirty(); }
    ok
}

/// Fire due reminders. Runs about once a second from `background`.
pub fn reminder_tick() {
    let now = now_abs();
    let fired: Vec<(String, bool)> = hive::with_hive(|h| {
        let id = match h.find_by_name(TASKS) { Some(i) => i, None => return Vec::new() };
        let due: Vec<(String, Task, bool, u64)> = h.memories[&id].blobs.iter().filter_map(|(k, b)| {
            let t = Task::decode(&b.value.display())?;
            if t.is_due(now) { Some((k.clone(), t, b.tainted, b.last_event)) } else { None }
        }).collect();
        let mut out = Vec::new();
        for (k, mut t, tainted, last) in due {
            t.status = Status::Fired;
            let parent = if last == 0 { None } else { Some(last) };
            // The kernel acts on the task; a task that came from an untrusted
            // peer stays untrusted.
            let ev = match h.write_blob_tx(Actor::Kernel, id, &k, BlobValue::Text(t.encode()), parent, tainted) {
                WriteResult::Written(e) => e,
                _ => 0,
            };
            // Focus: reminders from other threads are held for the digest.
            if !crate::threads::gate_reminder(h, id, &k, ev) {
                continue;
            }
            let mut m = String::from("Reminder: ");
            m.push_str(&t.text);
            let _ = write!(m, "  (due {})", fmt_hhmm(t.due));
            out.push((m, tainted));
        }
        out
    });
    if !fired.is_empty() {
        crate::disk::persist::mark_dirty();
    }
    for (mut m, tainted) in fired {
        if tainted { m.push_str("  [UNTRUSTED SOURCE]"); }
        notify(m);
    }
}

// ── Notes ─────────────────────────────────────────────────────────────────────

fn is_note_key(k: &str) -> bool {
    k.len() > 1 && k.starts_with('n') && k[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Add a note, route it to a thread, and ask the model to tag it (refused by
/// the kernel if the note landed in a private thread).
pub fn note_add(actor: Actor, text: &str) -> (String, crate::threads::Routed, Offload) {
    let (key, id) = hive::with_hive(|h| {
        let id = h.node_named(actor, NOTES);
        let key = next_key(h.memories[&id].blobs.keys().map(|k| k.as_str()), "n");
        let _ = h.write_blob_as(actor, id, &key, BlobValue::Text(clean_text(text.trim())), None);
        (key, id)
    });
    crate::disk::persist::mark_dirty();
    let routed = crate::threads::route_new(actor, NOTES, &key);
    let mut prompt = String::from("@key:");
    let _ = write!(prompt, "{}_tags Give two or three one-word topic tags for this note, separated by spaces.", key);
    let only = [key.as_str()];
    let o = offload(actor, id, &prompt, Some(&only));
    (key, routed, o)
}

/// (key, text, tags, tainted), newest last.
pub fn note_list() -> Vec<(String, String, Option<String>, bool)> {
    let mut v: Vec<(u64, String, String, Option<String>, bool)> = hive::with_hive(|h| {
        let id = match h.find_by_name(NOTES) { Some(i) => i, None => return Vec::new() };
        let m = &h.memories[&id];
        m.blobs.iter().filter(|(k, _)| is_note_key(k)).map(|(k, b)| {
            let mut tk = k.clone();
            tk.push_str("_tags");
            let tags = m.blobs.get(&tk).map(|t| t.value.display());
            (k[1..].parse().unwrap_or(0), k.clone(), b.value.display(), tags, b.tainted)
        }).collect()
    });
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|(_, k, t, g, ta)| (k, t, g, ta)).collect()
}

/// The current answer, if any: (text, tainted).
pub fn last_answer() -> Option<(String, bool)> {
    hive::with_hive(|h| {
        let id = h.find_by_name(NOTES)?;
        h.memories[&id].blobs.get("answer").map(|b| (b.value.display(), b.tainted))
    })
}

/// Ask the model a question about the notes. Only note texts are sent (not tags,
/// not earlier answers), under the taint policy.
pub fn ask(actor: Actor, question: &str) -> Offload {
    let id = hive::with_hive(|h| {
        let id = h.node_named(actor, NOTES);
        let _ = h.write_blob_as(actor, id, "question", BlobValue::Text(clean_text(question)), None);
        id
    });
    let keys: Vec<String> = hive::with_hive(|h| {
        h.memories[&id].blobs.keys().filter(|k| is_note_key(k)).cloned().collect()
    });
    let only: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
    let mut prompt = String::from("@key:answer Using only these notes, answer briefly: ");
    prompt.push_str(&clean_text(question));
    offload(actor, id, &prompt, Some(&only))
}

// ── Why and activity (in-kernel provenance views) ─────────────────────────────

/// One display line plus how to colour it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone { #[default] Normal, Model, Deny, Taint, Dim }

fn tone_of(e: &Event) -> Tone {
    if e.kind == Kind::CapDeny { return Tone::Deny; }
    if attr(&e.attrs, "taint") == Some("1") { return Tone::Taint; }
    match e.kind {
        Kind::Offload => Tone::Model,
        Kind::Write if matches!(e.actor, Actor::Model(_)) => Tone::Model,
        Kind::CapAllow | Kind::Read | Kind::Marker => Tone::Dim,
        _ => Tone::Normal,
    }
}

/// Short one-line description of an event.
pub fn describe(e: &Event) -> String {
    let mut s = String::new();
    let mut who = String::new();
    e.actor.tag(&mut who);
    let _ = write!(s, "#{} {} {}", e.id, e.kind.name(), who);
    if e.mem_id != 0 || e.key != "-" {
        let _ = write!(s, " {}/{}", e.mem_name, e.key);
    }
    if attr(&e.attrs, "route") == Some("1") {
        let _ = write!(s, " -> {} ({}", attr(&e.attrs, "thread").unwrap_or("?"), attr(&e.attrs, "reason").unwrap_or("?"));
        if let Some(w) = attr(&e.attrs, "why") { let _ = write!(s, ": {}", w); }
        s.push(')');
    } else if attr(&e.attrs, "held") == Some("1") {
        let _ = write!(s, " HELD ({} reminder during focus on {})", attr(&e.attrs, "thread").unwrap_or("?"),
            attr(&e.attrs, "focus").unwrap_or("?"));
    } else if let Some(r) = attr(&e.attrs, "reason") { let _ = write!(s, " DENY:{}", r); }
    if let Some(p) = attr(&e.attrs, "private_withheld") { let _ = write!(s, " private_kept={}", p); }
    if attr(&e.attrs, "removed") == Some("1") { s.push_str(" (removed)"); }
    if let Some(r) = attr(&e.attrs, "rollback_of").filter(|r| *r != "0") { let _ = write!(s, " (undid #{})", r); }
    if let Some(r) = attr(&e.attrs, "redacted") { let _ = write!(s, " redacted={}", r); }
    if attr(&e.attrs, "taint") == Some("1") { s.push_str(" [T]"); }
    s
}

/// The last `n` events, oldest first.
pub fn activity(n: usize) -> Vec<(String, Tone)> {
    let ev = prov::recent();
    let start = ev.len().saturating_sub(n);
    ev[start..].iter().map(|e| (describe(e), tone_of(e))).collect()
}

/// Explain the current value of `node/key`: its cause chain, and for a model
/// answer, which inputs the model was shown.
pub fn why(node: &str, key: &str) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    let mem_id = match hive::with_hive(|h| h.find_by_name(node)) {
        Some(i) => i,
        None => { out.push((alloc::format!("no node '{}'", node), Tone::Deny)); return out; }
    };
    let (last, tainted) = hive::with_hive(|h| {
        h.memories[&mem_id].blobs.get(key).map(|b| (b.last_event, b.tainted)).unwrap_or((0, false))
    });
    let events = prov::recent();
    let start = if last != 0 && events.iter().any(|e| e.id == last) {
        last
    } else {
        match crate::prov_core::latest_write_before(&events, mem_id, key, u64::MAX) {
            Some(e) => e.id,
            None => {
                out.push((alloc::format!("{}/{}: no write in the last {} events", node, key,
                    crate::prov_core::RECENT_CAP), Tone::Dim));
                return out;
            }
        }
    };
    let mut head = alloc::format!("why {}/{} ", node, key);
    head.push_str(if tainted { "(UNTRUSTED: derived from a peer)" } else { "(trusted origin)" });
    out.push((head, if tainted { Tone::Taint } else { Tone::Normal }));
    for (i, (e, link)) in why_chain(&events, start, 16).iter().enumerate() {
        let mut l = String::new();
        for _ in 0..i.min(6) { l.push(' '); }
        if *link != Link::Start {
            l.push_str("<- ");
            l.push_str(link.name());
            l.push(' ');
        }
        l.push_str(&describe(e));
        out.push((l, tone_of(e)));
        if e.kind == Kind::Read && e.key == "*" {
            for w in node_inputs(&events, e) {
                let mut l = String::new();
                for _ in 0..(i.min(6) + 4) { l.push(' '); }
                l.push_str("shown to model: ");
                l.push_str(&describe(&w));
                out.push((l, tone_of(&w)));
            }
        }
    }
    let first_actor = why_chain(&events, start, 16).last().map(|(e, _)| e.actor);
    if let Some(a) = first_actor {
        let mut who = String::new();
        a.tag(&mut who);
        out.push((alloc::format!("origin: {}", who), Tone::Dim));
    }
    out
}

// ── Background work shared by the shell and the desktop ───────────────────────

/// Everything that must keep running whichever screen is in front.
pub fn background(last_tick: &mut u64) {
    crate::net::poll_and_apply();
    crate::llm::poll_and_apply();
    crate::prov::drain(32);
    crate::cluster::tick();
    let t = crate::interrupts::current_tick();
    if t != *last_tick {
        *last_tick = t;
        // PIT default ~18.2 Hz -> roughly once per second.
        if t % 18 == 0 {
            crate::agent::tick_all();
            crate::threads::route_unlabelled();
            reminder_tick();
            crate::threads::focus_tick();
        }
        crate::disk::persist::autosave_tick(t);
    }
}

/// Human-readable summary of an offload outcome.
pub fn offload_msg(o: &Offload) -> (String, Tone) {
    match o {
        Offload::Sent { event, tainted, redacted, included, private } => {
            let mut s = alloc::format!("sent to model (event #{}, {} item(s))", event, included);
            if *private > 0 { let _ = write!(s, ", {} private item(s) kept back", private); }
            if *redacted > 0 { let _ = write!(s, ", {} untrusted item(s) left out", redacted); }
            if *tainted { s.push_str(", WARNING: includes untrusted content"); }
            (s, if *tainted { Tone::Taint } else { Tone::Model })
        }
        Offload::Blocked { event, keys } => (alloc::format!(
            "BLOCKED by taint policy: {} untrusted item(s) in context (event #{})", keys, event), Tone::Deny),
        Offload::Private { event } => (alloc::format!(
            "kept private: item is in a private thread, nothing sent (event #{})", event), Tone::Dim),
        Offload::NoDevice => ("no model attached (start with -LLM); saved without AI".to_string(), Tone::Dim),
        Offload::NotFound => ("node not found".to_string(), Tone::Deny),
    }
}
