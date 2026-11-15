//! Shell commands for the hive cluster.
//!
//!   cluster coord | cluster worker <name>       pick this instance's role
//!   cluster                                     status: workers, load, the current run
//!   cluster policy rr|least|adaptive            placement policy (coordinator)
//!   cluster verify <percent>                    re-run this share of results locally
//!   cluster run <prime|hash|matmul|collatz|ai|mixed|pipeline> <jobs> [size]
//!   cluster results [n]                         latest results
//!   cluster evil on|off                         (worker) corrupt 1 in 5 results, for the demo

use crate::cluster::{self, Role};
use crate::cluster_core::Policy;
use crate::vga_buffer::{self, Color};
use crate::println;

fn say(text: &str, c: Color) {
    vga_buffer::set_color(c, Color::Black);
    println!("  {}", text);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

pub fn cmd_cluster(args: &[&str]) {
    match args.first().copied() {
        None | Some("status") | Some("st") => status(),
        Some("coord") => {
            cluster::set_role(Role::Coord);
            say("This instance is the coordinator. Workers appear as their heartbeats arrive.", Color::Yellow);
        }
        Some("worker") if args.len() >= 2 => {
            let n: alloc::string::String = args[1].chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
            if n.is_empty() { println!("  Usage: cluster worker <name>"); return; }
            cluster::set_role(Role::Worker(n.clone()));
            say(&alloc::format!("This instance is worker '{}'. Waiting for jobs.", n), Color::Yellow);
        }
        Some("policy") => match args.get(1).and_then(|s| Policy::parse(s)) {
            Some(p) => { cluster::set_policy(p); println!("  Placement policy: {}", p.name()); }
            None => println!("  Usage: cluster policy rr|least|adaptive"),
        },
        Some("reset") => {
            cluster::reset_workers();
            println!("  Quarantine cleared; every worker may get jobs again.");
        }
        Some("use") => {
            let n = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            cluster::use_workers(n);
            if n == 0 { println!("  Using every worker."); } else { println!("  Using the first {} worker(s).", n); }
        }
        Some("verify") => {
            let p = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            cluster::set_verify(p);
            println!("  Re-running {}% of results on the coordinator to check workers.", p);
        }
        Some("evil") => {
            let on = args.get(1).copied() != Some("off");
            cluster::set_evil(on);
            say(if on { "This worker now corrupts one result in five (demo of verification)." } else { "Honest again." },
                if on { Color::LightRed } else { Color::White });
        }
        Some("run") if args.len() >= 3 => {
            if cluster::role() != Role::Coord {
                say("Only the coordinator runs jobs: cluster coord", Color::LightRed);
                return;
            }
            let jobs = args[2].parse().unwrap_or(16);
            let size = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            match cluster::workload(args[1], jobs, size) {
                Some(k) => {
                    let n = cluster::submit(args[1], k);
                    say(&alloc::format!("Submitted {} job(s). 'cluster' shows progress.", n), Color::Yellow);
                }
                None => println!("  Kinds: prime hash matmul collatz ai mixed pipeline"),
            }
        }
        Some("results") | Some("res") => {
            let n = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            for (id, kind, w, r, ms) in cluster::results(n) {
                println!("  {:<8} {:<8} {:<6} {:>6} ms  {}", id, kind, w, ms, r);
            }
        }
        _ => help(),
    }
}

fn status() {
    let s = cluster::status();
    match &s.role {
        Role::Off => { println!("  Not in a cluster. 'help cluster' explains how to start one."); return; }
        Role::Worker(n) => {
            if let Some((q, done, evil)) = s.worker_self {
                say(&alloc::format!("worker {}: {} job(s) here, {} done{}", n, q, done, if evil { ", EVIL MODE" } else { "" }),
                    if evil { Color::LightRed } else { Color::White });
            }
            return;
        }
        Role::Coord => {}
    }
    say(&alloc::format!("coordinator  policy={}  verify={}%", s.policy.name(), s.verify), Color::Yellow);
    if s.workers.is_empty() {
        println!("  No workers yet.");
    }
    println!("  worker   state        in-flight  done   ms/job  busy  verified");
    for (name, alive, q, inflight, done, ema, busy, ok, bad) in &s.workers {
        let state = if *q { "QUARANTINED" } else if *alive { "alive" } else { "LOST" };
        let c = if *q || !*alive { Color::LightRed } else { Color::White };
        say(&alloc::format!("{:<8} {:<12} {:>5} {:>8} {:>8} {:>4}%  {} ok {} bad",
            name, state, inflight, done, ema, busy / 10, ok, bad), c);
    }
    if let Some((name, total, done, inflight, ms, finished, re, mm)) = s.run {
        let bar_w = 30;
        let filled = if total == 0 { 0 } else { done * bar_w / total };
        let mut bar = alloc::string::String::new();
        for i in 0..bar_w { bar.push(if i < filled { '#' } else { '.' }); }
        say(&alloc::format!("run '{}' [{}] {}/{} done, {} in flight, {} ms{}", name, bar, done, total, inflight, ms,
            if finished { " FINISHED" } else { "" }), Color::Cyan);
        if re + mm > 0 {
            say(&alloc::format!("re-routed {} job(s), {} wrong result(s) caught", re, mm), Color::LightRed);
        }
    }
}

pub fn help() {
    println!("  Hive cluster: several instances working at once over the mesh");
    println!("  cluster coord             make this the coordinator (one per cluster)");
    println!("  cluster worker w1         make this a worker");
    println!("  cluster policy rr|least|adaptive    cluster verify 10");
    println!("  cluster run prime 64      also: hash matmul collatz ai mixed pipeline");
    println!("  cluster | cluster results    status, load per worker, progress");
    println!("  Host side: python services/cluster/hub.py  (wire + live dashboard)");
}
