// hivemind-observer/src/sidebar.rs
// Status panel + detail panel (30% of window).

use egui::{Color32, RichText, ScrollArea, Ui, Vec2};

use crate::{
    AgentSnapshot, HiveEventSnapshot, HiveSnapshot, HiveStatsSnapshot, MemorySnapshot,
    SelectedNode,
};

// ── Color palette ──
const AMBER: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x29);
const HONEY: Color32 = Color32::from_rgb(0xF7, 0xD0, 0x70);
const GREEN: Color32 = Color32::from_rgb(0x2E, 0xA0, 0x43);
const RED: Color32 = Color32::from_rgb(0xDA, 0x36, 0x33);
const GREY: Color32 = Color32::from_rgb(0x8B, 0x94, 0x9E);
const DIM_TEXT: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);

/// Render the sidebar panel.
pub fn render_sidebar(
    ui: &mut Ui,
    snapshot: &Option<HiveSnapshot>,
    selected: &mut Option<SelectedNode>,
) {
    ui.add_space(8.0);

    match snapshot {
        None => {
            render_connecting(ui);
        }
        Some(snap) => {
            render_stats(ui, &snap.stats);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            render_agents_list(ui, &snap.agents, selected);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            render_memory_list(ui, &snap.memories, selected);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            render_event_log(ui, &snap.events);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            render_detail_panel(ui, snap, selected);
        }
    }
}

fn render_connecting(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.spinner();
        ui.add_space(12.0);
        ui.label(
            RichText::new("Connecting to HiveMind...")
                .color(AMBER)
                .size(16.0),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("Waiting for kernel at localhost:8080")
                .color(DIM_TEXT)
                .size(12.0),
        );
    });
}

fn render_stats(ui: &mut Ui, stats: &HiveStatsSnapshot) {
    ui.label(RichText::new("⬡ HIVE STATUS").color(AMBER).size(14.0));
    ui.add_space(8.0);

    egui::Grid::new("stats_grid")
        .num_columns(2)
        .spacing(Vec2::new(16.0, 6.0))
        .show(ui, |ui| {
            stat_row(ui, "Memories", &stats.total_memories.to_string(), GREEN);
            stat_row(ui, "Blobs", &stats.total_blobs.to_string(), GREEN);
            stat_row(ui, "Agents", &stats.total_agents.to_string(), GREEN);
            stat_row(
                ui,
                "Signals/s",
                &format!("{:.1}", stats.signals_per_second),
                AMBER,
            );
            stat_row(
                ui,
                "LLM Total",
                &stats.llm_calls_total.to_string(),
                HONEY,
            );
            stat_row(
                ui,
                "  OpenAI",
                &stats.llm_calls_openai.to_string(),
                DIM_TEXT,
            );
            stat_row(
                ui,
                "  Anthropic",
                &stats.llm_calls_anthropic.to_string(),
                DIM_TEXT,
            );
        });
}

fn stat_row(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    ui.label(RichText::new(label).color(GREY).size(12.0));
    ui.label(RichText::new(value).color(color).size(18.0).strong());
    ui.end_row();
}

fn render_agents_list(
    ui: &mut Ui,
    agents: &[AgentSnapshot],
    selected: &mut Option<SelectedNode>,
) {
    ui.label(RichText::new("⧫ AGENTS").color(AMBER).size(14.0));
    ui.add_space(4.0);

    if agents.is_empty() {
        ui.label(RichText::new("No agents").color(DIM_TEXT).italics());
        return;
    }

    ScrollArea::vertical()
        .id_salt("agents_scroll")
        .max_height(140.0)
        .show(ui, |ui| {
            for agent in agents {
                let status_color = match agent.status.as_str() {
                    "Running" | "Active" => GREEN,
                    "Error" | "Failed" => RED,
                    _ => GREY,
                };

                let is_selected = matches!(selected, Some(SelectedNode::Agent(ref id)) if id == &agent.id);

                let response = ui
                    .horizontal(|ui| {
                        // Status dot
                        let (dot_rect, _) = ui.allocate_exact_size(
                            Vec2::splat(10.0),
                            egui::Sense::hover(),
                        );
                        ui.painter()
                            .circle_filled(dot_rect.center(), 4.0, status_color);

                        let text_color = if is_selected { AMBER } else { Color32::WHITE };
                        ui.label(
                            RichText::new(&agent.name)
                                .color(text_color)
                                .size(12.0),
                        );
                        ui.label(
                            RichText::new(format!("({})", agent.role))
                                .color(DIM_TEXT)
                                .size(10.0),
                        );
                    })
                    .response;

                if response.clicked() {
                    *selected = Some(SelectedNode::Agent(agent.id.clone()));
                }
            }
        });
}

fn render_memory_list(
    ui: &mut Ui,
    memories: &[MemorySnapshot],
    selected: &mut Option<SelectedNode>,
) {
    ui.label(RichText::new("◉ MEMORIES").color(AMBER).size(14.0));
    ui.add_space(4.0);

    if memories.is_empty() {
        ui.label(RichText::new("No memories").color(DIM_TEXT).italics());
        return;
    }

    ScrollArea::vertical()
        .id_salt("memories_scroll")
        .max_height(120.0)
        .show(ui, |ui| {
            for mem in memories {
                let is_selected =
                    matches!(selected, Some(SelectedNode::Memory(ref id)) if id == &mem.id);
                let text_color = if is_selected { AMBER } else { Color32::WHITE };

                let response = ui
                    .horizontal(|ui| {
                        ui.label(RichText::new("◉").color(HONEY).size(12.0));
                        ui.label(RichText::new(&mem.name).color(text_color).size(12.0));
                        ui.label(
                            RichText::new(format!("{} blobs", mem.blobs.len()))
                                .color(DIM_TEXT)
                                .size(10.0),
                        );
                    })
                    .response;

                if response.clicked() {
                    *selected = Some(SelectedNode::Memory(mem.id.clone()));
                }
            }
        });
}

