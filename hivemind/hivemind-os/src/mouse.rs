//! PS/2 mouse driver (IRQ12, auxiliary device on the 8042 controller).
//!
//! The controller delivers 3-byte movement packets. We accumulate relative
//! motion into a sub-cell position, clamp it to the 80x25 text grid, and expose
//! the current cursor cell + button/click state to the desktop UI.

use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::vga_buffer::{BUFFER_HEIGHT, BUFFER_WIDTH};

// 8042 controller ports.
const PS2_DATA:   u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD:    u16 = 0x64;

// Sub-cell resolution: `SCALE` movement counts move the cursor one text cell.
const SCALE: i32 = 6;
const FINE_W: i32 = BUFFER_WIDTH as i32 * SCALE;
const FINE_H: i32 = BUFFER_HEIGHT as i32 * SCALE;

// ── State ───────────────────────────────────────────────────────────────────

pub struct Mouse {
    pub col:     usize,
    pub row:     usize,
    pub left:    bool,
    pub right:   bool,
    /// Left-button press edge since the last `take_update`.
    pub clicked: bool,
}

struct MouseState {
    packet:    [u8; 3],
    idx:       usize,
    fine_x:    i32,
    fine_y:    i32,
    left:      bool,
    right:     bool,
    middle:    bool,
    prev_left: bool,
    clicked:   bool,
    dirty:     bool,
    enabled:   bool,
}

// SAFETY: only accessed under the Mutex; ports are plain data.
unsafe impl Send for MouseState {}

static MOUSE: Mutex<MouseState> = Mutex::new(MouseState {
    packet:    [0; 3],
    idx:       0,
    fine_x:    FINE_W / 2,
    fine_y:    FINE_H / 2,
    left:      false,
    right:     false,
    middle:    false,
    prev_left: false,
    clicked:   false,
    dirty:     false,
    enabled:   false,
});

// ── Low-level 8042 helpers ──────────────────────────────────────────────────

fn wait_write() {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..100_000u32 {
        if unsafe { status.read() } & 0x02 == 0 {
            return;
        }
    }
}

fn wait_read() -> bool {
    let mut status: Port<u8> = Port::new(PS2_STATUS);
    for _ in 0..100_000u32 {
        if unsafe { status.read() } & 0x01 != 0 {
            return true;
        }
    }
    false
}

fn write_cmd(cmd: u8) {
    wait_write();
    let mut port: Port<u8> = Port::new(PS2_CMD);
    unsafe { port.write(cmd) };
}

fn write_data(data: u8) {
    wait_write();
    let mut port: Port<u8> = Port::new(PS2_DATA);
    unsafe { port.write(data) };
}

fn read_data() -> u8 {
    wait_read();
    let mut port: Port<u8> = Port::new(PS2_DATA);
    unsafe { port.read() }
}

/// Send a command byte to the mouse itself (prefixed with 0xD4). Returns the ACK.
fn mouse_cmd(cmd: u8) -> u8 {
    write_cmd(0xD4);
    write_data(cmd);
    read_data()
}

// ── Init ─────────────────────────────────────────────────────────────────────

/// Program the controller + mouse and unmask IRQ12. Call once, before enabling
/// CPU interrupts.
pub fn init() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        // Enable the auxiliary (mouse) device.
        write_cmd(0xA8);

        // Enable IRQ12 in the controller command byte, keep the mouse clock on.
        write_cmd(0x20);
        let mut config = read_data();
        config |= 0x02;  // enable IRQ12 (second-port interrupt)
        config &= !0x20; // clear "disable mouse clock"
        write_cmd(0x60);
        write_data(config);

        // Mouse: restore defaults then enable streaming data reports.
        mouse_cmd(0xF6);
        mouse_cmd(0xF4);

        // Drain any leftover response bytes (e.g. the 0xFA ACK from 0xF4) so the
        // first real 3-byte movement packet isn't desynchronised by a stray byte.
        let mut status: Port<u8> = Port::new(PS2_STATUS);
        let mut data: Port<u8> = Port::new(PS2_DATA);
        for _ in 0..16 {
            if unsafe { status.read() } & 0x01 == 0 {
                break;
            }
            let _ = unsafe { data.read() };
        }

        MOUSE.lock().enabled = true;
    });

    // Unmask IRQ2 (cascade) on the master and IRQ12 on the slave PIC.
    interrupts::without_interrupts(|| {
        let mut pics = crate::interrupts::PICS.lock();
        let [m1, m2] = unsafe { pics.read_masks() };
        unsafe { pics.write_masks(m1 & !(1 << 2), m2 & !(1 << 4)) };
    });
}

// ── IRQ path ─────────────────────────────────────────────────────────────────

/// Feed one byte from the IRQ12 handler.
pub fn push_packet_byte(byte: u8) {
    let mut m = MOUSE.lock();
    if !m.enabled {
        return;
    }

    // The first packet byte always has bit 3 set. Also reject controller
    // response bytes (ACK 0xFA, self-test 0xAA, resend 0xFE) so a stray reply
    // can't masquerade as a packet header and desync the stream.
    if m.idx == 0 && (byte & 0x08 == 0 || byte == 0xFA || byte == 0xAA || byte == 0xFE) {
        return;
    }

    let idx = m.idx;
    m.packet[idx] = byte;
    m.idx += 1;
    if m.idx < 3 {
        return;
    }
    m.idx = 0;

    let flags = m.packet[0];
    // Discard packets that overflowed to avoid cursor jumps.
    let (dx, dy) = if flags & 0xC0 != 0 {
        (0, 0)
    } else {
        (m.packet[1] as i8 as i32, m.packet[2] as i8 as i32)
    };

    m.fine_x = (m.fine_x + dx).clamp(0, FINE_W - 1);
    m.fine_y = (m.fine_y - dy).clamp(0, FINE_H - 1); // screen Y is inverted

    m.left   = flags & 0x01 != 0;
    m.right  = flags & 0x02 != 0;
    m.middle = flags & 0x04 != 0;

    if m.left && !m.prev_left {
        m.clicked = true;
    }
    m.prev_left = m.left;
    m.dirty = true;
}

// ── Consumer API ───────────────────────────────────────────────────────────────

/// Return the current mouse state if it changed since the last call, clearing
/// the change + click flags. Returns `None` when nothing new happened.
pub fn take_update() -> Option<Mouse> {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut m = MOUSE.lock();
        if !m.dirty {
            return None;
        }
        m.dirty = false;
        let clicked = m.clicked;
        m.clicked = false;
        Some(Mouse {
            col:   (m.fine_x / SCALE) as usize,
            row:   (m.fine_y / SCALE) as usize,
            left:  m.left,
            right: m.right,
            clicked,
        })
    })
}

/// Current cursor cell without consuming events.
pub fn position() -> (usize, usize) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let m = MOUSE.lock();
        ((m.fine_x / SCALE) as usize, (m.fine_y / SCALE) as usize)
    })
}
