//! HiveMind text-mode desktop — a small windowing environment driven by the
//! PS/2 mouse and keyboard.
//!
//!  * Clickable launcher icons down the left edge open windows.
//!  * Windows have a draggable title bar and a close box; the focused window is
//!    highlighted and receives keyboard input.
//!  * A taskbar lists open windows; the top bar shows a live clock.
//!  * The mouse cursor is rendered by inverting the cell it sits on.
//!
//! Everything is painted with the raw CP437 helpers in `vga_buffer`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::hive;
use crate::hive::blob::BlobValue;
use crate::vga_buffer::{self, Color, BUFFER_HEIGHT, BUFFER_WIDTH};

// ── Window kinds (also indices into the geometry arrays) ──────────────────────

const TERMINAL: usize = 0;
const NOTEPAD:  usize = 1;
const HIVE:     usize = 2;
const FILES:    usize = 3;
const MESH:     usize = 4;
const HELP:     usize = 5;
const KINDS:    usize = 6;

const NAMES: [&str; KINDS] = ["Terminal", "Notepad", "Hive", "Files", "Mesh", "Help"];

// Default window geometry: (x, y, w, h).
const DEFAULT_GEOM: [(usize, usize, usize, usize); KINDS] = [
    (18, 3, 58, 18), // Terminal
    (20, 4, 50, 16), // Notepad
    (22, 3, 46, 15), // Hive
    (24, 4, 42, 15), // Files
    (26, 6, 40, 11), // Mesh
    (20, 3, 54, 19), // Help
];

// CP437 glyphs.
const SH_BG:  u8 = 0xB0; // ░ light shade (desktop background)
const D_TL: u8 = 0xC9; const D_TR: u8 = 0xBB; const D_BL: u8 = 0xC8; const D_BR: u8 = 0xBC;
const D_H:  u8 = 0xCD; const D_V:  u8 = 0xBA;
const S_TL: u8 = 0xDA; const S_TR: u8 = 0xBF; const S_BL: u8 = 0xC0; const S_BR: u8 = 0xD9;
const S_H:  u8 = 0xC4; const S_V:  u8 = 0xB3;

const LOG_CAP:  usize = 64;
const NOTE_CAP: usize = 1200;

pub fn run() {
    let mut app = DesktopApp::new();
    app.raise(HELP);
    app.render();

    loop {
        crate::net::poll_and_apply();

        let mut dirty = false;

        while let Some(c) = crate::shell::read_key() {
            if app.handle_key(c) {
                // Leaving the desktop — hand back to the shell.
                vga_buffer::clear_screen();
                vga_buffer::set_color(Color::LightGreen, Color::Black);
                crate::println!("Returned from HiveMind desktop.");
                return;
            }
            dirty = true;
        }

        if let Some(m) = crate::mouse::take_update() {
            app.handle_mouse(m);
            dirty = true;
        }

        if dirty {
            app.render();
        }

        x86_64::instructions::hlt();
    }
}

struct DesktopApp {
    open:     [bool; KINDS],
    zorder:   Vec<usize>, // last element = focused / top-most
    wx:       [usize; KINDS],
    wy:       [usize; KINDS],
    ww:       [usize; KINDS],
    wh:       [usize; KINDS],
    note:     String,
    cli:      String,
    logs:     Vec<String>,
    dragging: Option<(usize, usize, usize)>, // (kind, grab_off_x, grab_off_y)
    m_col:    usize,
    m_row:    usize,
    files_path: String,
}

impl DesktopApp {
    fn new() -> Self {
        let mut app = Self {
            open:     [false; KINDS],
            zorder:   Vec::new(),
            wx:       [0; KINDS],
            wy:       [0; KINDS],
            ww:       [0; KINDS],
            wh:       [0; KINDS],
            note:     String::new(),
            cli:      String::new(),
            logs:     Vec::new(),
            dragging: None,
            m_col:    BUFFER_WIDTH / 2,
            m_row:    BUFFER_HEIGHT / 2,
            files_path: "/".to_string(),
        };
        for k in 0..KINDS {
            let (x, y, w, h) = DEFAULT_GEOM[k];
            app.wx[k] = x; app.wy[k] = y; app.ww[k] = w; app.wh[k] = h;
        }
        app.add_log("Welcome to the HiveMind desktop.");
        app.add_log("Click an icon to open a window. Drag title bars to move.");
        app
    }

