#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

mod agent;
mod allocator;
mod boot_anim;
mod disk;
mod gdt;
mod hive;
mod interrupts;
mod memory;
mod net;
mod rtc;
mod serial;
mod shell;
mod vfs;
mod vga_buffer;

entry_point!(kernel_main);

pub fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use memory::BootInfoFrameAllocator;
    use x86_64::VirtAddr;

    // Boot animation (runs before any println!)
    boot_anim::play();

    vga_buffer::clear_screen();

    println!("╔═══════════════════════════════════════════════╗");
    println!("║       HiveMind OS  v0.1  —  Booting          ║");
    println!("║  A bare-metal Rust kernel with hive memory    ║");
    println!("╚═══════════════════════════════════════════════╝");
    println!();

    // --- CPU structures ---
    gdt::init();
    interrupts::init_idt();
    serial_println!("[serial] HiveMind OS v0.1 — boot started");

    // --- Hardware interrupts (PIC) ---
    unsafe { interrupts::PICS.lock().initialize() };
    x86_64::instructions::interrupts::enable();
    println!("[OK] CPU structures + interrupts initialized");

    // --- Memory management ---
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");
    println!("[OK] Memory management + heap initialized");

    // --- HiveMind memory kernel ---
    hive::init();
    println!("[OK] HiveMind memory kernel initialized");

    // --- Mesh networking (COM2 serial between VMs) ---
    net::init();
    println!("[OK] Mesh serial (COM2) ready — peer VMs will auto-sync blobs");

    // --- Agent runtime ---
    agent::init();
    println!("[OK] Agent runtime initialized");

    // --- Virtual filesystem ---
    vfs::init();
    println!("[OK] VFS initialized (/, /boot, /hive, /user)");

    // --- Disk (ATA slave) ---
    if disk::init() {
        println!("[OK] Data disk detected — type 'load' to restore saved state");
    } else {
        println!("[--] No data disk (run with -drive data.img to enable 'save'/'load')");
    }
    println!();
    println!("  Type 'help' to see shell commands.");
    println!("  Memory nodes, blobs, and signals are live.");
    println!("  Run 'run-os.ps1 -VMCount 2' on Windows to start a peer VM.");
    println!();

    shell::run()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("[PANIC] {}", info);
    println!();
    println!("╔════════════════════════════════════╗");
    println!("║           KERNEL PANIC             ║");
    println!("╚════════════════════════════════════╝");
    println!("{}", info);
    loop {
        x86_64::instructions::hlt();
    }
}
