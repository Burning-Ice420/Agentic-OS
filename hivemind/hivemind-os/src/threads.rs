//! Life threads, enforced by the kernel.
//!
//!  * Routing: every task and note is put in a thread (work, home, health...).
//!    The user's own rules decide first (`#work` in the text, then keywords);
//!    only when they do not, the model is asked, and the kernel validates its
//!    answer against the real thread names. Every decision is a provenance
//!    event, so `why tasks t3_thread` says why an item went where it did.
//!  * Focus: `focus work 90` holds reminders from every other thread and
//!    delivers them as one digest when focus ends.
//!  * Private threads: the kernel leaves their items out of every model
//!    context (apps and agents alike), whatever the caller asked for.
//!
//! State lives in the `threads` node (so it is saved with everything else):
//! one key per thread, plus `_focus` and `_held`. Values written there by a
//! mesh peer are ignored: a peer must not be able to make a thread public.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use crate::apps_core::{fmt_hhmm, Task};
use crate::hive::{self, blob::BlobValue, Hive};
use crate::prov::{self, Actor, Kind};
use crate::threads_core::{effective_thread, held_decode, held_encode, is_item_key, may_interrupt,
    parse_model_thread, private_keys, route_by_rules, valid_name, Focus, RuleRoute, Thread, INBOX};

pub const NODE: &str = "threads";

// ── Reading state (callers already hold the hive) ─────────────────────────────

/// Thread definitions, ignoring any written by an untrusted peer.
pub fn threads_in(h: &Hive) -> Vec<Thread> {
    let id = match h.find_by_name(NODE) { Some(i) => i, None => return Vec::new() };
    h.memories[&id].blobs.iter()
        .filter(|(k, b)| !k.starts_with('_') && !b.tainted)
        .filter_map(|(k, b)| Thread::decode(k, &b.value.display()))
        .collect()
}

fn focus_in(h: &Hive) -> Option<Focus> {
    let id = h.find_by_name(NODE)?;
    let b = h.memories[&id].blobs.get("_focus")?;
    if b.tainted { return None; }
    Focus::decode(&b.value.display())
}

fn held_in(h: &Hive) -> Vec<String> {
    h.find_by_name(NODE)
        .and_then(|id| h.memories[&id].blobs.get("_held"))
        .map(|b| held_decode(&b.value.display()))
        .unwrap_or_default()
}

/// The text routing looks at: a task's text, a note's whole value.
fn item_text(value: &str) -> String {
    match Task::decode(value) { Some(t) => t.text, None => value.to_string() }
}

/// The thread an item is in, as the kernel decides it (see `effective_thread`).
pub fn thread_of_in(h: &Hive, mem_id: u64, item: &str) -> String {
    let m = match h.memories.get(&mem_id) { Some(m) => m, None => return INBOX.to_string() };
    let b = match m.blobs.get(item) { Some(b) => b, None => return INBOX.to_string() };
    let mut lk = item.to_string();
    lk.push_str("_thread");
    let label = m.blobs.get(&lk).map(|l| (l.value.display(), l.tainted));
    effective_thread(&item_text(&b.value.display()), label.as_ref().map(|(l, t)| (l.as_str(), *t)),
        b.tainted, &threads_in(h))
}

/// Keys of a node that must never reach the model (items in private threads
/// and their derived keys). Called by `Hive::model_context`.
pub fn private_keys_in(h: &Hive, mem_id: u64) -> Vec<String> {
    let threads = threads_in(h);
    if !threads.iter().any(|t| t.private) { return Vec::new(); }
    let m = match h.memories.get(&mem_id) { Some(m) => m, None => return Vec::new() };
    let items: Vec<(String, String, bool, Option<(String, bool)>)> = m.blobs.iter()
        .filter(|(k, _)| is_item_key(k))
        .map(|(k, b)| {
            let mut lk = k.clone();
            lk.push_str("_thread");
            let label = m.blobs.get(&lk).map(|l| (l.value.display(), l.tainted));
            (k.clone(), item_text(&b.value.display()), b.tainted, label)
        }).collect();
    private_keys(&items, &threads)
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn list() -> Vec<Thread> {
    hive::with_hive(|h| threads_in(h))
}

pub fn thread_of(node: &str, item: &str) -> String {
    hive::with_hive(|h| match h.find_by_name(node) {
        Some(id) => thread_of_in(h, id, item),
        None => INBOX.to_string(),
    })
}

/// Create or replace a thread. Keeps the private flag unless `private` is given.
pub fn define(actor: Actor, name: &str, keywords: &[&str], private: Option<bool>) -> Result<(), &'static str> {
    if !valid_name(name) {
        return Err("thread names: up to 12 of a-z 0-9 -, and not 'inbox'");
    }
    hive::with_hive(|h| {
        let id = h.node_named(actor, NODE);
        let old = threads_in(h).into_iter().find(|t| t.name == name);
        let mut t = old.unwrap_or(Thread { name: name.to_string(), keywords: Vec::new(), private: false });
        if !keywords.is_empty() {
            t.keywords = keywords.iter().filter_map(|k| crate::threads_core::norm_word(k))
                .map(|k| k.trim_start_matches('#').to_string()).filter(|k| !k.is_empty()).collect();
        }
        if let Some(p) = private { t.private = p; }
        let _ = h.write_blob_as(actor, id, name, BlobValue::Text(t.encode()), None);
    });
    crate::disk::persist::mark_dirty();
    Ok(())
}