    // ── Window management ─────────────────────────────────────────────────────

    fn focused(&self) -> Option<usize> {
        self.zorder.last().copied()
    }

    fn raise(&mut self, kind: usize) {
        self.open[kind] = true;
        self.zorder.retain(|&k| k != kind);
        self.zorder.push(kind);
    }

    fn close(&mut self, kind: usize) {
        self.open[kind] = false;
        self.zorder.retain(|&k| k != kind);
        if let Some((k, _, _)) = self.dragging {
            if k == kind { self.dragging = None; }
        }
    }

    fn cycle_focus(&mut self) {
        if self.zorder.len() > 1 {
            let top = self.zorder.pop().unwrap();
            self.zorder.insert(0, top);
        }
    }

    // ── Keyboard ──────────────────────────────────────────────────────────────

    /// Returns true when the desktop should exit back to the shell.
    fn handle_key(&mut self, c: char) -> bool {
        match c {
            '\x1b' => {
                // Esc closes the focused window, or exits if none are open.
                if let Some(k) = self.focused() {
                    self.close(k);
                    return false;
                }
                return true;
            }
            '\t' => { self.cycle_focus(); return false; }
            _ => {}
        }

        let kind = match self.focused() {
            Some(k) => k,
            None => return false,
        };

        match kind {
            TERMINAL => self.terminal_key(c),
            NOTEPAD  => self.notepad_key(c),
            _ => {}
        }
        false
    }

    fn terminal_key(&mut self, c: char) {
        match c {
            '\n' => {
                let cmd = self.cli.trim().to_string();
                self.cli.clear();
                if cmd == "exit" {
                    self.close(TERMINAL);
                } else {
                    self.run_command(&cmd);
                }
            }
            '\x08' => { self.cli.pop(); }
            c if c.is_ascii() && !c.is_control() => {
                let max = self.ww[TERMINAL].saturating_sub(12);
                if self.cli.len() < max {
                    self.cli.push(c);
                }
            }
            _ => {}
        }
    }

    fn notepad_key(&mut self, c: char) {
        match c {
            '\n' => { if self.note.len() < NOTE_CAP { self.note.push('\n'); } }
            '\x08' => { self.note.pop(); }
            c if c.is_ascii() && !c.is_control() => {
                if self.note.len() < NOTE_CAP { self.note.push(c); }
            }
            _ => {}
        }
    }

    // ── Mouse ───────────────────────────────────────────────────────────────────

    fn handle_mouse(&mut self, m: crate::mouse::Mouse) {
        self.m_col = m.col;
        self.m_row = m.row;

        // In-progress drag takes priority over everything else.
        if let Some((k, offx, offy)) = self.dragging {
            if m.left {
                let nx = m.col.saturating_sub(offx).min(BUFFER_WIDTH.saturating_sub(self.ww[k]));
                let max_y = (BUFFER_HEIGHT - 1).saturating_sub(self.wh[k]);
                let ny = m.row.saturating_sub(offy).clamp(1, max_y.max(1));
                self.wx[k] = nx;
                self.wy[k] = ny;
            } else {
                self.dragging = None;
            }
            return;
        }

        if m.clicked {
            self.on_click(m.col, m.row);
        }
    }

