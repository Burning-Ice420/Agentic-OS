use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::hive;
use crate::hive::blob::BlobValue;
use crate::vga_buffer::{self, Color};

const NOTE_CAP: usize = 560;
const LOG_CAP: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Notepad,
    Cli,
}

pub fn run() {
    let mut app = DesktopApp::new();
    app.add_log("HiveMind desktop opened inside the OS.");
    app.add_log("Tab switches panes. Esc returns to hive shell.");
    app.render();

    loop {
        crate::shell::poll_keyboard();
        crate::net::poll_and_apply();

        let tick = crate::interrupts::current_tick();
        if tick > 0 && tick % 36 == 0 {
            crate::agent::tick_all();
        }

        while let Some(c) = crate::shell::read_key() {
            if app.handle_key(c) {
                vga_buffer::clear_screen();
                vga_buffer::set_color(Color::LightGreen, Color::Black);
                crate::println!("Returned from HiveMind desktop.");
                return;
            }
            app.render();
        }

        for _ in 0..50_000 {
            core::hint::spin_loop();
        }
    }
}

struct DesktopApp {
    focus: Focus,
    note: String,
    cli: String,
    logs: Vec<String>,
}

impl DesktopApp {
    fn new() -> Self {
        Self {
            focus: Focus::Notepad,
            note: String::new(),
            cli: String::new(),
            logs: Vec::new(),
        }
    }

