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
mod llm;
mod memory;
mod mouse;
mod net;
mod rtc;
mod serial;
mod shell;
mod sysinfo;
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
    // Unmask only timer (IRQ0), keyboard (IRQ1) and the slave cascade (IRQ2) on
    // the master; keep the whole slave PIC masked until the mouse driver enables
    // IRQ12. This keeps unused devices (notably ATA's IRQ14) from interrupting a
    // driver that is entirely polled.
    unsafe { interrupts::PICS.lock().write_masks(0xF8, 0xFF) };
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

    // --- AI accelerator (COM3 serial to a host LLM bridge) ---
    // The kernel's rule engine stays the fast reflex tier; this offloads heavy
    // reasoning to a small host-side model, like offloading to a GPU/NPU.
    llm::init();
    println!("[OK] AI accelerator (COM3) ready — offloads reasoning to a host model");

    // --- Agent runtime ---
    agent::init();
    println!("[OK] Agent runtime initialized");

    // --- Virtual filesystem ---
    vfs::init();
    println!("[OK] VFS initialized (/, /boot, /hive, /user)");

    // --- Disk (ATA slave) + auto-restore of persisted state ---
    if disk::init() {
        match disk::persist::load() {
            Ok(()) => println!("[OK] Data disk detected — previous state auto-restored"),
            Err(_) => println!("[OK] Data disk detected — fresh disk (state auto-saves as you work)"),
        }
    } else {
        println!("[--] No data disk (run with -drive data.img to enable persistence)");
    }

    // --- PS/2 mouse (for the desktop UI) ---
    mouse::init();
    println!("[OK] PS/2 mouse initialized");

    // --- Per-boot instance identity + system info ---
    // Sum the usable RAM regions (the map also contains a huge high-address
    // physical-memory-mapping region, so we must not just take the max address).
    use bootloader::bootinfo::MemoryRegionType;
    let total_ram: u64 = boot_info
        .memory_map
        .iter()
        .filter(|r| r.region_type == MemoryRegionType::Usable)
        .map(|r| r.range.end_addr() - r.range.start_addr())
        .sum();
    sysinfo::init(total_ram);
    sysinfo::with_uuid(|u| {
        println!("[OK] Instance UUID: {}", u);
        serial_println!("[boot] instance-uuid={}", u);
    });

    println!();
    println!("  Type 'help' to see shell commands.  'ui' opens the desktop.");
    println!("  PageUp/PageDown scroll the console history.");
    println!("  Run 'run-os.ps1 -VMCount 2' on Windows to start a peer VM.");
    println!();

    // Everything is initialized — turn on hardware interrupts. From here the
    // keyboard and mouse are fully interrupt-driven (no more polling), so no
    // long-running command can starve input.
    x86_64::instructions::interrupts::enable();
    println!("[OK] Interrupts enabled — keyboard & mouse live");

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
