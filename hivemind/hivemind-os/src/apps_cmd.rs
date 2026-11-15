//! Shell commands for the everyday apps and taint tools.
//!
//!   task add <+MIN|+Nh|HH:MM> <text> | task [list] | task done|del <tN> | task snooze <tN> [min]
//!   note <text> | note list | ask <question>
//!   why <node> <key> | activity [n]
//!   taint [status] | taint policy off|block|redact | quarantine | rewind <node> <key> | history <node> <key>

use alloc::string::String;
use alloc::vec::Vec;

use crate::apps::{self, Tone};
use crate::apps_core::{fmt_hhmm, Status, TaintPolicy};
use crate::hive;
use crate::prov::Actor;
use crate::vga_buffer::{self, Color};
use crate::{print, println};

fn color(t: Tone) -> Color {
    match t {
        Tone::Normal => Color::White,
        Tone::Model => Color::Yellow,
        Tone::Deny => Color::LightRed,
        Tone::Taint => Color::Pink,
        Tone::Dim => Color::DarkGray,
    }
}

fn line(text: &str, t: Tone) {
    vga_buffer::set_color(color(t), Color::Black);
    println!("  {}", text);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

/// Print a pending notification above the prompt (called from the shell loop).
pub fn show_notification(msg: &str) {
    vga_buffer::set_color(Color::Black, Color::Yellow);
    print!(" (!) {} ", msg);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!();
}

pub fn cmd_task(args: &[&str]) {
    match args.first().copied() {
        None | Some("list") | Some("ls") => {
            let tasks = apps::task_list();
            if tasks.is_empty() {
                println!("  No tasks. Try: task add +2 stretch   or   task add 18:30 call home");
                return;
            }
            let now = apps::now_abs();
            for (k, t, tainted) in tasks {
                let when = if t.due - now > 0 && t.due - now < 1440 {
                    alloc::format!("{} (in {} min)", fmt_hhmm(t.due), t.due - now)
                } else {
                    fmt_hhmm(t.due)
                };
                let tone = if tainted { Tone::Taint } else {
                    match t.status { Status::Open => Tone::Normal, Status::Fired => Tone::Model, Status::Done => Tone::Dim }
                };
                let mark = match t.status { Status::Open => "[ ]", Status::Fired => "[!]", Status::Done => "[x]" };
                let th = crate::threads::thread_of(apps::TASKS, &k);
                line(&alloc::format!("{} {:<4} {:<20} {:<8} {}{}", mark, k, when, th, t.text,
                    if tainted { "  [untrusted]" } else { "" }), tone);
            }
        }
        Some("add") if args.len() >= 3 => {
            let text = args[2..].join(" ");
            match apps::task_add(Actor::Shell, args[1], &text) {
                Ok((k, due, r)) => {
                    line(&alloc::format!("Added {} at {}: {}", k, fmt_hhmm(due), text), Tone::Normal);
                    crate::threads_cmd::show_routed(&k, &r);
                }
                Err(e) => line(e, Tone::Deny),
            }
        }
        Some("done") if args.len() >= 2 => {
            if apps::task_done(Actor::Shell, args[1]) { println!("  {} done.", args[1]); } else { println!("  No task {}.", args[1]); }
        }
        Some("del") | Some("rm") if args.len() >= 2 => {
            if apps::task_del(Actor::Shell, args[1]) { println!("  {} deleted.", args[1]); } else { println!("  No task {}.", args[1]); }
        }
        Some("snooze") if args.len() >= 2 => {
            let m = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
            if apps::task_snooze(Actor::Shell, args[1], m) { println!("  {} snoozed {} min.", args[1], m); }
            else { println!("  No task {}.", args[1]); }
        }
        _ => println!("  Usage: task add <+MIN|+Nh|HH:MM> <text> | task list | task done|del|snooze <tN>"),
    }
}

pub fn cmd_note(args: &[&str]) {
    match args.first().copied() {
        None | Some("list") | Some("ls") => {
            let notes = apps::note_list();
            if notes.is_empty() {
                println!("  No notes. Try: note the internship interview is on friday");
                return;
            }
            for (k, text, tags, tainted) in notes {
                let t = if tainted { Tone::Taint } else { Tone::Normal };
                let th = crate::threads::thread_of(apps::NOTES, &k);
                line(&alloc::format!("{:<4} {:<8} {}{}", k, th, text, if tainted { "  [untrusted]" } else { "" }), t);
                if let Some(g) = tags {
                    line(&alloc::format!("     tags: {}", g), Tone::Dim);
                }
            }
            if let Some((a, tainted)) = apps::last_answer() {
                line(&alloc::format!("last answer: {}", a), if tainted { Tone::Taint } else { Tone::Model });
            }
        }
        Some(_) => {
            let text = args.join(" ");
            let (k, r, o) = apps::note_add(Actor::Shell, &text);
            println!("  Saved {}.", k);
            crate::threads_cmd::show_routed(&k, &r);
            let (m, t) = apps::offload_msg(&o);
            line(&alloc::format!("tagging: {}", m), t);
        }
    }
}

pub fn cmd_ask(args: &[&str]) {
    if args.is_empty() {
        println!("  Usage: ask <question about your notes>");
        return;
    }
    let q = args.join(" ");
    let o = apps::ask(Actor::Shell, &q);
    let (m, t) = apps::offload_msg(&o);
    line(&m, t);
    if let apps::Offload::Sent { .. } = o {
        println!("  The answer appears here when the model replies; then: why notes answer");
    }
}

pub fn cmd_why(args: &[&str]) {
    if args.len() < 2 {
        println!("  Usage: why <node> <key>     e.g.  why notes answer");
        return;
    }
    for (l, t) in apps::why(args[0], args[1]) {
        line(&l, t);
    }
}

pub fn cmd_activity(args: &[&str]) {
    let n = args.first().and_then(|s| s.parse().ok()).unwrap_or(15);
    for (l, t) in apps::activity(n) {
        line(&l, t);
    }
}

pub fn cmd_taint(args: &[&str]) {
    match args.first().copied() {
        Some("policy") => match args.get(1).and_then(|s| TaintPolicy::parse(s)) {
            Some(p) => {
                hive::with_hive(|h| h.taint_policy = p);
                crate::prov::emit_plain(crate::prov::Kind::CapChange, Actor::Shell, "taint_policy", None,
                    crate::prov::attrs(&[("policy", p.name())]));
                crate::disk::persist::mark_dirty();
                println!("  Taint policy: {}", p.name());
                match p {
                    TaintPolicy::Off => println!("  Untrusted content may reach the model; answers are marked."),
                    TaintPolicy::Block => println!("  Model calls whose context holds untrusted content are refused."),
                    TaintPolicy::Redact => println!("  Untrusted entries are left out of model context."),
                }
            }
            None => println!("  Usage: taint policy off|block|redact"),
        },
        _ => {
            let (pol, list) = hive::with_hive(|h| (h.taint_policy, h.tainted_blobs()));
            println!("  Taint policy: {}   tainted values: {}", pol.name(), list.len());
            for (_, node, key) in list.iter().take(20) {
                line(&alloc::format!("{}/{}", node, key), Tone::Taint);
            }
            if !list.is_empty() {
                println!("  'quarantine' rolls all of them back; 'why <node> <key>' explains one.");
            }
        }
    }
}

pub fn cmd_quarantine() {
    let (restored, removed, ev) = hive::with_hive(|h| h.quarantine(Actor::Shell));
    crate::disk::persist::mark_dirty();
    line(&alloc::format!("Quarantine (event #{}): {} value(s) rolled back to a clean version, {} removed.",
        ev, restored, removed), if restored + removed > 0 { Tone::Model } else { Tone::Dim });
}

fn node_id(name: &str) -> Option<u64> {
    hive::with_hive(|h| h.find_by_name(name).or_else(|| name.parse().ok().filter(|i| h.memories.contains_key(i))))
}

pub fn cmd_rewind(args: &[&str]) {
    if args.len() < 2 {
        println!("  Usage: rewind <node> <key>   (undo the latest write)");
        return;
    }
    let id = match node_id(args[0]) { Some(i) => i, None => { println!("  No node {}.", args[0]); return; } };
    match hive::with_hive(|h| h.rewind(Actor::Shell, id, args[1])) {
        Some(ev) => { crate::disk::persist::mark_dirty(); println!("  Rewound {}/{} (event #{}).", args[0], args[1], ev); }
        None => println!("  No blob {}/{}.", args[0], args[1]),
    }
}

pub fn cmd_history(args: &[&str]) {
    if args.len() < 2 {
        println!("  Usage: history <node> <key>");
        return;
    }
    let id = match node_id(args[0]) { Some(i) => i, None => { println!("  No node {}.", args[0]); return; } };
    let rows: Option<Vec<(String, u64, bool)>> = hive::with_hive(|h| {
        let b = h.memories.get(&id)?.blobs.get(args[1])?;
        let mut v: Vec<(String, u64, bool)> = b.history.iter().map(|x| (x.value.display(), x.event, x.tainted)).collect();
        v.push((b.value.display(), b.last_event, b.tainted));
        Some(v)
    });
    match rows {
        None => println!("  No blob {}/{}.", args[0], args[1]),
        Some(v) => {
            let n = v.len();
            for (i, (val, ev, t)) in v.into_iter().enumerate() {
                let cur = if i + 1 == n { " <- current" } else { "" };
                line(&alloc::format!("v{} #{} {}{}{}", i + 1, ev, val, if t { " [T]" } else { "" }, cur),
                    if t { Tone::Taint } else { Tone::Normal });
            }
        }
    }
}

pub fn help() {
    println!("  Everyday apps");
    println!("  task add +5 stretch | task add 18:30 call home | task | task done t1");
    println!("  note <text>        save a note (the model tags it)");
    println!("  note list          notes, tags, last answer      ask <question>");
    println!("  Provenance and taint");
    println!("  why <node> <key>   cause chain of a value       activity [n]");
    println!("  taint              untrusted values  taint policy off|block|redact");
    println!("  quarantine         roll back everything derived from untrusted input");
    println!("  rewind <node> <key>   history <node> <key>");
    println!("  Threads: help thread   (route tasks/notes into work, home, health...)");
}
