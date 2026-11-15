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
const TASKS:    usize = 6;
const NOTES:    usize = 7;
const ACTIVITY: usize = 8;
const KINDS:    usize = 9;

const NAMES: [&str; KINDS] = ["Terminal", "Notepad", "Hive", "Files", "Mesh", "Help",
                              "Tasks", "Notes", "Activity"];

// Default window geometry: (x, y, w, h).
const DEFAULT_GEOM: [(usize, usize, usize, usize); KINDS] = [
    (18, 3, 58, 18), // Terminal
    (20, 4, 50, 16), // Notepad
    (22, 3, 46, 15), // Hive
    (24, 4, 42, 15), // Files
    (26, 6, 40, 11), // Mesh
    (20, 3, 54, 19), // Help
    (16, 3, 60, 17), // Tasks
    (15, 2, 64, 20), // Notes
    (14, 2, 66, 21), // Activity
];

// CP437 glyphs.
const SH_BG:  u8 = 0xB0; // ░ light shade (desktop background)
const D_TL: u8 = 0xC9; const D_TR: u8 = 0xBB; const D_BL: u8 = 0xC8; const D_BR: u8 = 0xBC;
const D_H:  u8 = 0xCD; const D_V:  u8 = 0xBA;
const S_TL: u8 = 0xDA; const S_TR: u8 = 0xBF; const S_BL: u8 = 0xC0; const S_BR: u8 = 0xD9;
const S_H:  u8 = 0xC4; const S_V:  u8 = 0xB3;

const LOG_CAP:  usize = 64;
const NOTE_CAP: usize = 1200;

