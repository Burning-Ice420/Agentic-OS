#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

mod agent;
mod allocator;
mod boot_anim;
mod desktop;
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
    // Program the PIC now, but keep CPU interrupts disabled until core kernel
    // state is ready. Early IRQs can otherwise run handlers while paging/heap
    // setup is still in progress.
    unsafe { interrupts::PICS.lock().initialize() };
    println!("[OK] CPU structures initialized");

    // --- Memory management ---
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");
    println!("[OK] Memory management + heap initialized");
    serial_println!("[boot] heap initialized");

    let mut heap_probe = alloc::vec::Vec::new();
    heap_probe.push(0x42u8);
    println!("[OK] Heap allocation probe passed");
    serial_println!("[boot] heap allocation probe passed: {}", heap_probe[0]);

    // --- HiveMind memory kernel ---
    println!("[..] Starting HiveMind memory kernel...");
    serial_println!("[boot] starting hive init");
    hive::init();
    println!("[OK] HiveMind memory kernel initialized");
    serial_println!("[boot] hive init complete");

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

    println!("[OK] Keyboard polling enabled");

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
