//! Multi-core support. The boot CPU (BSP) finds the other cores in the ACPI MADT,
//! starts each one (INIT, SIPI) through a small real-mode trampoline, and then
//! uses them as compute cores:
//!
//!  * the BSP keeps the shell, devices, hive memory, agents and the model link,
//!    exactly as on a single core (all interrupts from the PIC still go to it);
//!  * each other core (AP) runs jobs from the kernel's job table and halts when
//!    there is nothing to do; the BSP wakes it with an inter-processor interrupt;
//!  * placement is a kernel decision: `static` (job i on core i mod N, fixed up
//!    front) or `dynamic` (a core takes the next waiting job when it is free).
//!
//! The APs never touch the hive graph, devices or the screen. Jobs are the same
//! bounded kinds the cluster runs (`cluster_core::Kind`); results come back to
//! the BSP, which prints and records them.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::cluster_core::{Exec, Kind};
use crate::prov::{self, Actor, Kind as EvKind};
use crate::smp_core::{self, busy_pct, hog_cycles, static_plan, Sched};

pub const MAX_CPUS: usize = 16;
pub const IPI_VECTOR: u8 = 0xF0;
pub const SPURIOUS_VECTOR: u8 = 0xFF;
const LAPIC_VIRT: u64 = 0x5555_0000_0000;
const AP_STACK: usize = 64 * 1024;
const AP_IST: usize = 16 * 1024;

fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

// ── Trampoline (copied to a free page below 1 MiB; the AP starts there in real
//    mode, switches to protected mode, then to long mode with the BSP's page
//    tables, and calls `ap_entry`). Addresses are absolute: base + offset. ──────

core::arch::global_asm!(
    r#"
.pushsection .text.smp_tramp, "ax"
.code16
.global smp_tramp_start
smp_tramp_start:
    cli
    cld
    jmp 2f
.p2align 3
.global smp_tramp_base
smp_tramp_base:  .quad 0
.global smp_tramp_cr3
smp_tramp_cr3:   .quad 0
.global smp_tramp_stack
smp_tramp_stack: .quad 0
.global smp_tramp_entry
smp_tramp_entry: .quad 0
.global smp_tramp_cpu
smp_tramp_cpu:   .quad 0
.p2align 3
smp_tramp_gdt:
    .quad 0
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
.global smp_tramp_gdtr
smp_tramp_gdtr:
    .word 31
    .long 0
.global smp_tramp_far32
smp_tramp_far32:
    .long 0
    .word 0x08
.global smp_tramp_far64
smp_tramp_far64:
    .long 0
    .word 0x18
.global smp_tramp_gdt_off
smp_tramp_gdt_off:
    .long smp_tramp_gdt - smp_tramp_start
2:
    movw %cs, %ax
    movw %ax, %ds
    movl (smp_tramp_base - smp_tramp_start), %ebx
    lgdtl (smp_tramp_gdtr - smp_tramp_start)
    movl %cr0, %eax
    orl $1, %eax
    movl %eax, %cr0
    ljmpl *(smp_tramp_far32 - smp_tramp_start)
.code32
.global smp_tramp_pm
smp_tramp_pm:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4
    movl (smp_tramp_cr3 - smp_tramp_start)(%ebx), %eax
    movl %eax, %cr3
    movl $0xC0000080, %ecx
    rdmsr
    orl $0x900, %eax
    wrmsr
    movl %cr0, %eax
    orl $0x80010000, %eax
    movl %eax, %cr0
    ljmpl *(smp_tramp_far64 - smp_tramp_start)(%ebx)
.code64
.global smp_tramp_lm
smp_tramp_lm:
    xorl %eax, %eax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs
    movl %ebx, %ebx
    movq (smp_tramp_stack - smp_tramp_start)(%rbx), %rsp
    movq (smp_tramp_cpu - smp_tramp_start)(%rbx), %rdi
    movq (smp_tramp_entry - smp_tramp_start)(%rbx), %rax
    callq *%rax
3:  hlt
    jmp 3b
.global smp_tramp_end
smp_tramp_end:
.popsection
"#,
    options(att_syntax)
);

extern "C" {
    static smp_tramp_start: u8;
    static smp_tramp_end: u8;
    static smp_tramp_base: u8;
    static smp_tramp_cr3: u8;
    static smp_tramp_stack: u8;
    static smp_tramp_entry: u8;
    static smp_tramp_cpu: u8;
    static smp_tramp_gdtr: u8;
    static smp_tramp_gdt_off: u32;
    static smp_tramp_far32: u8;
    static smp_tramp_far64: u8;
    static smp_tramp_pm: u8;
    static smp_tramp_lm: u8;
}

