# HiveMind OS Knowledge Dump

This document explains what your HiveMind OS project is, what a kernel is,
what a shell is, how QEMU runs your OS, what each major source file does, how
the boot flow works, how keyboard input works, and what we debugged/fixed.

It assumes you are starting from zero. No shame in that. Operating systems are
one of those topics where every word hides five other words.

## 1. The Big Picture

You are building a tiny operating system kernel in Rust.

When you run this:

```powershell
cmd /c '"C:\msys64\mingw64\bin\qemu-system-x86_64.exe" -drive format=raw,file=C:\hivemind\boot.bin -drive file=C:\hivemind\data.img,format=raw,if=ide,index=1 -m 256M -no-reboot -no-shutdown -serial file:C:\hivemind\serial.log'
```

you are not running a normal Windows app. You are starting a fake computer
inside QEMU, and that fake computer boots your Rust kernel as if it were a
real OS.

The rough chain is:

```text
Rust source code
  -> cargo bootimage
  -> bootable raw disk image
  -> QEMU virtual machine
  -> bootloader
  -> your kernel_main()
  -> HiveMind OS shell
```

The important files are in:

```text
C:\Users\Ayush Thukral\Downloads\RustOS\hivemind\hivemind-os
```

Your kernel source code is mainly in:

```text
hivemind/hivemind-os/src
```

## 2. What Is An Operating System?

An operating system is the main software that controls a computer.

Examples:

```text
Windows
Linux
macOS
Android
iOS
```

The OS sits between programs and hardware.

Without an OS, programs would need to talk directly to the CPU, keyboard,
screen, disk, mouse, memory, network card, and timers. That would be chaos.

The OS provides controlled services:

```text
Program wants memory       -> OS gives memory safely
Program wants a file       -> OS talks to disk
Program wants keyboard     -> OS talks to keyboard controller
Program wants screen text  -> OS talks to display hardware
Program crashes            -> OS prevents whole machine from dying, ideally
```

Your HiveMind OS is very small. It does not run normal Windows/Linux programs.
It is a custom kernel that boots, initializes hardware-like components, and
provides its own simple command shell.

## 3. What Is A Kernel?

The kernel is the core of an operating system.

It is the first and most privileged part of the OS. It controls hardware and
provides the basic rules for everything else.

In a normal OS, the kernel handles things like:

```text
CPU setup
Memory management
Interrupts
Keyboard/mouse input
Disk access
Filesystems
Networking
Processes and threads
Security
System calls
```

In your project, the kernel is the Rust code that starts at:

```rust
pub fn kernel_main(boot_info: &'static BootInfo) -> ! {
```

That function lives in:

```text
hivemind/hivemind-os/src/main.rs
```

The `-> !` means this function never returns. A kernel does not finish and hand
control back to Windows. It owns the machine until the VM is stopped.

## 4. What Is A Shell?

A shell is a command interface.

In Windows, PowerShell is a shell. You type commands like:

```powershell
cd
dir
Get-Process
```

In Linux/macOS, Bash or Zsh are common shells.

In HiveMind OS, your shell is the `hive>` prompt.

When you see:

```text
hive>
```

that means your kernel has booted far enough to accept commands.

Your shell code lives in:

```text
hivemind/hivemind-os/src/shell.rs
```

The shell does three big things:

```text
1. Reads keyboard input
2. Builds a line of text from typed characters
3. Executes commands like help, hive, mem list, blob write, ls, save
```

The command dispatcher is this part:

```rust
match parts[0] {
    "help"          => cmd_help(),
    "clear"         => vga_buffer::clear_screen(),
    "hive"          => cmd_hive(),
    "mem" | "m"     => cmd_mem(&parts[1..]),
    "blob" | "b"    => cmd_blob(&parts[1..]),
    ...
}
```

So when you type:

```text
help
```

the shell calls:

```rust
cmd_help()
```

## 5. What Is QEMU?

QEMU is a machine emulator / virtualizer.

In simple words: QEMU creates a fake computer.

That fake computer has:

```text
CPU
RAM
Disk
Screen
Keyboard
Serial ports
IDE controller
```

Your real laptop is running Windows. QEMU opens a window and pretends to be a
separate bare-metal computer. Your Rust OS boots inside that fake computer.

