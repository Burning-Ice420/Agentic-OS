# HiveMind CLI - manage and inspect running HiveMind OS instances.
#
# Instances are registered by run-os.ps1 under C:\hivemind\instances\*.json.
# Each running OS generates a fresh per-boot UUID (see the kernel `sysinfo`
# command); this CLI reads it back from the instance's COM1 serial log.
#
# Run inside Windows Terminal / PowerShell so you get native scroll-back for
# large output (e.g. `hive-cli help` or a long `connect` stream).
#
# Usage:
#   .\hive-cli.ps1 list                 # how many instances + their UUIDs
#   .\hive-cli.ps1 info    <name|pid>   # full detail for one instance
#   .\hive-cli.ps1 uuid    <name|pid>   # just the UUID
#   .\hive-cli.ps1 connect <name|pid>   # live-follow that instance's console
#   .\hive-cli.ps1 stop    <name|pid>   # terminate that VM
#   .\hive-cli.ps1 help

param(
    [string]$Command = "list",
    [string]$Target
)

$InstDir = "C:\hivemind\instances"

function Get-InstanceUuid($serialLog) {
    if ($serialLog -and (Test-Path $serialLog)) {
        $hit = Select-String -Path $serialLog -Pattern 'instance-uuid=([0-9a-fA-F-]{36})' -ErrorAction SilentlyContinue |
               Select-Object -Last 1
        if ($hit) { return $hit.Matches[0].Groups[1].Value }
    }
    return "(pending)"
}

function Get-Instances {
    if (-not (Test-Path $InstDir)) { return @() }
    $list = @()
    foreach ($f in Get-ChildItem $InstDir -Filter *.json -ErrorAction SilentlyContinue) {
        try {
            $m = Get-Content $f.FullName -Raw | ConvertFrom-Json
        } catch { continue }
        $proc  = Get-Process -Id $m.pid -ErrorAction SilentlyContinue
        if (-not $proc) {
            # Stale manifest - process gone. Clean it up and skip.
            Remove-Item $f.FullName -Force -ErrorAction SilentlyContinue
            continue
        }
        $up = (New-TimeSpan -Start ([datetime]$m.launchedAt) -End (Get-Date))
        $m | Add-Member -NotePropertyName uuid   -NotePropertyValue (Get-InstanceUuid $m.serialLog) -Force
        $m | Add-Member -NotePropertyName uptime -NotePropertyValue ("{0:mm}m{0:ss}s" -f $up) -Force
        $list += $m
    }
    return $list
}

function Resolve-Instance($target) {
    $all = Get-Instances
    if (-not $target) { return $null }
    $hit = $all | Where-Object { $_.name -eq $target -or "$($_.pid)" -eq $target }
    return $hit | Select-Object -First 1
}

switch ($Command.ToLower()) {

    "list" {
        $all = @(Get-Instances)
        Write-Host ""
        Write-Host "HiveMind instances connected: $($all.Count)" -ForegroundColor Cyan
        Write-Host ("-" * 92) -ForegroundColor DarkGray
        if ($all.Count -eq 0) {
            Write-Host "  No running instances. Launch one with:  .\run-os.ps1" -ForegroundColor Yellow
            break
        }
        "{0,-6} {1,-8} {2,-38} {3,-8} {4,-6} {5,-7} {6}" -f `
            "NAME","PID","UUID","RAM","CPUS","DISK","UPTIME" | Write-Host -ForegroundColor White
        foreach ($m in $all) {
            "{0,-6} {1,-8} {2,-38} {3,-8} {4,-6} {5,-7} {6}" -f `
                $m.name, $m.pid, $m.uuid, "$($m.memoryMB)M", $m.cpus, "$($m.diskMB)M", $m.uptime |
                Write-Host
        }
        Write-Host ""
    }

    "info" {
        $m = Resolve-Instance $Target
        if (-not $m) { Write-Host "No such instance: '$Target'" -ForegroundColor Red; break }
        Write-Host ""
        Write-Host "Instance '$($m.name)'" -ForegroundColor Cyan
        Write-Host ("-" * 50) -ForegroundColor DarkGray
        Write-Host ("  UUID        : {0}" -f $m.uuid)
        Write-Host ("  PID         : {0}" -f $m.pid)
        Write-Host ("  Acceleration: {0}" -f $m.accel)
        Write-Host ("  Memory      : {0} MiB" -f $m.memoryMB)
        Write-Host ("  vCPUs       : {0}" -f $m.cpus)
        Write-Host ("  Data disk   : {0} MiB" -f $m.diskMB)
        Write-Host ("  Uptime      : {0}" -f $m.uptime)
        Write-Host ("  Serial log  : {0}" -f $m.serialLog)
        Write-Host ""
    }

    "uuid" {
        $m = Resolve-Instance $Target
        if (-not $m) { Write-Host "No such instance: '$Target'" -ForegroundColor Red; break }
        Write-Host $m.uuid
    }

    "connect" {
        $m = Resolve-Instance $Target
        if (-not $m) { Write-Host "No such instance: '$Target'" -ForegroundColor Red; break }
        Write-Host ""
        Write-Host "Connected to '$($m.name)'  uuid=$($m.uuid)  pid=$($m.pid)" -ForegroundColor Cyan
        Write-Host "Live console (COM1). Ctrl+C to disconnect. Scroll up to see history." -ForegroundColor DarkGray
        Write-Host "To type into the OS, use its QEMU window (keyboard + mouse)." -ForegroundColor DarkGray
        Write-Host ("-" * 70) -ForegroundColor DarkGray
        if ($m.serialLog -and (Test-Path $m.serialLog)) {
            Get-Content -Path $m.serialLog -Wait -Tail 40
        } else {
            Write-Host "  (serial log not available yet: $($m.serialLog))" -ForegroundColor Yellow
        }
    }

    "stop" {
        $m = Resolve-Instance $Target
        if (-not $m) { Write-Host "No such instance: '$Target'" -ForegroundColor Red; break }
        Stop-Process -Id $m.pid -Force -ErrorAction SilentlyContinue
        Remove-Item (Join-Path $InstDir "$($m.pid).json") -Force -ErrorAction SilentlyContinue
        Write-Host "Stopped '$($m.name)' (pid $($m.pid), uuid $($m.uuid))" -ForegroundColor Green
    }

    default {
        Write-Host ""
        Write-Host "HiveMind CLI - manage running HiveMind OS instances" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "  hive-cli list                 How many instances + their UUIDs/resources"
        Write-Host "  hive-cli info    <name|pid>   Full detail for one instance"
        Write-Host "  hive-cli uuid    <name|pid>   Print just the instance UUID"
        Write-Host "  hive-cli connect <name|pid>   Live-follow that instance's console (COM1)"
        Write-Host "  hive-cli stop    <name|pid>   Terminate that VM"
        Write-Host "  hive-cli help                 This help"
        Write-Host ""
        Write-Host "Each OS boot generates a fresh random UUID (kernel `sysinfo` command)." -ForegroundColor DarkGray
        Write-Host "Run this in Windows Terminal for scroll-back on long output." -ForegroundColor DarkGray
        Write-Host ""
    }
}