fn off(sym: *const u8) -> u64 {
    sym as u64 - unsafe { &smp_tramp_start as *const u8 as u64 }
}

// ── Local APIC ────────────────────────────────────────────────────────────────

static LAPIC: AtomicU64 = AtomicU64::new(0);

fn lapic_read(reg: u32) -> u32 {
    let b = LAPIC.load(Ordering::Relaxed);
    unsafe { core::ptr::read_volatile((b + reg as u64) as *const u32) }
}

fn lapic_write(reg: u32, v: u32) {
    let b = LAPIC.load(Ordering::Relaxed);
    unsafe { core::ptr::write_volatile((b + reg as u64) as *mut u32, v) }
}

fn lapic_id() -> u8 {
    (lapic_read(0x20) >> 24) as u8
}

fn lapic_enable() {
    lapic_write(0x80, 0); // task priority: accept everything
    let svr = lapic_read(0xF0);
    lapic_write(0xF0, svr | 0x100 | SPURIOUS_VECTOR as u32);
}

/// End of interrupt for a local APIC interrupt (the IPI vector).
pub fn eoi() {
    if LAPIC.load(Ordering::Relaxed) != 0 {
        lapic_write(0xB0, 0);
    }
}

fn send_ipi(apic: u8, low: u32) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        lapic_write(0x310, (apic as u32) << 24);
        lapic_write(0x300, low);
        let mut n = 0u32;
        while lapic_read(0x300) & (1 << 12) != 0 && n < 1_000_000 {
            core::hint::spin_loop();
            n += 1;
        }
    });
}

fn wake(cpu: usize) {
    send_ipi(APIC_ID[cpu].load(Ordering::Relaxed), IPI_VECTOR as u32);
}

// ── Cores ─────────────────────────────────────────────────────────────────────

const IDLE: u8 = 0;
const WORKING: u8 = 1;

static NCPU: AtomicUsize = AtomicUsize::new(1);
static APIC_ID: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static STATE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(IDLE) }; MAX_CPUS];
static JOBS_DONE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static WORK_CYC: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static HOG_CYC: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static HOG_PCT: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
static TSC_PER_MS: AtomicU64 = AtomicU64::new(1);

static mut STACKS: [[u8; AP_STACK]; MAX_CPUS] = [[0; AP_STACK]; MAX_CPUS];
static mut ISTS: [[u8; AP_IST]; MAX_CPUS] = [[0; AP_IST]; MAX_CPUS];

/// Number of CPUs online, including the boot CPU.
pub fn online() -> usize {
    (0..NCPU.load(Ordering::Relaxed)).filter(|&i| ONLINE[i].load(Ordering::Relaxed)).count()
}

/// Number of compute cores (online APs).
pub fn workers() -> usize {
    online().saturating_sub(1)
}

pub fn tsc_per_ms() -> u64 {
    TSC_PER_MS.load(Ordering::Relaxed)
}

fn delay_us(us: u64) {
    let t0 = tsc();
    let n = tsc_per_ms() * us / 1000;
    while tsc() - t0 < n { core::hint::spin_loop(); }
}