That is why a crash in your OS does not crash Windows. It only freezes or stops
the QEMU VM.

## 6. What The QEMU Command Means

Your QEMU command:

```powershell
cmd /c '"C:\msys64\mingw64\bin\qemu-system-x86_64.exe" -drive format=raw,file=C:\hivemind\boot.bin -drive file=C:\hivemind\data.img,format=raw,if=ide,index=1 -m 256M -no-reboot -no-shutdown -serial file:C:\hivemind\serial.log'
```

Breakdown:

```text
cmd /c
```

Run the command through Windows `cmd.exe`. This helps avoid some PowerShell
argument parsing issues.

```text
C:\msys64\mingw64\bin\qemu-system-x86_64.exe
```

This is the QEMU executable for an x86_64 machine.

```text
-drive format=raw,file=C:\hivemind\boot.bin
```

Attach your boot image as a raw disk. This is the disk QEMU boots from.

```text
-drive file=C:\hivemind\data.img,format=raw,if=ide,index=1
```

Attach a second disk. Your OS uses this for save/load persistence.

```text
-m 256M
```

Give the VM 256 MB of RAM.

```text
-no-reboot
```

If the guest OS crashes/reboots, QEMU should not automatically reboot.

```text
-no-shutdown
```

If the guest shuts down, keep the QEMU window around.

```text
-serial file:C:\hivemind\serial.log
```

Connect the guest's COM1 serial port to a file on Windows. When your kernel
calls `serial_println!`, output goes into `C:\hivemind\serial.log`.

## 7. What Is `cargo bootimage`?

Normally, `cargo build` creates an executable file for an existing OS.

But your kernel is not a Windows program. It is an OS kernel. It needs to be
packaged into something bootable.

That is what `cargo bootimage` does.

It:

```text
1. Compiles your Rust kernel
2. Builds a bootloader
3. Combines them into a bootable disk image
```

The output is:

```text
hivemind/hivemind-os/target/x86_64-hivemind-os/debug/bootimage-hivemind-os.bin
```

Then we copy it to:

```text
C:\hivemind\boot.bin
```

because QEMU behaves more reliably with a path that has no spaces.

## 8. Why This Project Uses `no_std` And `no_main`

At the top of `main.rs`:

```rust
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
```

### `#![no_std]`

Normal Rust programs use the standard library, `std`.

`std` depends on an operating system. For example, it expects files, threads,
environment variables, stdout, memory allocation, and OS services.

But you are writing the OS. There is no OS below you.

So the kernel uses:

```text
core  -> basic Rust language features
alloc -> heap-backed things like Vec, String, BTreeMap after allocator setup
```

### `#![no_main]`

Normal Rust programs start with:

```rust
fn main() {}
```

Your kernel does not use the normal Rust runtime. The bootloader jumps into
your custom entry point:

```rust
entry_point!(kernel_main);
```

### `#![feature(abi_x86_interrupt)]`

This enables a special Rust calling convention for x86 interrupt handlers.

It is used in:

```text
src/interrupts.rs
```

## 9. Boot Flow: What Happens When HiveMind OS Starts

The boot sequence is in:

```text
src/main.rs
```

Simplified:

```text
1. Bootloader starts your kernel
2. kernel_main() begins
3. Boot animation plays
4. VGA screen is cleared
5. GDT is initialized
6. IDT is initialized
7. PIC is initialized
8. Memory paging is connected
9. Heap allocator is initialized
10. Hive memory graph is initialized
11. Mesh serial networking is initialized
12. Agent runtime is initialized
13. VFS is initialized
14. Data disk is detected
15. Keyboard polling is enabled
16. Shell starts
```

The real code:

```rust
gdt::init();
interrupts::init_idt();
unsafe { interrupts::PICS.lock().initialize() };

let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
let mut mapper = unsafe { memory::init(phys_mem_offset) };
let mut frame_allocator =
    unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

allocator::init_heap(&mut mapper, &mut frame_allocator)
    .expect("heap initialization failed");

hive::init();
net::init();
agent::init();
vfs::init();
disk::init();

shell::run()
```

## 10. What Is VGA Text Mode?

