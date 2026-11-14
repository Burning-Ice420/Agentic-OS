//! Provenance glue: the global event ring, emit helpers, and the drain that ships
//! events to user space as `PEV|...` lines on COM1. Pure logic lives in
//! `prov_core.rs`; this file only owns globals and I/O.
//!
//! Lock order: HIVE -> PROV is allowed (events are emitted while the hive is
//! locked). PROV never takes HIVE.

use alloc::collections::VecDeque;
use alloc::string::String;
use core::fmt::Write;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

pub use crate::prov_core::{value_attrs, Actor, Kind};
use crate::prov_core::{format_line, Ring};

/// Ring capacity. Events beyond this between drains are dropped and counted.
const RING_CAP: usize = 512;

static RING: Mutex<Ring> = Mutex::new(Ring::new(RING_CAP));

/// Last drop count reported on the wire, so a `PSTAT` line is emitted whenever
/// loss occurs (loss is never silent).
static REPORTED_DROPS: Mutex<u64> = Mutex::new(0);

pub fn emit(kind: Kind, actor: Actor, mem_id: u64, mem_name: &str, key: &str,
            parent: Option<u64>, attrs: String) -> u64 {
    let tick = crate::interrupts::current_tick();
    without_interrupts(|| RING.lock().emit(tick, kind, actor, mem_id, mem_name, key, parent, attrs))
}

/// Emit with no memory target (agent lifecycle, policy changes).
pub fn emit_plain(kind: Kind, actor: Actor, key: &str, parent: Option<u64>, attrs: String) -> u64 {
    emit(kind, actor, 0, "-", key, parent, attrs)
}

pub fn set_enabled(on: bool) {
    without_interrupts(|| RING.lock().enabled = on);
}

pub fn enabled() -> bool {
    without_interrupts(|| RING.lock().enabled)
}

/// (emitted, dropped, drained, pending, next_id)
pub fn stats() -> (u64, u64, u64, usize, u64) {
    without_interrupts(|| {
        let r = RING.lock();
        (r.emitted, r.dropped, r.drained, r.pending(), r.next_id())
    })
}

/// Drop every pending event without shipping it (benchmarks only).
pub fn discard_all() {
    without_interrupts(|| {
        let mut r = RING.lock();
        while r.pop().is_some() {}
    });
}

/// Ship up to `max` events to COM1. Called from the shell loop.
pub fn drain(max: usize) {
    let mut batch: VecDeque<crate::prov_core::Event> = VecDeque::new();
    without_interrupts(|| {
        let mut r = RING.lock();
        for _ in 0..max {
            match r.pop() {
                Some(e) => batch.push_back(e),
                None => break,
            }
        }
    });
    let (_, dropped, ..) = stats();
    if batch.is_empty() && dropped == *REPORTED_DROPS.lock() {
        return;
    }
    crate::sysinfo::with_uuid(|uuid| {
        for e in &batch {
            crate::serial_println!("{}", format_line(e, uuid));
        }
    });
    let mut rep = REPORTED_DROPS.lock();
    if dropped != *rep {
        *rep = dropped;
        report_stats();
    }
}

/// Write a `PSTAT` line (also used by the `prov stats` command).
pub fn report_stats() {
    let (emitted, dropped, drained, pending, next) = stats();
    crate::sysinfo::with_uuid(|uuid| {
        crate::serial_println!("PSTAT|1|{}|emitted={};dropped={};drained={};pending={};next_id={}",
            uuid, emitted, dropped, drained, pending, next);
    });
}

// ── Policy decision log (for `cap explain`) ───────────────────────────────────

#[derive(Clone)]
pub struct DecisionRecord {
    pub event_id: u64,
    pub text: String,
}

static DECISIONS: Mutex<VecDeque<DecisionRecord>> = Mutex::new(VecDeque::new());
const DECISION_CAP: usize = 64;

pub fn record_decision(event_id: u64, text: String) {
    without_interrupts(|| {
        let mut d = DECISIONS.lock();
        if d.len() >= DECISION_CAP {
            d.pop_front();
        }
        d.push_back(DecisionRecord { event_id, text });
    });
}

pub fn explain(event_id: u64) -> Option<String> {
    without_interrupts(|| {
        DECISIONS.lock().iter().find(|r| r.event_id == event_id).map(|r| r.text.clone())
    })
}

/// Format a `k=v;k=v` attribute string.
pub fn attrs(pairs: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push(';');
        }
        let _ = write!(s, "{}={}", k, v);
    }
    s
}
