//! Mesh serial networking — COM2 (I/O base 0x2F8)
//!
//! Two HiveMind VMs communicate over a QEMU virtual serial link.
//! QEMU launch example (run-os.ps1 does this automatically):
//!   VM1:  ... -serial file:vm1.log  -serial tcp::4444,server,nowait
//!   VM2:  ... -serial file:vm2.log  -serial tcp:127.0.0.1:4444
//!
//! Wire protocol — one ASCII line per message:
//!   HMSG|<memory_name>|<key>|<value>\n
//!
//! Receiving VMs create or update the named Memory node + blob automatically.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as FmtWrite;
use spin::Mutex;
use x86_64::instructions::port::Port;

// ── Raw UART register block ───────────────────────────────────────────────────

struct Uart {
    data: Port<u8>, // RBR (read) / THR (write)
    ier:  Port<u8>, // Interrupt Enable
    fcr:  Port<u8>, // FIFO Control (write) / IIR (read)
    lcr:  Port<u8>, // Line Control
    mcr:  Port<u8>, // Modem Control
    lsr:  Port<u8>, // Line Status
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
            self.ier.write(0x00u8); // disable all interrupts
            self.lcr.write(0x80u8); // set DLAB to access baud divisor
            self.data.write(0x01u8);// divisor LSB → 115200 baud
            self.ier.write(0x00u8); // divisor MSB
            self.lcr.write(0x03u8); // 8N1, clear DLAB
            self.fcr.write(0xC7u8); // enable + clear FIFO, 14-byte threshold
            self.mcr.write(0x0Bu8); // RTS + DTR + OUT2
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
        // Spin with a safety cap — in QEMU virtual serial this completes instantly.
        for _ in 0..200_000u32 {
            if self.tx_empty() { break; }
        }
        unsafe { self.data.write(b); }
    }
}

// ── Line accumulator ─────────────────────────────────────────────────────────

const LINE_CAP: usize = 512;

struct LineAccum {
    buf: [u8; LINE_CAP],
    len: usize,
}

impl LineAccum {
    const fn new() -> Self {
        LineAccum { buf: [0u8; LINE_CAP], len: 0 }
    }

    /// Push one byte. Returns the completed line (without newline) when '\n'
    /// is received, or None if more bytes are needed.
    fn push(&mut self, b: u8) -> Option<&[u8]> {
        if b == b'\n' || b == b'\r' {
            if self.len > 0 {
                let n    = self.len;
                self.len = 0;
                return Some(&self.buf[..n]);
            }
        } else if self.len < LINE_CAP - 1 {
            self.buf[self.len] = b;
            self.len           += 1;
        }
        None
    }
}

// ── Message ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MeshMessage {
    pub memory_name: String,
    pub key:         String,
    pub value:       String,
}

fn parse_line(bytes: &[u8]) -> Option<MeshMessage> {
    let s   = core::str::from_utf8(bytes).ok()?;
    let s   = s.strip_prefix("HMSG|")?;
    let mut it = s.splitn(3, '|');
    Some(MeshMessage {
        memory_name: it.next()?.to_string(),
        key:         it.next()?.to_string(),
        value:       it.next()?.to_string(),
    })
}

// ── Global state ──────────────────────────────────────────────────────────────

struct MeshState {
    uart:     Uart,
    accum:    LineAccum,
    tx_count: u64,
    rx_count: u64,
}

// SAFETY: all fields are plain data or port wrappers; access is serialized by the Mutex.
unsafe impl Send for MeshState {}

static MESH: Mutex<MeshState> = Mutex::new(MeshState {
    uart:     Uart::new(0x2F8),
    accum:    LineAccum::new(),
    tx_count: 0,
    rx_count: 0,
});

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize COM2. Safe to call even if no second -serial is configured in QEMU;
/// the port simply stays silent.
pub fn init() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| MESH.lock().uart.init());
}