/// Open the desktop; `open` names a window to start on (tasks, notes, activity, ...).
pub fn run(open: Option<&str>) {
    let mut app = DesktopApp::new();
    app.raise(HELP);
    if let Some(name) = open {
        if let Some(k) = NAMES.iter().position(|n| n.eq_ignore_ascii_case(name)) {
            app.raise(k);
        }
    }
    app.render();

    let mut last_tick = 0u64;
    let mut last_paint = 0u64;
    loop {
        // Agents, reminders, model answers and provenance keep running while
        // the desktop is open.
        crate::apps::background(&mut last_tick);

        let mut dirty = false;
        while let Some(n) = crate::apps::take_notification() {
            app.add_log(&n);
            app.banner = Some((n, crate::interrupts::current_tick() + 18 * 8));
            dirty = true;
        }
        // Repaint about once a second so the clock, tasks and activity are live.
        let t = crate::interrupts::current_tick();
        if t.wrapping_sub(last_paint) >= 18 {
            last_paint = t;
            if let Some((_, until)) = &app.banner {
                if t >= *until { app.banner = None; }
            }
            dirty = true;
        }

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
    /// True when the notepad contents match what was last saved to the hive.
    note_saved: bool,
    /// Input lines of the Tasks, Notes and Activity windows.
    task_in:  String,
    notes_in: String,
    act_in:   String,
    /// Last status message per window, with its tone.
    msg:      [(String, crate::apps::Tone); KINDS],
    /// Activity window: a `why` result is shown instead of the live feed.
    why_view: Option<Vec<(String, crate::apps::Tone)>>,
    /// Reminder / answer banner and the tick it disappears at.
    banner:   Option<(String, u64)>,
}

impl DesktopApp {
    fn new() -> Self {
        // Restore any previously-saved notepad contents from the hive.
        let saved_note = hive::with_hive(|h| {
            h.memories
                .values()
                .find(|n| n.name == "notepad")
                .and_then(|n| n.blobs.get("content"))
                .and_then(|b| match &b.value {
                    BlobValue::Text(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        });

        let mut app = Self {
            open:     [false; KINDS],
            zorder:   Vec::new(),
            wx:       [0; KINDS],
            wy:       [0; KINDS],
            ww:       [0; KINDS],
            wh:       [0; KINDS],
            note:     saved_note,
            cli:      String::new(),
            logs:     Vec::new(),
            dragging: None,
            m_col:    BUFFER_WIDTH / 2,
            m_row:    BUFFER_HEIGHT / 2,
            files_path: "/".to_string(),
            note_saved: true,
            task_in:  String::new(),
            notes_in: String::new(),
            act_in:   String::new(),
            msg:      Default::default(),
            why_view: None,
            banner:   None,
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
            // F1..F9 open (or focus) windows in launcher order.
            c if (c as u32) >= crate::shell::KEY_F1 && (c as u32) < crate::shell::KEY_F1 + KINDS as u32 => {
                self.raise((c as u32 - crate::shell::KEY_F1) as usize);
                return false;
            }
            _ => {}
        }

        let kind = match self.focused() {
            Some(k) => k,
            None => return false,
        };

        match kind {
            TERMINAL => self.terminal_key(c),
            NOTEPAD  => self.notepad_key(c),
            TASKS | NOTES | ACTIVITY => self.app_key(kind, c),
            _ => {}
        }
        false
    }

    /// Line editing for the app windows; Enter runs the line.
    fn app_key(&mut self, kind: usize, c: char) {
        let max = self.ww[kind].saturating_sub(10);
        let buf = match kind { TASKS => &mut self.task_in, NOTES => &mut self.notes_in, _ => &mut self.act_in };
        match c {
            '\n' => {
                let line = buf.trim().to_string();
                buf.clear();
                match kind {
                    TASKS => self.tasks_enter(&line),
                    NOTES => self.notes_enter(&line),
                    _ => self.activity_enter(&line),
                }
            }
            '\x08' => { buf.pop(); }
            c if c.is_ascii() && !c.is_control() => { if buf.len() < max { buf.push(c); } }
            _ => {}
        }
    }

    fn tasks_enter(&mut self, line: &str) {
        use crate::apps::Tone;
        use crate::prov::Actor;
        let mut it = line.splitn(2, ' ');
        let first = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("").trim();
        self.msg[TASKS] = match first {
            "" => return,
            "done" => if crate::apps::task_done(Actor::Desktop, rest) { (format!("{} done", rest), Tone::Normal) }
                      else { (format!("no task {}", rest), Tone::Deny) },
            "del" => if crate::apps::task_del(Actor::Desktop, rest) { (format!("{} deleted", rest), Tone::Normal) }
                     else { (format!("no task {}", rest), Tone::Deny) },
            "snooze" => if crate::apps::task_snooze(Actor::Desktop, rest, 5) { (format!("{} snoozed 5 min", rest), Tone::Normal) }
                        else { (format!("no task {}", rest), Tone::Deny) },
            due => match crate::apps::task_add(Actor::Desktop, due, rest) {
                Ok((k, d)) => (format!("added {} for {}", k, crate::apps_core::fmt_hhmm(d)), Tone::Normal),
                Err(e) => (e.to_string(), Tone::Deny),
            },
        };
    }

    fn notes_enter(&mut self, line: &str) {
        use crate::prov::Actor;
        if line.is_empty() { return; }
        if let Some(q) = line.strip_prefix('?') {
            let o = crate::apps::ask(Actor::Desktop, q.trim());
            let (m, t) = crate::apps::offload_msg(&o);
            self.msg[NOTES] = (format!("ask: {}", m), t);
        } else {
            let (k, o) = crate::apps::note_add(Actor::Desktop, line);
            let (m, t) = crate::apps::offload_msg(&o);
            self.msg[NOTES] = (format!("saved {}; tags {}", k, m), t);
        }
    }

    fn activity_enter(&mut self, line: &str) {
        use crate::apps::Tone;
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            [] | ["live"] => { self.why_view = None; self.msg[ACTIVITY] = ("live feed".to_string(), Tone::Dim); }
            ["why", node, key] => {
                self.why_view = Some(crate::apps::why(node, key));
                self.msg[ACTIVITY] = (format!("why {}/{}  (Enter on empty line: back to live)", node, key), Tone::Dim);
            }
            ["quarantine"] | ["q"] => self.quarantine(),
            ["policy", p] => match crate::apps_core::TaintPolicy::parse(p) {
                Some(pol) => {
                    crate::hive::with_hive(|h| h.taint_policy = pol);
                    crate::prov::emit_plain(crate::prov::Kind::CapChange, crate::prov::Actor::Desktop, "taint_policy",
                        None, crate::prov::attrs(&[("policy", pol.name())]));
                    crate::disk::persist::mark_dirty();
                    self.msg[ACTIVITY] = (format!("taint policy: {}", pol.name()), Tone::Model);
                }
                None => self.msg[ACTIVITY] = ("policy off|block|redact".to_string(), Tone::Deny),
            },
            _ => self.msg[ACTIVITY] = ("why <node> <key> | quarantine | policy off|block|redact | live".to_string(), Tone::Deny),
        }
    }

    fn quarantine(&mut self) {
        use crate::apps::Tone;
        let (r, d, ev) = crate::hive::with_hive(|h| h.quarantine(crate::prov::Actor::Desktop));
        crate::disk::persist::mark_dirty();
        self.why_view = None;
        self.msg[ACTIVITY] = (format!("quarantine #{}: {} rolled back, {} removed", ev, r, d),
                              if r + d > 0 { Tone::Model } else { Tone::Dim });
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
            '\n' => { if self.note.len() < NOTE_CAP { self.note.push('\n'); self.note_saved = false; } }
            '\x08' => { if self.note.pop().is_some() { self.note_saved = false; } }
            c if c.is_ascii() && !c.is_control() => {
                if self.note.len() < NOTE_CAP { self.note.push(c); self.note_saved = false; }
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
                // Notepad Save button: [Save] sits just left of the close box.
                if k == NOTEPAD && row == y && col >= x + w - 10 && col < x + w - 3 {
                    self.save_note();
                    return;
                }
                // Activity [Quarantine] button.
                if k == ACTIVITY && row == y && col >= x + w - 16 && col < x + w - 4 {
                    self.quarantine();
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
                        .unwrap_or_else(|| h.create_memory_as(crate::prov::Actor::Desktop, parts[2], None));
                    let _ = h.write_blob_as(crate::prov::Actor::Desktop, id, parts[3], BlobValue::Text(text), None);
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

    /// Save the notepad contents into a hive memory node named "notepad".
    /// The hive auto-persists to disk, so notes survive a reboot.
    fn save_note(&mut self) {
        let text = self.note.clone();
        let bytes = text.len();
        hive::with_hive(|h| {
            let id = h
                .memories
                .iter()
                .find(|(_, n)| n.name == "notepad")
                .map(|(&id, _)| id)
                .unwrap_or_else(|| h.create_memory_as(crate::prov::Actor::Desktop, "notepad", None));
            let _ = h.write_blob_as(crate::prov::Actor::Desktop, id, "content", BlobValue::Text(text), None);
        });
        crate::disk::persist::mark_dirty();
        self.add_log(&format!("Saved {} bytes to hive memory 'notepad'", bytes));
        self.note_saved = true;
    }

    fn add_log(&mut self, line: &str) {
        if self.logs.len() >= LOG_CAP { self.logs.remove(0); }
        self.logs.push(line.to_string());
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    fn render(&self) {
        vga_buffer::begin_frame();
        self.render_frame();
        vga_buffer::end_frame();
    }

    fn render_frame(&self) {
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
        if let Some((b, _)) = &self.banner {
            vga_buffer::fill_rect(1, 0, 1, BUFFER_WIDTH, ' ', Color::Black, Color::Yellow);
            vga_buffer::write_at(1, 1, clip(&format!("(!) {}", b), BUFFER_WIDTH - 2), Color::Black, Color::Yellow);
        }
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
            // One-row button; the new apps get a distinct colour.
            let bg = if i >= TASKS { Color::Magenta } else { Color::Blue };
            let fg = if over { Color::Yellow } else { Color::White };
            let _ = border;
            vga_buffer::fill_rect(y, x, h, w, ' ', fg, bg);
            let name = NAMES[i];
            let lx = x + (w.saturating_sub(name.len())) / 2;
            vga_buffer::write_at(y, lx, name, fg, bg);
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
        // Notepad gets a Save button in the title bar (left of the close box).
        // Green when saved, yellow when there are unsaved edits.
        if kind == NOTEPAD {
            let sb = if self.note_saved { Color::LightGreen } else { Color::Yellow };
            vga_buffer::write_at(y, x + w - 10, "[Save]", Color::Black, sb);
        }
        if kind == ACTIVITY {
            vga_buffer::write_at(y, x + w - 16, "[Quarantine]", Color::White, Color::Magenta);
        }
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
            TASKS    => self.draw_tasks(iy, ix, ih, iw, focused),
            NOTES    => self.draw_notes(iy, ix, ih, iw, focused),
            ACTIVITY => self.draw_activity(iy, ix, ih, iw, focused),
            _ => {}
        }
    }

    /// Bottom two rows of an app window: status message, then the input line.
    fn draw_input(&self, kind: usize, y: usize, x: usize, h: usize, w: usize, focused: bool, prompt: &str, buf: &str) {
        if h < 3 { return; }
        let (m, t) = &self.msg[kind];
        vga_buffer::write_at(y + h - 2, x, clip(m, w), tone_color(*t), Color::Black);
        let iy = y + h - 1;
        vga_buffer::write_at(iy, x, prompt, Color::LightCyan, Color::Black);
        let shown = clip(buf, w.saturating_sub(prompt.len() + 2));
        vga_buffer::write_at(iy, x + prompt.len() + 1, shown, Color::White, Color::Black);
        if focused {
            let cx = (x + prompt.len() + 1 + shown.len()).min(x + w - 1);
            vga_buffer::put_raw_at(iy, cx, b'_', Color::Yellow, Color::Black);
        }
    }

    fn draw_tasks(&self, y: usize, x: usize, h: usize, w: usize, focused: bool) {
        use crate::apps_core::{fmt_hhmm, Status};
        let now = crate::apps::now_abs();
        vga_buffer::write_at(y, x, clip("Add: +5 stretch  |  18:30 call home   then: done t1, del t1, snooze t1", w),
            Color::DarkGray, Color::Black);
        let tasks = crate::apps::task_list();
        if tasks.is_empty() {
            vga_buffer::write_at(y + 2, x, "No tasks yet.", Color::DarkGray, Color::Black);
        }
        for (i, (k, t, tainted)) in tasks.iter().enumerate().take(h.saturating_sub(4)) {
            let (mark, col) = match t.status {
                Status::Open => ("[ ]", Color::White),
                Status::Fired => ("[!]", Color::Yellow),
                Status::Done => ("[x]", Color::DarkGray),
            };
            let col = if *tainted { Color::Pink } else { col };
            let left = t.due - now;
            let when = if t.status == Status::Open && left > 0 && left < 600 {
                format!("{} in {}m", fmt_hhmm(t.due), left)
            } else {
                fmt_hhmm(t.due)
            };
            let l = format!("{} {:<4} {:<13} {}{}", mark, k, when, t.text, if *tainted { " [untrusted]" } else { "" });
            vga_buffer::write_at(y + 1 + i, x, clip(&l, w), col, Color::Black);
        }
        self.draw_input(TASKS, y, x, h, w, focused, "task>", &self.task_in);
    }

    fn draw_notes(&self, y: usize, x: usize, h: usize, w: usize, focused: bool) {
        vga_buffer::write_at(y, x, clip("Type a note + Enter.  Start with ? to ask your notes.", w),
            Color::DarkGray, Color::Black);
        let mut rows: Vec<(String, Color)> = Vec::new();
        for (k, text, tags, tainted) in crate::apps::note_list() {
            let c = if tainted { Color::Pink } else { Color::White };
            rows.push((format!("{} {}{}", k, text, if tainted { " [untrusted]" } else { "" }), c));
            if let Some(g) = tags {
                rows.push((format!("   #{}", g), Color::DarkGray));
            }
        }
        let body = h.saturating_sub(6);
        let start = rows.len().saturating_sub(body);
        for (i, (l, c)) in rows[start..].iter().enumerate() {
            vga_buffer::write_at(y + 1 + i, x, clip(l, w), *c, Color::Black);
        }
        if let Some((a, tainted)) = crate::apps::last_answer() {
            let c = if tainted { Color::Pink } else { Color::Yellow };
            vga_buffer::write_at(y + h - 4, x, clip(&format!("AI: {}", a), w), c, Color::Black);
            let hint = if tainted { "answer used UNTRUSTED content; see Activity: why notes answer" }
                       else { "Activity window: why notes answer  shows what it used" };
            vga_buffer::write_at(y + h - 3, x, clip(hint, w), Color::DarkGray, Color::Black);
        }
        self.draw_input(NOTES, y, x, h, w, focused, "note>", &self.notes_in);
    }

    fn draw_activity(&self, y: usize, x: usize, h: usize, w: usize, focused: bool) {
        let (pol, n) = crate::hive::with_hive(|hv| (hv.taint_policy, hv.tainted_blobs().len()));
        let head = format!("taint policy: {}   untrusted values: {}   ({} events kept)", pol.name(), n,
            crate::prov_core::RECENT_CAP);
        vga_buffer::write_at(y, x, clip(&head, w), if n > 0 { Color::Pink } else { Color::LightCyan }, Color::Black);
        let body = h.saturating_sub(3);
        let lines = match &self.why_view {
            Some(v) => v.clone(),
            None => crate::apps::activity(body),
        };
        let start = if self.why_view.is_some() { 0 } else { lines.len().saturating_sub(body) };
        for (i, (l, t)) in lines[start..].iter().enumerate().take(body) {
            vga_buffer::write_at(y + 1 + i, x, clip(l, w), tone_color(*t), Color::Black);
        }
        self.draw_input(ACTIVITY, y, x, h, w, focused, "act>", &self.act_in);
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
            "  F1-F9 open windows (F7 Tasks,",
            "        F8 Notes, F9 Activity)",
            "  Esc   close window / exit desktop",
            "  Type into Terminal or Notepad",
            "",
            "Terminal: type 'help' for commands.",
            "",
            "Tasks: '+5 stretch' adds a reminder.",
            "Notes: type a note; '?question' asks AI.",
            "Activity: live events, 'why notes answer',",
            "  'policy block', [Quarantine] button.",
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

/// Launcher icon rectangle: (y, x, h, w). One-row buttons, a blank row apart.
fn icon_rect(i: usize) -> (usize, usize, usize, usize) {
    (2 + i * 2, 1, 1, 13)
}

fn tone_color(t: crate::apps::Tone) -> Color {
    use crate::apps::Tone;
    match t {
        Tone::Normal => Color::White,
        Tone::Model => Color::Yellow,
        Tone::Deny => Color::LightRed,
        Tone::Taint => Color::Pink,
        Tone::Dim => Color::DarkGray,
    }
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
