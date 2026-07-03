/// HiveMind OS — interactive shell
///
/// Keyboard characters are pushed from the keyboard interrupt handler via
/// `push_key()`.  The `run()` loop polls the ring buffer and processes lines.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::agent::Condition;
use crate::vfs;
use spin::Mutex;

use crate::hive;
use crate::hive::blob::BlobValue;
use crate::hive::EdgeType;
use crate::print;
use crate::println;
use crate::vga_buffer::{self, Color};

// ── Keyboard ring buffer ──────────────────────────────────────────────────────
// Uses a fixed-size array so it's safe before heap init.

const KEY_BUF: usize = 256;

struct KeyRing {
    data:  [char; KEY_BUF],
    head:  usize,
    tail:  usize,
}

impl KeyRing {
    const fn new() -> Self {
        KeyRing { data: ['\0'; KEY_BUF], head: 0, tail: 0 }
    }

    fn push(&mut self, c: char) {
        let next = (self.tail + 1) % KEY_BUF;
        if next != self.head {
            self.data[self.tail] = c;
            self.tail = next;
        }
    }

    fn pop(&mut self) -> Option<char> {
        if self.head == self.tail {
            None
        } else {
            let c    = self.data[self.head];
            self.head = (self.head + 1) % KEY_BUF;
            Some(c)
        }
    }
}

static KEYS: Mutex<KeyRing> = Mutex::new(KeyRing::new());

// ── Special-key encoding ────────────────────────────────────────────────────
// Non-printable keys are funnelled through the same `char` ring using otherwise
// unused ASCII control codes. Consumers (shell + desktop) match on these.

pub const KEY_UP:    char = '\u{11}';
pub const KEY_DOWN:  char = '\u{12}';
pub const KEY_LEFT:  char = '\u{13}';
pub const KEY_RIGHT: char = '\u{14}';
pub const KEY_PGUP:  char = '\u{15}';
pub const KEY_PGDN:  char = '\u{16}';
pub const KEY_HOME:  char = '\u{17}';
pub const KEY_END:   char = '\u{18}';
pub const KEY_DEL:   char = '\u{7f}';

/// Map a `pc_keyboard` raw (non-Unicode) key to our control-code encoding.
pub fn rawkey_to_char(code: pc_keyboard::KeyCode) -> Option<char> {
    use pc_keyboard::KeyCode;
    Some(match code {
        KeyCode::Backspace  => '\x08',
        KeyCode::Escape     => '\x1b',
        KeyCode::Tab        => '\t',
        KeyCode::Delete     => KEY_DEL,
        KeyCode::ArrowUp    => KEY_UP,
        KeyCode::ArrowDown  => KEY_DOWN,
        KeyCode::ArrowLeft  => KEY_LEFT,
        KeyCode::ArrowRight => KEY_RIGHT,
        KeyCode::PageUp     => KEY_PGUP,
        KeyCode::PageDown   => KEY_PGDN,
        KeyCode::Home       => KEY_HOME,
        KeyCode::End        => KEY_END,
        _ => return None,
    })
}

/// Called from the keyboard interrupt handler.
pub fn push_key(c: char) {
    KEYS.lock().push(c);
}

pub fn read_key() -> Option<char> {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| KEYS.lock().pop())
}

// ── Prompt ────────────────────────────────────────────────────────────────────

fn print_prompt() {
    vga_buffer::set_color(Color::LightCyan, Color::Black);
    print!("hive");
    vga_buffer::set_color(Color::White, Color::Black);
    print!(">");
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    print!(" ");
}

// ── Main REPL loop ────────────────────────────────────────────────────────────

