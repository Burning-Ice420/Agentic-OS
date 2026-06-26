use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use uuid::Uuid;

use hivemind_kernel::{BlobValue, Hive};

/// Status of a virtual machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VMStatus {
    Stopped,
    Starting,
    Running,
    Paused,
    Error,
}

impl VMStatus {
    pub fn as_str(&self) -> &str {
        match self {
            VMStatus::Stopped => "Stopped",
            VMStatus::Starting => "Starting",
            VMStatus::Running => "Running",
            VMStatus::Paused => "Paused",
            VMStatus::Error => "Error",
        }
    }
}

/// Networking mode for a VM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NetworkMode {
    NAT,
    Bridged,
    HostOnly,
    HiveMesh,
}

impl NetworkMode {
    pub fn as_str(&self) -> &str {
        match self {
            NetworkMode::NAT => "NAT",
            NetworkMode::Bridged => "Bridged",
            NetworkMode::HostOnly => "HostOnly",
            NetworkMode::HiveMesh => "HiveMesh",
        }
    }
}

/// Configuration for creating a new VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VMConfig {
    pub name: String,
    pub disk_image_path: String,
    pub ram_mb: Option<u32>,
    pub vcpus: Option<u8>,
    pub network_mode: Option<String>,
    pub iso_path: Option<String>, // Boot ISO for initial install
}

/// A running or configured VM instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VMInstance {
    pub id: Uuid,
    pub name: String,
    pub memory_node_id: Uuid,
    #[serde(skip)]
    pub qemu_pid: Option<u32>,
    pub disk_image_path: PathBuf,
    pub ram_mb: u32,
    pub vcpus: u8,
    pub vnc_port: u16,
    pub status: VMStatus,
    pub network_mode: NetworkMode,
    pub iso_path: Option<PathBuf>,
}

/// Snapshot of a VM for the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VMSnapshot {
    pub id: String,
    pub name: String,
    pub memory_node_id: String,
    pub status: String,
    pub ram_mb: u32,
    pub vcpus: u8,
    pub vnc_port: u16,
    pub network_mode: String,
    pub disk_image_path: String,
}

impl VMInstance {
    pub fn snapshot(&self) -> VMSnapshot {
        VMSnapshot {
            id: self.id.to_string(),
            name: self.name.clone(),
            memory_node_id: self.memory_node_id.to_string(),
            status: self.status.as_str().to_string(),
            ram_mb: self.ram_mb,
            vcpus: self.vcpus,
            vnc_port: self.vnc_port,
            network_mode: self.network_mode.as_str().to_string(),
            disk_image_path: self.disk_image_path.to_string_lossy().to_string(),
        }
    }
}

/// Manages the lifecycle of all VMs.
pub struct VMManager {
    pub vms: Arc<RwLock<HashMap<Uuid, VMInstance>>>,
    /// Handles to QEMU child processes (not serializable).
    processes: Arc<RwLock<HashMap<Uuid, Child>>>,
    /// Path to the QEMU binary.
    qemu_path: PathBuf,
    /// Directory for disk images.
    disk_dir: PathBuf,
    /// Next available VNC display number (starts at 1, port = 5900 + display).
    next_vnc_display: Arc<RwLock<u16>>,
    /// Reference to the Hive for registering VM memory nodes.
    hive: Arc<Hive>,
}

impl VMManager {
    /// Create a new VMManager.
    pub fn new(hive: Arc<Hive>, qemu_path: PathBuf, disk_dir: PathBuf) -> Self {
        tracing::info!("VMManager initialized — QEMU: {:?}, disks: {:?}", qemu_path, disk_dir);
        Self {
            vms: Arc::new(RwLock::new(HashMap::new())),
            processes: Arc::new(RwLock::new(HashMap::new())),
            qemu_path,
            disk_dir,
            next_vnc_display: Arc::new(RwLock::new(1)),
            hive: Arc::new((*hive).clone()),
        }
    }

