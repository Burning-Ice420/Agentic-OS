param(
    [switch]$Build,
    [string]$QEMU = "C:\msys64\mingw64\bin\qemu-system-x86_64.exe",
    [string]$BootImage = "",
    [string]$RuntimeDir = "C:\hivemind\mesh",
    [int]$TcpPort = 4444,
    [int]$RamMb = 256
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$OsRoot = Join-Path $RepoRoot "hivemind\hivemind-os"

if (-not (Test-Path $QEMU)) {
    throw "QEMU not found at $QEMU"
}

if ($Build) {
    Push-Location $OsRoot
    try {
        $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
        cargo bootimage
    } finally {
        Pop-Location
    }
}

if ([string]::IsNullOrWhiteSpace($BootImage)) {
    $BootImage = Join-Path $OsRoot "target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin"
}

if (-not (Test-Path $BootImage)) {
    throw "Boot image not found: $BootImage. Run with -Build or run cargo bootimage first."
}

New-Item -ItemType Directory -Path $RuntimeDir -Force | Out-Null

$BootCopy = Join-Path $RuntimeDir "boot.bin"
$Vm1Disk = Join-Path $RuntimeDir "vm1-data.img"
$Vm2Disk = Join-Path $RuntimeDir "vm2-data.img"
$Vm1Log = Join-Path $RuntimeDir "vm1-serial.log"
$Vm2Log = Join-Path $RuntimeDir "vm2-serial.log"

Copy-Item -Path $BootImage -Destination $BootCopy -Force

$QemuImg = Join-Path (Split-Path $QEMU) "qemu-img.exe"
foreach ($disk in @($Vm1Disk, $Vm2Disk)) {
    if (-not (Test-Path $disk)) {
        if (-not (Test-Path $QemuImg)) {
            throw "qemu-img.exe not found next to QEMU; cannot create $disk"
        }
        & $QemuImg create -f raw $disk 1M | Out-Null
    }
}

Remove-Item $Vm1Log, $Vm2Log -ErrorAction SilentlyContinue

Write-Host "Stopping old QEMU instances..." -ForegroundColor DarkGray
Get-Process qemu* -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

$Common = "-drive format=raw,file=$BootCopy -m ${RamMb}M -no-reboot -no-shutdown"

$Vm1Args = "$Common -drive file=$Vm1Disk,format=raw,if=ide,index=1 -serial file:$Vm1Log -serial tcp:127.0.0.1:$TcpPort,server=on,wait=off"
$Vm2Args = "$Common -drive file=$Vm2Disk,format=raw,if=ide,index=1 -serial file:$Vm2Log -serial tcp:127.0.0.1:$TcpPort"

Write-Host "Launching VM1 as COM2 mesh server on tcp://127.0.0.1:$TcpPort" -ForegroundColor Cyan
Start-Process -FilePath $QEMU -ArgumentList $Vm1Args
Start-Sleep -Milliseconds 900

Write-Host "Launching VM2 as COM2 mesh client" -ForegroundColor Cyan
Start-Process -FilePath $QEMU -ArgumentList $Vm2Args

Write-Host ""
Write-Host "Two HiveMind OS VMs are launching." -ForegroundColor Green
Write-Host "Runtime dir : $RuntimeDir"
Write-Host "VM1 log     : $Vm1Log"
Write-Host "VM2 log     : $Vm2Log"
Write-Host ""
Write-Host "Manual mesh test:" -ForegroundColor Yellow
Write-Host "  1. Click VM1 and type: net send SensorHub temp 85"
Write-Host "  2. Click VM2 and type: mem list"
Write-Host "  3. VM2 should show/create a SensorHub memory node after COM2 receives the HMSG line."
Write-Host ""
Write-Host "Useful in each VM:" -ForegroundColor Yellow
Write-Host "  help"
Write-Host "  net status"
Write-Host "  mem list"
Write-Host "  blob read 1 status"