Before modern graphics, PCs had a simple text screen mode called VGA text mode.

The screen is basically a grid of characters. Each cell contains:

```text
character byte
color byte
```

Your code writes directly to VGA memory at:

```text
0xb8000
```

That logic is in:

```text
src/vga_buffer.rs
```

When your kernel calls:

```rust
println!("hello");
```

it eventually writes characters into VGA text memory, and QEMU displays them.

## 11. What Is Serial Output?

Serial output is a simple text output channel through a COM port.

Your OS has:

```text
COM1 -> debug log
COM2 -> mesh networking
```

COM1 is handled by:

```text
src/serial.rs
```

The macro:

```rust
serial_println!("message");
```

writes to COM1.

QEMU maps COM1 to:

```text
C:\hivemind\serial.log
```

because your launch command includes:

```text
-serial file:C:\hivemind\serial.log
```

Serial logs are useful because even if the VGA screen freezes, you can often
see the last debug message written by the kernel.

## 12. What Is Memory Management?

Memory management means deciding which RAM belongs to what.

At boot, the bootloader gives your kernel a memory map. The memory map says:

```text
This range is usable RAM
This range belongs to bootloader
This range is reserved
This range contains kernel code
```

Your memory code lives in:

```text
src/memory.rs
src/allocator.rs
```

### Paging

Modern x86_64 CPUs use virtual memory.

Programs use virtual addresses. The CPU translates them to physical addresses
using page tables.

Your heap starts at a virtual address:

```rust
pub const HEAP_START: usize = 0x_4444_4444_0000;
```

The heap size is:

```rust
pub const HEAP_SIZE: usize = 2 * 1024 * 1024;
```

That is 2 MiB.

`allocator::init_heap()` maps virtual heap pages to real physical frames:

```rust
for page in page_range {
    let frame = frame_allocator
        .allocate_frame()
        .ok_or(MapToError::FrameAllocationFailed)?;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
}
```

After that, the kernel can use heap-backed Rust types:

```text
String
Vec
BTreeMap
Box
```

Before heap init, using those would be invalid.

## 13. What Is The Heap?

The heap is a region of memory used for dynamic allocation.

Stack memory is for local variables with predictable lifetimes.

Heap memory is for data whose size can grow/shrink at runtime:

```rust
let mut s = String::new();
let mut v = Vec::new();
let mut map = BTreeMap::new();
```

Your global allocator is:

```rust
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();
```

from:

```text
linked_list_allocator
```

It is initialized here:

```rust
unsafe { ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE) };
```

## 14. What Is The GDT?

GDT means Global Descriptor Table.

On x86_64, it is part of CPU setup. In long mode, segmentation is mostly
disabled, but the GDT is still needed for some CPU features, especially the
Task State Segment used for safer exception handling.

Your GDT code lives in:

```text
src/gdt.rs
```

You call it during boot:

```rust
gdt::init();
```

## 15. What Is The IDT?

IDT means Interrupt Descriptor Table.

It tells the CPU:

```text
If interrupt X happens, call function Y.
```

For example:

```text
Timer interrupt    -> timer_interrupt_handler
Keyboard interrupt -> keyboard_interrupt_handler
Page fault         -> page_fault_handler
Double fault       -> double_fault_handler
```

Your IDT code lives in:

```text
src/interrupts.rs
```

## 16. What Are Interrupts?

An interrupt is a hardware or CPU event that interrupts normal execution.

Examples:

```text
Timer tick
Keyboard key pressed
Mouse moved
Disk finished reading
Network packet arrived
CPU page fault
```

Normally, keyboard input works like:

```text
User presses key
Keyboard controller raises IRQ1
CPU pauses current code
CPU jumps to keyboard interrupt handler
Kernel reads scancode from port 0x60
Kernel decodes key
Kernel stores character for shell
CPU returns to previous code
```

In this project, enabling hardware interrupts was freezing the VM. To keep the
OS usable, we temporarily changed the shell to poll the keyboard instead of
waiting for keyboard interrupts.

That means the shell repeatedly checks:

```text
Is there keyboard data waiting?
If yes, read it.
If no, keep looping.
```

## 17. Keyboard Polling In Your Current Build

The keyboard polling code is in:

