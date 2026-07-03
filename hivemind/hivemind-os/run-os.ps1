# HiveMind OS - Build and Launch Script
#
# Usage:
#   .\run-os.ps1                       # Single VM, hardware-accelerated (WHPX)
#   .\run-os.ps1 -Release              # Optimized build
#   .\run-os.ps1 -VMCount 2            # Two VMs connected via COM2 mesh serial
#   .\run-os.ps1 -Serial               # Pipe COM1 serial log to this terminal
#   .\run-os.ps1 -Accel tcg            # Force pure software emulation
#   .\run-os.ps1 -Memory 512 -Cpus 2 -DiskMB 8
#   .\run-os.ps1 -QEMU <path>          # Override QEMU executable path
#
# Every launched VM is registered under C:\hivemind\instances\ so `hive-cli.ps1`
# can list running instances, their per-boot UUID and their resource allocation.

param(
    [switch]$Release,
    [switch]$Serial,
    [int]   $VMCount = 1,
    [int]   $Memory  = 256,           # RAM per VM, in MiB
    [int]   $Cpus    = 1,             # vCPUs per VM (the kernel itself is single-core)
    [int]   $DiskMB  = 1,             # data disk size, in MiB
    [string]$Accel   = "whpx",        # whpx | tcg
    [string]$QEMU    = "C:\msys64\mingw64\bin\qemu-system-x86_64.exe"
)

$ErrorActionPreference = "Continue"
$ROOT = "$PSScriptRoot"
Set-Location $ROOT
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

# ── 1. Check QEMU ─────────────────────────────────────────────────────────────
if (-not (Test-Path $QEMU)) {
    Write-Host "[ERROR] QEMU not found at: $QEMU" -ForegroundColor Red
    Write-Host "        Set -QEMU to the correct path." -ForegroundColor Yellow
    exit 1
}
Write-Host "[OK] QEMU: $QEMU" -ForegroundColor Green

# ── 2. Nightly toolchain ──────────────────────────────────────────────────────
Write-Host ""
Write-Host "[1/4] Rust nightly toolchain..." -ForegroundColor Cyan
$installed = rustup toolchain list 2>$null | Select-String "nightly"
if (-not $installed) {
    Write-Host "      Installing nightly..." -ForegroundColor Yellow
    rustup toolchain install nightly --component rust-src llvm-tools-preview 2>$null
} else {
    Write-Host "      Nightly OK" -ForegroundColor Green
}
rustup component add rust-src          --toolchain nightly 2>$null
rustup component add llvm-tools-preview --toolchain nightly 2>$null
Write-Host "      Components OK" -ForegroundColor Green

# ── 3. Bootimage tool ─────────────────────────────────────────────────────────
Write-Host ""
Write-Host "[2/4] Bootimage tool..." -ForegroundColor Cyan
$bi = Get-Command bootimage -ErrorAction SilentlyContinue
if (-not $bi) {
    Write-Host "      Installing bootimage..." -ForegroundColor Yellow
    cargo install bootimage 2>$null
} else {
    Write-Host "      Bootimage OK" -ForegroundColor Green
}

# ── 4. Build ──────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "[3/4] Building HiveMind OS..." -ForegroundColor Cyan

if ($Release) {
    Write-Host "      Mode: RELEASE" -ForegroundColor Magenta
    cargo bootimage --release 2>&1 | Out-Null
    $IMG = "target\x86_64-hivemind-os\release\bootimage-hivemind-os.bin"
} else {
    Write-Host "      Mode: debug" -ForegroundColor Gray
    cargo bootimage 2>&1 | Out-Null
    $IMG = "target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin"
}

if (-not (Test-Path $IMG)) {
    Write-Host "[FAILED] Boot image not found: $IMG" -ForegroundColor Red
    Write-Host "         Run 'cargo bootimage' manually to see errors." -ForegroundColor Yellow
    exit 1
}

$imgSize = (Get-Item $IMG).Length
Write-Host "      Built: $IMG ($imgSize bytes)" -ForegroundColor Green

# ── 5. Launch QEMU ────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "[4/4] Launching QEMU..." -ForegroundColor Cyan
Write-Host ""
Write-Host "  Click inside QEMU window for keyboard/mouse input" -ForegroundColor DarkCyan
Write-Host "  Ctrl+Alt+G to release the mouse, Ctrl+Alt+Q to quit" -ForegroundColor DarkCyan
Write-Host ""

# Copy boot image to C:\hivemind (QEMU can't handle spaces in paths)
$BootDir = "C:\hivemind"
if (-not (Test-Path $BootDir)) { New-Item -ItemType Directory -Path $BootDir -Force | Out-Null }

$BootImg = Join-Path $BootDir "boot.bin"
Copy-Item -Path $IMG -Destination $BootImg -Force
Write-Host "  Boot image -> $BootImg" -ForegroundColor Green

# ── Acceleration ──────────────────────────────────────────────────────────────
# WHPX (Windows Hypervisor Platform) runs guest code on real hardware. It cannot
# use an in-kernel IRQ chip, so we pin kernel-irqchip=off. Fall back with -Accel tcg.
switch ($Accel.ToLower()) {
    "whpx" { $accelArg = "-accel whpx,kernel-irqchip=off"; $accelName = "WHPX (hardware)" }
    "tcg"  { $accelArg = "-accel tcg";                     $accelName = "TCG (software)" }
    default { $accelArg = "-accel $Accel";                 $accelName = $Accel }
}