    fn on_click(&mut self, col: usize, row: usize) {
        // Taskbar.
        if row == BUFFER_HEIGHT - 1 {
            if let Some(k) = self.taskbar_hit(col) {
                self.raise(k);
            }
            return;
        }

        // Windows, top-most first.
        let order: Vec<usize> = self.zorder.iter().rev().copied().collect();
        for k in order {
            if !self.open[k] { continue; }
            let (x, y, w, h) = (self.wx[k], self.wy[k], self.ww[k], self.wh[k]);
            if row >= y && row < y + h && col >= x && col < x + w {
                self.raise(k);
                // Close box occupies the last 3 columns of the title row.
                if row == y && col >= x + w - 3 {
                    self.close(k);
                    return;
                }
                // Title bar → begin drag.
                if row == y {
                    self.dragging = Some((k, col - x, 0));
                }
                return;
            }
        }

        // Launcher icons.
        for i in 0..KINDS {
            let (iy, ix, ih, iw) = icon_rect(i);
            if row >= iy && row < iy + ih && col >= ix && col < ix + iw {
                self.raise(i);
                return;
            }
        }
    }

    /// Layout of taskbar buttons: returns (kind, start_col, end_col) inclusive.
    fn taskbar_layout(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        let mut c = 1usize;
        for &k in &self.zorder {
            let label_len = NAMES[k].len() + 2; // "[Name]"
            let start = c;
            let end = c + label_len - 1;
            if end >= BUFFER_WIDTH - 10 { break; }
            out.push((k, start, end));
            c = end + 2;
        }
        out
    }

    fn taskbar_hit(&self, col: usize) -> Option<usize> {
        for (k, s, e) in self.taskbar_layout() {
            if col >= s && col <= e {
                return Some(k);
            }
        }
        None
    }

    // ── Terminal command handling ─────────────────────────────────────────────

    fn run_command(&mut self, command: &str) {
        if command.is_empty() { return; }
        self.add_log(&format!("> {}", command));
        let parts: Vec<&str> = command.splitn(4, ' ').collect();
        match parts[0] {
            "help" => {
                self.add_log("cmds: hive, mem list, files, net status,");
                self.add_log("net send <mem> <key> <val>, note save <mem> <key>,");
                self.add_log("clear, exit");
            }
            "hive" => {
                hive::with_hive(|h| {
                    self.add_log(&format!(
                        "nodes={} blobs={} edges={} tick={}",
                        h.memories.len(), h.total_blobs(), h.total_edges(),
                        crate::interrupts::current_tick()
                    ));
                });
            }
            "mem" if parts.len() > 1 && parts[1] == "list" => self.list_memories(),
            "files" | "ls" => self.list_files(),
            "net" if parts.len() > 1 && parts[1] == "status" => {
                let (tx, rx) = crate::net::stats();
                self.add_log(&format!("mesh tx={} rx={}", tx, rx));
            }
            "net" if parts.len() == 4 && parts[1] == "send" => {
                let mut rest = parts[3].splitn(2, ' ');
                let key = rest.next().unwrap_or("");
                let value = rest.next().unwrap_or("");
                if key.is_empty() || value.is_empty() {
                    self.add_log("usage: net send <mem> <key> <val>");
                } else {
                    crate::net::send_blob(parts[2], key, value);
                    crate::disk::persist::mark_dirty();
                    self.add_log("sent blob over COM2 mesh");
                }
            }
            "note" if parts.len() >= 4 && parts[1] == "save" => {
                let text = self.note.clone();
                hive::with_hive(|h| {
                    let id = h.memories.iter()
                        .find(|(_, node)| node.name == parts[2])
                        .map(|(&id, _)| id)
                        .unwrap_or_else(|| h.create_memory(parts[2], None));
                    h.write_blob(id, parts[3], BlobValue::Text(text));
                });
                crate::disk::persist::mark_dirty();
                self.add_log("note saved into hive memory");
            }
            "clear" => self.logs.clear(),
            other => self.add_log(&format!("unknown command: {}", other)),
        }
    }

    fn list_memories(&mut self) {
        let mut lines = Vec::new();
        hive::with_hive(|h| {
            if h.memories.is_empty() {
                lines.push("no memory nodes".to_string());
            } else {
                for (_, node) in h.memories.iter().take(6) {
                    lines.push(format!("#{} {} blobs={}", node.id, node.name, node.blobs.len()));
                }
            }
        });
        for l in lines { self.add_log(&l); }
    }

