// hivemind-observer/src/main.rs
// HiveMind Observer — an egui/eframe desktop app that visualizes the HiveMind OS memory graph.

mod graph_view;
mod layout;
mod sidebar;

use eframe::egui;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── Snapshot types (local definitions matching the kernel API) ──

#[derive(Debug, Clone, Deserialize)]
pub struct HiveSnapshot {
    pub memories: Vec<MemorySnapshot>,
    pub agents: Vec<AgentSnapshot>,
    pub stats: HiveStatsSnapshot,
    pub events: Vec<HiveEventSnapshot>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemorySnapshot {
    pub id: String,
    pub name: String,
    pub blobs: Vec<BlobSnapshot>,
    pub edges: Vec<EdgeSnapshot>,
    pub subscriptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlobSnapshot {
    pub id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub modified_at: u64,
    pub read_refs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EdgeSnapshot {
    pub from_id: String,
    pub to_id: String,
    pub edge_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSnapshot {
    pub id: String,
    pub name: String,
    pub role: String,
    pub home_memory_id: String,
    pub status: String,
    pub last_actions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HiveStatsSnapshot {
    pub total_memories: usize,
    pub total_blobs: usize,
    pub total_agents: usize,
    pub signals_per_second: f64,
    pub llm_calls_total: u64,
    pub llm_calls_openai: u64,
    pub llm_calls_anthropic: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HiveEventSnapshot {
    pub timestamp: u64,
    pub event_type: String,
    pub description: String,
}

// ── Selection state ──

#[derive(Debug, Clone)]
pub enum SelectedNode {
    Memory(String),
    Agent(String),
}

// ── Colors ──

const BG_DARK: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x11, 0x17);
const SIDEBAR_BG: egui::Color32 = egui::Color32::from_rgb(0x16, 0x1B, 0x22);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xF0, 0xB4, 0x29);

// ── Application ──

struct ObserverApp {
    snapshot: Arc<Mutex<Option<HiveSnapshot>>>,
    layout: layout::ForceLayout,
    selected: Option<SelectedNode>,
    pan_offset: egui::Vec2,
    zoom: f32,
    /// Repaint context for the background thread to request repaints.
    _poll_handle: std::thread::JoinHandle<()>,
}

impl ObserverApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Apply custom dark style
        apply_hive_style(&cc.egui_ctx);

        let snapshot: Arc<Mutex<Option<HiveSnapshot>>> = Arc::new(Mutex::new(None));
        let snapshot_clone = Arc::clone(&snapshot);
        let ctx = cc.egui_ctx.clone();

        // Spawn background polling thread
        let handle = std::thread::Builder::new()
            .name("hive-poller".into())
            .spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .expect("Failed to create HTTP client");

                loop {
                    match client
                        .get("http://localhost:8080/hive/snapshot")
                        .send()
                    {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                match resp.json::<HiveSnapshot>() {
                                    Ok(snap) => {
                                        if let Ok(mut guard) = snapshot_clone.lock() {
                                            *guard = Some(snap);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to parse snapshot: {e}");
                                    }
                                }
                            } else {
                                tracing::warn!("Snapshot endpoint returned {}", resp.status());
                            }
                        }
                        Err(e) => {
                            tracing::trace!("Kernel not reachable: {e}");
                            // Clear snapshot so UI shows "Connecting..."
                            if let Ok(mut guard) = snapshot_clone.lock() {
                                *guard = None;
                            }
                        }
                    }

                    // Request a repaint so the UI updates
                    ctx.request_repaint();

                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .expect("Failed to spawn poller thread");

        Self {
            snapshot,
            layout: layout::ForceLayout::new(800.0, 600.0),
            selected: None,
            pan_offset: egui::Vec2::ZERO,
            zoom: 1.0,
            _poll_handle: handle,
        }
    }
}

impl eframe::App for ObserverApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let _ctx = ui.ctx().clone();
        // Clone the snapshot for this frame
        let snap = {
            self.snapshot
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
        };

        // ── Sidebar (right, 30%) ──
        egui::Panel::right("sidebar_panel")
            .resizable(true)
            .default_size(320.0)
            .min_size(260.0)
            .max_size(500.0)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_BG)
                    .inner_margin(egui::Margin::same(12))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x30, 0x36, 0x3D))),
            )
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_scroll")
                    .show(ui, |ui| {
                        sidebar::render_sidebar(ui, &snap, &mut self.selected);
                    });
            });

        // ── Graph view (central, remaining space) ──
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_DARK)
                    .inner_margin(egui::Margin::ZERO),
            )
            .show_inside(ui, |ui| {
                graph_view::render_graph(
                    ui,
                    &snap,
                    &mut self.layout,
                    &mut self.selected,
                    &mut self.pan_offset,
                    &mut self.zoom,
                );
            });
    }
}

/// Apply a dark hive-inspired visual style.
fn apply_hive_style(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();
    let mut visuals = egui::Visuals::dark();

    // Panel backgrounds
    visuals.panel_fill = SIDEBAR_BG;
    visuals.window_fill = egui::Color32::from_rgb(0x16, 0x1B, 0x22);
    visuals.extreme_bg_color = BG_DARK;

    // Widget colors
    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(0x1C, 0x22, 0x2A);
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0xC9, 0xD1, 0xD9));

    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x21, 0x26, 0x2D);
    visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x8B, 0x94, 0x9E));

    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0x30, 0x36, 0x3D);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, AMBER);

    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0x38, 0x3E, 0x47);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(2.0, AMBER);

    // Selection
    visuals.selection.bg_fill = egui::Color32::from_rgba_premultiplied(0xF0, 0xB4, 0x29, 60);
    visuals.selection.stroke = egui::Stroke::new(1.0, AMBER);

    // Separators
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(0.5, egui::Color32::from_rgb(0x30, 0x36, 0x3D));



    style.visuals = visuals;

    // Spacing
    style.spacing.item_spacing = egui::Vec2::new(8.0, 4.0);

    ctx.set_global_style(style);
}

// ── Entry point ──

fn main() -> eframe::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .compact()
        .init();

    tracing::info!("Starting HiveMind Observer...");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("⬡ HiveMind Observer")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "HiveMind Observer",
        options,
        Box::new(|cc| Ok(Box::new(ObserverApp::new(cc)))),
    )
}
