use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;



use crate::mesh_bridge::MeshBridge;
use crate::vm::{VMConfig, VMManager};

/// Shared VOS state for Axum handlers.
pub struct VosState {
    pub vm_manager: Arc<VMManager>,
    pub mesh_bridge: Arc<MeshBridge>,
    pub hive: Arc<hivemind_kernel::Hive>,
}

pub type VosAppState = Arc<VosState>;

/// Build the VOS API router.
pub fn vos_routes() -> Router<VosAppState> {
    Router::new()
        .route("/vms", get(list_vms).post(create_vm))
        .route("/vms/{id}", get(get_vm))
        .route("/vms/{id}/start", post(start_vm))
        .route("/vms/{id}/stop", post(stop_vm))
        .route("/vms/{id}/snapshot", post(snapshot_vm))
        .route("/vms/{id}/blobs", get(get_vm_blobs))
        .route("/vms/{id}/signal", post(signal_vm))
}

// ──────────────────────────────────────────────
// REQUEST TYPES
// ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateVMRequest {
    pub name: String,
    pub disk_image_path: Option<String>,
    pub ram_mb: Option<u32>,
    pub vcpus: Option<u8>,
    pub network_mode: Option<String>,
    pub iso_path: Option<String>,
    pub create_disk: Option<bool>,
    pub disk_size_gb: Option<u32>,
}

#[derive(Deserialize)]
pub struct SignalVMRequest {
    pub signal_type: String,
    pub payload: serde_json::Value,
}

// ──────────────────────────────────────────────
// HANDLERS
// ──────────────────────────────────────────────

/// GET /vms — List all VMs.
async fn list_vms(State(state): State<VosAppState>) -> impl IntoResponse {
    let vms = state.vm_manager.list_vms().await;
    Json(vms)
}

/// POST /vms — Create a new VM.
async fn create_vm(
    State(state): State<VosAppState>,
    Json(req): Json<CreateVMRequest>,
) -> impl IntoResponse {
    // Optionally create disk image first
    if req.create_disk.unwrap_or(false) {
        let disk_path = if let Some(ref p) = req.disk_image_path {
            std::path::PathBuf::from(p)
        } else {
            // Default path
            let disk_dir = std::env::var("HIVEMIND_DISK_IMAGES_DIR")
                .unwrap_or_else(|_| "C:\\hivemind\\disks".to_string());
            std::path::PathBuf::from(disk_dir).join(format!("{}.qcow2", req.name))
        };

        let size = req.disk_size_gb.unwrap_or(10);
        if let Err(e) = state.vm_manager.create_disk_image(&disk_path, size).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create disk: {}", e) })),
            );
        }
    }

    let config = VMConfig {
        name: req.name,
        disk_image_path: req.disk_image_path.unwrap_or_default(),
        ram_mb: req.ram_mb,
        vcpus: req.vcpus,
        network_mode: req.network_mode,
        iso_path: req.iso_path,
    };

    match state.vm_manager.create_vm(config).await {
        Ok(vm) => (
            StatusCode::CREATED,
            Json(serde_json::json!(vm.snapshot())),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /vms/{id} — Get VM details.
async fn get_vm(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    match state.vm_manager.get_vm(uuid).await {
        Ok(vm) => (StatusCode::OK, Json(serde_json::to_value(vm).unwrap())),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /vms/{id}/start — Start a VM.
async fn start_vm(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    match state.vm_manager.start_vm(uuid).await {
        Ok(()) => {
            // Start mesh bridge polling for this VM
            if let Err(e) = state.mesh_bridge.start_polling(uuid).await {
                tracing::warn!("Failed to start mesh polling: {}", e);
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "started" })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /vms/{id}/stop — Stop a VM.
async fn stop_vm(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    // Stop mesh bridge polling
    state.mesh_bridge.stop_polling(uuid).await;

    match state.vm_manager.stop_vm(uuid).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "stopped" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /vms/{id}/snapshot — Snapshot a VM.
async fn snapshot_vm(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    match state.vm_manager.snapshot_vm(uuid).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /vms/{id}/blobs — Get all blobs from a VM's Memory node.
async fn get_vm_blobs(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    // Get the VM's memory node ID
    let memory_node_id = {
        let vms = state.vm_manager.vms.read().await;
        match vms.get(&uuid) {
            Some(vm) => vm.memory_node_id,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "VM not found" })),
                )
            }
        }
    };

    match state.hive.get_all_blobs(memory_node_id).await {
        Ok(blobs) => {
            let blob_list: Vec<serde_json::Value> = blobs
                .iter()
                .map(|b| serde_json::to_value(b).unwrap())
                .collect();
            (StatusCode::OK, Json(serde_json::json!(blob_list)))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /vms/{id}/signal — Send a signal to a VM's Memory node.
async fn signal_vm(
    State(state): State<VosAppState>,
    Path(id): Path<String>,
    Json(req): Json<SignalVMRequest>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid UUID" })),
            )
        }
    };

    // Get the VM's memory node ID
    let memory_node_id = {
        let vms = state.vm_manager.vms.read().await;
        match vms.get(&uuid) {
            Some(vm) => vm.memory_node_id,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": "VM not found" })),
                )
            }
        }
    };

    match state
        .hive
        .broadcast_signal(memory_node_id, req.signal_type, req.payload)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "signal_sent" })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}
