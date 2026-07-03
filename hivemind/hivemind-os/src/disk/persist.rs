//! Hive + VFS persistence — save to / load from the ATA slave disk.
//!
//! Disk layout:
//!   Sector 0          : magic header  "HIVEMIND\0"
//!   Sectors 1–63      : hive records  (text, variable length)
//!   Sectors 64–127    : VFS records   (text, variable length)
//!
//! Text format (hive):
//!   MEM|<id>|<name>[|<parent_id>]\n
//!   BLOB|<memory_id>|<key>|T:<text> or N:<number> or B:<0/1>\n
//!   EDGE|<from_id>|<to_id>|<edge_type>\n
//!   SIGNAL|<from_id>|<type>|<payload>\n
//!
//! VFS format: produced by `vfs::Vfs::serialize()`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{
    is_present, read_sectors, write_sectors,
    HIVE_SECTOR_COUNT, HIVE_START_SECTOR, SECTOR_SIZE,
    VFS_SECTOR_COUNT, VFS_START_SECTOR,
};
use crate::hive;
use crate::hive::blob::BlobValue;
use crate::hive::EdgeType;
use crate::vfs;

const MAGIC: &[u8] = b"HIVEMIND\x01";

// ── Auto-persistence (debounced) ───────────────────────────────────────────────
//
// Mutating commands call `mark_dirty()`. The shell loop calls `autosave_tick()`
// every tick; once the state has been dirty and quiet for `AUTOSAVE_DELAY` ticks
// it flushes to disk. This gives "it just persists" behaviour without a slow
// disk write on every single keystroke-command.

static DIRTY:      AtomicBool = AtomicBool::new(false);
static DIRTY_TICK: AtomicU64  = AtomicU64::new(0);

/// PIT ≈ 18.2 Hz, so ~54 ticks ≈ 3 seconds of quiet before flushing.
const AUTOSAVE_DELAY: u64 = 54;

/// Flag that persistent state has changed and should be saved soon.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
    DIRTY_TICK.store(crate::interrupts::current_tick(), Ordering::Relaxed);
}

/// Called from the main loop each tick. Flushes to disk once changes have
/// settled. No-op when there is no data disk or nothing has changed.
pub fn autosave_tick(now: u64) {
    if !DIRTY.load(Ordering::Relaxed) || !is_present() {
        return;
    }
    let last = DIRTY_TICK.load(Ordering::Relaxed);
    if now.wrapping_sub(last) < AUTOSAVE_DELAY {
        return;
    }
    // Clear the flag first; if a change lands during the write it will re-arm.
    DIRTY.store(false, Ordering::Relaxed);
    let _ = save();
}

// ── Save ──────────────────────────────────────────────────────────────────────

pub fn save() -> Result<(), &'static str> {
    if !is_present() { return Err("No data disk attached"); }

    // ── Write magic header ────────────────────────────────────────────────────
    let mut header = [0u8; SECTOR_SIZE];
    header[..MAGIC.len()].copy_from_slice(MAGIC);
    write_sectors(0, &header)?;

    // ── Serialize hive ────────────────────────────────────────────────────────
    let hive_bytes = hive::with_hive(serialize_hive);
    let hive_sector_count = (hive_bytes.len() + SECTOR_SIZE - 1) / SECTOR_SIZE;
    let max = (HIVE_SECTOR_COUNT * SECTOR_SIZE as u32) as usize;
    if hive_bytes.len() > max {
        return Err("Hive state too large for disk sectors");
    }
    write_sectors(HIVE_START_SECTOR, &hive_bytes)?;

    // ── Serialize VFS ─────────────────────────────────────────────────────────
    let vfs_bytes = vfs::with_vfs(|v| v.serialize());
    let max_vfs = (VFS_SECTOR_COUNT * SECTOR_SIZE as u32) as usize;
    if vfs_bytes.len() > max_vfs {
        return Err("VFS state too large for disk sectors");
    }
    write_sectors(VFS_START_SECTOR, &vfs_bytes)?;

    Ok(())
}

// ── Load ──────────────────────────────────────────────────────────────────────

