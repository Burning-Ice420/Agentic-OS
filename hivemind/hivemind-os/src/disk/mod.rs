//! ATA PIO disk driver — primary bus (0x1F0), slave drive (drive 1).
//!
//! QEMU exposes the second `-drive` as the slave on the primary ATA bus.
//! We use LBA28 addressing and 28-bit PIO read/write.
//!
//! The data disk layout (1 MiB = 2048 sectors × 512 bytes):
//!   Sector 0    : HIVEMIND magic header
//!   Sector 1–63 : Hive state (text records)
//!   Sector 64–127: VFS state (text records)

use spin::Mutex;
use x86_64::instructions::port::Port;

// ── Port map ──────────────────────────────────────────────────────────────────

const DATA:       u16 = 0x1F0;
const FEATURES:   u16 = 0x1F1; // also Error register
const SECTOR_CNT: u16 = 0x1F2;
const LBA_LO:     u16 = 0x1F3;
const LBA_MID:    u16 = 0x1F4;
const LBA_HI:     u16 = 0x1F5;
const DRIVE_HEAD: u16 = 0x1F6;
const STATUS_CMD: u16 = 0x1F7; // read=Status, write=Command
const ALT_STATUS: u16 = 0x3F6; // also Device Control

const CMD_READ:       u8 = 0x20;
const CMD_WRITE:      u8 = 0x30;
const CMD_FLUSH:      u8 = 0xE7;
const CMD_IDENTIFY:   u8 = 0xEC;

const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;

pub const SECTOR_SIZE: usize = 512;
pub const HIVE_START_SECTOR:  u32 = 1;
pub const HIVE_SECTOR_COUNT:  u32 = 63;
pub const VFS_START_SECTOR:   u32 = 64;
pub const VFS_SECTOR_COUNT:   u32 = 64;

// ── Low-level helpers ─────────────────────────────────────────────────────────

struct AtaPorts {
    data:       Port<u16>,
    features:   Port<u8>,
    sector_cnt: Port<u8>,
    lba_lo:     Port<u8>,
    lba_mid:    Port<u8>,
    lba_hi:     Port<u8>,
    drive_head: Port<u8>,
    status_cmd: Port<u8>,
    alt_status: Port<u8>,
}

impl AtaPorts {
    const fn new() -> Self {
        AtaPorts {
            data:       Port::new(DATA),
            features:   Port::new(FEATURES),
            sector_cnt: Port::new(SECTOR_CNT),
            lba_lo:     Port::new(LBA_LO),
            lba_mid:    Port::new(LBA_MID),
            lba_hi:     Port::new(LBA_HI),
            drive_head: Port::new(DRIVE_HEAD),
            status_cmd: Port::new(STATUS_CMD),
            alt_status: Port::new(ALT_STATUS),
        }
    }

    fn status(&mut self) -> u8 {
        unsafe { self.status_cmd.read() }
    }

    /// 400ns delay by reading alt-status 4 times.
    fn delay_400ns(&mut self) {
        for _ in 0..4 { unsafe { self.alt_status.read(); } }
    }