pub fn remove(actor: Actor, name: &str) -> bool {
    let ok = hive::with_hive(|h| {
        let id = match h.find_by_name(NODE) { Some(i) => i, None => return false };
        h.remove_blob(actor, id, name, None).is_some()
    });
    if ok { crate::disk::persist::mark_dirty(); }
    ok
}

/// Record a routing decision and write the item's `_thread` label.
/// The label is as trusted as the item it describes.
fn record_route(h: &mut Hive, actor: Actor, mem_id: u64, item: &str, thread: &str,
                reason: &str, detail: &str, parent: Option<u64>) -> u64 {
    let name = h.memory_name(mem_id).unwrap_or("?").to_string();
    let (item_ev, tainted) = h.memories.get(&mem_id).and_then(|m| m.blobs.get(item))
        .map(|b| (b.last_event, b.tainted)).unwrap_or((0, false));
    let mut a = String::from("route=1;thread=");
    a.push_str(thread);
    let _ = write!(a, ";reason={}", reason);
    if !detail.is_empty() { let _ = write!(a, ";why={}", detail); }
    if tainted { a.push_str(";taint=1"); }
    let par = parent.or(if item_ev == 0 { None } else { Some(item_ev) });
    let ev = prov::emit(Kind::Marker, actor, mem_id, &name, item, par, a);
    let mut lk = item.to_string();
    lk.push_str("_thread");
    let _ = h.write_blob_tx(actor, mem_id, &lk, BlobValue::Text(thread.to_string()), Some(ev), tainted);
    ev
}

/// What routing did with a new item.
pub enum Routed {
    Rule { thread: String, how: String },
    /// No rule matched; the model was asked (the item sits in the inbox until it answers).
    AskedModel(crate::apps::Offload),
    Inbox(String),
}

/// Route a freshly added item. Rules first; the model only when rules do not
/// decide and the user allows it (`thread model on`, the default).
pub fn route_new(actor: Actor, node: &str, item: &str) -> Routed {
    let (mem_id, rule, threads, ask_model) = hive::with_hive(|h| {
        let id = match h.find_by_name(node) { Some(i) => i, None => return (0, RuleRoute::NoMatch, Vec::new(), false) };
        let text = h.memories[&id].blobs.get(item).map(|b| item_text(&b.value.display())).unwrap_or_default();
        let threads = threads_in(h);
        let ask = model_routing_in(h);
        (id, route_by_rules(&text, &threads), threads, ask)
    });
    if mem_id == 0 { return Routed::Inbox("no such node".to_string()); }
    let routed = match rule {
        RuleRoute::Tagged(t) => {
            hive::with_hive(|h| record_route(h, Actor::Kernel, mem_id, item, &t, "tag", &t, None));
            Routed::Rule { thread: t.clone(), how: alloc::format!("#{}", t) }
        }
        RuleRoute::Keyword { thread, word } => {
            hive::with_hive(|h| record_route(h, Actor::Kernel, mem_id, item, &thread, "keyword", &word, None));
            Routed::Rule { thread, how: alloc::format!("keyword '{}'", word) }
        }
        other => {
            let why = match &other { RuleRoute::Ambiguous(v) => alloc::format!("tie:{}", v.join("+")), _ => "no_rule".to_string() };
            hive::with_hive(|h| record_route(h, Actor::Kernel, mem_id, item, INBOX, "default", &why, None));
            if threads.is_empty() || !ask_model || !crate::llm::present() {
                Routed::Inbox(why)
            } else {
                let names: Vec<&str> = threads.iter().map(|t| t.name.as_str()).collect();
                let mut prompt = alloc::format!("@key:{}_thread Which one of these areas does this item belong to: ", item);
                prompt.push_str(&names.join(", "));
                prompt.push_str(". Answer with exactly one of those words.");
                let only = [item];
                Routed::AskedModel(crate::apps::offload(actor, mem_id, &prompt, Some(&only)))
            }
        }
    };
    crate::disk::persist::mark_dirty();
    routed
}