    fn list_files(&mut self) {
        let entries = crate::vfs::with_vfs(|v| v.ls(&self.files_path));
        match entries {
            Ok(list) => {
                self.add_log(&format!("{}:", self.files_path));
                for (name, is_dir) in list.iter().take(8) {
                    self.add_log(&format!("  {}{}", name, if *is_dir { "/" } else { "" }));
                }
            }
            Err(e) => self.add_log(e),
        }
    }

    fn add_log(&mut self, line: &str) {
        if self.logs.len() >= LOG_CAP { self.logs.remove(0); }
        self.logs.push(line.to_string());
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    fn render(&self) {
        // Desktop background.
        vga_buffer::fill_raw(1, 0, BUFFER_HEIGHT - 2, BUFFER_WIDTH, SH_BG, Color::Blue, Color::Black);

        self.draw_topbar();
        self.draw_icons();

        // Windows bottom-to-top.
        let focused = self.focused();
        for &k in &self.zorder {
            self.draw_window(k, Some(k) == focused);
        }

        self.draw_taskbar();
        self.draw_cursor();
    }

    fn draw_topbar(&self) {
        vga_buffer::fill_rect(0, 0, 1, BUFFER_WIDTH, ' ', Color::Black, Color::LightGray);
        vga_buffer::write_at(0, 1, "HiveMind OS", Color::Blue, Color::LightGray);
        vga_buffer::write_at(0, 14, "Desktop", Color::Black, Color::LightGray);

        // Short form of this instance's UUID so you can tell windows apart.
        crate::sysinfo::with_uuid(|u| {
            let short = &u[..u.len().min(8)];
            vga_buffer::write_at(0, 24, &format!("id:{}", short), Color::Blue, Color::LightGray);
        });

        // Live clock from the RTC.
        let dt = crate::rtc::read();
        let (_, time) = dt.display();
        if let Ok(t) = core::str::from_utf8(&time) {
            vga_buffer::write_at(0, BUFFER_WIDTH - 10, t, Color::Black, Color::LightGray);
        }
    }

    fn draw_icons(&self) {
        for i in 0..KINDS {
            let (y, x, h, w) = icon_rect(i);
            let over = self.point_in_icon(i, self.m_col, self.m_row);
            let border = if over { Color::Yellow } else { Color::LightCyan };
            let bg = Color::Black;
            // Icon panel.
            vga_buffer::fill_rect(y, x, h, w, ' ', Color::White, bg);
            draw_single_frame(y, x, h, w, border, bg);
            // Centered label.
            let name = NAMES[i];
            let lx = x + (w.saturating_sub(name.len())) / 2;
            vga_buffer::write_at(y + 1, lx, name, Color::White, bg);
        }
    }

    fn point_in_icon(&self, i: usize, col: usize, row: usize) -> bool {
        let (y, x, h, w) = icon_rect(i);
        row >= y && row < y + h && col >= x && col < x + w
    }

    fn draw_window(&self, kind: usize, focused: bool) {
        let (x, y, w, h) = (self.wx[kind], self.wy[kind], self.ww[kind], self.wh[kind]);

        // Body.
        vga_buffer::fill_rect(y, x, h, w, ' ', Color::LightGray, Color::Black);
        let border = if focused { Color::Yellow } else { Color::DarkGray };
        if focused {
            draw_double_frame(y, x, h, w, border, Color::Black);
        } else {
            draw_single_frame(y, x, h, w, border, Color::Black);
        }

        // Title bar.
        let title_bg = if focused { Color::Blue } else { Color::DarkGray };
        vga_buffer::fill_rect(y, x + 1, 1, w.saturating_sub(2), ' ', Color::White, title_bg);
        vga_buffer::write_at(y, x + 2, NAMES[kind], Color::White, title_bg);
        // Close box.
        vga_buffer::write_at(y, x + w - 3, "[X]", Color::White, Color::Red);

        // Content interior.
        let ix = x + 2;
        let iy = y + 1;
        let iw = w.saturating_sub(4);
        let ih = h.saturating_sub(2);
        match kind {
            TERMINAL => self.draw_terminal(iy, ix, ih, iw, focused),
            NOTEPAD  => self.draw_notepad(iy, ix, ih, iw, focused),
            HIVE     => self.draw_hive(iy, ix, ih, iw),
            FILES    => self.draw_files(iy, ix, ih, iw),
            MESH     => self.draw_mesh(iy, ix, ih, iw),
            HELP     => self.draw_help(iy, ix, ih, iw),
            _ => {}
        }
    }

    fn draw_terminal(&self, y: usize, x: usize, h: usize, w: usize, focused: bool) {
        let log_rows = h.saturating_sub(1);
        let start = self.logs.len().saturating_sub(log_rows);
        for (i, line) in self.logs[start..].iter().enumerate() {
            vga_buffer::write_at(y + i, x, clip(line, w), Color::LightGreen, Color::Black);
        }
        // Input line.
        let iy = y + h - 1;
        vga_buffer::write_at(iy, x, "hive>", Color::LightCyan, Color::Black);
        let shown = clip(&self.cli, w.saturating_sub(7));
        vga_buffer::write_at(iy, x + 6, shown, Color::White, Color::Black);
        if focused {
            let cx = (x + 6 + shown.len()).min(x + w - 1);
            vga_buffer::put_raw_at(iy, cx, b'_', Color::Yellow, Color::Black);
        }
    }

    fn draw_notepad(&self, y: usize, x: usize, h: usize, w: usize, focused: bool) {
        let mut row = 0usize;
        let mut col = 0usize;
        for ch in self.note.chars() {
            if row >= h { break; }
            if ch == '\n' {
                row += 1; col = 0; continue;
            }
            if col >= w { row += 1; col = 0; if row >= h { break; } }
            vga_buffer::put_raw_at(y + row, x + col, ch as u8, Color::White, Color::Black);
            col += 1;
        }
        if focused && row < h {
            vga_buffer::put_raw_at(y + row, x + col.min(w - 1), b'_', Color::Yellow, Color::Black);
        }
    }

    fn draw_hive(&self, y: usize, x: usize, h: usize, w: usize) {
        let mut lines: Vec<String> = Vec::new();
        hive::with_hive(|hv| {
            lines.push(format!("nodes : {}", hv.memories.len()));
            lines.push(format!("blobs : {}", hv.total_blobs()));
            lines.push(format!("edges : {}", hv.total_edges()));
            lines.push(format!("tick  : {}", crate::interrupts::current_tick()));
            lines.push("─ memory nodes ─".to_string());
            for (_, node) in hv.memories.iter().take(h.saturating_sub(6)) {
                lines.push(format!("#{} {} ({} blobs)", node.id, node.name, node.blobs.len()));
            }
        });
        for (i, l) in lines.iter().enumerate().take(h) {
            vga_buffer::write_at(y + i, x, clip(l, w), Color::LightCyan, Color::Black);
        }
    }

    fn draw_files(&self, y: usize, x: usize, h: usize, w: usize) {
        vga_buffer::write_at(y, x, clip(&format!("path: {}", self.files_path), w),
            Color::Yellow, Color::Black);
        let entries = crate::vfs::with_vfs(|v| v.ls(&self.files_path));
        match entries {
            Ok(list) => {
                for (i, (name, is_dir)) in list.iter().enumerate().take(h.saturating_sub(1)) {
                    let label = if *is_dir { format!("[{}]", name) } else { name.clone() };
                    let color = if *is_dir { Color::LightCyan } else { Color::White };
                    vga_buffer::write_at(y + 1 + i, x, clip(&label, w), color, Color::Black);
                }
            }
            Err(e) => vga_buffer::write_at(y + 1, x, e, Color::LightRed, Color::Black),
        }
    }

    fn draw_mesh(&self, y: usize, x: usize, _h: usize, w: usize) {
        let (tx, rx) = crate::net::stats();
        vga_buffer::write_at(y, x, "COM2 mesh serial", Color::White, Color::Black);
        vga_buffer::write_at(y + 1, x, clip(&format!("TX sent : {}", tx), w), Color::LightGreen, Color::Black);
        vga_buffer::write_at(y + 2, x, clip(&format!("RX recv : {}", rx), w), Color::LightGreen, Color::Black);
        vga_buffer::write_at(y + 4, x, "Use Terminal:", Color::DarkGray, Color::Black);
        vga_buffer::write_at(y + 5, x, clip("net send <mem> <key> <val>", w), Color::DarkGray, Color::Black);
    }

    fn draw_help(&self, y: usize, x: usize, h: usize, w: usize) {
        let lines = [
            "HiveMind Desktop",
            "",
            "Mouse:",
            "  Click an icon to open a window",
            "  Drag a title bar to move a window",
            "  Click [X] to close a window",
            "  Click a taskbar button to focus",
            "",
            "Keyboard:",
            "  Tab   cycle window focus",
            "  Esc   close window / exit desktop",
            "  Type into Terminal or Notepad",
            "",
            "Terminal: type 'help' for commands.",
        ];
        for (i, l) in lines.iter().enumerate().take(h) {
            vga_buffer::write_at(y + i, x, clip(l, w), Color::White, Color::Black);
        }
    }

    fn draw_taskbar(&self) {
        let row = BUFFER_HEIGHT - 1;
        vga_buffer::fill_rect(row, 0, 1, BUFFER_WIDTH, ' ', Color::Black, Color::LightGray);
        let focused = self.focused();
        for (k, s, _e) in self.taskbar_layout() {
            let (fg, bg) = if Some(k) == focused {
                (Color::White, Color::Blue)
            } else {
                (Color::Black, Color::LightGray)
            };
            vga_buffer::write_at(row, s, &format!("[{}]", NAMES[k]), fg, bg);
        }
        vga_buffer::write_at(row, BUFFER_WIDTH - 12, "Esc:shell", Color::DarkGray, Color::LightGray);
    }

    fn draw_cursor(&self) {
        // Invert the cell under the pointer so the cursor is always visible.
        let (ch, attr) = vga_buffer::read_cell(self.m_row, self.m_col);
        let inv = ((attr & 0x0F) << 4) | ((attr & 0xF0) >> 4);
        vga_buffer::write_cell_raw(self.m_row, self.m_col, ch, inv);
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

/// Launcher icon rectangle: (y, x, h, w).
fn icon_rect(i: usize) -> (usize, usize, usize, usize) {
    (2 + i * 3, 1, 3, 13)
}

fn draw_single_frame(y: usize, x: usize, h: usize, w: usize, fg: Color, bg: Color) {
    draw_frame(y, x, h, w, fg, bg, S_TL, S_TR, S_BL, S_BR, S_H, S_V);
}

fn draw_double_frame(y: usize, x: usize, h: usize, w: usize, fg: Color, bg: Color) {
    draw_frame(y, x, h, w, fg, bg, D_TL, D_TR, D_BL, D_BR, D_H, D_V);
}

#[allow(clippy::too_many_arguments)]
fn draw_frame(y: usize, x: usize, h: usize, w: usize, fg: Color, bg: Color,
              tl: u8, tr: u8, bl: u8, br: u8, hb: u8, vb: u8) {
    if w < 2 || h < 2 { return; }
    for c in x..x + w {
        vga_buffer::put_raw_at(y, c, hb, fg, bg);
        vga_buffer::put_raw_at(y + h - 1, c, hb, fg, bg);
    }
    for r in y..y + h {
        vga_buffer::put_raw_at(r, x, vb, fg, bg);
        vga_buffer::put_raw_at(r, x + w - 1, vb, fg, bg);
    }
    vga_buffer::put_raw_at(y, x, tl, fg, bg);
    vga_buffer::put_raw_at(y, x + w - 1, tr, fg, bg);
    vga_buffer::put_raw_at(y + h - 1, x, bl, fg, bg);
    vga_buffer::put_raw_at(y + h - 1, x + w - 1, br, fg, bg);
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max { return text; }
    let mut end = 0;
    for (i, _) in text.char_indices() {
        if i > max { break; }
        end = i;
    }
    &text[..end]
}
