# HiveMind OS — Build and Launch Script
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

$ErrorActionPreference = "Stop"
$ROOT = "$PSScriptRoot"

Set-Location $ROOT

# ── 0. PATH ───────────────────────────────────────────────────────────────────
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

# ── 1. Check QEMU ─────────────────────────────────────────────────────────────
if (-not (Test-Path $QEMU)) {
    Write-Host "[ERROR] QEMU not found at: $QEMU" -ForegroundColor Red
    Write-Host "        Set -QEMU to the correct path, e.g.:" -ForegroundColor Yellow
    Write-Host "        .\run-os.ps1 -QEMU 'C:\path\to\qemu-system-x86_64.exe'" -ForegroundColor Yellow
    exit 1
}
Write-Host "[OK] QEMU found: $QEMU" -ForegroundColor Green

# ── 2. Install nightly if needed ──────────────────────────────────────────────
Write-Host "`n[1/4] Checking Rust nightly toolchain..." -ForegroundColor Cyan
$toolchain = Get-Content "$ROOT\rust-toolchain.toml" | Select-String "channel" | ForEach-Object { $_ -replace '.*"(.*)".*', '$1' }
$installed = rustup toolchain list 2>&1 | Select-String "nightly"
if (-not $installed) {
    Write-Host "      Installing nightly toolchain (first time, may take a few minutes)..." -ForegroundColor Yellow
    rustup toolchain install nightly --component rust-src llvm-tools-preview
} else {
    Write-Host "      Nightly already installed." -ForegroundColor Green
}

# Ensure components
rustup component add rust-src        --toolchain nightly 2>&1 | Out-Null
rustup component add llvm-tools-preview --toolchain nightly 2>&1 | Out-Null
Write-Host "      Components: rust-src, llvm-tools-preview [OK]" -ForegroundColor Green

# ── 3. Install bootimage if needed ────────────────────────────────────────────
Write-Host "`n[2/4] Checking bootimage tool..." -ForegroundColor Cyan
$bi = Get-Command bootimage -ErrorAction SilentlyContinue
if (-not $bi) {
    Write-Host "      Installing bootimage (required once)..." -ForegroundColor Yellow
    cargo install bootimage
} else {
    Write-Host "      bootimage already installed." -ForegroundColor Green
}

# ── 4. Build ──────────────────────────────────────────────────────────────────
Write-Host "`n[3/4] Building HiveMind OS..." -ForegroundColor Cyan

if ($Release) {
    Write-Host "      Mode: RELEASE" -ForegroundColor Magenta
    cargo bootimage --release
    $IMG = "target\x86_64-hivemind-os\release\bootimage-hivemind-os.bin"
} else {
    Write-Host "      Mode: debug" -ForegroundColor Gray
    cargo bootimage
    $IMG = "target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin"
}

if ($LASTEXITCODE -ne 0) {
    Write-Host "`n[FAILED] Build failed. See errors above." -ForegroundColor Red
    exit 1
}

Write-Host "      Build image: $IMG" -ForegroundColor Green

# ── 5. Launch QEMU ────────────────────────────────────────────────────────────
Write-Host "`n[4/4] Launching QEMU..." -ForegroundColor Cyan
Write-Host ""
Write-Host "  ╔════════════════════════════════════════════╗" -ForegroundColor DarkCyan
Write-Host "  ║        HiveMind OS — QEMU Window           ║" -ForegroundColor DarkCyan
Write-Host "  ║                                            ║" -ForegroundColor DarkCyan
Write-Host "  ║  Keyboard input: click inside QEMU window ║" -ForegroundColor DarkCyan
Write-Host "  ║  Exit QEMU:      Ctrl+Alt+Q  or  Alt+F4   ║" -ForegroundColor DarkCyan
Write-Host "  ║  Release mouse:  Ctrl+Alt+G                ║" -ForegroundColor DarkCyan
if ($Serial) {
Write-Host "  ║  Serial log:     this terminal             ║" -ForegroundColor DarkCyan
}
Write-Host "  ╚════════════════════════════════════════════╝" -ForegroundColor DarkCyan
Write-Host ""

