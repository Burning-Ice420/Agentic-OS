use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;

// ── Colours ──────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black       = 0,
    Blue        = 1,
    Green       = 2,
    Cyan        = 3,
    Red         = 4,
    Magenta     = 5,
    Brown       = 6,
    LightGray   = 7,
    DarkGray    = 8,
    LightBlue   = 9,
    LightGreen  = 10,
    LightCyan   = 11,
    LightRed    = 12,
    Pink        = 13,
    Yellow      = 14,
    White       = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    const fn new(fg: Color, bg: Color) -> ColorCode {
        ColorCode((bg as u8) << 4 | (fg as u8))
    }
}

// ── Screen character ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code:      ColorCode,
}

pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH:  usize = 80;

/// Number of logical text rows retained for scrollback (including on-screen).
pub const SCROLLBACK: usize = 200;

/// Rows moved per PageUp / PageDown.
const PAGE_STEP: usize = BUFFER_HEIGHT - 3;

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

// Scrollback storage lives in .bss (zeroed) so the huge array never touches the
// boot stack. A zero byte is rendered as a space.
static mut RING: [[ScreenChar; BUFFER_WIDTH]; SCROLLBACK] =
    [[ScreenChar { ascii_character: 0, color_code: ColorCode(0) }; BUFFER_WIDTH]; SCROLLBACK];

// ── Writer ────────────────────────────────────────────────────────────────────

pub struct Writer {
    column_position: usize,
    color_code:      ColorCode,
    /// Ring index of the current (live bottom) logical row.
    cur:             usize,
    /// Number of valid logical rows in the ring (1..=SCROLLBACK).
    filled:          usize,
    /// How many rows we are scrolled up from the live bottom (0 = live).
    view:            usize,
    ring:            &'static mut [[ScreenChar; BUFFER_WIDTH]; SCROLLBACK],
    buffer:          &'static mut Buffer,
}

impl Writer {
    pub fn set_color(&mut self, fg: Color, bg: Color) {
        self.color_code = ColorCode::new(fg, bg);
    }

    fn blank(&self) -> ScreenChar {
        ScreenChar { ascii_character: b' ', color_code: self.color_code }
    }

    /// If the viewport is scrolled up into history, snap back to the live bottom
    /// so new output is always visible (standard terminal behaviour).
    fn ensure_live(&mut self) {
        if self.view != 0 {
            self.view = 0;
            self.render();
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        self.ensure_live();
        match byte {
            b'\n' => self.new_line(),
            b'\x08' => {
                if self.column_position > 0 {
                    self.column_position -= 1;
                    let col   = self.column_position;
                    let blank = self.blank();
                    self.ring[self.cur][col] = blank;
                    self.buffer.chars[BUFFER_HEIGHT - 1][col].write(blank);
                }
            }
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }
                let col = self.column_position;
                let sc  = ScreenChar { ascii_character: byte, color_code: self.color_code };
                self.ring[self.cur][col] = sc;
                self.buffer.chars[BUFFER_HEIGHT - 1][col].write(sc);
                self.column_position += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' | b'\x08' => self.write_byte(byte),
                _ => self.write_byte(0xfe), // replacement char for non-ASCII
            }
        }
    }

    /// Advance to a fresh logical row and scroll the hardware screen up by one.
    fn new_line(&mut self) {
        self.cur = (self.cur + 1) % SCROLLBACK;
        let blank = self.blank();
        for c in 0..BUFFER_WIDTH {
            self.ring[self.cur][c] = blank;
        }
        if self.filled < SCROLLBACK {
            self.filled += 1;
        }
        self.column_position = 0;

        // Live path: shift the visible hardware buffer up one row (cheap).
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let ch = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(ch);
            }
        }
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[BUFFER_HEIGHT - 1][col].write(blank);
        }
    }

    /// Repaint the whole hardware screen from the ring at the current `view`.
    fn render(&mut self) {
        for r in 0..BUFFER_HEIGHT {
            // Logical rows above the live bottom (0 = live bottom row).
            let logical = self.view + (BUFFER_HEIGHT - 1 - r);
            if logical < self.filled {
                let idx = (self.cur + SCROLLBACK - logical) % SCROLLBACK;
                for c in 0..BUFFER_WIDTH {
                    let mut sc = self.ring[idx][c];
                    if sc.ascii_character == 0 {
                        sc.ascii_character = b' ';
                    }
                    self.buffer.chars[r][c].write(sc);
                }
            } else {
                let blank = ScreenChar { ascii_character: b' ', color_code: ColorCode(0) };
                for c in 0..BUFFER_WIDTH {
                    self.buffer.chars[r][c].write(blank);
                }
            }
        }
    }

    fn max_view(&self) -> usize {
        self.filled.saturating_sub(BUFFER_HEIGHT)
    }

    fn scroll_by(&mut self, up: i32) {
        let maxv = self.max_view() as i32;
        let mut v = self.view as i32 + up;
        if v < 0 { v = 0; }
        if v > maxv { v = maxv; }
        let nv = v as usize;
        if nv != self.view {
            self.view = nv;
            self.render();
        }
    }

    /// Reset scrollback and clear the screen.
    fn clear(&mut self) {
        self.column_position = 0;
        self.cur    = 0;
        self.filled = 1;
        self.view   = 0;
        let blank = self.blank();
        for c in 0..BUFFER_WIDTH {
            self.ring[0][c] = blank;
        }
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                self.buffer.chars[row][col].write(blank);
            }
        }
    }

    /// Clear only the visible hardware buffer (used by the full-screen desktop
    /// UI, which paints absolute cells and does not use scrollback).
    fn clear_hw(&mut self) {
        let blank = self.blank();
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                self.buffer.chars[row][col].write(blank);
            }
        }
    }

    fn write_at_byte(&mut self, row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return;
        }
        let byte = match byte {
            0x20..=0x7e => byte,
            _ => b' ',
        };
        self.buffer.chars[row][col].write(ScreenChar {
            ascii_character: byte,
            color_code: ColorCode::new(fg, bg),
        });
    }

    /// Write any CP437 code point verbatim (box-drawing, shades, cursor glyphs).
    fn write_raw_byte(&mut self, row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return;
        }
        self.buffer.chars[row][col].write(ScreenChar {
            ascii_character: byte,
            color_code: ColorCode::new(fg, bg),
        });
    }

    fn read_cell(&self, row: usize, col: usize) -> (u8, u8) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return (b' ', 0);
        }
        let sc = self.buffer.chars[row][col].read();
        (sc.ascii_character, sc.color_code.0)
    }

    fn write_cell_raw(&mut self, row: usize, col: usize, ch: u8, color: u8) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return;
        }
        self.buffer.chars[row][col].write(ScreenChar {
            ascii_character: ch,
            color_code: ColorCode(color),
        });
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// ── Global writer ─────────────────────────────────────────────────────────────

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::LightGreen, Color::Black),
        cur:    0,
        filled: 1,
        view:   0,
        ring:   unsafe { &mut *core::ptr::addr_of_mut!(RING) },
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    });
}

