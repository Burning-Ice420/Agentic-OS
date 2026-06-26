mod api;
mod mesh_bridge;
mod vm;

use api::{vos_routes, VosAppState, VosState};
use mesh_bridge::MeshBridge;
use vm::VMManager;

use hivemind_kernel::{kernel_routes, Hive};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .with_level(true)
        .init();

    tracing::info!("╔══════════════════════════════════════════════╗");
    tracing::info!("║        HiveMind Virtual OS Runtime           ║");
    tracing::info!("║        v0.1.0 — Memory Kernel Active         ║");
    tracing::info!("╚══════════════════════════════════════════════╝");

    // Read configuration from environment
    let port: u16 = std::env::var("HIVEMIND_KERNEL_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()
        .expect("Invalid HIVEMIND_KERNEL_PORT");

    let qemu_path = std::env::var("HIVEMIND_QEMU_PATH")
        .unwrap_or_else(|_| {
            tracing::warn!("HIVEMIND_QEMU_PATH not set — using default path");
            "C:\\Program Files\\qemu\\qemu-system-x86_64.exe".to_string()
        });

    let disk_dir = std::env::var("HIVEMIND_DISK_IMAGES_DIR")
        .unwrap_or_else(|_| {
            tracing::warn!("HIVEMIND_DISK_IMAGES_DIR not set — using default path");
            "C:\\hivemind\\disks".to_string()
        });

    // Ensure disk directory exists
    let disk_path = PathBuf::from(&disk_dir);
    if !disk_path.exists() {
        std::fs::create_dir_all(&disk_path).ok();
        tracing::info!("Created disk images directory: {:?}", disk_path);
    }

    // Initialize the Hive
    let hive = Arc::new(Hive::new());

    // Initialize the VM Manager
    let vm_manager = Arc::new(VMManager::new(
        hive.clone(),
        PathBuf::from(&qemu_path),
        disk_path,
    ));

    // Initialize the Mesh Bridge
    let mesh_bridge = Arc::new(MeshBridge::new(hive.clone(), vm_manager.clone()));

    // Build shared VOS state
    let vos_state: VosAppState = Arc::new(VosState {
        vm_manager,
        mesh_bridge,
        hive: hive.clone(),
    });

    // Build the combined router:
    // - Kernel routes under /hive/*
    // - VOS routes under /vms/*
    let app = axum::Router::new()
        .merge(kernel_routes().with_state(hive.clone()))
        .merge(vos_routes().with_state(vos_state))
        .layer(CorsLayer::permissive());

    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("HTTP API listening on http://{}", addr);
    tracing::info!("Observer should connect to http://localhost:{}/hive/snapshot", port);
    tracing::info!("QEMU path: {}", qemu_path);
    tracing::info!("Disk images: {}", disk_dir);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind to address");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}