$baseArgs = @(
    "-drive", "format=raw,file=$IMG",
    "-m",     "256M",
    "-no-reboot",
    "-no-shutdown"
)

# ── Data disk (ATA slave for save/load) ───────────────────────────────────────
$DataImg = "$ROOT\data.img"
$QEMU_IMG = Join-Path (Split-Path $QEMU) "qemu-img.exe"

if (-not (Test-Path $DataImg)) {
    if (Test-Path $QEMU_IMG) {
        Write-Host "  Creating 1 MiB data disk: data.img" -ForegroundColor Yellow
        & $QEMU_IMG create -f raw $DataImg 1M | Out-Null
        Write-Host "  data.img created — 'save' and 'load' commands will now work." -ForegroundColor Green
    } else {
        Write-Host "  [WARN] qemu-img not found — data disk skipped (save/load disabled)" -ForegroundColor Yellow
    }
}

if (Test-Path $DataImg) {
    $baseArgs += @("-drive", "file=$DataImg,format=raw,if=ide,index=1")
    Write-Host "  Data disk: $DataImg attached as IDE slave" -ForegroundColor Green
}

if ($VMCount -ge 2) {
    # ── Multi-VM: two QEMU windows connected via COM2 TCP serial ─────────────
    Write-Host "  Launching $VMCount VMs with COM2 mesh link (TCP port 4444)..." -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  VM1  COM2 = TCP server :4444" -ForegroundColor White
    Write-Host "  VM2  COM2 = TCP client  127.0.0.1:4444" -ForegroundColor White
    Write-Host ""
    Write-Host "  DEMO — in VM1 shell type:" -ForegroundColor Green
    Write-Host "    mem new SensorHub" -ForegroundColor White
    Write-Host "    blob write 2 temperature 85" -ForegroundColor White
    Write-Host "    net send SensorHub temperature 85" -ForegroundColor White
    Write-Host "  Then in VM2 shell type:" -ForegroundColor Green
    Write-Host "    mem list      <- SensorHub appears automatically" -ForegroundColor White
    Write-Host "    net status    <- RX count shows received messages" -ForegroundColor White
    Write-Host ""

    # VM1 — COM1 log + COM2 TCP server
    $vm1Args = $baseArgs + @("-serial", "file:vm1-serial.log", "-serial", "tcp::4444,server,nowait")
    Write-Host "  Launching VM1 (TCP server on :4444)..." -ForegroundColor Cyan
    Start-Process -FilePath $QEMU -ArgumentList $vm1Args -WindowStyle Normal

    # Give the TCP server a moment to bind before VM2 connects.
    Start-Sleep -Milliseconds 800

    # VM2 — COM1 log + COM2 TCP client
    $vm2Args = $baseArgs + @("-serial", "file:vm2-serial.log", "-serial", "tcp:127.0.0.1:4444")
    Write-Host "  Launching VM2 (TCP client → 127.0.0.1:4444)..." -ForegroundColor Cyan
    Start-Process -FilePath $QEMU -ArgumentList $vm2Args -WindowStyle Normal

    # VM3+ run standalone (no mesh to them; useful for load testing)
    for ($i = 3; $i -le $VMCount; $i++) {
        $vmArgs = $baseArgs + @("-serial", "file:vm${i}-serial.log")
        Write-Host "  Launching VM$i (standalone)..." -ForegroundColor Cyan
        Start-Process -FilePath $QEMU -ArgumentList $vmArgs -WindowStyle Normal
        Start-Sleep -Milliseconds 300
    }

    Write-Host ""
    Write-Host "  All VMs running. Serial logs: vm1-serial.log  vm2-serial.log" -ForegroundColor Green

} else {
    # ── Single VM ─────────────────────────────────────────────────────────────
    if ($Serial) {
        $qemuArgs = $baseArgs + @("-serial", "stdio")
    } else {
        $qemuArgs = $baseArgs + @("-serial", "file:hivemind-serial.log")
        Write-Host "  Serial output → hivemind-serial.log" -ForegroundColor Gray
    }
    & $QEMU @qemuArgs
}
