//! Boot animation — runs before the normal kernel banner.
//!
//! Writes directly to VGA memory at 0xb8000.
//! Uses busy-loop delays (interrupts are not yet enabled).

use core::ptr;

// VGA attribute bytes: fg | (bg << 4)
const BLACK_ON_BLACK: u8    = 0x00;
const AMBER_ON_BLACK: u8    = 0x06; // dark yellow / amber
const CYAN_ON_BLACK:  u8    = 0x03;
const WHITE_ON_BLACK: u8    = 0x07;
const GREEN_ON_BLACK: u8    = 0x02;
const BRIGHT_AMBER:   u8    = 0x0E; // bright yellow
const BRIGHT_GREEN:   u8    = 0x0A;
const BRIGHT_WHITE:   u8    = 0x0F;
const DARK_GREY:      u8    = 0x08;

const COLS: usize = 80;
const ROWS: usize = 25;
const VGA:  usize = 0xb8000;

// ── Low-level VGA helpers ─────────────────────────────────────────────────────

fn put(row: usize, col: usize, ch: u8, attr: u8) {
    if row >= ROWS || col >= COLS { return; }
    let offset = (row * COLS + col) * 2;
    unsafe {
        ptr::write_volatile((VGA + offset) as *mut u8, ch);
        ptr::write_volatile((VGA + offset + 1) as *mut u8, attr);
    }
}

fn fill_row(row: usize, ch: u8, attr: u8) {
    for c in 0..COLS { put(row, c, ch, attr); }
}

fn clear_all(attr: u8) {
    for r in 0..ROWS { fill_row(r, b' ', attr); }
}

fn print_at(row: usize, col: usize, s: &[u8], attr: u8) {
    for (i, &b) in s.iter().enumerate() {
        put(row, col + i, b, attr);
    }
}

fn print_centered(row: usize, s: &[u8], attr: u8) {
    let start = COLS.saturating_sub(s.len()) / 2;
    print_at(row, start, s, attr);
}

// ── Busy-wait delay ───────────────────────────────────────────────────────────

fn delay(ms: u32) {
    // ~1 000 000 iterations ≈ ~10ms in QEMU debug build.
    // Tune by eye; exact timing doesn't matter for an animation.
    let iters = ms as u64 * 100_000;
    for _ in 0..iters {
        unsafe { core::arch::asm!("nop"); }
    }
}

// ── Hexagonal dot pattern ─────────────────────────────────────────────────────

const HEX_PATTERN: &[&[u8]] = &[
    b"      . . . . . . . . . . .      ",
    b"    . . . . . . . . . . . . .    ",
    b"  . . . . . . . . . . . . . . .  ",
    b"    . . . . . . . . . . . . .    ",
    b"      . . . . . . . . . . .      ",
];

const HEX_LIT: &[&[u8]] = &[
    b"      # # # # # # # # # # #      ",
    b"    # # # # # # # # # # # # #    ",
    b"  # # # # # # # # # # # # # # #  ",
    b"    # # # # # # # # # # # # #    ",
    b"      # # # # # # # # # # #      ",
];

// ── Main entry ────────────────────────────────────────────────────────────────

pub fn play() {
    clear_all(BLACK_ON_BLACK);

    // ── Phase 1: fade in hex grid ─────────────────────────────────────────────
    let hex_start_row = 6usize;
    for (i, line) in HEX_PATTERN.iter().enumerate() {
        print_centered(hex_start_row + i, line, DARK_GREY);
        delay(30);
    }
    delay(200);

    // ── Phase 2: pulse hex bright ─────────────────────────────────────────────
    for _ in 0..2 {
        for (i, line) in HEX_LIT.iter().enumerate() {
            print_centered(hex_start_row + i, line, BRIGHT_AMBER);
        }
        delay(120);
        for (i, line) in HEX_PATTERN.iter().enumerate() {
            print_centered(hex_start_row + i, line, AMBER_ON_BLACK);
        }
        delay(120);
    }
    for (i, line) in HEX_LIT.iter().enumerate() {
        print_centered(hex_start_row + i, line, BRIGHT_AMBER);
    }

    // ── Phase 3: draw title ───────────────────────────────────────────────────
    delay(150);
    print_centered(13, b"H I V E M I N D   O S", BRIGHT_WHITE);
    delay(100);
    print_centered(14, b"v 0 . 1  -  M e m o r y  K e r n e l", CYAN_ON_BLACK);
    delay(100);
    print_centered(15, b"Bare-metal Rust  |  x86_64", DARK_GREY);

    // ── Phase 4: loading bar ──────────────────────────────────────────────────
    delay(200);
    let bar_row   = 18usize;
    let bar_width = 40usize;
    let bar_col   = (COLS - bar_width) / 2;

    // Border
    put(bar_row, bar_col - 1, b'[', WHITE_ON_BLACK);
    put(bar_row, bar_col + bar_width, b']', WHITE_ON_BLACK);

    let steps: [(&[u8], usize, u8); 6] = [
        (b"Initializing CPU ...",       5,  GREEN_ON_BLACK),
        (b"Mapping memory  ...",        8,  GREEN_ON_BLACK),
        (b"Loading hive kernel ...",    14, GREEN_ON_BLACK),
        (b"Starting mesh bridge ...",   20, GREEN_ON_BLACK),
        (b"Spawning agents ...",        26, GREEN_ON_BLACK),
        (b"Ready.",                     40, BRIGHT_GREEN),
    ];

    let status_row = bar_row + 2;
    let mut filled  = 0usize;

    for (msg, target, color) in &steps {
        // Overwrite status text (padded to clear previous)
        print_centered(status_row, *msg, *color);

        // Fill bar to target
        while filled < *target {
            put(bar_row, bar_col + filled, 0xDB, BRIGHT_AMBER); // solid block █
            filled += 1;
            delay(18);
        }
    }

    delay(400);

    // ── Phase 5: flash white then clear to normal boot ────────────────────────
    clear_all(WHITE_ON_BLACK);
    delay(60);
    clear_all(BLACK_ON_BLACK);
    delay(60);

    // Set the VGA attribute byte for the whole screen to green-on-black
    // (matches what vga_buffer.rs expects)
    for r in 0..ROWS {
        for c in 0..COLS {
            let off = (r * COLS + c) * 2 + 1;
            unsafe { ptr::write_volatile((VGA + off) as *mut u8, GREEN_ON_BLACK); }
        }
    }
}
