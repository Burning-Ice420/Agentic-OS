//! AI-accelerator driver — COM3 (I/O base 0x3E8).
//!
//! Two-tier cognition. The in-kernel rule engine (`agent`) is the *fast reflex*
//! tier: instant, deterministic, fully in the kernel. This module is the *slow
//! deliberation* tier: the kernel offloads heavy reasoning to an external LLM the
//! same way it would offload compute to a GPU/NPU — over a device. The agent is
//! still a kernel entity; only the inference is offloaded.
//!
//! A host-side bridge (`hive-llm-bridge.py`) runs a small local model and speaks
//! this line protocol over the serial link:
//!   OS  -> bridge:  LLMREQ|<memory_name>|<prompt>|<key=val,key=val,...>\n
//!   bridge -> OS:   LLMRSP|<memory_name>|<key>|<value>\n
//! The returned action is applied to the hive, where the reflex tier can react
//! to it — so deliberation feeds reflex.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as FmtWrite;
use spin::Mutex;
use x86_64::instructions::port::Port;

// ── Raw UART register block (COM3 @ 0x3E8) ────────────────────────────────────

struct Uart {
    data: Port<u8>,
    ier:  Port<u8>,
    fcr:  Port<u8>,
    lcr:  Port<u8>,
    mcr:  Port<u8>,
    lsr:  Port<u8>,
}

impl Uart {
    const fn new(base: u16) -> Self {
        Uart {
            data: Port::new(base),
            ier:  Port::new(base + 1),
            fcr:  Port::new(base + 2),
            lcr:  Port::new(base + 3),
            mcr:  Port::new(base + 4),
            lsr:  Port::new(base + 5),
        }
    }

    fn init(&mut self) {
        unsafe {
            self.ier.write(0x00u8);
            self.lcr.write(0x80u8); // DLAB
            self.data.write(0x01u8);// 115200
            self.ier.write(0x00u8);
            self.lcr.write(0x03u8); // 8N1
            self.fcr.write(0xC7u8);
            self.mcr.write(0x0Bu8);
        }
    }

    #[inline]
    fn data_ready(&mut self) -> bool {
        let lsr = unsafe { self.lsr.read() };
        (lsr & 0x01 != 0) && (lsr != 0xFF)
    }

    #[inline]
    fn tx_empty(&mut self) -> bool {
        let lsr = unsafe { self.lsr.read() };
        (lsr & 0x20 != 0) && (lsr != 0xFF)
    }

    fn read_byte(&mut self) -> Option<u8> {
        if self.data_ready() { Some(unsafe { self.data.read() }) } else { None }
    }

    fn write_byte(&mut self, b: u8) {
        for _ in 0..200_000u32 {
            if self.tx_empty() { break; }
        }
        unsafe { self.data.write(b); }
    }
}

// ── Line accumulator ──────────────────────────────────────────────────────────

const LINE_CAP: usize = 1024;

struct LineAccum {
    buf: [u8; LINE_CAP],
    len: usize,
}

impl LineAccum {
    const fn new() -> Self {
        LineAccum { buf: [0u8; LINE_CAP], len: 0 }
    }

    fn push(&mut self, b: u8) -> Option<&[u8]> {
        if b == b'\n' || b == b'\r' {
            if self.len > 0 {
                let n = self.len;
                self.len = 0;
                return Some(&self.buf[..n]);
            }
        } else if self.len < LINE_CAP - 1 {
            self.buf[self.len] = b;
            self.len += 1;
        }
        None
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

struct LlmState {
    uart:  Uart,
    accum: LineAccum,
    tx:    u64,
    rx:    u64,
}

// SAFETY: plain data + port wrappers, serialized by the Mutex.
unsafe impl Send for LlmState {}

static LLM: Mutex<LlmState> = Mutex::new(LlmState {
    uart:  Uart::new(0x3E8),
    accum: LineAccum::new(),
    tx:    0,
    rx:    0,
});

// ── Public API ────────────────────────────────────────────────────────────────

pub fn init() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| LLM.lock().uart.init());
}

/// Offload a reasoning request to the AI accelerator. Non-blocking; the response
/// is applied later by `poll_and_apply`.
pub fn request(memory: &str, prompt: &str, context: &str) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut m = LLM.lock();
        let mut line = String::new();
        let _ = write!(line, "LLMREQ|{}|{}|{}\n", memory, prompt, context);
        for b in line.bytes() {
            m.uart.write_byte(b);
        }
        m.tx += 1;
    });
}

/// Poll the accelerator for completed `LLMRSP` actions and apply them to the
/// hive. Non-blocking — call from the main loop. Returns applied action count.
pub fn poll_and_apply() -> usize {
    use x86_64::instructions::interrupts;
    use crate::hive;
    use crate::hive::blob::BlobValue;

    // Drain bytes and collect completed responses while holding the LLM lock.
    let responses: Vec<(String, String, String)> = interrupts::without_interrupts(|| {
        let mut m = LLM.lock();
        let mut out = Vec::new();
        for _ in 0..1024 {
            match m.uart.read_byte() {
                Some(b) => {
                    if let Some(line) = m.accum.push(b) {
                        if let Some(parsed) = parse_rsp(line) {
                            m.rx += 1;
                            out.push(parsed);
                        }
                    }
                }
                None => break,
            }
        }
        out
    });

    if responses.is_empty() {
        return 0;
    }

    // Apply outside the LLM lock so we can lock HIVE safely.
    for (memory, key, value) in &responses {
        let val = BlobValue::parse(value);
        hive::with_hive(|h| {
            let id = h
                .memories
                .iter()
                .find(|(_, n)| &n.name == memory)
                .map(|(&id, _)| id)
                .unwrap_or_else(|| h.create_memory(memory, None));
            h.write_blob(id, key, val);
        });
        crate::println!("  [AI] {} <- {} = {}", memory, key, value);
    }
    crate::disk::persist::mark_dirty();
    responses.len()
}

pub fn stats() -> (u64, u64) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let m = LLM.lock();
        (m.tx, m.rx)
    })
}

fn parse_rsp(bytes: &[u8]) -> Option<(String, String, String)> {
    let s = core::str::from_utf8(bytes).ok()?;
    let s = s.strip_prefix("LLMRSP|")?;
    let mut it = s.splitn(3, '|');
    let memory = it.next()?.to_string();
    let key = it.next()?.to_string();
    let value = it.next()?.to_string();
    Some((memory, key, value))
}