/// The model's routing answer has just been written to `<item>_thread`.
/// Validate it: only a real thread name is accepted, otherwise the inbox.
pub fn on_model_label(node: &str, label_key: &str, answer: &str) {
    let item = match label_key.strip_suffix("_thread") { Some(i) => i, None => return };
    let res = hive::with_hive(|h| {
        let id = h.find_by_name(node)?;
        let model_ev = h.memories[&id].blobs.get(label_key).map(|b| b.last_event)?;
        let threads = threads_in(h);
        let (t, reason) = match parse_model_thread(answer, &threads) {
            Some(t) => (t, "model"),
            None => (INBOX.to_string(), "model_rejected"),
        };
        record_route(h, Actor::Kernel, id, item, &t, reason, "", Some(model_ev));
        Some((t, reason))
    });
    if let Some((t, reason)) = res {
        crate::disk::persist::mark_dirty();
        if reason == "model" {
            crate::apps::notify(alloc::format!("{} filed under '{}' (model's choice, checked by the kernel)", item, t));
        } else {
            crate::apps::notify(alloc::format!("{}: model answer '{}' is not a thread; kept in inbox", item, answer));
        }
    }
}

/// Move an item by hand.
pub fn reroute(actor: Actor, node: &str, item: &str, thread: &str) -> Result<(), &'static str> {
    hive::with_hive(|h| {
        let id = h.find_by_name(node).ok_or("no such node")?;
        if !h.memories[&id].blobs.contains_key(item) { return Err("no such item"); }
        if thread != INBOX && !threads_in(h).iter().any(|t| t.name == thread) { return Err("no such thread"); }
        record_route(h, actor, id, item, thread, "manual", "", None);
        Ok(())
    })?;
    crate::disk::persist::mark_dirty();
    Ok(())
}

/// Route anything that has no label yet (items that arrived over the mesh, or
/// were made before threads existed). Rules only; nothing goes to the model.
pub fn route_unlabelled() {
    let n = hive::with_hive(|h| {
        let threads = threads_in(h);
        let mut todo: Vec<(u64, String, String)> = Vec::new();
        for node in [crate::apps::TASKS, crate::apps::NOTES] {
            if let Some(id) = h.find_by_name(node) {
                let m = &h.memories[&id];
                for (k, b) in &m.blobs {
                    let mut lk = k.clone();
                    lk.push_str("_thread");
                    if is_item_key(k) && !m.blobs.contains_key(&lk) {
                        todo.push((id, k.clone(), item_text(&b.value.display())));
                    }
                }
            }
        }
        for (id, k, text) in &todo {
            let (t, reason, why) = match route_by_rules(text, &threads) {
                RuleRoute::Tagged(t) => (t.clone(), "tag", t),
                RuleRoute::Keyword { thread, word } => (thread, "keyword", word),
                _ => (INBOX.to_string(), "default", "no_rule".to_string()),
            };
            record_route(h, Actor::Kernel, *id, k, &t, reason, &why, None);
        }
        todo.len()
    });
    if n > 0 { crate::disk::persist::mark_dirty(); }
}

// ── Model routing switch ──────────────────────────────────────────────────────

fn model_routing_in(h: &Hive) -> bool {
    h.find_by_name(NODE)
        .and_then(|id| h.memories[&id].blobs.get("_model"))
        .filter(|b| !b.tainted)
        .map_or(true, |b| b.value.display() != "off")
}

pub fn model_routing() -> bool {
    hive::with_hive(|h| model_routing_in(h))
}

pub fn set_model_routing(actor: Actor, on: bool) {
    hive::with_hive(|h| {
        let id = h.node_named(actor, NODE);
        let _ = h.write_blob_as(actor, id, "_model", BlobValue::Text(if on { "on" } else { "off" }.to_string()), None);
    });
    crate::disk::persist::mark_dirty();
}