    fn wait_ready(&mut self) -> Result<(), &'static str> {
        self.delay_400ns();
        for _ in 0..100_000u32 {
            let s = self.status();
            if s & STATUS_ERR != 0 { return Err("ATA error"); }
            if s & STATUS_BSY == 0 { return Ok(()); }
        }
        Err("ATA timeout (BSY)")
    }

    fn wait_drq(&mut self) -> Result<(), &'static str> {
        for _ in 0..100_000u32 {
            let s = self.status();
            if s & STATUS_ERR != 0 { return Err("ATA DRQ error"); }
            if s & STATUS_DRQ != 0 { return Ok(()); }
        }
        Err("ATA timeout (DRQ)")
    }

    /// Select slave drive (bit 4) with LBA mode (bit 6) and upper LBA bits.
    fn select_slave_lba28(&mut self, lba: u32) {
        unsafe {
            // 0xF0 = slave (bit 4) + LBA (bit 6) set, bits 24-27 of LBA in low nibble
            self.drive_head.write(0xF0 | ((lba >> 24) as u8 & 0x0F));
        }
    }

    // ── Read one 512-byte sector ──────────────────────────────────────────────

    pub fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        self.wait_ready()?;
        self.select_slave_lba28(lba);
        unsafe {
            self.sector_cnt.write(1);
            self.lba_lo.write((lba & 0xFF) as u8);
            self.lba_mid.write(((lba >> 8) & 0xFF) as u8);
            self.lba_hi.write(((lba >> 16) & 0xFF) as u8);
            self.status_cmd.write(CMD_READ);
        }
        self.wait_drq()?;

        // Read 256 words = 512 bytes
        for i in 0..256 {
            let word: u16 = unsafe { self.data.read() };
            buf[i * 2]     = (word & 0xFF) as u8;
            buf[i * 2 + 1] = (word >> 8)   as u8;
        }
        Ok(())
    }

    // ── Write one 512-byte sector ─────────────────────────────────────────────

    pub fn write_sector(&mut self, lba: u32, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        self.wait_ready()?;
        self.select_slave_lba28(lba);
        unsafe {
            self.sector_cnt.write(1);
            self.lba_lo.write((lba & 0xFF) as u8);
            self.lba_mid.write(((lba >> 8) & 0xFF) as u8);
            self.lba_hi.write(((lba >> 16) & 0xFF) as u8);
            self.status_cmd.write(CMD_WRITE);
        }
        self.wait_drq()?;

        // Write 256 words
        for i in 0..256 {
            let lo = buf[i * 2] as u16;
            let hi = buf[i * 2 + 1] as u16;
            let word = lo | (hi << 8);
            unsafe { self.data.write(word); }
        }

        // Flush write cache
        self.wait_ready()?;
        unsafe { self.status_cmd.write(CMD_FLUSH); }
        self.wait_ready()?;

        Ok(())
    }

    /// Detect whether a drive is present using IDENTIFY.
    pub fn identify_slave(&mut self) -> bool {
        unsafe { self.drive_head.write(0xB0); } // select slave
        self.delay_400ns();
        unsafe {
            self.sector_cnt.write(0);
            self.lba_lo.write(0);
            self.lba_mid.write(0);
            self.lba_hi.write(0);
            self.status_cmd.write(CMD_IDENTIFY);
        }
        let status = self.status();
        if status == 0 { return false; }
        // Wait for DRQ or ERR
        for _ in 0..10_000u32 {
            let s = self.status();
            if s & STATUS_DRQ != 0 { return true; }
            if s & STATUS_ERR != 0 { return false; }
        }
        false
    }
}

// SAFETY: single-threaded kernel, Mutex ensures exclusive access.
unsafe impl Send for AtaPorts {}

static ATA: Mutex<AtaPorts> = Mutex::new(AtaPorts::new());
static DISK_PRESENT: spin::Mutex<bool> = spin::Mutex::new(false);

// ── Public API ────────────────────────────────────────────────────────────────

pub mod persist;

/// Probe for the slave drive. Returns true if present.
pub fn init() -> bool {
    let present = ATA.lock().identify_slave();
    *DISK_PRESENT.lock() = present;
    present
}

pub fn is_present() -> bool {
    *DISK_PRESENT.lock()
}

/// Read `count` sectors starting at `lba` into `out`.
/// `out` must be at least `count * 512` bytes.
pub fn read_sectors(lba: u32, count: u32, out: &mut alloc::vec::Vec<u8>) -> Result<(), &'static str> {
    let mut disk = ATA.lock();
    for i in 0..count {
        let mut buf = [0u8; SECTOR_SIZE];
        disk.read_sector(lba + i, &mut buf)?;
        out.extend_from_slice(&buf);
    }
    Ok(())
}

/// Write bytes to disk starting at `lba`. Pads the last sector with zeros.
pub fn write_sectors(lba: u32, data: &[u8]) -> Result<(), &'static str> {
    let mut disk = ATA.lock();
    let mut offset = 0usize;
    let mut sector = lba;
    while offset < data.len() {
        let mut buf = [0u8; SECTOR_SIZE];
        let end = (offset + SECTOR_SIZE).min(data.len());
        buf[..end - offset].copy_from_slice(&data[offset..end]);
        disk.write_sector(sector, &buf)?;
        offset += SECTOR_SIZE;
        sector += 1;
    }
    Ok(())
}