fn render_event_log(ui: &mut Ui, events: &[HiveEventSnapshot]) {
    ui.label(RichText::new("📋 EVENT LOG").color(AMBER).size(14.0));
    ui.add_space(4.0);

    if events.is_empty() {
        ui.label(RichText::new("No events yet").color(DIM_TEXT).italics());
        return;
    }

    ScrollArea::vertical()
        .id_salt("events_scroll")
        .max_height(160.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for event in events.iter().rev().take(50) {
                ui.horizontal(|ui| {
                    let ts = format_timestamp(event.timestamp);
                    ui.label(RichText::new(&ts).color(DIM_TEXT).size(10.0).monospace());

                    let type_color = match event.event_type.as_str() {
                        "Error" => RED,
                        "Signal" => AMBER,
                        "AgentAction" => GREEN,
                        _ => GREY,
                    };
                    ui.label(
                        RichText::new(&event.event_type)
                            .color(type_color)
                            .size(10.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(&event.description)
                            .color(Color32::from_rgb(0xC9, 0xD1, 0xD9))
                            .size(10.0),
                    );
                });
            }
        });
}

fn render_detail_panel(
    ui: &mut Ui,
    snap: &HiveSnapshot,
    selected: &mut Option<SelectedNode>,
) {
    let sel = match selected {
        Some(s) => s.clone(),
        None => {
            ui.label(
                RichText::new("Click a node to inspect")
                    .color(DIM_TEXT)
                    .italics()
                    .size(12.0),
            );
            return;
        }
    };

    ui.label(RichText::new("🔍 DETAIL").color(AMBER).size(14.0));
    ui.add_space(4.0);

    // Close button
    if ui
        .small_button(RichText::new("✕ Close").color(GREY))
        .clicked()
    {
        *selected = None;
        return;
    }

    ui.add_space(4.0);

    match sel {
        SelectedNode::Memory(ref id) => {
            if let Some(mem) = snap.memories.iter().find(|m| &m.id == id) {
                render_memory_detail(ui, mem);
            } else {
                ui.label(RichText::new("Memory not found").color(RED));
            }
        }
        SelectedNode::Agent(ref id) => {
            if let Some(agent) = snap.agents.iter().find(|a| &a.id == id) {
                render_agent_detail(ui, agent);
            } else {
                ui.label(RichText::new("Agent not found").color(RED));
            }
        }
    }
}

fn render_memory_detail(ui: &mut Ui, mem: &MemorySnapshot) {
    detail_field(ui, "Name", &mem.name);
    detail_field(ui, "ID", &mem.id);
    detail_field(ui, "Blobs", &mem.blobs.len().to_string());
    detail_field(ui, "Edges", &mem.edges.len().to_string());
    detail_field(ui, "Subscriptions", &mem.subscriptions.len().to_string());

    if !mem.blobs.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Blobs:").color(HONEY).size(11.0));

        ScrollArea::vertical()
            .id_salt("detail_blobs_scroll")
            .max_height(120.0)
            .show(ui, |ui| {
                for blob in &mem.blobs {
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&blob.key).color(AMBER).size(11.0).strong(),
                            );
                            ui.label(
                                RichText::new(format!("refs:{}", blob.read_refs.len()))
                                    .color(DIM_TEXT)
                                    .size(9.0),
                            );
                        });
                        // Truncated value preview
                        let val_str = blob.value.to_string();
                        let preview = if val_str.len() > 80 {
                            format!("{}...", &val_str[..80])
                        } else {
                            val_str
                        };
                        ui.label(
                            RichText::new(preview)
                                .color(Color32::from_rgb(0xA0, 0xA8, 0xB0))
                                .size(10.0)
                                .monospace(),
                        );
                    });
                }
            });
    }
}

fn render_agent_detail(ui: &mut Ui, agent: &AgentSnapshot) {
    detail_field(ui, "Name", &agent.name);
    detail_field(ui, "ID", &agent.id);
    detail_field(ui, "Role", &agent.role);
    detail_field(ui, "Status", &agent.status);
    detail_field(ui, "Home Memory", &agent.home_memory_id);

    if !agent.last_actions.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Recent Actions:").color(HONEY).size(11.0));

        ScrollArea::vertical()
            .id_salt("detail_actions_scroll")
            .max_height(100.0)
            .show(ui, |ui| {
                for action in &agent.last_actions {
                    ui.label(
                        RichText::new(format!("• {action}"))
                            .color(Color32::from_rgb(0xC9, 0xD1, 0xD9))
                            .size(10.0),
                    );
                }
            });
    }
}

fn detail_field(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{label}:")).color(GREY).size(11.0));
        ui.label(
            RichText::new(value)
                .color(Color32::WHITE)
                .size(11.0)
                .monospace(),
        );
    });
}

fn format_timestamp(ts: u64) -> String {
    let secs = ts / 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let ms = ts % 1000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}
