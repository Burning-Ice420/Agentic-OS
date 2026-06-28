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
        }
    }
}
