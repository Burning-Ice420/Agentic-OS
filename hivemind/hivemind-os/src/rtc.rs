//! Real-time clock — reads current date/time from CMOS via ports 0x70/0x71.
//!
//! CMOS values default to BCD format. We check Register B to determine
//! whether they're binary or BCD.

use x86_64::instructions::port::Port;

#[derive(Debug, Clone, Copy)]
pub struct DateTime {
    pub year:    u16,
    pub month:   u8,
    pub day:     u8,
    pub hour:    u8,
    pub minute:  u8,
    pub second:  u8,
}

// ── CMOS I/O ──────────────────────────────────────────────────────────────────

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;
const NMI_DISABLE: u8 = 0x80;  // set bit 7 to disable NMI while reading

fn cmos_read(reg: u8) -> u8 {
    let mut addr: Port<u8> = Port::new(CMOS_ADDR);
    let mut data: Port<u8> = Port::new(CMOS_DATA);
    unsafe {
        addr.write(NMI_DISABLE | reg);
        data.read()
    }
}

fn is_update_in_progress() -> bool {
    cmos_read(0x0A) & 0x80 != 0
}

fn bcd_to_bin(v: u8) -> u8 {
    (v & 0x0F) + ((v >> 4) * 10)
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn read() -> DateTime {
    // Wait for any update cycle to complete (register 0x0A bit 7 = UIP)
    while is_update_in_progress() {}

    // Read raw values
    let mut second = cmos_read(0x00);
    let mut minute = cmos_read(0x02);
    let mut hour   = cmos_read(0x04);
    let mut day    = cmos_read(0x07);
    let mut month  = cmos_read(0x08);
    let mut year   = cmos_read(0x09) as u16;

    // Register B bit 2 = 0 → BCD mode
    let reg_b = cmos_read(0x0B);
    let is_bcd    = reg_b & 0x04 == 0;
    let is_24h    = reg_b & 0x02 != 0;

    if is_bcd {
        second = bcd_to_bin(second);
        minute = bcd_to_bin(minute);
        day    = bcd_to_bin(day);
        month  = bcd_to_bin(month);
        year   = bcd_to_bin(year as u8) as u16;
        // Hour in BCD is special for 12h format
        let h = hour & 0x7F;
        let pm = hour & 0x80 != 0;
        hour = bcd_to_bin(h);
        if !is_24h && pm {
            hour = (hour % 12) + 12;
        }
    }

    // Century: QEMU puts it at 0x32; add 2000 if year looks like a 2-digit year
    let century = bcd_to_bin(cmos_read(0x32)) as u16;
    let full_year = if century > 0 && century < 99 {
        century * 100 + year
    } else if year < 70 {
        2000 + year
    } else {
        1900 + year
    };

    DateTime {
        year:   full_year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

impl DateTime {
    pub fn display(&self) -> ([u8; 10], [u8; 8]) {
        // Returns (date bytes "YYYY-MM-DD", time bytes "HH:MM:SS")
        let mut date = *b"YYYY-MM-DD";
        let mut time = *b"HH:MM:SS";
        write_u16(&mut date, 0, self.year);
        write_u8_2d(&mut date, 5, self.month);
        write_u8_2d(&mut date, 8, self.day);
        write_u8_2d(&mut time, 0, self.hour);
        write_u8_2d(&mut time, 3, self.minute);
        write_u8_2d(&mut time, 6, self.second);
        (date, time)
    }
}

fn write_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off]     = b'0' + ((v / 1000) % 10) as u8;
    buf[off + 1] = b'0' + ((v / 100)  % 10) as u8;
    buf[off + 2] = b'0' + ((v / 10)   % 10) as u8;
    buf[off + 3] = b'0' + (v          % 10) as u8;
}

fn write_u8_2d(buf: &mut [u8], off: usize, v: u8) {
    buf[off]     = b'0' + (v / 10) % 10;
    buf[off + 1] = b'0' + v % 10;
}