/// Find the CPUs, map the local APIC and the trampoline page, and start every
/// other core. Needs interrupts on (the TSC is calibrated against the PIT).
pub fn init(mapper: &mut OffsetPageTable, frames: &mut impl FrameAllocator<Size4KiB>,
            phys_off: u64, memory_map: &bootloader::bootinfo::MemoryMap) {
    use bootloader::bootinfo::MemoryRegionType;
    TSC_PER_MS.store((crate::bench::calibrate(2) * 1000 / 54_925).max(1), Ordering::Relaxed);
    ONLINE[0].store(true, Ordering::Relaxed);

    // 1. ACPI tables (read through the physical-memory mapping).
    let phys = |a: u64, len: usize| unsafe { core::slice::from_raw_parts((phys_off + a) as *const u8, len) };
    let root = match smp_core::find_rsdp(phys(0xE0000, 0x20000)) {
        Some(r) => r,
        None => { crate::serial_println!("[smp] no ACPI RSDP; single core"); return; }
    };
    let (addr, wide) = match root { smp_core::Root::Rsdt(a) => (a, false), smp_core::Root::Xsdt(a) => (a, true) };
    let len = smp_core::table_len(phys(addr, 36)).unwrap_or(36);
    let mut madt = None;
    for t in smp_core::root_entries(phys(addr, len), wide) {
        let h = phys(t, 36);
        if &h[0..4] == b"APIC" {
            let l = smp_core::table_len(h).unwrap_or(36).min(64 * 1024);
            madt = smp_core::parse_madt(phys(t, l));
        }
    }
    let madt = match madt {
        Some(m) => m,
        None => { crate::serial_println!("[smp] no MADT; single core"); return; }
    };

    // 2. Map the local APIC registers (uncached).
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH;
    let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(LAPIC_VIRT));
    let frame = PhysFrame::containing_address(PhysAddr::new(madt.lapic_addr));
    match unsafe { mapper.map_to(page, frame, flags, frames) } {
        Ok(f) => f.flush(),
        Err(_) => { crate::serial_println!("[smp] cannot map the local APIC; single core"); return; }
    }
    LAPIC.store(LAPIC_VIRT, Ordering::Relaxed);
    lapic_enable();
    let me = lapic_id();
    APIC_ID[0].store(me, Ordering::Relaxed);

    // 3. A page below 1 MiB for the trampoline: one the bootloader used (its code
    //    is no longer needed), never a usable frame the frame allocator may hand out.
    let mut tramp = 0u64;
    let mut a = 0x1000u64;
    while a < 0x9F000 && tramp == 0 {
        let ok = memory_map.iter().any(|r| r.region_type == MemoryRegionType::Bootloader
            && r.range.start_addr() <= a && a + 0x1000 <= r.range.end_addr());
        if ok { tramp = a; }
        a += 0x1000;
    }
    if tramp == 0 {
        for r in memory_map.iter() {
            if r.range.start_addr() < 0x100000 {
                crate::serial_println!("[smp] low region {:#x}-{:#x} {:?}", r.range.start_addr(), r.range.end_addr(), r.region_type);
            }
        }
        crate::serial_println!("[smp] no free page below 1 MiB; single core");
        return;
    }
    let tpage: Page<Size4KiB> = Page::containing_address(VirtAddr::new(tramp));
    let tframe = PhysFrame::containing_address(PhysAddr::new(tramp));
    match unsafe { mapper.map_to(tpage, tframe, PageTableFlags::PRESENT | PageTableFlags::WRITABLE, frames) } {
        Ok(f) => f.flush(),
        Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(f)) if f == tframe => {}
        Err(_) => { crate::serial_println!("[smp] cannot identity-map {:#x}; single core", tramp); return; }
    }

    // 4. Copy the trampoline and fill in its absolute addresses.
    let code = unsafe {
        let s = &smp_tramp_start as *const u8;
        let n = &smp_tramp_end as *const u8 as usize - s as usize;
        core::slice::from_raw_parts(s, n)
    };
    let dst = (phys_off + tramp) as *mut u8;
    let put64 = |sym: *const u8, v: u64| unsafe { core::ptr::write_unaligned(dst.add(off(sym) as usize) as *mut u64, v) };
    let put32 = |at: u64, v: u32| unsafe { core::ptr::write_unaligned(dst.add(at as usize) as *mut u32, v) };
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), dst, code.len());
        let gdt_off = smp_tramp_gdt_off as u64;
        put64(&smp_tramp_base, tramp);
        put64(&smp_tramp_cr3, x86_64::registers::control::Cr3::read().0.start_address().as_u64());
        put64(&smp_tramp_entry, ap_entry as *const () as usize as u64);
        put32(off(&smp_tramp_gdtr) + 2, (tramp + gdt_off) as u32);
        put32(off(&smp_tramp_far32), (tramp + off(&smp_tramp_pm)) as u32);
        put32(off(&smp_tramp_far64), (tramp + off(&smp_tramp_lm)) as u32);
    }

    // 5. Start each other core in turn: INIT, then SIPI (twice if needed).
    let mut n = 1usize;
    for &id in madt.apic_ids.iter() {
        if id == me || n >= MAX_CPUS { continue; }
        APIC_ID[n].store(id, Ordering::Relaxed);
        let top = unsafe { (core::ptr::addr_of!(STACKS[n]) as u64) + AP_STACK as u64 } & !0xF;
        put64(unsafe { &smp_tramp_stack }, top);
        put64(unsafe { &smp_tramp_cpu }, n as u64);
        NCPU.store(n + 1, Ordering::SeqCst);
        send_ipi(id, 0x4500);                       // INIT
        delay_us(10_000);
        let sipi = 0x4600 | (tramp >> 12) as u32;   // start-up at the trampoline page
        send_ipi(id, sipi);
        let mut waited = 0;
        while !ONLINE[n].load(Ordering::SeqCst) && waited < 2_000 { delay_us(100); waited += 1; }
        if !ONLINE[n].load(Ordering::SeqCst) {
            send_ipi(id, sipi);
            waited = 0;
            while !ONLINE[n].load(Ordering::SeqCst) && waited < 10_000 { delay_us(100); waited += 1; }
        }
        if ONLINE[n].load(Ordering::SeqCst) {
            n += 1;
        } else {
            crate::serial_println!("[smp] cpu apic={} did not start", id);
        }
    }
    NCPU.store(n, Ordering::SeqCst);
    crate::serial_println!("[smp] cpus={} madt={} tramp={:#x} bsp_apic={}", online(), madt.apic_ids.len(), tramp, me);
}

