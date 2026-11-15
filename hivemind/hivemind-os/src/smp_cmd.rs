//! Shell commands for multi-core runs.
//!
//!   cpus | smp                       cores online, what each is doing
//!   smp run <prime|hash|matmul|collatz|mixed> <jobs> [size]
//!   smp sched static|dynamic         how the kernel places jobs on cores
//!   smp use <n>                      use only the first n compute cores (0 = all)
//!   smp hog <core> <percent>         give a core a competing load (demo of imbalance)
//!   smp bench <kind> <jobs>          same jobs on 1, 2, 4 ... cores; prints speedup
//!   smp results [n] | smp stop

use crate::println;
use crate::smp;
use crate::smp_core::Sched;
use crate::vga_buffer::{self, Color};

fn say(text: &str, c: Color) {
    vga_buffer::set_color(c, Color::Black);
    println!("  {}", text);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

pub fn cmd_smp(args: &[&str]) {
    match args.first().copied() {
        None | Some("status") | Some("st") => status(),
        Some("sched") if args.len() >= 2 => match Sched::parse(args[1]) {
            Some(s) => { smp::set_sched(s); println!("  Placement: {}", s.name()); }
            None => println!("  Usage: smp sched static|dynamic"),
        },
        Some("use") if args.len() >= 2 => {
            let n: usize = args[1].parse().unwrap_or(0);
            smp::use_cores(n);
            println!("  Using {} of {} compute cores.", smp::cores_in_use(), smp::workers());
        }
        Some("hog") if args.len() >= 3 => {
            let c: usize = args[1].parse().unwrap_or(0);
            let p: u32 = args[2].parse().unwrap_or(0);
            if smp::set_hog(c, p) {
                println!("  Core {} now spends {}% of its time on a competing load.", c, p.min(95));
            } else {
                say("No such compute core (core 0 runs the shell).", Color::LightRed);
            }
        }
        Some("run") if args.len() >= 3 => {
            let jobs: u64 = args[2].parse().unwrap_or(0);
            let size: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            start(args[1], jobs, size);
        }
        Some("bench") if args.len() >= 3 => {
            let jobs: u64 = args[2].parse().unwrap_or(0);
            bench(args[1], jobs);
        }
        Some("results") | Some("res") => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12);
            for (i, core, spec, r, ms) in smp::last_jobs(n) {
                let r = r.map(|v| alloc::format!("{}", v)).unwrap_or_else(|| alloc::string::String::from("-"));
                println!("  job {:>3}  core {:>2}  {:<28} = {:<14} {} ms", i, core, spec, r, ms);
            }
        }
        Some("stop") => { smp::cancel(); println!("  Run stopped."); }
        _ => help(),
    }
}

fn start(kind: &str, jobs: u64, size: u64) -> bool {
    if kind == "pipeline" || kind == "ai" {
        say("Cores run compute jobs; model calls go through the shell core (use cluster for those).", Color::LightRed);
        return false;
    }
    match crate::cluster::workload(kind, jobs, size) {
        Some(kinds) => match smp::submit(kind, kinds) {
            Ok((id, cores)) => {
                say(&alloc::format!("Run {} started: {} {} job(s) on {} core(s), {} placement.",
                    id, jobs, kind, cores, smp::sched().name()), Color::Yellow);
                true
            }
            Err(e) => { say(e, Color::LightRed); false }
        },
        None => { println!("  Kinds: prime hash matmul collatz mixed"); false }
    }
}

/// Wait for the current run, halting between interrupts (cores wake the shell
/// core with an IPI when the last job ends).
fn wait() -> Option<smp::LastRun> {
    loop {
        if smp::tick().is_some() { return smp::last(); }
        if !smp::running() && smp::last().map(|l| l.done == l.jobs).unwrap_or(true) {
            smp::tick();
            return smp::last();
        }
        x86_64::instructions::hlt();
    }
}

fn bench(kind: &str, jobs: u64) {
    let total = smp::workers();
    if total == 0 { say("No compute cores online (start QEMU with -smp 2 or more).", Color::LightRed); return; }
    let keep = smp::cores_in_use();
    let mut ns = alloc::vec::Vec::new();
    let mut n = 1;
    while n < total { ns.push(n); n *= 2; }
    ns.push(total);
    println!("  {} x {} on 1..{} cores, {} placement", jobs, kind, total, smp::sched().name());
    let mut base = 0u64;
    for n in ns {
        smp::use_cores(n);
        if !start(kind, jobs, 0) { break; }
        if let Some(l) = wait() {
            if n == 1 { base = l.ms.max(1); }
            let sp = base * 100 / l.ms.max(1);
            let avg = if l.busy.is_empty() { 0 } else { l.busy.iter().sum::<u32>() / l.busy.len() as u32 };
            println!("  {:>2} core(s): {:>7} ms   speedup {}.{:02}x   busy {}% avg", n, l.ms, sp / 100, sp % 100, avg);
        }
    }
    smp::use_cores(keep);
}

fn status() {
    let cores = smp::cores();
    println!("  {} CPU(s) online: core 0 runs the shell, {} compute core(s); placement {}, using {}",
        cores.len(), smp::workers(), smp::sched().name(), smp::cores_in_use());
    for c in &cores {
        let what = if c.cpu == 0 { "shell, devices, memory graph" } else if c.working { "working" } else { "idle (halted)" };
        let hog = if c.hog > 0 { alloc::format!("  hog {}%", c.hog) } else { alloc::string::String::new() };
        println!("  core {:>2}  apic {:>2}  {:<28} jobs {:>4}  busy {} ms{}", c.cpu, c.apic, what, c.jobs, c.work_ms, hog);
    }
    if let Some(l) = smp::last() {
        let busy = if l.busy.is_empty() { alloc::string::String::new() } else { alloc::format!("  busy % per core {:?}", l.busy) };
        say(&alloc::format!("run {} '{}' on {} core(s), {}: {}/{} done, {} ms{}",
            l.id, l.name, l.cores, l.sched.name(), l.done, l.jobs, l.ms, busy), Color::Cyan);
    }
}

pub fn help() {
    println!("  Multi-core: core 0 runs the shell; the other cores run jobs");
    println!("  cpus | smp                status of every core");
    println!("  smp run prime 48          also: hash matmul collatz mixed");
    println!("  smp sched static|dynamic  smp use 4   smp hog 1 50");
    println!("  smp bench prime 48        1, 2, 4 ... cores, speedup table");
    println!("  smp results | smp stop");
}