```text
src/shell.rs
```

It reads the PS/2 controller status port:

```rust
let mut status_port: Port<u8> = Port::new(0x64);
```

Then it reads keyboard data from:

```rust
let mut data_port: Port<u8> = Port::new(0x60);
```

The key part:

```rust
let status = unsafe { status_port.read() };
if status & 0x01 == 0 {
    break;
}

let scancode = unsafe { data_port.read() };
```

Port `0x64` tells whether data is available.

Port `0x60` gives the scancode.

Then `pc-keyboard` decodes it:

```rust
keyboard.add_byte(scancode)
keyboard.process_keyevent(key_event)
```

Finally it pushes the character into the shell input buffer:

```rust
push_key(character)
```

## 18. What Is A Scancode?

When you press a key, the keyboard does not send the letter `a`.

It sends a hardware code.

Example idea:

```text
Key A pressed  -> scancode
Key A released -> another scancode
Enter pressed  -> scancode
```

The `pc-keyboard` crate turns those scancodes into meaningful keys:

```text
scancode -> DecodedKey::Unicode('a')
scancode -> DecodedKey::RawKey(KeyCode::Backspace)
```

## 19. The Shell Input Buffer

The shell has a small ring buffer:

```rust
const KEY_BUF: usize = 256;
```

It stores typed characters until the shell loop consumes them.

Why use a ring buffer?

Because keyboard input can arrive independently of command processing. A buffer
lets the OS store keys safely until the shell is ready to handle them.

The important operations are:

```rust
fn push(&mut self, c: char)
fn pop(&mut self) -> Option<char>
```

## 20. What Is The Hive?

The Hive is your custom in-memory knowledge graph.

It is not a normal OS concept. It is your project's main idea.

The Hive stores:

```text
Memory nodes
Blobs attached to memory nodes
Edges between memory nodes
Signals broadcast by nodes
```

The code lives in:

```text
src/hive/mod.rs
src/hive/memory_node.rs
src/hive/blob.rs
```

## 21. Hive Data Model

### `Hive`

```rust
pub struct Hive {
    pub memories:   BTreeMap<u64, MemoryNode>,
    pub edges:      Vec<MemoryEdge>,
    pub signal_log: Vec<Signal>,
}
```

Meaning:

```text
memories   -> all memory nodes, indexed by ID
edges      -> relationships between memory nodes
signal_log -> recent messages/events
```

### `MemoryNode`

```rust
pub struct MemoryNode {
    pub id:            u64,
    pub name:          String,
    pub blobs:         BTreeMap<String, Blob>,
    pub parent_id:     Option<u64>,
    pub children:      Vec<u64>,
    pub subscriptions: Vec<u64>,
}
```

Meaning:

```text
id            -> numeric identity
name          -> human-friendly name
blobs         -> key/value data stored inside the node
parent_id     -> optional parent node
children      -> child nodes
subscriptions -> nodes this one listens to
```

### `Blob`

A blob is one key/value item inside a memory node.

```rust
pub struct Blob {
    pub id:              u64,
    pub key:             String,
    pub value:           BlobValue,
    pub owner_memory_id: u64,
    pub created_tick:    u64,
    pub modified_tick:   u64,
}
```

Example:

```text
Memory node: kernel-root
Blob key:    status
Blob value:  running
```

### `BlobValue`

```rust
pub enum BlobValue {
    Text(String),
    Number(i64),
    Bool(bool),
    Binary(Vec<u8>),
}
```

So blobs can store:

```text
Text
Numbers
Booleans
Binary data
```

## 22. Hive Boot Initialization

During boot:

```rust
hive::init();
```

creates the root memory node:

```rust
let root = h.create_memory("kernel-root", None);
```

Then it writes boot metadata:

```rust
h.write_blob(root, "version",  BlobValue::Text("0.1".to_string()));
h.write_blob(root, "status",   BlobValue::Text("running".to_string()));
```

Then it stores the Hive in a global:

```rust
static HIVE: Mutex<Option<Hive>> = Mutex::new(None);
```

This means there is one global Hive instance protected by a spinlock.

## 23. What Is A Mutex?

A mutex protects shared data.

Only one part of the kernel can access the protected data at a time.

