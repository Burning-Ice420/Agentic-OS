use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::gdt;
use crate::println;
use crate::serial_println;

// ── PIC offsets ───────────────────────────────────────────────────────────────

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer    = PIC_1_OFFSET,
    Keyboard = PIC_1_OFFSET + 1,
    // IRQ12 = PS/2 mouse (auxiliary device), on the slave PIC.
    Mouse    = PIC_1_OFFSET + 12,
}

impl InterruptIndex {
    fn as_u8(self)    -> u8    { self as u8 }
    fn as_usize(self) -> usize { self as u8 as usize }
}

// ── System tick counter (used by hive for timestamps) ─────────────────────────
//
// Lock-free atomic: the timer handler bumps it every tick, so a plain `Mutex`
// here would deadlock the moment any other code held the lock when the timer
// fired. `Relaxed` is fine — we only need a monotonically increasing counter.

pub static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn current_tick() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

// ── IDT ───────────────────────────────────────────────────────────────────────

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);

        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_handler);

        // Default handler for every PIC IRQ vector (32..=47) so an unexpected or
        // spurious IRQ (e.g. a stale ATA IRQ14 or spurious IRQ7/15) is EOI'd
        // gracefully instead of hitting an empty slot and double-faulting.
        for vec in PIC_1_OFFSET as usize..(PIC_1_OFFSET as usize + 16) {
            idt[vec].set_handler_fn(spurious_irq_handler);
        }

        // Real handlers for the IRQs we actually use.
        idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Mouse.as_usize()].set_handler_fn(mouse_interrupt_handler);

        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

// ── Exception handlers ────────────────────────────────────────────────────────

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("[EXCEPTION] BREAKPOINT at {:#?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    // DIAGNOSTIC: write the faulting RIP to VGA row 24 without taking any locks.
    let cr2 = x86_64::registers::control::Cr2::read_raw();
    vga_report(23, b"DF CR2=", cr2);
    vga_report(24, b"DF RIP=", stack_frame.instruction_pointer.as_u64());
    loop {
        x86_64::instructions::hlt();
    }
}

/// Write a short label + a 64-bit hex value directly to VGA (no locks).
fn vga_report(row: usize, label: &[u8], value: u64) {
    let vga = 0xb8000 as *mut u8;
    let hexd = b"0123456789abcdef";
    let base = row * 80 * 2;
    unsafe {
        for (i, &c) in label.iter().enumerate() {
            core::ptr::write_volatile(vga.add(base + i * 2), c);
            core::ptr::write_volatile(vga.add(base + i * 2 + 1), 0x4f);
        }
        for i in 0..16 {
            let nib = ((value >> ((15 - i) * 4)) & 0xf) as usize;
            let off = base + (label.len() + i) * 2;
            core::ptr::write_volatile(vga.add(off), hexd[nib]);
            core::ptr::write_volatile(vga.add(off + 1), 0x4f);
        }
    }
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    vga_report(23, b"GP err=", error_code);
    vga_report(24, b"GP RIP=", stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    vga_report(24, b"UD RIP=", stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    vga_report(22, b"TS err=", error_code);
    vga_report(24, b"TS RIP=", stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    vga_report(22, b"NP err=", error_code);
    vga_report(24, b"NP RIP=", stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn stack_segment_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    vga_report(22, b"SS err=", error_code);
    vga_report(24, b"SS RIP=", stack_frame.instruction_pointer.as_u64());
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    println!("[PAGE FAULT] at {:?} — error: {:?}", Cr2::read(), error_code);
    println!("{:#?}", stack_frame);
    loop {
        x86_64::instructions::hlt();
    }
}

// ── Hardware interrupt handlers ───────────────────────────────────────────────

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use lazy_static::lazy_static;
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
    use spin::Mutex;
    use x86_64::instructions::port::Port;

    lazy_static! {
        static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
            Mutex::new(Keyboard::new(
                layouts::Us104Key,
                ScancodeSet1,
                HandleControl::Ignore,
            ));
    }

    let mut keyboard = KEYBOARD.lock();
    let mut port: Port<u8> = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => crate::shell::push_key(character),
                DecodedKey::RawKey(code) => {
                    if let Some(c) = crate::shell::rawkey_to_char(code) {
                        crate::shell::push_key(c);
                    }
                }
            }
        }
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

/// Catch-all for unused PIC IRQ vectors. Sends a non-specific EOI to both PICs
/// so the interrupt controller doesn't wedge, then returns.
extern "x86-interrupt" fn spurious_irq_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut slave:  Port<u8> = Port::new(0xA0);
        let mut master: Port<u8> = Port::new(0x20);
        slave.write(0x20u8);  // EOI to slave (harmless if it wasn't the source)
        master.write(0x20u8); // EOI to master
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    let mut data: Port<u8> = Port::new(0x60);
    let byte: u8 = unsafe { data.read() };
    crate::mouse::push_packet_byte(byte);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}
