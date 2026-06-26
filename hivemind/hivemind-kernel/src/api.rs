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

use crate::agent::{AgentConfig, AgentRole, LLMProvider};
use crate::blob::BlobValue;
use crate::hive::Hive;
use crate::memory::EdgeType;

/// Shared application state for Axum handlers.
pub type AppState = Arc<Hive>;

/// Build the kernel API router.
pub fn kernel_routes() -> Router<AppState> {
    Router::new()
        // Snapshot endpoint for the observer
        .route("/hive/snapshot", get(get_snapshot))
        // Memory endpoints
        .route("/hive/memories", get(list_memories).post(create_memory))
        .route("/hive/memories/{id}", get(get_memory))
        .route(
            "/hive/memories/{id}/blobs",
            get(get_memory_blobs).post(write_blob),
        )
        .route("/hive/memories/{id}/blobs/{key}", get(read_blob))
        // Link and signal
        .route("/hive/memories/link", post(link_memories))
        .route("/hive/signal", post(broadcast_signal))
        // Agent endpoints
        .route("/hive/agents", post(spawn_agent))
        .route("/hive/agents/{id}", get(get_agent))
        // Stats
        .route("/hive/stats", get(get_stats))
}

// ──────────────────────────────────────────────
// REQUEST / RESPONSE TYPES
// ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateMemoryRequest {
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Deserialize)]
pub struct WriteBlobRequest {
    pub key: String,
    pub value: BlobValue,
}

#[derive(Deserialize)]
pub struct LinkMemoriesRequest {
    pub from_id: String,
    pub to_id: String,
    pub edge_type: String, // "Sync", "Signal", "Mirror", "Dependency"
}

#[derive(Deserialize)]
pub struct BroadcastSignalRequest {
    pub from_memory_id: String,
    pub signal_type: String,
    pub payload: serde_json::Value,
}

#[derive(Deserialize)]
pub struct SpawnAgentRequest {
    pub memory_id: String,
    pub name: String,
    pub role: String, // "Executor", "Monitor", "Planner", "Router", "Orchestrator"
    pub system_prompt: Option<String>,
    pub llm_provider: Option<String>, // "OpenAI", "Anthropic", "None"
}

// ──────────────────────────────────────────────
// HANDLERS
// ──────────────────────────────────────────────

/// GET /hive/snapshot — Full hive state for the observer.
async fn get_snapshot(State(hive): State<AppState>) -> impl IntoResponse {
    let snapshot = hive.snapshot().await;
    Json(snapshot)
}

/// GET /hive/memories — List all memories.
async fn list_memories(State(hive): State<AppState>) -> impl IntoResponse {
    let memories = hive.list_memories().await;
    let list: Vec<serde_json::Value> = memories
        .iter()
        .map(|(id, name)| {
            serde_json::json!({
                "id": id.to_string(),
                "name": name,
            })
        })
        .collect();
    Json(list)
}

