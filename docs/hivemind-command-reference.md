# HiveMind Command Reference

This is the quick command sheet for the current repository.

## Bare-Metal HiveMind OS

Build the boot image:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind\hivemind-os"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo bootimage
```

Run one VM:

```powershell
.\run-os.ps1
```

Run one VM manually:

```powershell
Copy-Item "target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin" "C:\hivemind\boot.bin" -Force
cmd /c '"C:\msys64\mingw64\bin\qemu-system-x86_64.exe" -drive format=raw,file=C:\hivemind\boot.bin -drive file=C:\hivemind\data.img,format=raw,if=ide,index=1 -m 256M -no-reboot -no-shutdown -serial file:C:\hivemind\serial.log'
```

Run two connected HiveMind OS VMs:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS"
.\hivemind\scripts\run-two-hivemind-vms.ps1 -Build
```

Two-VM mesh smoke test:

```text
VM1: net send SensorHub temp 85
VM2: mem list
VM2: net status
```

Useful shell commands inside `hive>`:

```text
help
hive
mem list
mem new notes
mem show 1
blob write 1 mood learning
blob read 1 mood
link 1 2 Sync
signal 1 note hello
log
net status
net send SensorHub temp 85
ls
mkdir projects
touch note.txt
write note.txt hello from HiveMind
cat note.txt
save
load
time
ps
clear
halt
```

## Host HiveMind Workspace

Check all Rust workspace crates:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind"
cargo check --workspace
```

Run the HTTP memory kernel / VOS API:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind"
$env:HIVEMIND_QEMU_PATH = "C:\msys64\mingw64\bin\qemu-system-x86_64.exe"
$env:HIVEMIND_DISK_IMAGES_DIR = "C:\hivemind\disks"
cargo run -p hivemind-vos
```

Run the observer desktop UI:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind"
cargo run -p hivemind-observer
```

Seed the HTTP Hive with demo memories, blobs, edges, and agents:

```powershell
cd "C:\Users\Ayush Thukral\Downloads\RustOS\hivemind"
.\demo.ps1
```

HTTP API examples:

```powershell
Invoke-RestMethod http://localhost:8080/hive/snapshot
Invoke-RestMethod http://localhost:8080/hive/memories
Invoke-RestMethod -Method POST http://localhost:8080/hive/memories -ContentType "application/json" -Body '{"name":"Scratch"}'
```
