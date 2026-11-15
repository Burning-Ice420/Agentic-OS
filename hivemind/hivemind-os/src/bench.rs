//! In-kernel micro-benchmarks for the overhead of provenance and capability
//! enforcement. Timed with the TSC; the TSC rate is calibrated against the PIT
//! (18.2 Hz) so results are also reported in nanoseconds. Results are printed and
//! written to COM1 as `BENCH|...` lines for the experiment harness.

use crate::cap_core::{CapTable, Resource, Subject, OP_READ, OP_WRITE};
use crate::hive::{self, blob::BlobValue};
use crate::prov::{self, Actor};
use alloc::string::ToString;

pub fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC ticks per PIT tick, measured over `pit` PIT ticks (needs interrupts on).
pub fn calibrate(pit: u64) -> u64 {
    let start = crate::interrupts::current_tick();
    while crate::interrupts::current_tick() == start {
        core::hint::spin_loop();
    }
    let t0 = tsc();
    let s0 = crate::interrupts::current_tick();
    while crate::interrupts::current_tick() < s0 + pit {
        core::hint::spin_loop();
    }
    (tsc() - t0) / pit
}

fn ns(cycles: u64, tsc_per_pit: u64) -> u64 {
    // PIT tick = 1/18.2065 s = 54_925_400 ns
    if tsc_per_pit == 0 { 0 } else { cycles.saturating_mul(54_925) / tsc_per_pit.max(1) * 1000 / 1000 }
}

fn report(name: &str, n: usize, cycles: u64, cal: u64) {
    let per = cycles / n.max(1) as u64;
    let per_ns = (per as u128 * 54_925_400u128 / cal.max(1) as u128) as u64;
    crate::println!("  {:<28} {:>8} cycles/op  ~{:>6} ns/op", name, per, per_ns);
    crate::sysinfo::with_uuid(|u| crate::serial_println!(
        "BENCH|1|{}|{}|n={};cycles_per_op={};ns_per_op={};tsc_per_pit={}", u, name, n, per, per_ns, cal));
    let _ = ns;
}

/// Cost of an attributed blob write with provenance off, on (record only), and
/// on + shipping every event to COM1.
pub fn prov_writes(n: usize) {
    let cal = calibrate(18);
    let node = hive::with_hive(|h| h.find_by_name("bench").unwrap_or_else(|| h.create_memory_as(Actor::Shell, "bench", None)));
    let was = prov::enabled();

    let run = |label: &str, mode: u8| {
        if mode == 0 { prov::set_enabled(false); } else { prov::set_enabled(true); }
        prov::discard_all();
        let t0 = tsc();
        for i in 0..n {
            hive::with_hive(|h| { let _ = h.write_blob_as(Actor::Shell, node, "k", BlobValue::Number(i as i64), None); });
            if mode == 1 && i % 256 == 255 { prov::discard_all(); }
            if mode == 2 && i % 32 == 31 { prov::drain(32); }
        }
        if mode == 2 { prov::drain(64); }
        let dt = tsc() - t0;
        prov::discard_all();
        report(label, n, dt, cal);
    };
    run("write.prov_off", 0);
    run("write.prov_on_record", 1);
    run("write.prov_on_ship_com1", 2);
    prov::set_enabled(was);
}

/// Cost of one capability check against a table holding `ncaps` capabilities
/// (the matching one placed last: worst case for the linear scan).
pub fn cap_checks(n: usize, ncaps: usize) {
    let cal = calibrate(18);
    let mut t = CapTable::new();
    t.set_enforce(true);
    for i in 0..ncaps.saturating_sub(1) {
        t.grant(Subject::Agent(10_000 + i as u64), Resource::Node(1), OP_READ, None, 0);
    }
    t.grant(Subject::Agent(7), Resource::Prefix("work/".to_string()), OP_READ | OP_WRITE, None, 0);
    let mut allowed = 0usize;
    let t0 = tsc();
    for i in 0..n {
        if t.check_node(Subject::Agent(7), OP_WRITE, i as u64, "work/item", 0).allowed() {
            allowed += 1;
        }
    }
    let dt = tsc() - t0;
    let mut label = alloc::string::String::from("cap.check.caps=");
    label.push_str(&ncaps.to_string());
    report(&label, n, dt, cal);
    if allowed != n {
        crate::println!("  WARNING: {} of {} checks allowed (expected all)", allowed, n);
    }
}