pub fn load() -> Result<(), &'static str> {
    if !is_present() { return Err("No data disk attached"); }

    // Check magic
    let mut header_buf = Vec::new();
    read_sectors(0, 1, &mut header_buf)?;
    if !header_buf.starts_with(MAGIC) {
        return Err("Disk has no HiveMind data (not yet saved?)");
    }

    // ── Load hive ─────────────────────────────────────────────────────────────
    let mut hive_buf = Vec::new();
    read_sectors(HIVE_START_SECTOR, HIVE_SECTOR_COUNT, &mut hive_buf)?;
    // Strip trailing nulls
    let hive_data = trim_nulls(&hive_buf);
    if !hive_data.is_empty() {
        hive::with_hive(|h| deserialize_hive(h, hive_data));
    }

    // ── Load VFS ──────────────────────────────────────────────────────────────
    let mut vfs_buf = Vec::new();
    read_sectors(VFS_START_SECTOR, VFS_SECTOR_COUNT, &mut vfs_buf)?;
    let vfs_data = trim_nulls(&vfs_buf);
    if !vfs_data.is_empty() {
        vfs::with_vfs(|v| v.deserialize(vfs_data));
    }

    Ok(())
}

// ── Hive serializer ───────────────────────────────────────────────────────────

fn serialize_hive(h: &mut hive::Hive) -> Vec<u8> {
    use core::fmt::Write as FmtWrite;
    let mut out = String::new();

    for (id, mem) in &h.memories {
        if let Some(pid) = mem.parent_id {
            let _ = write!(out, "MEM|{}|{}|{}\n", id, mem.name, pid);
        } else {
            let _ = write!(out, "MEM|{}|{}\n", id, mem.name);
        }
        for (key, blob) in &mem.blobs {
            let val = match &blob.value {
                BlobValue::Text(s)   => alloc::format!("T:{}", s),
                BlobValue::Number(n) => alloc::format!("N:{}", n),
                BlobValue::Bool(b)   => alloc::format!("B:{}", if *b { 1 } else { 0 }),
                BlobValue::Binary(b) => alloc::format!("X:{}", b.len()), // binary not saved
            };
            let _ = write!(out, "BLOB|{}|{}|{}\n", id, key, val);
        }
    }
    for edge in &h.edges {
        let _ = write!(out, "EDGE|{}|{}|{}\n",
            edge.from_id, edge.to_id, edge.edge_type.name());
    }

    out.into_bytes()
}

// ── Hive deserializer ─────────────────────────────────────────────────────────

fn deserialize_hive(h: &mut hive::Hive, data: &[u8]) {
    // Reset hive (re-init from scratch)
    h.memories.clear();
    h.edges.clear();
    h.signal_log.clear();

    if let Ok(s) = core::str::from_utf8(data) {
        // Two passes: first create memories, then blobs + edges
        // (edges reference both sides by ID)

        for line in s.lines() {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.is_empty() { continue; }
            match parts[0] {
                "MEM" if parts.len() >= 3 => {
                    let id: u64 = match parts[1].parse() { Ok(n) => n, Err(_) => continue };
                    let name = parts[2];
                    let parent: Option<u64> = parts.get(3).and_then(|s| s.parse().ok());
                    let mut node = crate::hive::memory_node::MemoryNode::new(id, name);
                    node.parent_id = parent;
                    h.memories.insert(id, node);
                    // Update ID counter so new nodes don't clash
                    crate::hive::bump_id(id);
                }
                _ => {}
            }
        }

        for line in s.lines() {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.is_empty() { continue; }
            match parts[0] {
                "BLOB" if parts.len() == 4 => {
                    let id: u64 = match parts[1].parse() { Ok(n) => n, Err(_) => continue };
                    let key = parts[2];
                    let val_str = parts[3];
                    let value = if let Some(s) = val_str.strip_prefix("T:") {
                        BlobValue::Text(s.to_string())
                    } else if let Some(s) = val_str.strip_prefix("N:") {
                        s.parse::<i64>().ok().map(BlobValue::Number)
                            .unwrap_or(BlobValue::Text(s.to_string()))
                    } else if let Some(s) = val_str.strip_prefix("B:") {
                        BlobValue::Bool(s == "1")
                    } else {
                        BlobValue::Text(val_str.to_string())
                    };
                    h.write_blob(id, key, value);
                }
                "EDGE" if parts.len() == 4 => {
                    let from: u64 = match parts[1].parse() { Ok(n) => n, Err(_) => continue };
                    let to:   u64 = match parts[2].parse() { Ok(n) => n, Err(_) => continue };
                    let etype = match parts[3] {
                        "Sync"       => EdgeType::Sync,
                        "Signal"     => EdgeType::Signal,
                        "Mirror"     => EdgeType::Mirror,
                        "Dependency" => EdgeType::Dependency,
                        _            => EdgeType::Sync,
                    };
                    h.edges.push(crate::hive::MemoryEdge { from_id: from, to_id: to, edge_type: etype });
                }
                _ => {}
            }
        }
    }
}

fn trim_nulls(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(0);
    &data[..end]
}