Example:

```rust
static HIVE: Mutex<Option<Hive>> = Mutex::new(None);
```

To use it:

```rust
let mut guard = HIVE.lock();
```

If something else already locked it, this waits.

In a normal OS, waiting may put a thread to sleep. In this bare-metal kernel,
the `spin` mutex spins in a loop until the lock is free.

That is why accidental double-locking can freeze the kernel.

## 24. The Hive Init Bug We Found

The kernel was freezing here:

```text
[hive] mark running
```

The old flow was:

```text
1. Create Hive locally
2. Store it in global HIVE
3. Call with_hive() to lock global HIVE again
4. Write status = running
```

That second global update was unnecessary and caused the system to hang.

The fix was:

```text
Write status = running before storing the Hive globally.
Do not immediately call with_hive() during init.
```

Current logic:

```rust
h.write_blob(root, "status", BlobValue::Text("running".to_string()));
*HIVE.lock() = Some(h);
```

## 25. What Is The VFS?

VFS means Virtual File System.

Your VFS is an in-memory filesystem. It gives commands like:

```text
ls
mkdir
touch
write
cat
rm
cd
pwd
```

The code lives in:

```text
src/vfs/mod.rs
```

It creates directories:

```text
/
/boot
/hive
/user
```

This is not NTFS, FAT32, ext4, or a real disk filesystem. It is a tree stored
in RAM using Rust data structures.

Persistence is handled separately by serializing the data to `data.img`.

## 26. What Is The Data Disk?

Your QEMU command attaches:

```text
C:\hivemind\data.img
```

as a second IDE disk.

Your disk code lives in:

```text
src/disk/mod.rs
src/disk/persist.rs
```

The kernel uses ATA PIO ports to talk to the disk.

The disk layout is:

```text
Sector 0      -> header/magic
Sectors 1-63  -> Hive state
Sectors 64-127 -> VFS state
```

The shell commands:

```text
save
load
```

write/read Hive and VFS state.

## 27. What Is ATA PIO?

ATA is an old disk interface.

PIO means Programmed I/O.

Instead of using advanced DMA, the CPU directly reads/writes disk data through
I/O ports.

Your code talks to ports like:

```text
0x1F0 -> data
0x1F7 -> status/command
0x1F6 -> drive/head select
```

This is low-level hardware-style programming.

QEMU emulates the ATA disk, so your kernel thinks it is talking to real
hardware.

## 28. What Is Mesh Serial Networking?

Your OS has a small VM-to-VM sync idea using COM2.

Code:

```text
src/net/mod.rs
```

COM1 is debug serial.

COM2 is mesh networking.

The protocol is line-based:

```text
HMSG|<memory_name>|<key>|<value>\n
```

Example:

```text
HMSG|SensorHub|temp|85
```

If another VM receives that message, it creates or updates a memory node and
blob.

## 29. The COM2 Bug We Fixed

You were launching only one serial port:

```text
-serial file:C:\hivemind\serial.log
```

That creates COM1.

But COM2 was not connected.

When the kernel tried reading a missing COM2 UART, QEMU returned `0xFF`.

The old code treated that like "data is available", so it could loop forever.

The fix was to treat `0xFF` as invalid/missing hardware:

```rust
(lsr & 0x01 != 0) && (lsr != 0xFF)
```

and similarly for transmit-ready checks.

## 30. What Is The Agent Runtime?

The agent runtime is a small reactive rules system.

Code:

```text
src/agent/mod.rs
```

An agent watches a memory node and applies rules.

Example idea:

```text
If memory node status == error
Then write alert = kernel_error
```

The built-in boot agent is:

```text
kernel-watchdog
```

Agents are controlled with shell commands:

```text
agent list
agent new <name> <mem_id>
agent rule <id> <watch> <cond> <akey> <aval>
agent tick
```

## 31. What Is RTC?

RTC means Real-Time Clock.

It is the hardware clock in a PC.

Your code:

```text
src/rtc.rs
```

reads the CMOS/RTC to show date/time.

The shell command:

```text
time
```

uses it.

## 32. Why The Kernel Froze When Enabling Interrupts

At one point, the OS booted all the way through the disk message but froze
right before the shell prompt.

