//! Virtual Filesystem — in-memory tree (RAM disk).
//!
//! Provides a POSIX-like file/directory API.
//! All state lives in RAM; use `crate::disk::persist` to flush to disk.
//!
//! Structure
//!   /              ← root
//!   /boot/         ← kernel config
//!   /hive/         ← auto-generated hive snapshots
//!   /user/         ← user files

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

// ── Node ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum VfsNode {
    File(Vec<u8>),
    Dir(BTreeMap<String, VfsNode>),
}

impl VfsNode {
    fn is_dir(&self) -> bool { matches!(self, VfsNode::Dir(_)) }
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Split an absolute path into components, discarding empty segments.
fn split_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Resolve `cwd` + relative-or-absolute `path` into a canonical absolute path.
pub fn resolve(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let base = if path.starts_with('/') { "" } else { cwd };
    for seg in base.split('/').chain(path.split('/')).filter(|s| !s.is_empty()) {
        match seg {
            ".."  => { parts.pop(); }
            "."   => {}
            other => parts.push(other),
        }
    }
    if parts.is_empty() { "/".to_string() }
    else { alloc::format!("/{}", parts.join("/")) }
}

// ── Filesystem ────────────────────────────────────────────────────────────────

pub struct Vfs {
    root: BTreeMap<String, VfsNode>,
    pub cwd: String,
}

impl Vfs {
    fn new() -> Self {
        let mut root = BTreeMap::new();
        // Pre-create standard directories
        root.insert("boot".to_string(), VfsNode::Dir(BTreeMap::new()));
        root.insert("hive".to_string(), VfsNode::Dir(BTreeMap::new()));
        root.insert("user".to_string(), VfsNode::Dir(BTreeMap::new()));

        // Default boot config
        if let Some(VfsNode::Dir(boot)) = root.get_mut("boot") {
            boot.insert(
                "config.txt".to_string(),
                VfsNode::File(b"# HiveMind OS boot config\nport=8080\nlog_level=info\n".to_vec()),
            );
        }

        Vfs { root, cwd: "/".to_string() }
    }

