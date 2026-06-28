# HiveMind OS - Build and Launch Script
#
# Usage:
#   .\run-os.ps1                   # Single VM (debug build)
#   .\run-os.ps1 -Release          # Single VM (optimized)
#   .\run-os.ps1 -VMCount 2        # Two VMs connected via COM2 mesh serial
#   .\run-os.ps1 -Serial           # Pipe COM1 serial log to this terminal
#   .\run-os.ps1 -QEMU <path>      # Override QEMU executable path

param(
    [switch]$Release,
    [switch]$Serial,
    [int]   $VMCount = 1,
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
Write-Host "  Click inside QEMU window for keyboard input" -ForegroundColor DarkCyan
Write-Host "  Ctrl+Alt+Q or Alt+F4 to exit" -ForegroundColor DarkCyan
Write-Host "  Ctrl+Alt+G to release mouse" -ForegroundColor DarkCyan
Write-Host ""

# Copy boot image to C:\hivemind (QEMU can't handle spaces in paths)
$BootDir = "C:\hivemind"
if (-not (Test-Path $BootDir)) { New-Item -ItemType Directory -Path $BootDir -Force | Out-Null }

$BootImg = Join-Path $BootDir "boot.bin"
Copy-Item -Path $IMG -Destination $BootImg -Force
Write-Host "  Boot image -> $BootImg" -ForegroundColor Green

# Data disk for save/load
$DataImg = Join-Path $BootDir "data.img"
$QEMU_IMG = Join-Path (Split-Path $QEMU) "qemu-img.exe"

if (-not (Test-Path $DataImg)) {
    if (Test-Path $QEMU_IMG) {
        Write-Host "  Creating 1 MiB data disk..." -ForegroundColor Yellow
        & $QEMU_IMG create -f raw $DataImg 1M 2>$null | Out-Null
        Write-Host "  Data disk -> $DataImg" -ForegroundColor Green
    } else {
        Write-Host "  [WARN] qemu-img not found, save/load disabled" -ForegroundColor Yellow
    }
}

# Build QEMU argument string (flat string avoids PowerShell mangling)
$qemuCmd = "-drive format=raw,file=$BootImg -m 256M -no-reboot -no-shutdown"

if (Test-Path $DataImg) {
    $qemuCmd += " -drive file=$DataImg,format=raw,if=ide,index=1"
    Write-Host "  Data disk attached" -ForegroundColor Green
}

$serialLog = Join-Path $BootDir "serial.log"

if ($VMCount -ge 2) {
    # ── Multi-VM mode ─────────────────────────────────────────────────────────
    Write-Host "  Launching $VMCount VMs with COM2 mesh (TCP 4444)..." -ForegroundColor Yellow

    $vm1Cmd = "$qemuCmd -serial file:$BootDir\vm1.log -serial tcp::4444,server,nowait"
    Write-Host "  VM1 (server :4444)..." -ForegroundColor Cyan
    Start-Process -FilePath $QEMU -ArgumentList $vm1Cmd
    Start-Sleep -Milliseconds 800

    $vm2Cmd = "$qemuCmd -serial file:$BootDir\vm2.log -serial tcp:127.0.0.1:4444"
    Write-Host "  VM2 (client)..." -ForegroundColor Cyan
    Start-Process -FilePath $QEMU -ArgumentList $vm2Cmd

    for ($i = 3; $i -le $VMCount; $i++) {
        $vmCmd = "$qemuCmd -serial file:$BootDir\vm${i}.log"
        Write-Host "  VM$i (standalone)..." -ForegroundColor Cyan
        Start-Process -FilePath $QEMU -ArgumentList $vmCmd
        Start-Sleep -Milliseconds 300
    }

    Write-Host ""
    Write-Host "  All VMs running." -ForegroundColor Green
    Write-Host "  Demo: in VM1 type 'net send SensorHub temp 85'" -ForegroundColor White
    Write-Host "        in VM2 type 'mem list' to see it arrive" -ForegroundColor White

} else {
    # ── Single VM mode ────────────────────────────────────────────────────────
    if ($Serial) {
        $qemuCmd += " -serial stdio"
    } else {
        $qemuCmd += " -serial file:$serialLog"
        Write-Host "  Serial log -> $serialLog" -ForegroundColor Gray
    }

    Write-Host "  Starting..." -ForegroundColor Green
    Write-Host ""

    # Use cmd /c to bypass PowerShell argument parsing entirely
    cmd /c "`"$QEMU`" $qemuCmd"
}
