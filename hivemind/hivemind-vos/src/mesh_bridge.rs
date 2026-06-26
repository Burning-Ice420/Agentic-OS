use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use uuid::Uuid;

use hivemind_kernel::{BlobValue, Hive};

use crate::vm::VMManager;

/// The MeshBridge polls each running VM's mesh-agent and syncs state
/// as blobs into that VM's Memory node in the hive. VMs can message
/// each other by writing to shared Memory nodes.
pub struct MeshBridge {
    hive: Arc<Hive>,
    vm_manager: Arc<VMManager>,
    http_client: reqwest::Client,
    /// Active polling tasks, keyed by VM ID.
    active_polls: Arc<RwLock<HashMap<Uuid, tokio::task::JoinHandle<()>>>>,
    /// Poll interval in milliseconds.
    poll_interval_ms: u64,
}

impl MeshBridge {
    pub fn new(hive: Arc<Hive>, vm_manager: Arc<VMManager>) -> Self {
        Self {
            hive,
            vm_manager,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("Failed to build HTTP client"),
            active_polls: Arc::new(RwLock::new(HashMap::new())),
            poll_interval_ms: 1000,
        }
    }

    /// Start polling a specific VM's mesh-agent.
    pub async fn start_polling(&self, vm_id: Uuid) -> Result<(), String> {
        let vms = self.vm_manager.vms.read().await;
        let vm = vms.get(&vm_id).ok_or("VM not found")?;

        // Calculate the host port that forwards to VM's port 7070
        // NAT mode maps host port 10000+display to guest port 7070
        let vnc_display = vm.vnc_port - 5900;
        let mesh_port = 10000 + vnc_display as u32;
        let memory_node_id = vm.memory_node_id;
        let vm_name = vm.name.clone();

        drop(vms);

        let hive = self.hive.clone();
        let client = self.http_client.clone();
        let interval_ms = self.poll_interval_ms;
        let vm_mgr = self.vm_manager.clone();

        let handle = tokio::spawn(async move {
            let mut poll_timer = interval(Duration::from_millis(interval_ms));
            let url = format!("http://127.0.0.1:{}/state", mesh_port);

            tracing::info!(
                "MeshBridge: starting poll for VM '{}' at {}",
                vm_name,
                url
            );

            loop {
                poll_timer.tick().await;

                // Check if VM is still running
                {
                    let vms = vm_mgr.vms.read().await;
                    if let Some(vm) = vms.get(&vm_id) {
                        if vm.status != crate::vm::VMStatus::Running {
                            tracing::info!("MeshBridge: VM '{}' no longer running, stopping poll", vm_name);
                            break;
                        }
                    } else {
                        break;
                    }
                }

                // Poll the mesh-agent inside the VM
                match client.get(&url).send().await {
                    Ok(response) => {
                        if let Ok(body) = response.json::<serde_json::Value>().await {
                            // Write the received state as a blob in the VM's memory node
                            if let Err(e) = hive
                                .write_blob(
                                    memory_node_id,
                                    "mesh_state".to_string(),
                                    BlobValue::Json(body),
                                )
                                .await
                            {
                                tracing::warn!("MeshBridge: failed to write mesh state: {}", e);
                            }

                            // Update heartbeat
                            let _ = hive
                                .write_blob(
                                    memory_node_id,
                                    "mesh_heartbeat".to_string(),
                                    BlobValue::Number(
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs() as f64,
                                    ),
                                )
                                .await;
                        }
                    }
                    Err(_) => {
                        // Mesh agent not responding — this is normal during VM boot
                        let _ = hive
                            .write_blob(
                                memory_node_id,
                                "mesh_status".to_string(),
                                BlobValue::Text("unreachable".to_string()),
                            )
                            .await;
                    }
                }

                // Check for outbound messages to deliver to the VM
                if let Ok(Some(msg_blob)) =
                    hive.read_blob(memory_node_id, "outbound_message", None).await
                {
                    if let BlobValue::Json(payload) = &msg_blob.value {
                        let deliver_url = format!("http://127.0.0.1:{}/deliver", mesh_port);
                        match client.post(&deliver_url).json(payload).send().await {
                            Ok(_) => {
                                tracing::debug!("MeshBridge: delivered message to VM '{}'", vm_name);
                                // Clear the outbound message after delivery
                                let _ = hive
                                    .write_blob(
                                        memory_node_id,
                                        "outbound_message".to_string(),
                                        BlobValue::Json(serde_json::Value::Null),
                                    )
                                    .await;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "MeshBridge: failed to deliver message to VM '{}': {}",
                                    vm_name,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        });

        let mut polls = self.active_polls.write().await;
        polls.insert(vm_id, handle);

        Ok(())
    }

    /// Stop polling a specific VM.
    pub async fn stop_polling(&self, vm_id: Uuid) {
        let mut polls = self.active_polls.write().await;
        if let Some(handle) = polls.remove(&vm_id) {
            handle.abort();
            tracing::info!("MeshBridge: stopped polling VM {}", vm_id);
        }
    }

    /// Stop all polling tasks.
    pub async fn stop_all(&self) {
        let mut polls = self.active_polls.write().await;
        for (id, handle) in polls.drain() {
            handle.abort();
            tracing::info!("MeshBridge: stopped polling VM {}", id);
        }
    }

    /// Send a message to a specific VM via its Memory node.
    /// The MeshBridge will deliver it on the next poll cycle.
    pub async fn send_message_to_vm(
        &self,
        vm_id: Uuid,
        message: serde_json::Value,
    ) -> Result<(), String> {
        let vms = self.vm_manager.vms.read().await;
        let vm = vms.get(&vm_id).ok_or("VM not found")?;
        let memory_node_id = vm.memory_node_id;
        drop(vms);

        self.hive
            .write_blob(
                memory_node_id,
                "outbound_message".to_string(),
                BlobValue::Json(message),
            )
            .await?;

        Ok(())
    }
}