/// Wake the CPU from `hlt` when mesh bytes arrive: enable the COM2 receive
/// interrupt and unmask IRQ3 (the catch-all IRQ handler acknowledges it; the
/// main loop then drains the FIFO). Without this an idle node only notices a
/// message on the next timer tick, 55 ms later, 16 bytes at a time.
pub fn enable_rx_wakeup() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        unsafe { MESH.lock().uart.ier.write(0x01u8); }
        let mut pics = crate::interrupts::PICS.lock();
        let [m1, m2] = unsafe { pics.read_masks() };
        unsafe { pics.write_masks(m1 & !(1 << 3), m2); }
    });
}

/// Send a blob update to all connected peer VMs.
/// Format on wire: `HMSG|<memory_name>|<key>|<value>\n`
pub fn send_blob(memory_name: &str, key: &str, value: &str) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut m   = MESH.lock();
        let mut msg = String::new();
        let _       = write!(msg, "HMSG|{}|{}|{}\n", memory_name, key, value);
        for b in msg.bytes() {
            m.uart.write_byte(b);
        }
        m.tx_count += 1;
    });
}

/// Poll COM2 for incoming bytes, parse complete lines, and apply any received
/// blob updates to the local hive. Non-blocking — call from the main loop.
pub fn poll_and_apply() {
    use x86_64::instructions::interrupts;
    use crate::hive;
    use crate::hive::blob::BlobValue;

    // Drain the UART FIFO while holding the MESH lock.
    let msgs: Vec<MeshMessage> = interrupts::without_interrupts(|| {
        let mut m   = MESH.lock();
        let mut out = Vec::new();
        // Limit reads so we don't stall interrupts indefinitely
        for _ in 0..256 {
            match m.uart.read_byte() {
                Some(b) => {
                    if let Some(line) = m.accum.push(b) {
                        if let Some(msg) = parse_line(line) {
                            m.rx_count += 1;
                            out.push(msg);
                        }
                    }
                }
                None => break,
            }
        }
        out
    });

    // Apply messages outside the MESH lock so we can safely lock HIVE.
    if !msgs.is_empty() {
        crate::disk::persist::mark_dirty();
    }
    for msg in msgs {
        // Cluster traffic (jobs, results, load) goes to the scheduler, not the graph.
        if crate::cluster::on_mesh(&msg.memory_name, &msg.key, &msg.value) {
            continue;
        }
        let val = BlobValue::parse(&msg.value);
        hive::with_hive(|h| {
            use crate::prov::Actor;
            // The wire carries no sender identity, so the writer is `peer` and the
            // parent is unknown on this side (the collector may infer a link from
            // the sender's `mesh_send` event, labelled as inferred).
            let existing = h.find_by_name(&msg.memory_name);
            if existing.is_none() {
                // Gate creation too: a peer without write access must not be able
                // to plant new nodes either.
                if h.authorize(Actor::Peer, crate::cap_core::OP_WRITE, 0, &msg.memory_name, &msg.key, None).is_err() {
                    return;
                }
            }
            let id = existing.unwrap_or_else(|| h.create_memory_as(Actor::Peer, &msg.memory_name, None));
            let _ = h.write_blob_as(Actor::Peer, id, &msg.key, val, None);
        });
    }
}

/// Broadcast a blob update and record it as a `mesh_send` provenance event whose
/// parent is the local write that produced it.
pub fn send_blob_tracked(memory_name: &str, key: &str, value: &str,
                         actor: crate::prov::Actor, mem_id: u64, parent: Option<u64>) {
    crate::prov::emit(crate::prov::Kind::MeshSend, actor, mem_id, memory_name, key, parent,
                      crate::prov::value_attrs(value));
    send_blob(memory_name, key, value);
}

/// Return (tx_count, rx_count).
pub fn stats() -> (u64, u64) {
    let m = MESH.lock();
    (m.tx_count, m.rx_count)
}