/// POST /hive/memories — Create a new Memory.
async fn create_memory(
    State(hive): State<AppState>,
    Json(req): Json<CreateMemoryRequest>,
) -> impl IntoResponse {
    let parent_id = req
        .parent_id
        .and_then(|s| Uuid::parse_str(&s).ok());

    match hive.create_memory(req.name, parent_id).await {
        Ok(memory) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": memory.id.to_string(),
                "name": memory.name,
                "status": "created",
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /hive/memories/{id} — Get Memory details.
async fn get_memory(
    State(hive): State<AppState>,
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

    match hive.get_memory(uuid).await {
        Ok(snapshot) => (StatusCode::OK, Json(serde_json::to_value(snapshot).unwrap())),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /hive/memories/{id}/blobs — List all blobs in a Memory.
async fn get_memory_blobs(
    State(hive): State<AppState>,
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

    match hive.get_all_blobs(uuid).await {
        Ok(blobs) => {
            let blob_list: Vec<serde_json::Value> = blobs
                .iter()
                .map(|b| serde_json::to_value(b).unwrap())
                .collect();
            (StatusCode::OK, Json(serde_json::json!(blob_list)))
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /hive/memories/{id}/blobs — Write a blob.
async fn write_blob(
    State(hive): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<WriteBlobRequest>,
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

    match hive.write_blob(uuid, req.key, req.value).await {
        Ok(blob) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": blob.id.to_string(),
                "key": blob.key,
                "status": "written",
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /hive/memories/{id}/blobs/{key} — Read a specific blob.
async fn read_blob(
    State(hive): State<AppState>,
    Path((id, key)): Path<(String, String)>,
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

    match hive.read_blob(uuid, &key, None).await {
        Ok(Some(blob)) => (StatusCode::OK, Json(serde_json::to_value(blob).unwrap())),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("Blob '{}' not found", key) })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /hive/memories/link — Link two memories.
async fn link_memories(
    State(hive): State<AppState>,
    Json(req): Json<LinkMemoriesRequest>,
) -> impl IntoResponse {
    let from_id = match Uuid::parse_str(&req.from_id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid from_id UUID" })),
            )
        }
    };
    let to_id = match Uuid::parse_str(&req.to_id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid to_id UUID" })),
            )
        }
    };

    let edge_type = match req.edge_type.as_str() {
        "Sync" => EdgeType::Sync,
        "Signal" => EdgeType::Signal,
        "Mirror" => EdgeType::Mirror,
        "Dependency" => EdgeType::Dependency,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid edge_type. Must be Sync, Signal, Mirror, or Dependency" })),
            )
        }
    };

    match hive.link_memories(from_id, to_id, edge_type).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "linked" })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// POST /hive/signal — Broadcast a signal.
async fn broadcast_signal(
    State(hive): State<AppState>,
    Json(req): Json<BroadcastSignalRequest>,
) -> impl IntoResponse {
    let from_id = match Uuid::parse_str(&req.from_memory_id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid from_memory_id UUID" })),
            )
        }
    };

    match hive
        .broadcast_signal(from_id, req.signal_type, req.payload)
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

/// POST /hive/agents — Spawn an agent.
async fn spawn_agent(
    State(hive): State<AppState>,
    Json(req): Json<SpawnAgentRequest>,
) -> impl IntoResponse {
    let memory_id = match Uuid::parse_str(&req.memory_id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid memory_id UUID" })),
            )
        }
    };

    let role = match req.role.as_str() {
        "Executor" => AgentRole::Executor,
        "Monitor" => AgentRole::Monitor,
        "Planner" => AgentRole::Planner,
        "Router" => AgentRole::Router,
        "Orchestrator" => AgentRole::Orchestrator,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid role" })),
            )
        }
    };

    let llm_provider = match req.llm_provider.as_deref().unwrap_or("None") {
        "OpenAI" => LLMProvider::OpenAI,
        "Anthropic" => LLMProvider::Anthropic,
        "None" | "" => LLMProvider::None,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid llm_provider" })),
            )
        }
    };

    let config = AgentConfig {
        name: req.name,
        role,
        system_prompt: req.system_prompt.unwrap_or_default(),
        llm_provider,
    };

    match hive.spawn_agent(memory_id, config).await {
        Ok(agent) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": agent.id.to_string(),
                "name": agent.name,
                "status": "spawned",
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /hive/agents/{id} — Get agent details.
async fn get_agent(
    State(hive): State<AppState>,
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

    match hive.get_agent(uuid).await {
        Ok(snapshot) => (StatusCode::OK, Json(serde_json::to_value(snapshot).unwrap())),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

/// GET /hive/stats — Get live stats.
async fn get_stats(State(hive): State<AppState>) -> impl IntoResponse {
    let stats = hive.stats.read().await;
    Json(serde_json::json!({
        "total_memories": stats.total_memories,
        "total_blobs": stats.total_blobs,
        "total_agents": stats.total_agents,
        "signals_per_second": stats.signals_per_second,
        "llm_calls_total": stats.llm_calls_total,
        "llm_calls_openai": stats.llm_calls_openai,
        "llm_calls_anthropic": stats.llm_calls_anthropic,
    }))
}