/// First Rust code on an application processor.
extern "C" fn ap_entry(cpu: u64) -> ! {
    use x86_64::instructions::segmentation::{Segment, CS, SS};
    use x86_64::instructions::tables::load_tss;
    use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
    use x86_64::structures::tss::TaskStateSegment;
    let cpu = cpu as usize;

    // Own GDT + TSS (the double-fault stack is per core).
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[crate::gdt::DOUBLE_FAULT_IST_INDEX as usize] =
        VirtAddr::new(unsafe { core::ptr::addr_of!(ISTS[cpu]) as u64 } + AP_IST as u64);
    let tss: &'static TaskStateSegment = Box::leak(Box::new(tss));
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.add_entry(Descriptor::kernel_code_segment());
    let tsel = gdt.add_entry(Descriptor::tss_segment(tss));
    let gdt: &'static GlobalDescriptorTable = Box::leak(Box::new(gdt));
    gdt.load();
    unsafe {
        CS::set_reg(code);
        SS::set_reg(SegmentSelector(0));
        load_tss(tsel);
    }
    crate::interrupts::init_idt();
    lapic_enable();
    ONLINE[cpu].store(true, Ordering::SeqCst);
    ap_loop(cpu)
}

// ── Runs ──────────────────────────────────────────────────────────────────────

pub struct JobRec {
    pub kind: Kind,
    pub core: u8,
    pub result: Option<u64>,
    pub start_ms: u64,
    pub end_ms: u64,
}

pub struct Run {
    pub id: u64,
    pub name: String,
    pub sched: Sched,
    pub cores: usize,
    pub jobs: Vec<JobRec>,
    next: usize,
    plan: Vec<VecDeque<usize>>,
    pub left: usize,
    pub t0: u64,
    pub t_end: u64,
    pub reported: bool,
    work0: Vec<u64>,
    pub work: Vec<u64>,
}

struct Pool {
    run: Option<Run>,
    runs: u64,
    sched: Sched,
    use_n: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { run: None, runs: 0, sched: Sched::Dynamic, use_n: 0 });

fn ms_since(t0: u64, t: u64) -> u64 {
    t.saturating_sub(t0) / tsc_per_ms()
}

/// Take the next job for core `cpu` (cores 1..=N of the run take part).
fn take(cpu: usize) -> Option<(u64, usize, Kind)> {
    let mut p = POOL.lock();
    let run = p.run.as_mut()?;
    if cpu > run.cores || run.left == 0 { return None; }
    let idx = match run.sched {
        Sched::Dynamic => {
            if run.next >= run.jobs.len() { return None; }
            run.next += 1;
            run.next - 1
        }
        Sched::Static => run.plan.get_mut(cpu - 1)?.pop_front()?,
    };
    let now = ms_since(run.t0, tsc());
    let j = &mut run.jobs[idx];
    j.core = cpu as u8;
    j.start_ms = now;
    Some((run.id, idx, j.kind.clone()))
}

fn finish(cpu: usize, run_id: u64, idx: usize, result: Option<u64>) {
    let last = {
        let mut p = POOL.lock();
        match p.run.as_mut() {
            Some(run) if run.id == run_id => {
                let now = tsc();
                let j = &mut run.jobs[idx];
                j.result = result;
                j.end_ms = ms_since(run.t0, now);
                run.left -= 1;
                if run.left == 0 { run.t_end = now; true } else { false }
            }
            _ => false,
        }
    };
    JOBS_DONE[cpu].fetch_add(1, Ordering::Relaxed);
    if last { wake(0); }
}