    fn handle_key(&mut self, c: char) -> bool {
        match c {
            '\x1b' => return true,
            '\t' => self.toggle_focus(),
            '\n' => {
                if self.focus == Focus::Cli {
                    let command = self.cli.trim().to_string();
                    self.cli.clear();
                    if command == "exit" {
                        return true;
                    }
                    self.run_command(&command);
                } else if self.note.len() < NOTE_CAP {
                    self.note.push('\n');
                }
            }
            '\x08' => {
                if self.focus == Focus::Notepad {
                    self.note.pop();
                } else {
                    self.cli.pop();
                }
            }
            c if c.is_ascii() && !c.is_control() => {
                if self.focus == Focus::Notepad {
                    if self.note.len() < NOTE_CAP {
                        self.note.push(c);
                    }
                } else if self.cli.len() < 52 {
                    self.cli.push(c);
                }
            }
            _ => {}
        }
        false
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Notepad => Focus::Cli,
            Focus::Cli => Focus::Notepad,
        };
    }

    fn run_command(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }

        self.add_log(&format!("> {}", command));
        let parts: Vec<&str> = command.splitn(4, ' ').collect();
        match parts[0] {
            "help" => {
                self.add_log("help, hive, mem list, net status, net send <mem> <key> <val>");
                self.add_log("note save <mem> <key>, clear, exit");
            }
            "hive" => {
                hive::with_hive(|h| {
                    self.add_log(&format!(
                        "nodes={} blobs={} edges={} tick={}",
                        h.memories.len(),
                        h.total_blobs(),
                        h.total_edges(),
                        crate::interrupts::current_tick()
                    ));
                });
            }
            "mem" if parts.len() > 1 && parts[1] == "list" => self.list_memories(),
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
                    self.add_log("sent blob over COM2 mesh");
                }
            }
            "note" if parts.len() >= 4 && parts[1] == "save" => {
                let text = self.note.clone();
                hive::with_hive(|h| {
                    let id = h.memories
                        .iter()
                        .find(|(_, node)| node.name == parts[2])
                        .map(|(&id, _)| id)
                        .unwrap_or_else(|| h.create_memory(parts[2], None));
                    h.write_blob(id, parts[3], BlobValue::Text(text));
                });
                self.add_log("note saved into hive memory");
            }
            "clear" => self.logs.clear(),
            other => self.add_log(&format!("unknown command: {}", other)),
        }
    }

    fn list_memories(&mut self) {
        hive::with_hive(|h| {
            if h.memories.is_empty() {
                self.add_log("no memory nodes");
                return;
            }
            for (_, node) in h.memories.iter().take(5) {
                self.add_log(&format!(
                    "#{} {} blobs={} children={}",
                    node.id,
                    node.name,
                    node.blobs.len(),
                    node.children.len()
                ));
            }
        });
    }

    fn add_log(&mut self, line: &str) {
        if self.logs.len() >= LOG_CAP {
            self.logs.remove(0);
        }
        self.logs.push(line.to_string());
    }

    fn render(&self) {
        vga_buffer::clear_screen_with(Color::LightGray, Color::Black);
        self.draw_frame();
        self.draw_notepad();
        self.draw_cli();
        self.draw_status();
    }

    fn draw_frame(&self) {
        vga_buffer::fill_rect(0, 0, 1, 80, ' ', Color::White, Color::Blue);
        vga_buffer::write_at(0, 2, "HiveMind OS Desktop", Color::White, Color::Blue);
        vga_buffer::write_at(0, 55, "Tab focus  Esc shell", Color::Yellow, Color::Blue);

        vga_buffer::fill_rect(2, 1, 19, 38, ' ', Color::LightGray, Color::Black);
        vga_buffer::fill_rect(2, 41, 19, 38, ' ', Color::LightGray, Color::Black);
        draw_box(1, 0, 21, 40, self.focus == Focus::Notepad, " NOTEPAD ");
        draw_box(1, 40, 21, 40, self.focus == Focus::Cli, " CLI ");
    }

    fn draw_notepad(&self) {
        let mut row = 3;
        let mut col = 3;
        for ch in self.note.chars() {
            if row > 19 {
                break;
            }
            if ch == '\n' || col > 36 {
                row += 1;
                col = 3;
                if ch == '\n' {
                    continue;
                }
            }
            vga_buffer::put_char_at(row, col, ch, Color::White, Color::Black);
            col += 1;
        }

        if self.focus == Focus::Notepad && row <= 19 && col <= 36 {
            vga_buffer::put_char_at(row, col, '_', Color::Yellow, Color::Black);
        }
    }

    fn draw_cli(&self) {
        let mut row = 3;
        for line in self.logs.iter() {
            vga_buffer::write_at(row, 43, trim_to(line, 34), Color::LightGreen, Color::Black);
            row += 1;
            if row > 14 {
                break;
            }
        }

        vga_buffer::write_at(16, 43, "hive-ui>", Color::LightCyan, Color::Black);
        vga_buffer::write_at(16, 52, trim_to(&self.cli, 24), Color::White, Color::Black);
        if self.focus == Focus::Cli {
            let cursor = core::cmp::min(52 + self.cli.len(), 77);
            vga_buffer::put_char_at(16, cursor, '_', Color::Yellow, Color::Black);
        }

        vga_buffer::write_at(18, 43, "Type help for UI commands", Color::DarkGray, Color::Black);
        vga_buffer::write_at(19, 43, "Mouse: PS/2 driver pending", Color::DarkGray, Color::Black);
    }

    fn draw_status(&self) {
        let (tx, rx) = crate::net::stats();
        let focus = match self.focus {
            Focus::Notepad => "notepad",
            Focus::Cli => "cli",
        };
        let status = format!(
            "focus={} note={} bytes mesh tx={} rx={}",
            focus,
            self.note.len(),
            tx,
            rx
        );
        vga_buffer::fill_rect(23, 0, 2, 80, ' ', Color::Black, Color::LightGray);
        vga_buffer::write_at(23, 2, trim_to(&status, 75), Color::Black, Color::LightGray);
        vga_buffer::write_at(24, 2, "Commands: ui/notepad from shell. In UI: note save Memory key, net send Memory key value", Color::Black, Color::LightGray);
    }
}

fn draw_box(row: usize, col: usize, height: usize, width: usize, active: bool, title: &str) {
    let color = if active { Color::Yellow } else { Color::DarkGray };
    for x in col..col + width {
        vga_buffer::put_char_at(row, x, '-', color, Color::Black);
        vga_buffer::put_char_at(row + height - 1, x, '-', color, Color::Black);
    }
    for y in row..row + height {
        vga_buffer::put_char_at(y, col, '|', color, Color::Black);
        vga_buffer::put_char_at(y, col + width - 1, '|', color, Color::Black);
    }
    vga_buffer::put_char_at(row, col, '+', color, Color::Black);
    vga_buffer::put_char_at(row, col + width - 1, '+', color, Color::Black);
    vga_buffer::put_char_at(row + height - 1, col, '+', color, Color::Black);
    vga_buffer::put_char_at(row + height - 1, col + width - 1, '+', color, Color::Black);
    vga_buffer::write_at(row, col + 2, title, color, Color::Black);
}

fn trim_to(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }

    let mut end = 0;
    for (i, _) in text.char_indices() {
        if i > max {
            break;
        }
        end = i;
    }
    &text[..end]
}