// ── Public helpers ────────────────────────────────────────────────────────────

pub fn clear_screen() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().clear());
}

pub fn set_color(fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().set_color(fg, bg);
    });
}

pub fn clear_screen_with(fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        w.set_color(fg, bg);
        w.clear_hw();
        w.column_position = 0;
    });
}

pub fn put_char_at(row: usize, col: usize, ch: char, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().write_at_byte(row, col, ch as u8, fg, bg);
    });
}

pub fn write_at(row: usize, col: usize, text: &str, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        for (i, byte) in text.bytes().enumerate() {
            let target_col = col + i;
            if target_col >= BUFFER_WIDTH {
                break;
            }
            w.write_at_byte(row, target_col, byte, fg, bg);
        }
    });
}

pub fn fill_rect(row: usize, col: usize, height: usize, width: usize, ch: char, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        for y in row..core::cmp::min(row + height, BUFFER_HEIGHT) {
            for x in col..core::cmp::min(col + width, BUFFER_WIDTH) {
                w.write_at_byte(y, x, ch as u8, fg, bg);
            }
        }
    });
}

/// Write a raw CP437 byte (allows box-drawing / shade / cursor glyphs).
pub fn put_raw_at(row: usize, col: usize, byte: u8, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().write_raw_byte(row, col, byte, fg, bg);
    });
}

pub fn fill_raw(row: usize, col: usize, height: usize, width: usize, byte: u8, fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        for y in row..core::cmp::min(row + height, BUFFER_HEIGHT) {
            for x in col..core::cmp::min(col + width, BUFFER_WIDTH) {
                w.write_raw_byte(y, x, byte, fg, bg);
            }
        }
    });
}

/// Read back a cell's (glyph, attribute) — used to save/restore under the mouse cursor.
pub fn read_cell(row: usize, col: usize) -> (u8, u8) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().read_cell(row, col))
}

/// Write a cell from a raw (glyph, attribute) pair.
pub fn write_cell_raw(row: usize, col: usize, ch: u8, color: u8) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().write_cell_raw(row, col, ch, color));
}

// ── Scrollback controls ────────────────────────────────────────────────────────

/// Scroll the viewport up into history by one page.
pub fn scroll_up() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(PAGE_STEP as i32));
}

/// Scroll the viewport back down toward live output by one page.
pub fn scroll_down() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(-(PAGE_STEP as i32)));
}

/// Scroll up by a single line.
pub fn scroll_line_up() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(1));
}

/// Scroll down by a single line.
pub fn scroll_line_down() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(-1));
}

/// Jump to the oldest retained line.
pub fn scroll_home() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(SCROLLBACK as i32));
}

/// Jump back to live output.
pub fn scroll_end() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().scroll_by(-(SCROLLBACK as i32)));
}

/// True when the viewport is scrolled up into history.
pub fn is_scrolled() -> bool {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| WRITER.lock().view != 0)
}

// ── Macros ────────────────────────────────────────────────────────────────────

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    ()              => ($crate::print!("\n"));
    ($($arg:tt)*)   => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().write_fmt(args).unwrap();
    });
}