# ── Resource allocation ───────────────────────────────────────────────────────
Write-Host ""
Write-Host "  Resource allocation per VM:" -ForegroundColor White
Write-Host "    Acceleration : $accelName" -ForegroundColor Gray
Write-Host "    Memory       : $Memory MiB" -ForegroundColor Gray
Write-Host "    vCPUs        : $Cpus  (kernel uses 1)" -ForegroundColor Gray
Write-Host "    Data disk    : $DiskMB MiB" -ForegroundColor Gray
Write-Host ""

$baseArgs = "$accelArg -m ${Memory}M -smp $Cpus -no-reboot -no-shutdown"
$qemuCmd  = "-drive format=raw,file=$BootImg $baseArgs"

# ── Instance registry ─────────────────────────────────────────────────────────
$InstDir = Join-Path $BootDir "instances"
if (-not (Test-Path $InstDir)) { New-Item -ItemType Directory -Path $InstDir -Force | Out-Null }
# Clear stale manifests whose process is gone.
Get-ChildItem $InstDir -Filter *.json -ErrorAction SilentlyContinue | ForEach-Object {
    try {
        $m = Get-Content $_.FullName -Raw | ConvertFrom-Json
        if (-not (Get-Process -Id $m.pid -ErrorAction SilentlyContinue)) { Remove-Item $_.FullName -Force }
    } catch { Remove-Item $_.FullName -Force }
}

function Register-Instance($name, $proc, $serialLog) {
    $manifest = [ordered]@{
        name       = $name
        pid        = $proc.Id
        serialLog  = $serialLog
        memoryMB   = $Memory
        cpus       = $Cpus
        diskMB     = $DiskMB
        accel      = $accelName
        launchedAt = (Get-Date).ToString("s")
    }
    $path = Join-Path $InstDir "$($proc.Id).json"
    $manifest | ConvertTo-Json | Out-File -FilePath $path -Encoding utf8
    Write-Host "  Registered instance '$name' (pid $($proc.Id)) -> $path" -ForegroundColor DarkGray
}

$QEMU_IMG = Join-Path (Split-Path $QEMU) "qemu-img.exe"

function New-DataDisk($path) {
    if (-not (Test-Path $path)) {
        if (Test-Path $QEMU_IMG) {
            & $QEMU_IMG create -f raw $path "${DiskMB}M" 2>$null | Out-Null
            Write-Host "  Created ${DiskMB} MiB data disk -> $path" -ForegroundColor Green
        } else {
            Write-Host "  [WARN] qemu-img not found, save/load disabled for $path" -ForegroundColor Yellow
        }
    }
}

if ($VMCount -ge 2) {
    # ── Multi-VM mode ─────────────────────────────────────────────────────────
    Write-Host "  Launching $VMCount VMs with COM2 mesh (TCP 4444)..." -ForegroundColor Yellow

    for ($i = 1; $i -le $VMCount; $i++) {
        $disk = Join-Path $BootDir "data_vm$i.img"
        New-DataDisk $disk
        $serialLog = "$BootDir\vm$i.log"
        $vmArgs = "$qemuCmd -drive file=$disk,format=raw,if=ide,index=1 -serial file:$serialLog"
        if ($i -eq 1) {
            $vmArgs += " -serial tcp::4444,server,nowait"
        } elseif ($i -eq 2) {
            $vmArgs += " -serial tcp:127.0.0.1:4444"
        }
        Write-Host "  VM$i ..." -ForegroundColor Cyan
        $proc = Start-Process -FilePath $QEMU -ArgumentList $vmArgs -PassThru
        Register-Instance "vm$i" $proc $serialLog
        Start-Sleep -Milliseconds 700
    }

    Write-Host ""
    Write-Host "  All VMs running." -ForegroundColor Green
    Write-Host "  Manage them:  .\hive-cli.ps1 list" -ForegroundColor White
    Write-Host "  Demo: in VM1 'net send SensorHub temp 85', in VM2 'mem list'" -ForegroundColor White

} else {
    # ── Single VM mode ────────────────────────────────────────────────────────
    $DataImg = Join-Path $BootDir "data.img"
    New-DataDisk $DataImg
    if (Test-Path $DataImg) {
        $qemuCmd += " -drive file=$DataImg,format=raw,if=ide,index=1"
        Write-Host "  Data disk attached" -ForegroundColor Green
    }

    $serialLog = Join-Path $BootDir "serial.log"
    if ($Serial) {
        $qemuCmd += " -serial stdio"
        Write-Host "  Starting (serial on this terminal)..." -ForegroundColor Green
        Write-Host ""
        cmd /c "`"$QEMU`" $qemuCmd"
    } else {
        $qemuCmd += " -serial file:$serialLog"
        Write-Host "  Serial log -> $serialLog" -ForegroundColor Gray
        Write-Host "  Starting..." -ForegroundColor Green
        Write-Host ""
        $proc = Start-Process -FilePath $QEMU -ArgumentList $qemuCmd -PassThru
        Register-Instance "vm1" $proc $serialLog
        Write-Host "  Manage it:  .\hive-cli.ps1 list" -ForegroundColor White
    }
}