    /// Walk path components, returning a mutable reference to the target dir map.
    fn dir_at_mut<'a>(
        root: &'a mut BTreeMap<String, VfsNode>,
        parts: &[&str],
    ) -> Option<&'a mut BTreeMap<String, VfsNode>> {
        let mut cur = root;
        for &part in parts {
            cur = match cur.get_mut(part)? {
                VfsNode::Dir(d) => d,
                _ => return None,
            };
        }
        Some(cur)
    }

    fn dir_at<'a>(
        root: &'a BTreeMap<String, VfsNode>,
        parts: &[&str],
    ) -> Option<&'a BTreeMap<String, VfsNode>> {
        let mut cur = root;
        for &part in parts {
            cur = match cur.get(part)? {
                VfsNode::Dir(d) => d,
                _ => return None,
            };
        }
        Some(cur)
    }

    // ── Public operations ─────────────────────────────────────────────────────

    /// List contents of `path`. Returns (name, is_dir) pairs.
    pub fn ls(&self, path: &str) -> Result<Vec<(String, bool)>, &'static str> {
        let parts = split_path(path);
        let dir = Self::dir_at(&self.root, &parts).ok_or("Directory not found")?;
        let mut entries: Vec<(String, bool)> = dir
            .iter()
            .map(|(k, v)| (k.clone(), v.is_dir()))
            .collect();
        entries.sort_by(|a, b| {
            // Directories first
            b.1.cmp(&a.1).then(a.0.cmp(&b.0))
        });
        Ok(entries)
    }

    /// Create a directory at `path`.
    pub fn mkdir(&mut self, path: &str) -> Result<(), &'static str> {
        let parts = split_path(path);
        let (parent_parts, name) = parts.split_at(parts.len().saturating_sub(1));
        let name = name.first().copied().ok_or("Invalid path")?;
        let dir = Self::dir_at_mut(&mut self.root, parent_parts).ok_or("Parent not found")?;
        if dir.contains_key(name) { return Err("Already exists"); }
        dir.insert(name.to_string(), VfsNode::Dir(BTreeMap::new()));
        Ok(())
    }

    /// Create or truncate a file at `path` (empty content).
    pub fn touch(&mut self, path: &str) -> Result<(), &'static str> {
        self.write_file(path, &[])
    }

    /// Write bytes to a file (creates if absent, overwrites if present).
    pub fn write_file(&mut self, path: &str, content: &[u8]) -> Result<(), &'static str> {
        let parts = split_path(path);
        let (parent_parts, name) = parts.split_at(parts.len().saturating_sub(1));
        let name = name.first().copied().ok_or("Invalid path")?;
        let dir = Self::dir_at_mut(&mut self.root, parent_parts).ok_or("Parent directory not found")?;
        dir.insert(name.to_string(), VfsNode::File(content.to_vec()));
        Ok(())
    }

    /// Read a file's contents.
    pub fn read_file(&self, path: &str) -> Result<&[u8], &'static str> {
        let parts = split_path(path);
        let (parent_parts, name) = parts.split_at(parts.len().saturating_sub(1));
        let name = name.first().copied().ok_or("Invalid path")?;
        let dir = Self::dir_at(&self.root, parent_parts).ok_or("Directory not found")?;
        match dir.get(name) {
            Some(VfsNode::File(data)) => Ok(data),
            Some(VfsNode::Dir(_)) => Err("Is a directory"),
            None => Err("File not found"),
        }
    }

    /// Remove a file or empty directory.
    pub fn rm(&mut self, path: &str) -> Result<(), &'static str> {
        let parts = split_path(path);
        let (parent_parts, name) = parts.split_at(parts.len().saturating_sub(1));
        let name = name.first().copied().ok_or("Invalid path")?;
        let dir = Self::dir_at_mut(&mut self.root, parent_parts).ok_or("Parent not found")?;
        match dir.get(name) {
            Some(VfsNode::Dir(d)) if !d.is_empty() => Err("Directory not empty"),
            Some(_) => { dir.remove(name); Ok(()) }
            None => Err("Not found"),
        }
    }

    /// Change working directory.
    pub fn cd(&mut self, path: &str) -> Result<(), &'static str> {
        let abs = resolve(&self.cwd, path);
        if abs == "/" {
            self.cwd = "/".to_string();
            return Ok(());
        }
        let parts = split_path(&abs);
        if Self::dir_at(&self.root, &parts).is_some() {
            self.cwd = abs;
            Ok(())
        } else {
            Err("Directory not found")
        }
    }

    // ── Serialization (for disk persistence) ─────────────────────────────────

    /// Serialize entire filesystem to a flat list of records, one per line.
    /// Format: `D:/path\n` for dirs, `F:/path|<hex bytes>\n` for files.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        serialize_node(&self.root, "", &mut out);
        out
    }

    /// Restore filesystem from serialized bytes produced by `serialize()`.
    pub fn deserialize(&mut self, data: &[u8]) {
        // Reset to empty (keep standard dirs)
        self.root.clear();
        self.root.insert("boot".to_string(), VfsNode::Dir(BTreeMap::new()));
        self.root.insert("hive".to_string(), VfsNode::Dir(BTreeMap::new()));
        self.root.insert("user".to_string(), VfsNode::Dir(BTreeMap::new()));

        if let Ok(s) = core::str::from_utf8(data) {
            for line in s.lines() {
                if line.starts_with("D:") {
                    let _ = self.mkdir(&line[2..]);
                } else if line.starts_with("F:") {
                    if let Some(pipe) = line.find('|') {
                        let path    = &line[2..pipe];
                        let hex_str = &line[pipe + 1..];
                        let content = hex_decode(hex_str);
                        let _ = self.write_file(path, &content);
                    }
                }
            }
        }
    }
}

fn serialize_node(dir: &BTreeMap<String, VfsNode>, prefix: &str, out: &mut Vec<u8>) {
    for (name, node) in dir {
        let mut path = alloc::format!("{}/{}", prefix, name);
        match node {
            VfsNode::Dir(sub) => {
                // Write dir record
                out.extend_from_slice(b"D:");
                out.extend_from_slice(path.as_bytes());
                out.push(b'\n');
                serialize_node(sub, &path, out);
            }
            VfsNode::File(data) => {
                out.extend_from_slice(b"F:");
                out.extend_from_slice(path.as_bytes());
                out.push(b'|');
                // Encode content as hex to avoid newline issues
                for byte in data {
                    let hi = hex_nibble(byte >> 4);
                    let lo = hex_nibble(byte & 0xF);
                    out.push(hi);
                    out.push(lo);
                }
                out.push(b'\n');
            }
        }
    }
}

fn hex_nibble(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + n - 10 }
}

fn hex_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = hex_val(bytes[i]);
        let lo = hex_val(bytes[i + 1]);
        out.push((hi << 4) | lo);
        i += 2;
    }
    out
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

// ── Global singleton ──────────────────────────────────────────────────────────

static VFS: Mutex<Option<Vfs>> = Mutex::new(None);

pub fn init() {
    *VFS.lock() = Some(Vfs::new());
}

pub fn with_vfs<F, R>(f: F) -> R
where
    F: FnOnce(&mut Vfs) -> R,
{
    f(VFS.lock().as_mut().expect("VFS not initialized"))
}
