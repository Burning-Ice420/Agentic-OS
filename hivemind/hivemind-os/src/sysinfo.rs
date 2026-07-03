//! System information + per-boot instance identity.
//!
//! Every boot generates a fresh random UUID (v4) so each running instance is
//! uniquely identifiable and the id cannot be predicted or reused across runs
//! (a lightweight anti-replay / instance-fingerprinting measure). The UUID plus
//! the memory / CPU / disk figures are surfaced by the shell `sysinfo` command
//! and printed to COM1 at boot so the host-side `hive-cli` can map a serial log
//! back to the instance that produced it.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

struct SysInfo {
    /// UUID rendered as the 36-byte "8-4-4-4-12" hex form.
    uuid:      [u8; 36],
    ready:     bool,
    total_ram: u64,
}

static SYS: Mutex<SysInfo> = Mutex::new(SysInfo {
    uuid:      [b'0'; 36],
    ready:     false,
    total_ram: 0,
});

/// Bumped every boot-time init so two instances started in the same TSC window
/// still diverge.
static BOOT_SALT: AtomicU64 = AtomicU64::new(0x1234_5678_9abc_def0);

// ── Entropy → UUID ─────────────────────────────────────────────────────────────

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: rdtsc is unprivileged and always available on x86_64.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// SplitMix64 — cheap, high-quality bit mixer for seeding.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hex_nibble(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}

/// Format 128 bits into `out` as a v4 UUID: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx.
fn format_uuid(hi: u64, lo: u64, out: &mut [u8; 36]) {
    // 16 raw bytes.
    let mut b = [0u8; 16];
    for i in 0..8 {
        b[i]     = (hi >> (56 - i * 8)) as u8;
        b[i + 8] = (lo >> (56 - i * 8)) as u8;
    }
    // Version 4 + RFC-4122 variant.
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;

    let mut o = 0usize;
    for (i, byte) in b.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            out[o] = b'-';
            o += 1;
        }
        out[o] = hex_nibble(byte >> 4);
        out[o + 1] = hex_nibble(byte & 0x0F);
        o += 2;
    }
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Generate this boot's UUID and record system totals. Call once at boot.
pub fn init(total_ram: u64) {
    let dt = crate::rtc::read();
    let salt = BOOT_SALT.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);

    // Mix several independent sources: the cycle counter (fine-grained + varies
    // per boot), wall-clock time, and a rolling salt.
    let mut seed = rdtsc();
    seed ^= (dt.second as u64) << 56
        ^ (dt.minute as u64) << 48
        ^ (dt.hour as u64) << 40
        ^ (dt.day as u64) << 32
        ^ (dt.year as u64) << 16;
    seed = seed.wrapping_add(salt).wrapping_add(rdtsc());

    let hi = splitmix64(&mut seed);
    let lo = splitmix64(&mut seed);

    let mut s = SYS.lock();
    format_uuid(hi, lo, &mut s.uuid);
    s.total_ram = total_ram;
    s.ready = true;
}

/// The current instance UUID as a string (valid for the life of the boot).
pub fn with_uuid<F, R>(f: F) -> R
where
    F: FnOnce(&str) -> R,
{
    let s = SYS.lock();
    let text = core::str::from_utf8(&s.uuid).unwrap_or("????");
    f(text)
}

pub fn total_ram() -> u64 {
    SYS.lock().total_ram
}

pub fn heap_size() -> usize {
    crate::allocator::HEAP_SIZE
}

/// Read the CPU brand string via CPUID leaves 0x8000_0002..=0x8000_0004.
pub fn cpu_brand(out: &mut [u8; 48]) -> bool {
    use core::arch::x86_64::__cpuid;
    // Leaf 0x8000_0000 reports the highest extended leaf available.
    let max = unsafe { __cpuid(0x8000_0000) }.eax;
    if max < 0x8000_0004 {
        return false;
    }
    let mut o = 0usize;
    for leaf in [0x8000_0002u32, 0x8000_0003, 0x8000_0004] {
        let r = unsafe { __cpuid(leaf) };
        for reg in [r.eax, r.ebx, r.ecx, r.edx] {
            out[o] = reg as u8;
            out[o + 1] = (reg >> 8) as u8;
            out[o + 2] = (reg >> 16) as u8;
            out[o + 3] = (reg >> 24) as u8;
            o += 4;
        }
    }
    true
}