    /// Provision a new VM: creates disk image placeholder, registers a Memory node in the hive.
    pub async fn create_vm(&self, config: VMConfig) -> Result<VMInstance, String> {
        let disk_path = if config.disk_image_path.is_empty() {
            self.disk_dir.join(format!("{}.qcow2", config.name))
        } else {
            PathBuf::from(&config.disk_image_path)
        };

        // Create a Memory node for this VM in the hive
        let vm_registry = self.ensure_vm_registry().await?;
        let memory_node = self
            .hive
            .create_memory(format!("vm-{}", config.name), Some(vm_registry))
            .await?;

        let mut vnc_display = self.next_vnc_display.write().await;
        let vnc_port = 5900 + *vnc_display;
        *vnc_display += 1;

        let network_mode = match config.network_mode.as_deref() {
            Some("Bridged") => NetworkMode::Bridged,
            Some("HostOnly") => NetworkMode::HostOnly,
            Some("HiveMesh") => NetworkMode::HiveMesh,
            _ => NetworkMode::NAT,
        };

        let vm = VMInstance {
            id: Uuid::new_v4(),
            name: config.name.clone(),
            memory_node_id: memory_node.id,
            qemu_pid: None,
            disk_image_path: disk_path,
            ram_mb: config.ram_mb.unwrap_or(512),
            vcpus: config.vcpus.unwrap_or(1),
            vnc_port,
            status: VMStatus::Stopped,
            network_mode,
            iso_path: config.iso_path.map(PathBuf::from),
        };

        // Store VM config as blobs in its memory node
        self.hive
            .write_blob(
                memory_node.id,
                "vm_config".to_string(),
                BlobValue::Json(serde_json::to_value(&vm.snapshot()).unwrap()),
            )
            .await?;

        let vm_id = vm.id;
        {
            let mut vms = self.vms.write().await;
            vms.insert(vm_id, vm.clone());
        }

        tracing::info!("VM '{}' created (id={}, vnc={})", config.name, vm_id, vnc_port);
        Ok(vm)
    }