The line after boot was supposed to be:

```text
[OK] Hardware interrupts enabled
```

But the freeze happened inside:

```rust
x86_64::instructions::interrupts::enable();
```

That means some pending hardware interrupt fired immediately, and one of the
interrupt handlers or PIC paths locked up.

To keep the OS usable, we changed the shell to keyboard polling and stopped
enabling interrupts at the end of boot.

Current message:

```text
[OK] Keyboard polling enabled
```

This is a practical workaround. Later, the cleaner fix is to debug the PIC/IRQ
path properly and restore interrupt-driven keyboard input.

## 33. Why The Tick Counter Is Weird Now

Originally, the timer interrupt updated:

```rust
pub static TICKS: Mutex<u64> = Mutex::new(0);
```

The timer interrupt handler increments it:

```rust
*TICKS.lock() += 1;
```

But because hardware interrupts are currently not enabled, timer ticks will not
advance normally.

Commands like:

```text
tick
ps
hive
```

may show a tick value that stays at zero.

That is expected for the current polling workaround.

## 34. Commands You Can Try

Once you see:

```text
hive>
```

try:

```text
help
```

Show hive overview:

```text
hive
```

List memory nodes:

```text
mem list
```

Create a memory node:

```text
mem new notes
```

Write a blob:

```text
blob write 1 mood learning
```

Read a blob:

```text
blob read 1 mood
```

List files:

```text
ls
```

Create a file:

```text
touch hello.txt
```

Write a file:

```text
write hello.txt Hello from HiveMind OS
```

Read a file:

```text
cat hello.txt
```

Save state:

```text
save
```

Load state:

```text
load
```

## 35. How The Code Is Organized

High-level map:

```text
src/main.rs          Boot sequence and kernel entry point
src/vga_buffer.rs    Text output to screen
src/serial.rs        Serial debug output on COM1
src/gdt.rs           CPU GDT/TSS setup
src/interrupts.rs    IDT, PIC, interrupt handlers, tick counter
src/memory.rs        Page table setup and frame allocator
src/allocator.rs     Heap allocator
src/shell.rs         Interactive command shell and keyboard polling
src/hive/            Hive memory graph
src/vfs/             In-memory filesystem
src/disk/            ATA disk and persistence
src/net/             COM2 mesh serial networking
src/agent/           Reactive agent/rule runtime
src/rtc.rs           Real-time clock
src/boot_anim.rs     Boot animation
```

## 36. The Boot Messages Explained

You see:

```text
[OK] CPU structures initialized
```

Means:

```text
GDT, IDT, and PIC setup completed.
```

You see:

```text
[OK] Memory management + heap initialized
```

Means:

```text
Page tables were configured and heap memory is usable.
```

You see:

```text
[OK] Heap allocation probe passed
```

Means:

```text
The kernel successfully allocated a Vec on the heap.
```

You see:

```text
[hive] new
[hive] create root
[hive] write version
[hive] write status
[hive] store global
[hive] done
```

Means:

```text
The Hive graph was created, filled with root metadata, and stored globally.
```

You see:

```text
[OK] Mesh serial (COM2) ready
```

Means:

```text
COM2 mesh networking was initialized.
```

You see:

```text
[OK] Agent runtime initialized
```

Means:

```text
The built-in rule/agent system is ready.
```

You see:

```text
[OK] VFS initialized
```

Means:

```text
The in-memory filesystem has /, /boot, /hive, and /user.
```

You see:

```text
[OK] Data disk detected
```

Means:

```text
The second QEMU disk C:\hivemind\data.img is visible to the kernel.
```

You should see:

```text
[OK] Keyboard polling enabled
hive>
```

Means:

```text
The shell is running and should accept keyboard input.
```

## 37. Important Debugging Lessons From This Project

### Lesson 1: Bare-metal freezes often have no error message

In a normal app, you often get:

```text
Exception
Stack trace
Error code
```

In a kernel, a bug can simply freeze the CPU.

That is why we added breadcrumbs:

```rust
println!("[hive] write status");
serial_println!("[hive] write status");
```

The last printed line tells us where execution stopped.

### Lesson 2: Locking is dangerous in kernels

A spinlock mistake can freeze the whole OS.