/// Run one job on this core, burning the configured hog share alongside it.
fn compute(cpu: usize, run_id: u64, kind: Kind) -> Option<u64> {
    let mut e = Exec::new(kind)?;
    loop {
        let t0 = tsc();
        let r = e.step(1 << 16);
        let worked = tsc() - t0;
        WORK_CYC[cpu].fetch_add(worked, Ordering::Relaxed);
        let hog = HOG_PCT[cpu].load(Ordering::Relaxed);
        if hog > 0 {
            let h0 = tsc();
            let burn = hog_cycles(worked, hog);
            while tsc() - h0 < burn { core::hint::spin_loop(); }
            HOG_CYC[cpu].fetch_add(tsc() - h0, Ordering::Relaxed);
        }
        if r.is_some() { return r; }
        if cancelled(run_id) { return None; }
    }
}

fn cancelled(run_id: u64) -> bool {
    match POOL.try_lock() {
        Some(p) => p.run.as_ref().map(|r| r.id != run_id).unwrap_or(true),
        None => false,
    }
}

fn ap_loop(cpu: usize) -> ! {
    use x86_64::instructions::interrupts;
    loop {
        interrupts::disable();
        match take(cpu) {
            Some((run_id, idx, kind)) => {
                interrupts::enable();
                STATE[cpu].store(WORKING, Ordering::Relaxed);
                let r = compute(cpu, run_id, kind);
                finish(cpu, run_id, idx, r);
            }
            None => {
                STATE[cpu].store(IDLE, Ordering::Relaxed);
                // Enable interrupts and halt in one step: a wake-up IPI sent after
                // `take` found nothing still ends this halt.
                interrupts::enable_and_hlt();
            }
        }
    }
}

// ── BSP side ──────────────────────────────────────────────────────────────────

pub fn sched() -> Sched { POOL.lock().sched }
pub fn set_sched(s: Sched) { POOL.lock().sched = s; }
pub fn use_cores(n: usize) { POOL.lock().use_n = n.min(workers()); }
pub fn cores_in_use() -> usize {
    let n = POOL.lock().use_n;
    if n == 0 { workers() } else { n.min(workers()) }
}

pub fn set_hog(core: usize, pct: u32) -> bool {
    if core == 0 || core >= online() { return false; }
    HOG_PCT[core].store(pct.min(95), Ordering::Relaxed);
    true
}

fn line(s: String) {
    crate::sysinfo::with_uuid(|u| crate::serial_println!("SMP|1|{}|{}", u, s));
}

/// Start a run of `kinds` on the cores in use. Only local compute kinds.
pub fn submit(name: &str, kinds: Vec<Kind>) -> Result<(u64, usize), &'static str> {
    if workers() == 0 { return Err("no other cores online (start QEMU with -smp 2 or more)"); }
    if kinds.iter().any(|k| !matches!(k, Kind::Prime { .. } | Kind::Hash { .. } | Kind::Matmul { .. } | Kind::Collatz { .. })) {
        return Err("only compute jobs run on cores (prime, hash, matmul, collatz, mixed)");
    }
    let cores = cores_in_use();
    let (id, sched) = {
        let mut p = POOL.lock();
        if p.run.as_ref().map(|r| r.left > 0).unwrap_or(false) { return Err("a run is in progress"); }
        p.runs += 1;
        let n = kinds.len();
        let plan = static_plan(n, cores).into_iter().map(VecDeque::from).collect();
        let work0 = (0..MAX_CPUS).map(|i| WORK_CYC[i].load(Ordering::Relaxed) + HOG_CYC[i].load(Ordering::Relaxed)).collect();
        let jobs = kinds.into_iter().map(|kind| JobRec { kind, core: 0, result: None, start_ms: 0, end_ms: 0 }).collect();
        let run = Run { id: p.runs, name: String::from(name), sched: p.sched, cores, jobs, next: 0, plan,
                        left: n, t0: tsc(), t_end: 0, reported: false, work0, work: Vec::new() };
        let r = (run.id, run.sched);
        p.run = Some(run);
        r
    };
    prov::emit(EvKind::Marker, Actor::Shell, 0, "-", "smp", None,
        alloc::format!("smp=1;run={};kind={};cores={};sched={}", id, name, cores, sched.name()));
    line(alloc::format!("start;run={};kind={};cores={};sched={}", id, name, cores, sched.name()));
    for c in 1..=cores { wake(c); }
    Ok((id, cores))
}

