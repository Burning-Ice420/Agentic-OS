$base = "http://localhost:8080"

Write-Host "=== HiveMind OS Demo ===" -ForegroundColor Cyan
Write-Host "Make sure hivemind-vos is running first!`n" -ForegroundColor DarkGray

# 1. Create memory nodes
Write-Host "[1] Creating memory nodes..." -ForegroundColor Yellow
$hub = Invoke-RestMethod -Method POST "$base/hive/memories" -ContentType "application/json" -Body '{"name": "SensorHub"}'
$hubId = $hub.id
Write-Host "  SensorHub: $hubId" -ForegroundColor Green

$temp = Invoke-RestMethod -Method POST "$base/hive/memories" -ContentType "application/json" -Body "{`"name`": `"TempSensors`", `"parent_id`": `"$hubId`"}"
$tempId = $temp.id
Write-Host "  TempSensors: $tempId" -ForegroundColor Green

$pressure = Invoke-RestMethod -Method POST "$base/hive/memories" -ContentType "application/json" -Body "{`"name`": `"PressureSensors`", `"parent_id`": `"$hubId`"}"
$pressureId = $pressure.id
Write-Host "  PressureSensors: $pressureId" -ForegroundColor Green

$alerts = Invoke-RestMethod -Method POST "$base/hive/memories" -ContentType "application/json" -Body '{"name": "AlertCenter"}'
$alertsId = $alerts.id
Write-Host "  AlertCenter: $alertsId" -ForegroundColor Green

# 2. Link memories
Write-Host "`n[2] Linking memories..." -ForegroundColor Yellow
Invoke-RestMethod -Method POST "$base/hive/memories/link" -ContentType "application/json" -Body "{`"from_id`": `"$tempId`", `"to_id`": `"$alertsId`", `"edge_type`": `"Signal`"}"
Write-Host "  TempSensors --Signal--> AlertCenter"
Invoke-RestMethod -Method POST "$base/hive/memories/link" -ContentType "application/json" -Body "{`"from_id`": `"$pressureId`", `"to_id`": `"$alertsId`", `"edge_type`": `"Signal`"}"
Write-Host "  PressureSensors --Signal--> AlertCenter"
Invoke-RestMethod -Method POST "$base/hive/memories/link" -ContentType "application/json" -Body "{`"from_id`": `"$tempId`", `"to_id`": `"$pressureId`", `"edge_type`": `"Mirror`"}"
Write-Host "  TempSensors --Mirror--> PressureSensors"

# 3. Write sensor data
Write-Host "`n[3] Writing sensor data..." -ForegroundColor Yellow
Invoke-RestMethod -Method POST "$base/hive/memories/$tempId/blobs" -ContentType "application/json" -Body '{"key": "temp_01", "value": {"Number": 72.5}}'
Invoke-RestMethod -Method POST "$base/hive/memories/$tempId/blobs" -ContentType "application/json" -Body '{"key": "temp_02", "value": {"Number": 68.3}}'
Invoke-RestMethod -Method POST "$base/hive/memories/$tempId/blobs" -ContentType "application/json" -Body '{"key": "unit", "value": {"Text": "fahrenheit"}}'
Write-Host "  3 blobs -> TempSensors"
Invoke-RestMethod -Method POST "$base/hive/memories/$pressureId/blobs" -ContentType "application/json" -Body '{"key": "psi_main", "value": {"Number": 14.7}}'
Invoke-RestMethod -Method POST "$base/hive/memories/$pressureId/blobs" -ContentType "application/json" -Body '{"key": "status", "value": {"Text": "nominal"}}'
Write-Host "  2 blobs -> PressureSensors"
Invoke-RestMethod -Method POST "$base/hive/memories/$alertsId/blobs" -ContentType "application/json" -Body '{"key": "threshold", "value": {"Json": {"temp_max": 100, "psi_max": 30}}}'
Write-Host "  1 blob  -> AlertCenter"

# 4. Spawn agents
Write-Host "`n[4] Spawning agents..." -ForegroundColor Yellow
$a1 = Invoke-RestMethod -Method POST "$base/hive/agents" -ContentType "application/json" -Body "{`"memory_id`": `"$tempId`", `"name`": `"TempWatcher`", `"role`": `"Monitor`"}"
Write-Host "  TempWatcher (Monitor): $($a1.id)" -ForegroundColor Green
$a2 = Invoke-RestMethod -Method POST "$base/hive/agents" -ContentType "application/json" -Body "{`"memory_id`": `"$alertsId`", `"name`": `"AlertRouter`", `"role`": `"Router`"}"
Write-Host "  AlertRouter (Router): $($a2.id)" -ForegroundColor Green
$a3 = Invoke-RestMethod -Method POST "$base/hive/agents" -ContentType "application/json" -Body "{`"memory_id`": `"$hubId`", `"name`": `"HiveOrchestrator`", `"role`": `"Orchestrator`"}"
Write-Host "  HiveOrchestrator (Orchestrator): $($a3.id)" -ForegroundColor Green

# 5. Send a signal
Write-Host "`n[5] Broadcasting signal..." -ForegroundColor Yellow
Invoke-RestMethod -Method POST "$base/hive/signal" -ContentType "application/json" -Body "{`"from_memory_id`": `"$tempId`", `"signal_type`": `"temp_alert`", `"payload`": {`"sensor`": `"temp_01`", `"value`": 72.5}}"
Write-Host "  Signal sent: temp_alert from TempSensors"

# 6. Final snapshot
Write-Host "`n[6] Final state:" -ForegroundColor Green
$snap = Invoke-RestMethod "$base/hive/snapshot"
Write-Host "  Memories: $($snap.stats.total_memories)"
Write-Host "  Blobs:    $($snap.stats.total_blobs)"
Write-Host "  Agents:   $($snap.stats.total_agents)"
Write-Host "`n=== Check the Observer window to see the live graph! ===" -ForegroundColor Cyan
