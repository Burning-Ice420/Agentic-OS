//! Pure logic for multi-core (SMP) support: finding the CPUs in the firmware's
//! ACPI tables, and the two ways the kernel places jobs on cores.
//! Only `core` + `alloc`, so the host crate unit-tests this exact file.

use alloc::vec::Vec;

// ── ACPI: RSDP -> RSDT/XSDT -> MADT ───────────────────────────────────────────

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    let s = b.get(i..i + 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

/// ACPI checksum: all bytes sum to 0 mod 256.
pub fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Where the RSDP says the root table is.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Root {
    Rsdt(u64),
    Xsdt(u64),
}

/// Find the RSDP in a memory area (the BIOS area 0xE0000..0x100000). It sits on
/// a 16-byte boundary and starts with "RSD PTR ". Returns the root table address.
pub fn find_rsdp(area: &[u8]) -> Option<Root> {
    let mut i = 0;
    while i + 20 <= area.len() {
        if &area[i..i + 8] == b"RSD PTR " && checksum_ok(&area[i..i + 20]) {
            let rev = area[i + 15];
            if rev >= 2 {
                if let Some(x) = u64_at(area, i + 24) {
                    if x != 0 { return Some(Root::Xsdt(x)); }
                }
            }
            return Some(Root::Rsdt(u32_at(area, i + 16)? as u64));
        }
        i += 16;
    }
    None
}

/// Length field of an ACPI table header (bytes 4..8).
pub fn table_len(header: &[u8]) -> Option<usize> {
    u32_at(header, 4).map(|l| l as usize)
}

/// Table addresses listed in an RSDT (4-byte entries) or XSDT (8-byte entries).
pub fn root_entries(table: &[u8], wide: bool) -> Vec<u64> {
    let len = table_len(table).unwrap_or(0).min(table.len());
    let step = if wide { 8 } else { 4 };
    let mut v = Vec::new();
    let mut i = 36;
    while i + step <= len {
        let a = if wide { u64_at(table, i) } else { u32_at(table, i).map(|x| x as u64) };
        if let Some(a) = a { v.push(a); }
        i += step;
    }
    v
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Madt {
    pub lapic_addr: u64,
    /// Local APIC ids of the usable CPUs, in table order (the boot CPU is first on
    /// QEMU and on most firmware, but callers must not assume it).
    pub apic_ids: Vec<u8>,
}

/// Parse a MADT ("APIC" table). Type 0 entries are processors; bit 0 of their
/// flags = enabled. Type 5 overrides the local APIC address.
pub fn parse_madt(t: &[u8]) -> Option<Madt> {
    if t.get(0..4)? != b"APIC" { return None; }
    let len = table_len(t)?.min(t.len());
    if len < 44 || !checksum_ok(&t[..len]) { return None; }
    let mut m = Madt { lapic_addr: u32_at(t, 36)? as u64, apic_ids: Vec::new() };
    let mut i = 44;
    while i + 2 <= len {
        let ty = t[i];
        let l = t[i + 1] as usize;
        if l < 2 || i + l > len { break; }
        match ty {
            0 if l >= 8 => {
                let flags = u32_at(t, i + 4)?;
                if flags & 1 == 1 { m.apic_ids.push(t[i + 3]); }
            }
            5 if l >= 12 => m.lapic_addr = u64_at(t, i + 4)?,
            _ => {}
        }
        i += l;
    }
    Some(m)
}

// ── Placement ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sched {
    /// Decided up front: job i runs on core i mod N, whatever the load.
    Static,
    /// Each core takes the next waiting job when it becomes free.
    Dynamic,
}

impl Sched {
    pub fn name(&self) -> &'static str {
        match self { Sched::Static => "static", Sched::Dynamic => "dynamic" }
    }
    pub fn parse(s: &str) -> Option<Sched> {
        match s { "static" | "rr" => Some(Sched::Static), "dynamic" | "dyn" => Some(Sched::Dynamic), _ => None }
    }
}

/// Static plan: per core, the job indexes it will run, in order.
pub fn static_plan(jobs: usize, cores: usize) -> Vec<Vec<usize>> {
    let cores = cores.max(1);
    let mut v: Vec<Vec<usize>> = (0..cores).map(|_| Vec::new()).collect();
    for j in 0..jobs { v[j % cores].push(j); }
    v
}

/// Busy share of a window, in percent (0..=100).
pub fn busy_pct(busy: u64, window: u64) -> u32 {
    if window == 0 { 0 } else { ((busy.min(window) * 100) / window) as u32 }
}

/// Cycles to burn after `worked` cycles so that a core running at `hog` percent
/// for another tenant spends that share of its time on the hog.
pub fn hog_cycles(worked: u64, hog: u32) -> u64 {
    let hog = hog.min(95) as u64;
    if hog == 0 { 0 } else { worked * hog / (100 - hog) }
}
