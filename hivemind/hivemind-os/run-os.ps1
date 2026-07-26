# HiveMind OS - Build and Launch Script
#
# Usage:
#   .\run-os.ps1                       # Single VM, hardware-accelerated (WHPX)
#   .\run-os.ps1 -Release              # Optimized build
#   .\run-os.ps1 -VMCount 2            # Two VMs connected via COM2 mesh serial
#   .\run-os.ps1 -Serial               # Pipe COM1 serial log to this terminal
#   .\run-os.ps1 -Observe              # One command: boot + serial->8080 bridge + observer GUI
#   .\run-os.ps1 -Accel tcg            # Force pure software emulation
#   .\run-os.ps1 -Memory 512 -Cpus 2 -DiskMB 8
#   .\run-os.ps1 -QEMU <path>          # Override QEMU executable path
#
# Every launched VM is registered under C:\hivemind\instances\ so `hive-cli.ps1`
# can list running instances, their per-boot UUID and their resource allocation.

param(
    [switch]$Release,
    [switch]$Serial,
    [switch]$LLM,                    # expose COM3 to the AI-accelerator bridge
    [switch]$Observe,                # one-command stack: COM2+COM3 exposed, bridge + observer auto-launched
    [string]$Model   = "llama3.2:1b",# model the observer bridge feeds to
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
    if ($Observe) {
        # Observed swarm: start the mesh HUB + observer feed FIRST, then every VM
        # connects to it as a COM2 client. The hub relays HMSG between guests (the
        # real mesh) and records each instance for the observer.
        $bridge = Join-Path $ROOT "hive-observer-bridge.py"
        Start-Process -FilePath "python" -ArgumentList "`"$bridge`" --hub --model $Model" -WorkingDirectory $ROOT
        Write-Host "  Mesh hub + observer feed -> :4444 (mesh) / :8080 (observer), model $Model" -ForegroundColor Magenta
        Write-Host "  (requires Ollama:  ollama serve )" -ForegroundColor DarkGray
        Start-Sleep -Milliseconds 1500
    } else {
        Write-Host "  Launching $VMCount VMs with COM2 mesh (TCP 4444)..." -ForegroundColor Yellow
    }

    for ($i = 1; $i -le $VMCount; $i++) {
        $disk = Join-Path $BootDir "data_vm$i.img"
        New-DataDisk $disk
        $serialLog = "$BootDir\vm$i.log"
        $vmArgs = "$qemuCmd -drive file=$disk,format=raw,if=ide,index=1 -serial file:$serialLog"
        if ($Observe) {
            # every VM is a COM2 client of the hub; VM1 also owns the COM3 AI device
            $vmArgs += " -serial tcp:127.0.0.1:4444"
            if ($i -eq 1) { $vmArgs += " -serial tcp:127.0.0.1:4455,server,nowait" }
        } elseif ($i -eq 1) {
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
    if ($Observe) {
        $obs = Join-Path $ROOT "..\target\debug\hivemind-observer.exe"
        if (Test-Path $obs) {
            Start-Process -FilePath $obs
            Write-Host "  Observer GUI launched (reads localhost:8080)" -ForegroundColor White
        } else {
            Write-Host "  Observer GUI not built; run 'cargo run -p hivemind-observer' from ..\ " -ForegroundColor DarkGray
        }
        Write-Host ""
        Write-Host "  Watch memory propagate across instances. In VM1's shell:" -ForegroundColor White
        Write-Host "    net send shared temp 85           -> vm1:shared AND vm2:shared appear, linked" -ForegroundColor Gray
        Write-Host "    agent new T 1" -ForegroundColor Gray
        Write-Host "    agent rule 2 temp gt:80 decision ai:overheating decide" -ForegroundColor Gray
        Write-Host "    blob write 1 temp 95              -> VM1's AI node updates live" -ForegroundColor Gray
    } else {
        Write-Host "  Manage them:  .\hive-cli.ps1 list" -ForegroundColor White
        Write-Host "  Demo: in VM1 'net send SensorHub temp 85', in VM2 'mem list'" -ForegroundColor White
    }

} else {
    # ── Single VM mode ────────────────────────────────────────────────────────
    $DataImg = Join-Path $BootDir "data.img"
    New-DataDisk $DataImg
    if (Test-Path $DataImg) {
        $qemuCmd += " -drive file=$DataImg,format=raw,if=ide,index=1"
        Write-Host "  Data disk attached" -ForegroundColor Green
    }

    $serialLog = Join-Path $BootDir "serial.log"

    # COM1 = console/serial log. Order maps to COM1, COM2, COM3.
    #   -Observe : COM2 (mesh) AND COM3 (LLM) both exposed as TCP servers so the
    #              serial->8080 bridge can tap both and feed the observer.
    #   -LLM     : COM2 = null, COM3 = the AI-accelerator socket.
    if ($Observe) {
        $qemuCmd += " -serial file:$serialLog"
        $qemuCmd += " -serial tcp:127.0.0.1:4444,server,nowait -serial tcp:127.0.0.1:4455,server,nowait"
    } elseif ($Serial) {
        $qemuCmd += " -serial stdio"
    } else {
        $qemuCmd += " -serial file:$serialLog"
    }
    if ($LLM -and -not $Observe) {
        $qemuCmd += " -serial null -serial tcp:127.0.0.1:4455,server,nowait"
        Write-Host "  AI accelerator (COM3) enabled." -ForegroundColor Magenta
        Write-Host "  In another terminal run:  python hive-llm-bridge.py" -ForegroundColor White
        Write-Host "  (needs Ollama + a small model, e.g. 'ollama pull llama3.2:1b')" -ForegroundColor DarkGray
    }

    if ($Serial -and -not $Observe) {
        Write-Host "  Starting (serial on this terminal)..." -ForegroundColor Green
        Write-Host ""
        cmd /c "`"$QEMU`" $qemuCmd"
    } else {
        Write-Host "  Serial log -> $serialLog" -ForegroundColor Gray
        Write-Host "  Starting..." -ForegroundColor Green
        Write-Host ""
        $proc = Start-Process -FilePath $QEMU -ArgumentList $qemuCmd -PassThru
        Register-Instance "vm1" $proc $serialLog

        if ($Observe) {
            Start-Sleep -Milliseconds 900
            Write-Host ""
            Write-Host "  ── Observer stack ──────────────────────────────────" -ForegroundColor Magenta
            # 1. serial->8080 bridge: acts as the AI accelerator AND feeds the observer
            $bridge = Join-Path $ROOT "hive-observer-bridge.py"
            Start-Process -FilePath "python" -ArgumentList "`"$bridge`" --model $Model" -WorkingDirectory $ROOT
            Write-Host "    bridge  -> http://localhost:8080/hive/snapshot   (model: $Model)" -ForegroundColor White
            # 2. the observer GUI, if it is built
            $obs = Join-Path $ROOT "..\target\debug\hivemind-observer.exe"
            if (Test-Path $obs) {
                Start-Process -FilePath $obs
                Write-Host "    observer-> launched (reads localhost:8080)" -ForegroundColor White
            } else {
                Write-Host "    observer-> not built; run 'cargo run -p hivemind-observer' from ..\ " -ForegroundColor DarkGray
            }
            Write-Host "    (requires Ollama:  ollama serve )" -ForegroundColor DarkGray
            Write-Host ""
            Write-Host "  In the OS shell, watch the observer update as you type:" -ForegroundColor White
            Write-Host "    agent new TempAI 1" -ForegroundColor Gray
            Write-Host "    agent rule 2 temp gt:80 decision ai:overheating decide" -ForegroundColor Gray
            Write-Host "    blob write 1 temp 95" -ForegroundColor Gray
        }
        Write-Host "  Manage it:  .\hive-cli.ps1 list" -ForegroundColor White
    }
}