pub fn run() -> ! {
    print_prompt();
    let mut line = String::new();
    let mut last_tick: u64 = 0;

    loop {
        // Poll COM2 for incoming mesh messages from peer VMs.
        crate::net::poll_and_apply();

        // Periodic work, gated on the tick actually advancing so it runs at a
        // steady rate regardless of how fast the loop spins.
        let t = crate::interrupts::current_tick();
        if t != last_tick {
            last_tick = t;
            // PIT default ≈ 18.2 Hz → roughly once per second.
            if t % 18 == 0 {
                crate::agent::tick_all();
            }
            crate::disk::persist::autosave_tick(t);
        }

        while let Some(c) = read_key() {
            match c {
                // ── scrollback navigation ──
                KEY_PGUP => vga_buffer::scroll_up(),
                KEY_PGDN => vga_buffer::scroll_down(),
                KEY_UP   => vga_buffer::scroll_line_up(),
                KEY_DOWN => vga_buffer::scroll_line_down(),
                KEY_HOME => vga_buffer::scroll_home(),
                KEY_END  => vga_buffer::scroll_end(),
                '\n' => {
                    println!();
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        vga_buffer::set_color(Color::LightGreen, Color::Black);
                        execute(&trimmed);
                    }
                    line.clear();
                    print_prompt();
                }
                '\x08' => {
                    // Backspace
                    if !line.is_empty() {
                        line.pop();
                        print!("\x08"); // triggers vga_buffer backspace
                    }
                }
                c if c.is_ascii() && !c.is_control() => {
                    line.push(c);
                    print!("{}", c);
                }
                _ => {}
            }
        }

        // Sleep until the next interrupt (timer or keyboard) instead of busy
        // spinning. Keeps the CPU cool and the keyboard responsive.
        x86_64::instructions::hlt();
    }
}

// ── Command dispatcher ────────────────────────────────────────────────────────