/// Called from the BSP main loop: report a finished run once.
pub fn tick() -> Option<u64> {
    let mut p = POOL.lock();
    let run = p.run.as_mut()?;
    if run.left > 0 || run.reported { return None; }
    run.reported = true;
    run.work = (0..MAX_CPUS).map(|i| (WORK_CYC[i].load(Ordering::Relaxed) + HOG_CYC[i].load(Ordering::Relaxed))
        .saturating_sub(run.work0[i])).collect();
    let wall = run.t_end.saturating_sub(run.t0);
    let ms = wall / tsc_per_ms();
    let mut busy = String::new();
    for c in 1..=run.cores {
        if c > 1 { busy.push(','); }
        busy.push_str(&alloc::format!("{}", busy_pct(run.work[c], wall)));
    }
    let id = run.id;
    let summary = alloc::format!("done;run={};kind={};cores={};sched={};jobs={};ms={};busy={}",
        run.id, run.name, run.cores, run.sched.name(), run.jobs.len(), ms, busy);
    let mut jl = Vec::new();
    for (i, j) in run.jobs.iter().enumerate() {
        jl.push(alloc::format!("job;run={};i={};core={};spec={};result={};start={};end={}", run.id, i, j.core,
            j.kind.encode(), j.result.map(|r| alloc::format!("{}", r)).unwrap_or_else(|| String::from("-")),
            j.start_ms, j.end_ms));
    }
    drop(p);
    for l in jl { line(l); }
    line(summary.clone());
    prov::emit(EvKind::Marker, Actor::Shell, 0, "-", "smp", None, alloc::format!("smp=1;{}", summary));
    Some(id)
}

/// Is a run in progress?
pub fn running() -> bool {
    POOL.lock().run.as_ref().map(|r| r.left > 0).unwrap_or(false)
}

/// Stop the current run (cores drop their job at the next slice).
pub fn cancel() {
    POOL.lock().run = None;
}

pub struct CoreView {
    pub cpu: usize,
    pub apic: u8,
    pub working: bool,
    pub jobs: u64,
    pub work_ms: u64,
    pub hog: u32,
}

pub fn cores() -> Vec<CoreView> {
    (0..NCPU.load(Ordering::Relaxed)).filter(|&i| ONLINE[i].load(Ordering::Relaxed)).map(|i| CoreView {
        cpu: i,
        apic: APIC_ID[i].load(Ordering::Relaxed),
        working: i > 0 && STATE[i].load(Ordering::Relaxed) == WORKING,
        jobs: JOBS_DONE[i].load(Ordering::Relaxed),
        work_ms: WORK_CYC[i].load(Ordering::Relaxed) / tsc_per_ms(),
        hog: HOG_PCT[i].load(Ordering::Relaxed),
    }).collect()
}

pub struct LastRun {
    pub id: u64,
    pub name: String,
    pub cores: usize,
    pub sched: Sched,
    pub jobs: usize,
    pub done: usize,
    pub ms: u64,
    pub busy: Vec<u32>,
}

pub fn last() -> Option<LastRun> {
    let p = POOL.lock();
    let r = p.run.as_ref()?;
    let end = if r.left == 0 { r.t_end } else { tsc() };
    let wall = end.saturating_sub(r.t0);
    let busy = if r.work.is_empty() { Vec::new() } else { (1..=r.cores).map(|c| busy_pct(r.work[c], wall)).collect() };
    Some(LastRun { id: r.id, name: r.name.clone(), cores: r.cores, sched: r.sched, jobs: r.jobs.len(),
                   done: r.jobs.len() - r.left, ms: wall / tsc_per_ms(), busy })
}

/// Jobs of the last run: (index, core, spec, result, ms).
pub fn last_jobs(n: usize) -> Vec<(usize, u8, String, Option<u64>, u64)> {
    let p = POOL.lock();
    match p.run.as_ref() {
        Some(r) => r.jobs.iter().enumerate().take(n)
            .map(|(i, j)| (i, j.core, j.kind.encode(), j.result, j.end_ms.saturating_sub(j.start_ms))).collect(),
        None => Vec::new(),
    }
}
