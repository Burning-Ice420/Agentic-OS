//! Shell commands for life threads.
//!
//!   thread                         list threads, counts, focus
//!   thread add <name> [kw ...]     create (or replace keywords of) a thread
//!   thread private|public <name>   private items never reach the model
//!   thread del <name>              remove a thread (its items fall back to rules / inbox)
//!   thread show <name>             tasks and notes in one thread
//!   thread model on|off            may the model route items no rule claims?
//!   route <tN|nN> <thread>         move an item by hand
//!   focus <thread> [min] | focus off | focus

use alloc::string::String;
use alloc::vec::Vec;

use crate::apps::{self, Tone};
use crate::apps_core::Status;
use crate::prov::Actor;
use crate::threads::{self, Routed};
use crate::threads_core::INBOX;
use crate::vga_buffer::{self, Color};
use crate::println;

fn line(text: &str, t: Tone) {
    let c = match t {
        Tone::Normal => Color::White,
        Tone::Model => Color::Yellow,
        Tone::Deny => Color::LightRed,
        Tone::Taint => Color::Pink,
        Tone::Dim => Color::DarkGray,
    };
    vga_buffer::set_color(c, Color::Black);
    println!("  {}", text);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

pub fn routed_short(r: &Routed) -> String {
    match r {
        Routed::Rule { thread, .. } => thread.clone(),
        Routed::AskedModel(apps::Offload::Sent { .. }) => String::from("inbox, asking model"),
        Routed::AskedModel(_) | Routed::Inbox(_) => String::from(INBOX),
    }
}

pub fn show_routed(key: &str, r: &Routed) {
    match r {
        Routed::Rule { thread, how } => line(&alloc::format!("{} -> {} (by {})", key, thread, how), Tone::Normal),
        Routed::Inbox(why) => line(&alloc::format!("{} -> inbox ({})", key, why), Tone::Dim),
        Routed::AskedModel(o) => {
            let (m, t) = apps::offload_msg(o);
            line(&alloc::format!("{} -> inbox for now; no rule matched, routing {}", key, m), t);
        }
    }
}

fn counts(name: &str) -> (usize, usize) {
    let open = apps::task_list().iter()
        .filter(|(k, t, _)| t.status != Status::Done && threads::thread_of(apps::TASKS, k) == name).count();
    let notes = apps::note_list().iter().filter(|(k, ..)| threads::thread_of(apps::NOTES, k) == name).count();
    (open, notes)
}

pub fn cmd_thread(args: &[&str]) {
    match args.first().copied() {
        None | Some("list") | Some("ls") => {
            let list = threads::list();
            if list.is_empty() {
                println!("  No threads yet. Try:");
                println!("    thread add work meeting email standup acme");
                println!("    thread add home groceries milk rent laundry");
                println!("    thread add health doctor meds gym     then: thread private health");
                return;
            }
            for t in &list {
                let (o, n) = counts(&t.name);
                line(&alloc::format!("{:<12} {:>2} open, {:>2} notes  {}  keys: {}", t.name, o, n,
                    if t.private { "PRIVATE" } else { "       " }, t.keywords.join(" ")),
                    if t.private { Tone::Model } else { Tone::Normal });
            }
            let (o, n) = counts(INBOX);
            line(&alloc::format!("{:<12} {:>2} open, {:>2} notes  (nothing matched)", INBOX, o, n), Tone::Dim);
            match threads::focus() {
                Some(f) => line(&alloc::format!("{}; held reminders: {}", threads::focus_label(&f), threads::held().len()), Tone::Model),
                None => line("no focus (all reminders interrupt)", Tone::Dim),
            }
            line(&alloc::format!("model routing: {}", if threads::model_routing() { "on (only when no rule matches)" } else { "off (rules only)" }), Tone::Dim);
        }
        Some("add") | Some("new") if args.len() >= 2 => match threads::define(Actor::Shell, args[1], &args[2..], None) {
            Ok(()) => line(&alloc::format!("Thread '{}' keywords: {}", args[1], args[2..].join(" ")), Tone::Normal),
            Err(e) => line(e, Tone::Deny),
        },
        Some("private") | Some("public") if args.len() >= 2 => {
            let p = args[0] == "private";
            if !threads::list().iter().any(|t| t.name == args[1]) {
                line("no such thread", Tone::Deny);
                return;
            }
            let _ = threads::define(Actor::Shell, args[1], &[], Some(p));
            if p {
                line(&alloc::format!("'{}' is private: the kernel leaves its items out of every model call.", args[1]), Tone::Model);
            } else {
                line(&alloc::format!("'{}' is public.", args[1]), Tone::Normal);
            }
        }
        Some("del") | Some("rm") if args.len() >= 2 => {
            if threads::remove(Actor::Shell, args[1]) { println!("  Removed '{}'.", args[1]); }
            else { println!("  No thread '{}'.", args[1]); }
        }
        Some("show") if args.len() >= 2 => {
            let name = args[1];
            for (k, t, tainted) in apps::task_list() {
                if threads::thread_of(apps::TASKS, &k) != name { continue; }
                let mark = match t.status { Status::Open => "[ ]", Status::Fired => "[!]", Status::Done => "[x]" };
                line(&alloc::format!("{} {:<4} {} {}", mark, k, crate::apps_core::fmt_hhmm(t.due), t.text),
                    if tainted { Tone::Taint } else if t.status == Status::Done { Tone::Dim } else { Tone::Normal });
            }
            for (k, text, _, tainted) in apps::note_list() {
                if threads::thread_of(apps::NOTES, &k) != name { continue; }
                line(&alloc::format!("{:<4} {}", k, text), if tainted { Tone::Taint } else { Tone::Normal });
            }
        }
        Some("model") => match args.get(1).copied() {
            Some("on") => { threads::set_model_routing(Actor::Shell, true); println!("  Model routing on (only when no rule matches)."); }
            Some("off") => { threads::set_model_routing(Actor::Shell, false); println!("  Model routing off: rules only, unmatched items go to inbox."); }
            _ => println!("  Usage: thread model on|off"),
        },
        _ => println!("  Usage: thread | thread add <name> [kw..] | thread private|public|del|show <name> | thread model on|off"),
    }
}

pub fn cmd_route(args: &[&str]) {
    if args.len() < 2 {
        println!("  Usage: route <tN|nN> <thread>     (why tasks tN_thread explains a routing)");
        return;
    }
    let node = if args[0].starts_with('t') { apps::TASKS } else { apps::NOTES };
    match threads::reroute(Actor::Shell, node, args[0], args[1]) {
        Ok(()) => println!("  {} -> {}", args[0], args[1]),
        Err(e) => line(e, Tone::Deny),
    }
}

pub fn cmd_focus(args: &[&str]) {
    match args.first().copied() {
        None => match threads::focus() {
            Some(f) => {
                line(&threads::focus_label(&f), Tone::Model);
                let held: Vec<String> = threads::held();
                line(&alloc::format!("held reminders: {}", if held.is_empty() { String::from("none") } else { held.join(" ") }), Tone::Normal);
            }
            None => println!("  No focus. Usage: focus <thread> [minutes] | focus off"),
        },
        Some("off") | Some("end") => match threads::focus() {
            Some(f) => {
                let d = threads::focus_off(Actor::Shell);
                threads::notify_digest(&f, &d);
            }
            None => println!("  Not focusing."),
        },
        Some(name) => {
            let mins = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            match threads::focus_on(Actor::Shell, name, mins) {
                Ok(f) => line(&alloc::format!("{}. Reminders from other threads are held until then.",
                    threads::focus_label(&f)), Tone::Model),
                Err(e) => line(e, Tone::Deny),
            }
        }
    }
}

pub fn help() {
    println!("  Threads (life areas, enforced by the kernel)");
    println!("  thread add work meeting email acme   thread add health doctor meds");
    println!("  thread private health   items there never reach the model");
    println!("  thread | thread show work | thread model on|off | route t3 home");
    println!("  focus work 90           hold other threads' reminders 90 min, then a digest");
    println!("  focus off | why tasks t3_thread   (why an item went where it did)");
    println!("  #work anywhere in a task or note files it there directly.");
}