fn execute(line: &str) {
    let parts: Vec<&str> = line.splitn(16, ' ').collect();

    // Commands that change persistent state → flag for the debounced autosave.
    if matches!(
        parts[0],
        "mem" | "m" | "blob" | "b" | "link" | "signal" | "s"
            | "mkdir" | "touch" | "write" | "rm" | "agent" | "ag"
    ) {
        crate::disk::persist::mark_dirty();
    }

    match parts[0] {
        "help"          => cmd_help(),
        "clear"         => vga_buffer::clear_screen(),
        "ui" | "desktop" | "notepad" => crate::desktop::run(),
        "hive"          => cmd_hive(),
        "mem" | "m"     => cmd_mem(&parts[1..]),
        "blob" | "b"    => cmd_blob(&parts[1..]),
        "link"          => cmd_link(&parts[1..]),
        "signal" | "s"  => cmd_signal(&parts[1..]),
        "log"           => cmd_log(),
        "tick"          => {
            let t = crate::interrupts::current_tick();
            println!("  System ticks: {}", t);
        }
        "halt"          => {
            // Flush any pending changes so nothing is lost on shutdown.
            if crate::disk::is_present() {
                println!("  Flushing state to disk...");
                let _ = crate::disk::persist::save();
            }
            println!("  Halting HiveMind OS. Goodbye.");
            x86_64::instructions::interrupts::disable();
            loop { x86_64::instructions::hlt(); }
        }
        "net"           => cmd_net(&parts[1..]),
        "agent" | "ag"  => cmd_agent(&parts[1..]),
        // ── filesystem ──
        "ls"            => cmd_ls(&parts[1..]),
        "mkdir"         => cmd_mkdir(&parts[1..]),
        "touch"         => cmd_touch(&parts[1..]),
        "write"         => cmd_fs_write(&parts[1..]),
        "cat"           => cmd_cat(&parts[1..]),
        "rm"            => cmd_rm(&parts[1..]),
        "cd"            => cmd_cd(&parts[1..]),
        "pwd"           => {
            let cwd = crate::vfs::with_vfs(|v| v.cwd.clone());
            println!("  {}", cwd);
        }
        // ── persistence ──
        "save"          => cmd_save(),
        "load"          => cmd_load(),
        // ── clock ──
        "time"          => cmd_time(),
        // ── system info + instance identity ──
        "sysinfo" | "whoami" | "uuid" => cmd_sysinfo(),
        // ── process list ──
        "ps"            => cmd_ps(),
        other           => {
            vga_buffer::set_color(Color::LightRed, Color::Black);
            println!("  Unknown command: '{}'. Type 'help'.", other);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
    }
}

// ── help ──────────────────────────────────────────────────────────────────────

fn cmd_help() {
    vga_buffer::set_color(Color::Yellow, Color::Black);
    println!("  ╔══════════════════════════════════════════════════╗");
    println!("  ║          HiveMind OS  —  Shell Commands          ║");
    println!("  ╠══════════════════╦═══════════════════════════════╣");
    println!("  ║ hive             ║ Show hive overview            ║");
    println!("  ║ ui / notepad     ║ Open desktop, notepad, CLI    ║");
    println!("  ║ mem list         ║ List memory nodes             ║");
    println!("  ║ mem new <name>   ║ Create memory node            ║");
    println!("  ║ mem new <n> <id> ║ Create child of node <id>     ║");
    println!("  ║ mem show <id>    ║ Show node + its blobs         ║");
    println!("  ║ blob write <id> <key> <val>                      ║");
    println!("  ║                  ║ Write blob to node            ║");
    println!("  ║ blob read  <id> <key>                            ║");
    println!("  ║                  ║ Read blob from node           ║");
    println!("  ║ link <a> <b> <type>                              ║");
    println!("  ║                  ║ Link nodes (Sync/Signal/      ║");
    println!("  ║                  ║  Mirror/Dependency)           ║");
    println!("  ║ signal <id> <type> <msg>                         ║");
    println!("  ║                  ║ Broadcast signal from node    ║");
    println!("  ║ log              ║ Show signal log               ║");
    println!("  ╠══════════════════╩═══════════════════════════════╣");
    println!("  ║ Filesystem                                       ║");
    println!("  ║ ls [path]         ║ List directory               ║");
    println!("  ║ mkdir <name>      ║ Create directory             ║");
    println!("  ║ touch <name>      ║ Create empty file            ║");
    println!("  ║ write <name> <content>                           ║");
    println!("  ║ cat <name>        ║ Read file                    ║");
    println!("  ║ rm <name>         ║ Remove file or dir           ║");
    println!("  ║ cd <path>         ║ Change directory             ║");
    println!("  ║ pwd               ║ Print working directory      ║");
    println!("  ╠══════════════════╩═══════════════════════════════╣");
    println!("  ║ save              ║ Persist hive+FS to disk      ║");
    println!("  ║ load              ║ Restore hive+FS from disk    ║");
    println!("  ║ time              ║ Show current RTC date/time   ║");
    println!("  ║ sysinfo / whoami  ║ Instance UUID + RAM/CPU/disk ║");
    println!("  ║ ps                ║ Show running agents          ║");
    println!("  ║ tick             ║ Show system tick counter      ║");
    println!("  ║ clear            ║ Clear screen                  ║");
    println!("  ║ halt             ║ Halt the OS                   ║");
    println!("  ╠══════════════════╩═══════════════════════════════╣");
    println!("  ║ Mesh (VM-to-VM over COM2 serial)                 ║");
    println!("  ║ net status       ║ TX/RX message counts          ║");
    println!("  ║ net send <mem> <key> <val>                       ║");
    println!("  ║                  ║ Broadcast blob to peer VMs    ║");
    println!("  ╠══════════════════╩═══════════════════════════════╣");
    println!("  ║ Agents (reactive kernel AI)                      ║");
    println!("  ║ agent list       ║ List agents + rules           ║");
    println!("  ║ agent new <name> <mem_id>                        ║");
    println!("  ║ agent rule <id> <watch> <cond> <akey> <aval>    ║");
    println!("  ║                  ║ cond: gt:N  lt:N  eq:S  any   ║");
    println!("  ║ agent tick       ║ Fire all agents now           ║");
    println!("  ╚══════════════════════════════════════════════════╝");
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

// ── hive ──────────────────────────────────────────────────────────────────────

fn cmd_hive() {
    hive::with_hive(|h| {
        let tick = crate::interrupts::current_tick();
        println!("  HiveMind OS — kernel hive");
        println!("  Memory nodes : {}", h.memories.len());
        println!("  Total blobs  : {}", h.total_blobs());
        println!("  Edges        : {}", h.total_edges());
        println!("  Signal log   : {} entries", h.signal_log.len());
        println!("  System tick  : {}", tick);
    });
}

// ── mem ───────────────────────────────────────────────────────────────────────

fn cmd_mem(args: &[&str]) {
    match args.first().copied() {
        Some("list") | Some("ls") | None => {
            hive::with_hive(|h| {
                if h.memories.is_empty() {
                    println!("  No memory nodes.");
                    return;
                }
                println!("  {:<6} {:<22} {:<6} {:<8}", "ID", "Name", "Blobs", "Parent");
                println!("  {}", "─".repeat(46));
                for (id, node) in &h.memories {
                    let parent = node.parent_id.map(|p| {
                        let mut s = String::new();
                        use core::fmt::Write;
                        let _ = write!(s, "{}", p);
                        s
                    }).unwrap_or_else(|| "—".to_string());
                    println!("  {:<6} {:<22} {:<6} {}", id, node.name, node.blobs.len(), parent);
                }
            });
        }
        Some("new") | Some("create") => {
            if args.len() < 2 {
                println!("  Usage: mem new <name> [parent_id]");
                return;
            }
            let name   = args[1];
            let parent = args.get(2).and_then(|s| s.parse::<u64>().ok());
            let id     = hive::with_hive(|h| h.create_memory(name, parent));
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  Created memory '{}' → id {}", name, id);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Some("show") => {
            let id = match args.get(1).and_then(|s| s.parse::<u64>().ok()) {
                Some(n) => n,
                None    => { println!("  Usage: mem show <id>"); return; }
            };
            hive::with_hive(|h| {
                match h.memories.get(&id) {
                    None      => println!("  Memory node {} not found.", id),
                    Some(mem) => {
                        println!("  Memory #{} — '{}'", mem.id, mem.name);
                        println!("    Parent     : {:?}", mem.parent_id);
                        println!("    Children   : {:?}", mem.children);
                        println!("    Subscribed : {:?}", mem.subscriptions);
                        println!("    Blobs ({}):", mem.blobs.len());
                        if mem.blobs.is_empty() {
                            println!("      (none)");
                        }
                        for (k, blob) in &mem.blobs {
                            println!("      [{}] {} = {}  (t={})",
                                blob.id, k, blob.value.display(), blob.modified_tick);
                        }
                    }
                }
            });
        }
        Some(other) => println!("  Unknown mem sub-command: '{}'. Try: list, new, show", other),
    }
}

// ── blob ──────────────────────────────────────────────────────────────────────

fn cmd_blob(args: &[&str]) {
    match args.first().copied() {
        Some("write") | Some("w") => {
            if args.len() < 4 {
                println!("  Usage: blob write <memory_id> <key> <value>");
                return;
            }
            let id: u64 = match args[1].parse() {
                Ok(n)  => n,
                Err(_) => { println!("  Invalid memory id."); return; }
            };
            let key   = args[2];
            let value = BlobValue::parse(&args[3..].join(" "));
            let ok    = hive::with_hive(|h| h.write_blob(id, key, value));
            if ok {
                vga_buffer::set_color(Color::White, Color::Black);
                println!("  Written '{}' to memory {}.", key, id);
                vga_buffer::set_color(Color::LightGreen, Color::Black);
            } else {
                println!("  Memory {} not found.", id);
            }
        }
        Some("read") | Some("r") => {
            if args.len() < 3 {
                println!("  Usage: blob read <memory_id> <key>");
                return;
            }
            let id: u64 = match args[1].parse() {
                Ok(n)  => n,
                Err(_) => { println!("  Invalid memory id."); return; }
            };
            let key = args[2];
            hive::with_hive(|h| {
                match h.read_blob(id, key) {
                    None       => println!("  Blob '{}' not found in memory {}.", key, id),
                    Some(blob) => {
                        vga_buffer::set_color(Color::White, Color::Black);
                        println!("  {} = {}", blob.key, blob.value.display());
                        vga_buffer::set_color(Color::LightGreen, Color::Black);
                        println!("    created  t={}", blob.created_tick);
                        println!("    modified t={}", blob.modified_tick);
                    }
                }
            });
        }
        _ => println!("  Usage: blob [write|read] ..."),
    }
}

// ── link ──────────────────────────────────────────────────────────────────────

fn cmd_link(args: &[&str]) {
    if args.len() < 3 {
        println!("  Usage: link <from_id> <to_id> <Sync|Signal|Mirror|Dependency>");
        return;
    }
    let from: u64 = match args[0].parse() { Ok(n) => n, Err(_) => { println!("  Invalid from_id."); return; } };
    let to:   u64 = match args[1].parse() { Ok(n) => n, Err(_) => { println!("  Invalid to_id."); return; } };
    let edge = match args[2] {
        "Sync"       | "sync"       => EdgeType::Sync,
        "Signal"     | "signal"     => EdgeType::Signal,
        "Mirror"     | "mirror"     => EdgeType::Mirror,
        "Dependency" | "dependency" => EdgeType::Dependency,
        other => { println!("  Unknown edge type: '{}'. Use: Sync Signal Mirror Dependency", other); return; }
    };
    let type_name = edge.name().to_string();
    let ok = hive::with_hive(|h| h.link_memories(from, to, edge));
    if ok {
        vga_buffer::set_color(Color::White, Color::Black);
        println!("  Linked {} --{}→ {}", from, type_name, to);
        vga_buffer::set_color(Color::LightGreen, Color::Black);
    } else {
        println!("  One or both memory nodes not found.");
    }
}

// ── signal ────────────────────────────────────────────────────────────────────

fn cmd_signal(args: &[&str]) {
    if args.len() < 3 {
        println!("  Usage: signal <memory_id> <type> <message...>");
        return;
    }
    let id: u64 = match args[0].parse() { Ok(n) => n, Err(_) => { println!("  Invalid memory id."); return; } };
    let sig_type = args[1];
    let payload  = args[2..].join(" ");

    let exists = hive::with_hive(|h| h.memories.contains_key(&id));
    if !exists {
        println!("  Memory {} not found.", id);
        return;
    }
    hive::with_hive(|h| h.broadcast_signal(id, sig_type, &payload));
    vga_buffer::set_color(Color::White, Color::Black);
    println!("  Signal '{}' broadcast from memory {}.", sig_type, id);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

// ── log ───────────────────────────────────────────────────────────────────────

fn cmd_log() {
    hive::with_hive(|h| {
        if h.signal_log.is_empty() {
            println!("  Signal log empty.");
            return;
        }
        println!("  Signal log (last {}):", h.signal_log.len());
        for sig in h.signal_log.iter().rev().take(20) {
            println!("  t={:<8} mem#{} — {} : {}", sig.tick, sig.from_id, sig.signal_type, sig.payload);
        }
    });
}
// \u2500\u2500 net \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\n
fn cmd_net(args: &[&str]) {
    match args.first().copied() {
        Some("status") | None => {
            let (tx, rx) = crate::net::stats();
            println!("  COM2 Mesh Serial:");
            println!("    TX (sent to peers) : {}", tx);
            println!("    RX (from peers)    : {}", rx);
            if tx == 0 && rx == 0 {
                println!("    No traffic yet.");
                println!("    Launch a second VM: .\\run-os.ps1 -VMCount 2");
            }
        }
        Some("send") => {
            if args.len() < 4 {
                println!("  Usage: net send <memory_name> <key> <value>");
                return;
            }
            let mem = args[1];
            let key = args[2];
            let val = args[3..].join(" ");
            crate::net::send_blob(mem, key, &val);
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  Sent -> peer VMs: {}:{} = {}", mem, key, val);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Some(other) => {
            println!("  Unknown net sub-command: '{}'. Try: status, send", other);
        }
    }
}

// \u2500\u2500 agent \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

fn cmd_agent(args: &[&str]) {
    match args.first().copied() {
        Some("list") | Some("ls") | None => {
            crate::agent::with_agents(|agents| {
                if agents.is_empty() {
                    println!("  No agents.");
                    return;
                }
                for ag in agents {
                    println!("  Agent #{} '{}' -> memory #{}", ag.id, ag.name, ag.memory_id);
                    println!("    Triggered: {} time(s)", ag.trigger_count);
                    if ag.rules.is_empty() {
                        println!("    Rules: (none)");
                    } else {
                        for r in &ag.rules {
                            println!("    Rule: if [{}] {} -> write [{}] = {}",
                                r.watch_key, r.condition.describe(),
                                r.action_key, r.action_value);
                        }
                    }
                }
            });
        }
        Some("new") | Some("create") => {
            if args.len() < 3 {
                println!("  Usage: agent new <name> <memory_id>");
                return;
            }
            let name = args[1];
            let mid: u64 = match args[2].parse() {
                Ok(n)  => n,
                Err(_) => { println!("  Invalid memory_id (must be a number)."); return; }
            };
            let id = crate::agent::create_agent(name, mid);
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  Agent '{}' created -> id #{}, watching memory #{}", name, id, mid);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Some("rule") => {
            // agent rule <id> <watch_key> <condition> <action_key> <action_value>
            if args.len() < 6 {
                println!("  Usage: agent rule <id> <watch_key> <cond> <action_key> <action_val>");
                println!("  Conditions:  gt:80  lt:20  eq:online  any");
                return;
            }
            let id: u64 = match args[1].parse() {
                Ok(n)  => n,
                Err(_) => { println!("  Invalid agent id."); return; }
            };
            let watch      = args[2];
            let cond_str   = args[3];
            let action_key = args[4];
            let action_val = args[5..].join(" ");
            let cond = match Condition::parse(cond_str) {
                Some(c) => c,
                None    => {
                    println!("  Invalid condition '{}'. Use: gt:N  lt:N  eq:str  any", cond_str);
                    return;
                }
            };
            if crate::agent::add_rule(id, watch, cond, action_key, &action_val) {
                vga_buffer::set_color(Color::White, Color::Black);
                println!("  Rule added to agent #{}.", id);
                vga_buffer::set_color(Color::LightGreen, Color::Black);
            } else {
                println!("  Agent #{} not found.", id);
            }
        }
        Some("tick") => {
            crate::agent::tick_all();
            println!("  All agents ticked.");
        }
        Some(other) => {
            println!("  Unknown agent sub-command: '{}'. Try: list, new, rule, tick", other);
        }
    }
}

// ── ls ────────────────────────────────────────────────────────────────────────

fn cmd_ls(args: &[&str]) {
    let path_arg = args.first().copied().unwrap_or(".");
    let abs_path = vfs::with_vfs(|v| vfs::resolve(&v.cwd, path_arg));
    let result   = vfs::with_vfs(|v| v.ls(&abs_path));
    match result {
        Err(e)       => println!("  ls: {}", e),
        Ok(entries)  => {
            println!("  {}/", abs_path);
            if entries.is_empty() {
                println!("  (empty)");
            } else {
                for (name, is_dir) in &entries {
                    if *is_dir {
                        vga_buffer::set_color(Color::LightCyan, Color::Black);
                        println!("  {}/", name);
                    } else {
                        vga_buffer::set_color(Color::LightGreen, Color::Black);
                        println!("  {}", name);
                    }
                }
                vga_buffer::set_color(Color::LightGreen, Color::Black);
            }
        }
    }
}

fn cmd_mkdir(args: &[&str]) {
    let name = match args.first() { Some(&n) => n, None => { println!("  Usage: mkdir <name>"); return; } };
    let abs  = vfs::with_vfs(|v| vfs::resolve(&v.cwd, name));
    match vfs::with_vfs(|v| v.mkdir(&abs)) {
        Ok(()) => println!("  Created: {}", abs),
        Err(e) => println!("  mkdir: {}", e),
    }
}

fn cmd_touch(args: &[&str]) {
    let name = match args.first() { Some(&n) => n, None => { println!("  Usage: touch <name>"); return; } };
    let abs  = vfs::with_vfs(|v| vfs::resolve(&v.cwd, name));
    match vfs::with_vfs(|v| v.touch(&abs)) {
        Ok(()) => println!("  Created: {}", abs),
        Err(e) => println!("  touch: {}", e),
    }
}

fn cmd_fs_write(args: &[&str]) {
    if args.len() < 2 { println!("  Usage: write <name> <content...>"); return; }
    let name    = args[0];
    let content = args[1..].join(" ");
    let abs     = vfs::with_vfs(|v| vfs::resolve(&v.cwd, name));
    match vfs::with_vfs(|v| v.write_file(&abs, content.as_bytes())) {
        Ok(()) => {
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  Written {} bytes to {}", content.len(), abs);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Err(e) => println!("  write: {}", e),
    }
}

fn cmd_cat(args: &[&str]) {
    let name = match args.first() { Some(&n) => n, None => { println!("  Usage: cat <name>"); return; } };
    let abs  = vfs::with_vfs(|v| vfs::resolve(&v.cwd, name));
    let result = vfs::with_vfs(|v| v.read_file(&abs).map(|d| d.to_vec()));
    match result {
        Err(e)   => println!("  cat: {}", e),
        Ok(data) => {
            if let Ok(s) = core::str::from_utf8(&data) {
                vga_buffer::set_color(Color::White, Color::Black);
                println!("{}", s);
                vga_buffer::set_color(Color::LightGreen, Color::Black);
            } else {
                println!("  (binary file, {} bytes)", data.len());
            }
        }
    }
}

fn cmd_rm(args: &[&str]) {
    let name = match args.first() { Some(&n) => n, None => { println!("  Usage: rm <name>"); return; } };
    let abs  = vfs::with_vfs(|v| vfs::resolve(&v.cwd, name));
    match vfs::with_vfs(|v| v.rm(&abs)) {
        Ok(()) => println!("  Removed: {}", abs),
        Err(e) => println!("  rm: {}", e),
    }
}

fn cmd_cd(args: &[&str]) {
    let path = args.first().copied().unwrap_or("/");
    match vfs::with_vfs(|v| v.cd(path)) {
        Ok(())  => {}
        Err(e)  => println!("  cd: {}", e),
    }
}

fn cmd_save() {
    println!("  Saving hive + VFS to disk...");
    match crate::disk::persist::save() {
        Ok(()) => {
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  [OK] Saved.");
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Err(e) => {
            vga_buffer::set_color(Color::LightRed, Color::Black);
            println!("  [FAIL] {}", e);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
    }
}

fn cmd_load() {
    println!("  Loading hive + VFS from disk...");
    match crate::disk::persist::load() {
        Ok(()) => {
            vga_buffer::set_color(Color::White, Color::Black);
            println!("  [OK] State restored.");
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
        Err(e) => {
            vga_buffer::set_color(Color::LightRed, Color::Black);
            println!("  [FAIL] {}", e);
            vga_buffer::set_color(Color::LightGreen, Color::Black);
        }
    }
}

fn cmd_sysinfo() {
    vga_buffer::set_color(Color::Yellow, Color::Black);
    println!("  HiveMind OS — instance & resources");
    vga_buffer::set_color(Color::White, Color::Black);

    crate::sysinfo::with_uuid(|u| println!("  Instance UUID : {}", u));

    // CPU brand (CPUID), if available.
    let mut brand = [0u8; 48];
    if crate::sysinfo::cpu_brand(&mut brand) {
        let s = core::str::from_utf8(&brand).unwrap_or("").trim_matches(|c| c == ' ' || c == '\0');
        println!("  CPU          : {}", s);
    }
    println!("  CPU cores    : 1 (single-core kernel)");

    let ram = crate::sysinfo::total_ram();
    println!("  Usable RAM   : {} MiB ({} bytes)", ram / (1024 * 1024), ram);
    let heap = crate::sysinfo::heap_size();
    println!("  Kernel heap  : {} MiB", heap / (1024 * 1024));

    let disk = crate::disk::capacity_bytes();
    if crate::disk::is_present() && disk > 0 {
        println!("  Data disk    : {} KiB (ATA slave, present)", disk / 1024);
    } else if crate::disk::is_present() {
        println!("  Data disk    : present (size unknown)");
    } else {
        println!("  Data disk    : none");
    }

    let (tx, rx) = crate::net::stats();
    println!("  Mesh COM2    : tx={} rx={}", tx, rx);
    println!("  System tick  : {}", crate::interrupts::current_tick());
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

fn cmd_time() {
    let dt = crate::rtc::read();
    let (date, time) = dt.display();
    let d = core::str::from_utf8(&date).unwrap_or("??");
    let t = core::str::from_utf8(&time).unwrap_or("??");
    vga_buffer::set_color(Color::White, Color::Black);
    println!("  {}  {}", d, t);
    vga_buffer::set_color(Color::LightGreen, Color::Black);
}

fn cmd_ps() {
    let tick = crate::interrupts::current_tick();
    println!("  {:<4} {:<24} {:<6} Triggers", "ID", "Name", "Mem#");
    println!("  {}", "-".repeat(46));
    crate::agent::with_agents(|agents| {
        if agents.is_empty() { println!("  (no agents running)"); return; }
        for ag in agents {
            println!("  {:<4} {:<24} {:<6} {}", ag.id, ag.name, ag.memory_id, ag.trigger_count);
        }
    });
    println!("  System tick: {}", tick);
}