    /// Start a VM by spawning the QEMU process.
    pub async fn start_vm(&self, vm_id: Uuid) -> Result<(), String> {
        let (vm, qemu_args) = {
            let vms = self.vms.read().await;
            let vm = vms.get(&vm_id).ok_or("VM not found")?;

            if vm.status == VMStatus::Running {
                return Err("VM is already running".to_string());
            }

            let mut args = vec![
                // Use Q35 chipset — fixes IO-APIC kernel panics in modern Linux guests
                "-machine".to_string(),
                "q35,accel=tcg".to_string(),
                "-cpu".to_string(),
                "max".to_string(),
                "-m".to_string(),
                vm.ram_mb.to_string(),
                "-smp".to_string(),
                vm.vcpus.to_string(),
                "-vnc".to_string(),
                format!(":{}", vm.vnc_port - 5900),
                "-monitor".to_string(),
                "none".to_string(),
                // -serial for guest console output (useful for debugging)
                "-serial".to_string(),
                "stdio".to_string(),
            ];

            // Add disk image
            if vm.disk_image_path.exists() {
                args.extend_from_slice(&["-hda".to_string(), vm.disk_image_path.to_string_lossy().to_string()]);
            }

            // Add boot ISO if specified
            if let Some(ref iso) = vm.iso_path {
                if iso.exists() {
                    args.extend_from_slice(&[
                        "-cdrom".to_string(),
                        iso.to_string_lossy().to_string(),
                        "-boot".to_string(),
                        "d".to_string(),
                    ]);
                }
            }

            // Network configuration
            match vm.network_mode {
                NetworkMode::NAT => {
                    args.extend_from_slice(&[
                        "-netdev".to_string(),
                        format!("user,id=net0,hostfwd=tcp::{}-:7070", 10000 + (vm.vnc_port - 5900) as u32),
                        "-device".to_string(),
                        "virtio-net-pci,netdev=net0".to_string(),
                    ]);
                }
                NetworkMode::HostOnly => {
                    args.extend_from_slice(&[
                        "-netdev".to_string(),
                        "user,id=net0,restrict=on".to_string(),
                        "-device".to_string(),
                        "virtio-net-pci,netdev=net0".to_string(),
                    ]);
                }
                _ => {
                    // Bridged and HiveMesh use default for now
                    args.extend_from_slice(&[
                        "-netdev".to_string(),
                        "user,id=net0".to_string(),
                        "-device".to_string(),
                        "virtio-net-pci,netdev=net0".to_string(),
                    ]);
                }
            }

            (vm.clone(), args)
        };

        tracing::info!("Starting VM '{}' with QEMU: {:?} {:?}", vm.name, self.qemu_path, qemu_args);

        // Update status to Starting
        {
            let mut vms = self.vms.write().await;
            if let Some(v) = vms.get_mut(&vm_id) {
                v.status = VMStatus::Starting;
            }
        }

        // Spawn QEMU process
        let child = Command::new(&self.qemu_path)
            .args(&qemu_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start QEMU: {}. Is QEMU installed at {:?}?", e, self.qemu_path))?;

        let pid = child.id();

        {
            let mut processes = self.processes.write().await;
            processes.insert(vm_id, child);
        }

        // Update VM status
        {
            let mut vms = self.vms.write().await;
            if let Some(v) = vms.get_mut(&vm_id) {
                v.status = VMStatus::Running;
                v.qemu_pid = pid;
            }
        }

        // Update hive memory with running status
        self.hive
            .write_blob(
                vm.memory_node_id,
                "vm_status".to_string(),
                BlobValue::Text("Running".to_string()),
            )
            .await
            .ok();

        tracing::info!("VM '{}' started (pid={:?}, vnc={})", vm.name, pid, vm.vnc_port);
        Ok(())
    }

    /// Stop a VM gracefully.
    pub async fn stop_vm(&self, vm_id: Uuid) -> Result<(), String> {
        let memory_node_id;
        let vm_name;

        {
            let vms = self.vms.read().await;
            let vm = vms.get(&vm_id).ok_or("VM not found")?;
            memory_node_id = vm.memory_node_id;
            vm_name = vm.name.clone();
        }

        // Kill the QEMU process
        {
            let mut processes = self.processes.write().await;
            if let Some(mut child) = processes.remove(&vm_id) {
                let _ = child.kill().await;
                tracing::info!("QEMU process killed for VM '{}'", vm_name);
            }
        }

        // Update VM status
        {
            let mut vms = self.vms.write().await;
            if let Some(v) = vms.get_mut(&vm_id) {
                v.status = VMStatus::Stopped;
                v.qemu_pid = None;
            }
        }

        // Update hive memory
        self.hive
            .write_blob(
                memory_node_id,
                "vm_status".to_string(),
                BlobValue::Text("Stopped".to_string()),
            )
            .await
            .ok();

        tracing::info!("VM '{}' stopped", vm_name);
        Ok(())
    }

    /// Snapshot a VM: saves its blob state to JSON.
    pub async fn snapshot_vm(&self, vm_id: Uuid) -> Result<serde_json::Value, String> {
        let vms = self.vms.read().await;
        let vm = vms.get(&vm_id).ok_or("VM not found")?;

        let blobs = self.hive.get_all_blobs(vm.memory_node_id).await?;
        let snapshot = serde_json::json!({
            "vm": vm.snapshot(),
            "blobs": blobs.iter().map(|b| serde_json::to_value(b).unwrap()).collect::<Vec<_>>(),
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });

        // Also write the snapshot as a blob
        self.hive
            .write_blob(
                vm.memory_node_id,
                "last_snapshot".to_string(),
                BlobValue::Json(snapshot.clone()),
            )
            .await
            .ok();

        Ok(snapshot)
    }

    /// List all VMs.
    pub async fn list_vms(&self) -> Vec<VMSnapshot> {
        let vms = self.vms.read().await;
        vms.values().map(|v| v.snapshot()).collect()
    }

    /// Get a specific VM.
    pub async fn get_vm(&self, vm_id: Uuid) -> Result<VMSnapshot, String> {
        let vms = self.vms.read().await;
        vms.get(&vm_id)
            .map(|v| v.snapshot())
            .ok_or_else(|| "VM not found".to_string())
    }

    /// Ensure the "vm-registry" Memory node exists in the hive.
    async fn ensure_vm_registry(&self) -> Result<Uuid, String> {
        let memories = self.hive.list_memories().await;
        for (id, name) in &memories {
            if name == "vm-registry" {
                return Ok(*id);
            }
        }
        // Create it
        let registry = self.hive.create_memory("vm-registry".to_string(), None).await?;
        Ok(registry.id)
    }

    /// Create a QCOW2 disk image using qemu-img.
    pub async fn create_disk_image(&self, path: &PathBuf, size_gb: u32) -> Result<(), String> {
        let qemu_img = self.qemu_path.parent()
            .map(|p| p.join("qemu-img.exe"))
            .unwrap_or_else(|| PathBuf::from("qemu-img"));

        let output = Command::new(&qemu_img)
            .args(["create", "-f", "qcow2", &path.to_string_lossy(), &format!("{}G", size_gb)])
            .output()
            .await
            .map_err(|e| format!("Failed to run qemu-img: {}", e))?;

        if output.status.success() {
            tracing::info!("Created disk image: {:?} ({}GB)", path, size_gb);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("qemu-img failed: {}", stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_snapshot() {
        let vm = VMInstance {
            id: Uuid::new_v4(),
            name: "test-vm".to_string(),
            memory_node_id: Uuid::new_v4(),
            qemu_pid: None,
            disk_image_path: PathBuf::from("C:\\test\\disk.qcow2"),
            ram_mb: 512,
            vcpus: 2,
            vnc_port: 5901,
            status: VMStatus::Stopped,
            network_mode: NetworkMode::NAT,
            iso_path: None,
        };
        let snap = vm.snapshot();
        assert_eq!(snap.name, "test-vm");
        assert_eq!(snap.status, "Stopped");
        assert_eq!(snap.vnc_port, 5901);
    }
}
