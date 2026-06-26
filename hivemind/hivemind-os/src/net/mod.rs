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
    fn data_ready(&mut self) -> bool { unsafe { self.lsr.read() & 0x01 != 0 } }

    #[inline]
    fn tx_empty(&mut self) -> bool   { unsafe { self.lsr.read() & 0x20 != 0 } }

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
        while let Some(b) = m.uart.read_byte() {
            if let Some(line) = m.accum.push(b) {
                if let Some(msg) = parse_line(line) {
                    m.rx_count += 1;
                    out.push(msg);
                }
            }
        }
        out
    });

    // Apply messages outside the MESH lock so we can safely lock HIVE.
    for msg in msgs {
        let val = BlobValue::parse(&msg.value);
        hive::with_hive(|h| {
            // Reuse an existing memory node if names match, otherwise create one.
            let id = h.memories
                .iter()
                .find(|(_, n)| n.name == msg.memory_name)
                .map(|(&id, _)| id)
                .unwrap_or_else(|| h.create_memory(&msg.memory_name, None));
            h.write_blob(id, &msg.key, val);
        });
    }
}

/// Return (tx_count, rx_count).
pub fn stats() -> (u64, u64) {
    let m = MESH.lock();
    (m.tx_count, m.rx_count)
}
