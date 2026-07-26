use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The atomic data unit of the hive.
#[derive(Clone, Debug)]
pub enum BlobValue {
    Text(String),
    Number(i64),
    Bool(bool),
    Binary(Vec<u8>),
}

impl BlobValue {
    /// Human-readable representation without std formatting.
    pub fn display(&self) -> String {
        use core::fmt::Write as FmtWrite;
        match self {
            BlobValue::Text(s)    => s.clone(),
            BlobValue::Number(n)  => {
                let mut s = String::new();
                let _ = write!(s, "{}", n);
                s
            }
            BlobValue::Bool(b)    => if *b { "true".to_string() } else { "false".to_string() },
            BlobValue::Binary(b)  => {
                let mut s = String::new();
                let _ = write!(s, "<binary {} bytes>", b.len());
                s
            }
        }
    }

    /// Parse a string into a BlobValue: numbers, booleans, then text.
    pub fn parse(s: &str) -> BlobValue {
        if let Ok(n) = s.parse::<i64>() {
            return BlobValue::Number(n);
        }
        match s {
            "true"  => BlobValue::Bool(true),
            "false" => BlobValue::Bool(false),
            other   => BlobValue::Text(other.to_string()),
        }
    }

    /// Encode for the tiered-memory page store (same tags as disk persistence).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            BlobValue::Text(s)   => { let mut v = alloc::vec![b'T', b':']; v.extend_from_slice(s.as_bytes()); v }
            BlobValue::Number(n) => alloc::format!("N:{}", n).into_bytes(),
            BlobValue::Bool(b)   => alloc::format!("B:{}", if *b { 1 } else { 0 }).into_bytes(),
            BlobValue::Binary(b) => { let mut v = alloc::vec![b'X', b':']; v.extend_from_slice(b); v }
        }
    }

    /// Decode a value previously produced by `encode`.
    pub fn decode(bytes: &[u8]) -> BlobValue {
        if bytes.len() >= 2 && &bytes[..2] == b"T:" {
            BlobValue::Text(String::from_utf8_lossy(&bytes[2..]).into_owned())
        } else if bytes.len() >= 2 && &bytes[..2] == b"N:" {
            core::str::from_utf8(&bytes[2..]).ok().and_then(|s| s.parse().ok())
                .map(BlobValue::Number).unwrap_or(BlobValue::Number(0))
        } else if bytes.len() >= 2 && &bytes[..2] == b"B:" {
            BlobValue::Bool(bytes.get(2) == Some(&b'1'))
        } else if bytes.len() >= 2 && &bytes[..2] == b"X:" {
            BlobValue::Binary(bytes[2..].to_vec())
        } else {
            BlobValue::Text(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

/// An individual key-value entry inside a MemoryNode.
#[derive(Clone, Debug)]
pub struct Blob {
    pub id:              u64,
    pub key:             String,
    pub value:           BlobValue,
    pub owner_memory_id: u64,
    pub created_tick:    u64,
    pub modified_tick:   u64,
    /// Some(sector) when the value has been paged out to disk (tiered memory).
    /// While paged, `value` holds an empty placeholder and the real bytes live
    /// on disk; a read pages it back in.
    pub paged_sector:    Option<u32>,
}

impl Blob {
    pub fn new(id: u64, key: &str, value: BlobValue, owner: u64) -> Self {
        let tick = crate::interrupts::current_tick();
        Blob {
            id,
            key:             key.to_string(),
            value,
            owner_memory_id: owner,
            created_tick:    tick,
            modified_tick:   tick,
            paged_sector:    None,
        }
    }
}