// ── Focus ─────────────────────────────────────────────────────────────────────

pub fn focus() -> Option<Focus> {
    hive::with_hive(|h| focus_in(h))
}

pub fn held() -> Vec<String> {
    hive::with_hive(|h| held_in(h))
}

pub fn focus_on(actor: Actor, thread: &str, minutes: i64) -> Result<Focus, &'static str> {
    let now = crate::apps::now_abs();
    let f = Focus { thread: thread.to_string(), until: if minutes > 0 { now + minutes } else { 0 } };
    hive::with_hive(|h| {
        if thread != INBOX && !threads_in(h).iter().any(|t| t.name == thread) { return Err("no such thread"); }
        let id = h.node_named(actor, NODE);
        let _ = h.write_blob_as(actor, id, "_focus", BlobValue::Text(f.encode()), None);
        Ok(())
    })?;
    crate::disk::persist::mark_dirty();
    Ok(f)
}

/// End focus and return the digest of held reminders (text, thread).
pub fn focus_off(actor: Actor) -> Vec<(String, String)> {
    let digest = hive::with_hive(|h| {
        let id = match h.find_by_name(NODE) { Some(i) => i, None => return Vec::new() };
        let keys = held_in(h);
        let tasks = h.find_by_name(crate::apps::TASKS);
        let mut out = Vec::new();
        for k in &keys {
            if let Some(tid) = tasks {
                if let Some(b) = h.memories[&tid].blobs.get(k) {
                    let text = item_text(&b.value.display());
                    out.push((alloc::format!("{} {}", k, text), thread_of_in(h, tid, k)));
                }
            }
        }
        let mut a = String::new();
        let _ = write!(a, "focus_end=1;held={}", keys.len());
        let ev = prov::emit(Kind::Marker, actor, id, NODE, "_focus", None, a);
        let _ = h.remove_blob(actor, id, "_focus", Some(ev));
        let _ = h.remove_blob(actor, id, "_held", Some(ev));
        out
    });
    crate::disk::persist::mark_dirty();
    digest
}

/// Called by the reminder tick for each reminder that just fired. Returns true
/// if it may interrupt now; otherwise it is held (and recorded).
pub fn gate_reminder(h: &mut Hive, tasks_id: u64, key: &str, ev: u64) -> bool {
    let now = crate::apps::now_abs();
    let f = focus_in(h);
    let thread = thread_of_in(h, tasks_id, key);
    if may_interrupt(f.as_ref(), &thread, now) {
        return true;
    }
    let f = f.unwrap();
    let id = h.node_named(Actor::Kernel, NODE);
    let mut a = alloc::format!("held=1;thread={};focus={}", thread, f.thread);
    if h.blob_tainted(tasks_id, key) { a.push_str(";taint=1"); }
    let hev = prov::emit(Kind::Marker, Actor::Kernel, tasks_id, crate::apps::TASKS, key, Some(ev), a);
    let mut keys = held_in(h);
    if !keys.iter().any(|k| k == key) { keys.push(key.to_string()); }
    let _ = h.write_blob_as(Actor::Kernel, id, "_held", BlobValue::Text(held_encode(&keys)), Some(hev));
    false
}

/// End focus when its time is up; deliver the digest. Runs from the reminder tick.
pub fn focus_tick() {
    let now = crate::apps::now_abs();
    let f = match focus() { Some(f) => f, None => return };
    if !f.expired(now) { return; }
    let digest = focus_off(Actor::Kernel);
    notify_digest(&f, &digest);
}

pub fn notify_digest(f: &Focus, digest: &[(String, String)]) {
    if digest.is_empty() {
        crate::apps::notify(alloc::format!("Focus on '{}' ended. Nothing was held.", f.thread));
        return;
    }
    let mut m = alloc::format!("Focus on '{}' ended. {} held: ", f.thread, digest.len());
    for (i, (t, th)) in digest.iter().enumerate() {
        if i > 0 { m.push_str("; "); }
        let _ = write!(m, "{} ({})", t, th);
    }
    crate::apps::notify(m);
}

pub fn focus_label(f: &Focus) -> String {
    if f.until == 0 { alloc::format!("focus: {}", f.thread) }
    else { alloc::format!("focus: {} until {}", f.thread, fmt_hhmm(f.until)) }
}