The Hive bug was an example.

### Lesson 3: Interrupts are powerful but risky

Interrupts can happen almost anywhere.

If an interrupt handler tries to lock something already locked, the kernel can
freeze.

### Lesson 4: Missing hardware can return fake-looking values

COM2 was not connected, and QEMU returned `0xFF`.

The kernel had to learn:

```text
0xFF from this UART can mean "device missing", not "data ready".
```

### Lesson 5: Boot order matters

You cannot use heap types before heap init.

You should avoid enabling interrupts before core kernel state is ready.

## 38. Why The Code Uses `unsafe`

Rust normally protects you from memory bugs.

But kernel code must do things Rust cannot prove safe:

```text
Read/write CPU ports
Dereference physical memory mappings
Initialize global allocator
Load CPU tables
Talk to hardware
```

So kernel code uses `unsafe` in places like:

```rust
unsafe { self.status_cmd.read() }
unsafe { ALLOCATOR.lock().init(...) }
unsafe { memory::init(...) }
```

`unsafe` does not mean "wrong". It means "the programmer must manually uphold
the rules here".

## 39. Current Known Limitations

The current build is intentionally pragmatic.

Known limitations:

```text
Hardware interrupts are not fully enabled.
Keyboard input uses polling.
Timer ticks may not advance.
The diagnostic [hive] lines are still visible during boot.
COM2 mesh only works when you launch a second VM with a connected serial link.
This OS cannot run normal applications.
The filesystem is simple and custom.
Persistence is custom and sector-based.
```

## 40. How To Build And Run

From Admin PowerShell:

```powershell
Get-Process qemu* -ErrorAction SilentlyContinue | Stop-Process -Force

cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind\hivemind-os"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cargo bootimage
Copy-Item "target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin" "C:\hivemind\boot.bin" -Force

cmd /c '"C:\msys64\mingw64\bin\qemu-system-x86_64.exe" -drive format=raw,file=C:\hivemind\boot.bin -drive file=C:\hivemind\data.img,format=raw,if=ide,index=1 -m 256M -no-reboot -no-shutdown -serial file:C:\hivemind\serial.log'
```

If QEMU says `[Paused]`, unpause it:

```text
Machine -> Pause
```

or:

```text
Ctrl + Alt + P
```

To release mouse/keyboard grab:

```text
Ctrl + Alt + G
```

## 41. Mental Model Summary

Think of the project like this:

```text
QEMU
  is the fake computer.

bootimage-hivemind-os.bin
  is the fake boot disk.

bootloader
  loads your kernel.

kernel_main()
  is the first Rust function in your OS.

vga_buffer
  prints text to the QEMU screen.

serial
  prints debug logs to serial.log.

memory + allocator
  make dynamic Rust data structures possible.

hive
  is your OS's knowledge/memory graph.

vfs
  is your in-memory filesystem.

disk
  saves/loads data from data.img.

net
  syncs messages over COM2 between VMs.

agent
  runs reactive rules.

shell
  lets you type commands into the OS.
```

## 42. What To Learn Next

If you want to understand this deeply, learn in this order:

```text
1. Basic Rust ownership, structs, enums, match
2. What no_std means
3. CPU boot process at a high level
4. VGA text mode
5. I/O ports
6. x86_64 paging
7. Heap allocation
8. Interrupts and PIC/APIC
9. Keyboard scancodes
10. Filesystem basics
11. Disk sectors
12. Serial communication
```

This project touches all of those.

## 43. A Short Plain-English Explanation

HiveMind OS is a tiny Rust operating system that boots inside QEMU. QEMU acts
like a separate computer. The bootloader loads your kernel. The kernel sets up
CPU structures, screen output, memory, heap allocation, a custom memory graph
called the Hive, a simple filesystem, a disk persistence layer, serial
networking, an agent system, and finally a command shell. The shell is the
`hive>` prompt where you type commands. The current build uses keyboard polling
instead of hardware keyboard interrupts because enabling hardware interrupts was
freezing the VM. The Hive is your custom data model: memory nodes connected by
edges, with blobs of data stored inside them. The whole thing is a learning OS:
small enough to understand, but real enough to teach low-level concepts.

