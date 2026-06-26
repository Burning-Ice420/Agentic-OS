// hivemind-observer/src/graph_view.rs
// Force-directed graph rendering (70% of window).

use egui::{Color32, Painter, Pos2, Rect, Stroke, Ui, Vec2};

use crate::layout::{ForceLayout, LayoutEdge};
use crate::{HiveSnapshot, SelectedNode};

// ── Color palette ──
const BG_DARK: Color32 = Color32::from_rgb(0x0D, 0x11, 0x17);
const AMBER: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x29);
const HONEY: Color32 = Color32::from_rgb(0xF7, 0xD0, 0x70);
const GREEN: Color32 = Color32::from_rgb(0x2E, 0xA0, 0x43);
const RED: Color32 = Color32::from_rgb(0xDA, 0x36, 0x33);
const GREY: Color32 = Color32::from_rgb(0x8B, 0x94, 0x9E);
const DIM_TEXT: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);

const MEMORY_RADIUS: f32 = 40.0;
const BLOB_SIZE: f32 = 14.0;
const AGENT_SIZE: f32 = 12.0;

/// Edge colors by type.
fn edge_color(edge_type: &str) -> Color32 {
    match edge_type {
        "Sync" => Color32::from_rgb(0x58, 0xA6, 0xFF),   // blue
        "Signal" => AMBER,
        "Mirror" => Color32::from_rgb(0xA5, 0x71, 0xF5),  // purple
        "Dependency" => GREY,
        _ => Color32::from_rgb(0x48, 0x4F, 0x58),
    }
}

/// Edge stroke width by type.
fn edge_stroke_width(edge_type: &str) -> f32 {
    match edge_type {
        "Signal" => 2.5,
        "Sync" => 2.0,
        "Mirror" => 1.8,
        "Dependency" => 1.2,
        _ => 1.0,
    }
}

/// Render the graph canvas.
pub fn render_graph(
    ui: &mut Ui,
    snapshot: &Option<HiveSnapshot>,
    layout: &mut ForceLayout,
    selected: &mut Option<SelectedNode>,
    pan_offset: &mut Vec2,
    zoom: &mut f32,
) {
    let available = ui.available_size();
    let (response, painter) =
        ui.allocate_painter(available, egui::Sense::click_and_drag());
    let canvas_rect = response.rect;

    // Dark background
    painter.rect_filled(canvas_rect, 0.0, BG_DARK);

    // Draw subtle grid
    draw_grid(&painter, canvas_rect, *pan_offset, *zoom);

    match snapshot {
        None => {
            draw_connecting(&painter, canvas_rect);
            return;
        }
        Some(snap) => {
            // ── Sync layout with snapshot ──
            layout.set_area(available.x, available.y);
            sync_layout(layout, snap);

            // Run one physics step per frame
            layout.step();

            // ── Handle pan & zoom ──
            if response.dragged_by(egui::PointerButton::Secondary) {
                *pan_offset += response.drag_delta();
            }

            // Handle scroll-zoom
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let factor = 1.0 + scroll * 0.002;
                *zoom = (*zoom * factor).clamp(0.3, 3.0);
            }

            // Transform helper: layout pos → screen pos
            let to_screen = |pos: Pos2| -> Pos2 {
                // Transform layout position to screen position
                let centered = pos - Pos2::new(available.x / 2.0, available.y / 2.0);
                let scaled = centered * *zoom;
                canvas_rect.center() + scaled + *pan_offset
            };

            // ── Draw edges ──
            for mem in &snap.memories {
                for edge in &mem.edges {
                    if let (Some(from_pos), Some(to_pos)) =
                        (layout.get_pos(&edge.from_id), layout.get_pos(&edge.to_id))
                    {
                        let p1 = to_screen(from_pos);
                        let p2 = to_screen(to_pos);
                        let color = edge_color(&edge.edge_type);
                        let width = edge_stroke_width(&edge.edge_type) * zoom.sqrt();

                        match edge.edge_type.as_str() {
                            "Dependency" => {
                                // Dashed line
                                draw_dashed_line(&painter, p1, p2, color, width, 8.0, 4.0);
                            }
                            "Mirror" => {
                                // Dotted line
                                draw_dashed_line(&painter, p1, p2, color, width, 3.0, 5.0);
                            }
                            _ => {
                                // Solid line
                                painter.line_segment([p1, p2], Stroke::new(width, color));
                            }
                        }

                        // Arrowhead
                        draw_arrowhead(&painter, p1, p2, color, width);
                    }
                }
            }

            // ── Draw memory nodes ──
            for mem in &snap.memories {
                if let Some(pos) = layout.get_pos(&mem.id) {
                    let screen_pos = to_screen(pos);
                    let radius = MEMORY_RADIUS * *zoom;

                    let is_selected =
                        matches!(selected, Some(SelectedNode::Memory(ref id)) if id == &mem.id);

                    // Glow effect: two larger semi-transparent circles behind
                    let glow_color = if is_selected {
                        Color32::from_rgba_premultiplied(0xF0, 0xB4, 0x29, 25)
                    } else {
                        Color32::from_rgba_premultiplied(0xF7, 0xD0, 0x70, 15)
                    };
                    painter.circle_filled(screen_pos, radius + 12.0 * *zoom, glow_color);
                    painter.circle_filled(
                        screen_pos,
                        radius + 6.0 * *zoom,
                        Color32::from_rgba_premultiplied(0xF7, 0xD0, 0x70, 8),
                    );

                    // Main circle
                    let fill = Color32::from_rgb(0x1C, 0x22, 0x2A);
                    painter.circle_filled(screen_pos, radius, fill);

                    // Border
                    let border_color = if is_selected { AMBER } else { HONEY };
                    let border_width = if is_selected { 2.5 } else { 1.5 };
                    painter.circle_stroke(
                        screen_pos,
                        radius,
                        Stroke::new(border_width * *zoom, border_color),
                    );

                    // Label
                    let label_text = if mem.name.len() > 10 {
                        format!("{}…", &mem.name[..9])
                    } else {
                        mem.name.clone()
                    };
                    painter.text(
                        screen_pos,
                        egui::Align2::CENTER_CENTER,
                        &label_text,
                        egui::FontId::proportional(11.0 * *zoom),
                        Color32::WHITE,
                    );

                    // Blob count badge
                    if !mem.blobs.is_empty() {
                        let badge_pos = screen_pos + Vec2::new(radius * 0.6, -radius * 0.7);
                        let badge_r = 9.0 * *zoom;
                        painter.circle_filled(badge_pos, badge_r, AMBER);
                        painter.text(
                            badge_pos,
                            egui::Align2::CENTER_CENTER,
                            &mem.blobs.len().to_string(),
                            egui::FontId::proportional(8.0 * *zoom),
                            Color32::BLACK,
                        );
                    }

                    // Draw blobs around the memory node
                    draw_blobs(&painter, screen_pos, radius, &mem.blobs, *zoom);

                    // Click detection
                    let click_rect = Rect::from_center_size(
                        screen_pos,
                        Vec2::splat(radius * 2.0),
                    );
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if click_rect.contains(pointer) {
                                *selected = Some(SelectedNode::Memory(mem.id.clone()));
                            }
                        }
                    }

                    // Drag detection for pinning
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            if click_rect.contains(pointer) {
                                // Reverse-transform to layout coordinates
                                let layout_pos = {
                                    let offset = pointer - canvas_rect.center() - *pan_offset;
                                    let unscaled = offset / *zoom;
                                    Pos2::new(
                                        available.x / 2.0 + unscaled.x,
                                        available.y / 2.0 + unscaled.y,
                                    )
                                };
                                layout.pin_node(&mem.id, layout_pos);
                            }
                        }
                    }
                }
            }

            // ── Draw agents ──
            for agent in &snap.agents {
                // Place agent near its home memory
                let agent_layout_id = format!("agent_{}", agent.id);
                if let Some(pos) = layout.get_pos(&agent_layout_id) {
                    let screen_pos = to_screen(pos);
                    let size = AGENT_SIZE * *zoom;

                    let is_selected =
                        matches!(selected, Some(SelectedNode::Agent(ref id)) if id == &agent.id);

                    let color = match agent.status.as_str() {
                        "Running" | "Active" => GREEN,
                        "Error" | "Failed" => RED,
                        _ => GREY,
                    };

                    // Draw triangle
                    let tri = [
                        Pos2::new(screen_pos.x, screen_pos.y - size),
                        Pos2::new(screen_pos.x - size * 0.866, screen_pos.y + size * 0.5),
                        Pos2::new(screen_pos.x + size * 0.866, screen_pos.y + size * 0.5),
                    ];

                    let fill = if is_selected {
                        color
                    } else {
                        Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 180)
                    };

                    painter.add(egui::Shape::convex_polygon(
                        tri.to_vec(),
                        fill,
                        Stroke::new(1.0 * *zoom, color),
                    ));

                    // Agent name label
                    painter.text(
                        screen_pos + Vec2::new(0.0, size + 8.0 * *zoom),
                        egui::Align2::CENTER_TOP,
                        &agent.name,
                        egui::FontId::proportional(9.0 * *zoom),
                        DIM_TEXT,
                    );

                    // Click detection
                    if response.clicked() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let agent_rect =
                                Rect::from_center_size(screen_pos, Vec2::splat(size * 3.0));
                            if agent_rect.contains(pointer) {
                                *selected = Some(SelectedNode::Agent(agent.id.clone()));
                            }
                        }
                    }
                }
            }

            // ── HUD overlay ──
            draw_hud(&painter, canvas_rect, snap, layout.is_settled(), *zoom);
        }
    }

    // Request repaint if layout is still animating
    if !layout.is_settled() {
        ui.ctx().request_repaint();
    }
}

/// Sync the force layout with the current snapshot data.
fn sync_layout(layout: &mut ForceLayout, snap: &HiveSnapshot) {
    let mut all_ids: Vec<String> = Vec::new();
    let mut edges: Vec<LayoutEdge> = Vec::new();

    // Add memory nodes
    for mem in &snap.memories {
        layout.ensure_node(&mem.id);
        all_ids.push(mem.id.clone());

        // Add edges
        for edge in &mem.edges {
            // Ensure target nodes exist too
            layout.ensure_node(&edge.to_id);
            edges.push(LayoutEdge {
                from: edge.from_id.clone(),
                to: edge.to_id.clone(),
            });
        }
    }

    // Add agent nodes (near their home memory)
    for agent in &snap.agents {
        let agent_id = format!("agent_{}", agent.id);
        layout.ensure_node(&agent_id);
        all_ids.push(agent_id.clone());

        // Edge from agent to home memory
        edges.push(LayoutEdge {
            from: agent_id.clone(),
            to: agent.home_memory_id.clone(),
        });
    }

    layout.set_edges(edges);
    layout.retain_nodes(&all_ids);
}

fn draw_blobs(
    painter: &Painter,
    center: Pos2,
    mem_radius: f32,
    blobs: &[crate::BlobSnapshot],
    zoom: f32,
) {
    let max_visible = 6;
    let count = blobs.len().min(max_visible);
    if count == 0 {
        return;
    }

    let blob_size = BLOB_SIZE * zoom;
    let orbit_radius = mem_radius * 0.55;

    for (i, blob) in blobs.iter().take(count).enumerate() {
        let angle = (i as f32 / count as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let bx = center.x + orbit_radius * angle.cos();
        let by = center.y + orbit_radius * angle.sin();
        let blob_center = Pos2::new(bx, by);

        let blob_rect = Rect::from_center_size(blob_center, Vec2::new(blob_size, blob_size * 0.7));

        let fill = Color32::from_rgb(0x24, 0x2A, 0x33);
        painter.rect_filled(blob_rect, 3.0 * zoom, fill);
        painter.rect_stroke(blob_rect, 3.0 * zoom, Stroke::new(0.8 * zoom, HONEY), egui::StrokeKind::Inside);

        // Blob key label (tiny)
        let key_label = if blob.key.len() > 4 {
            &blob.key[..4]
        } else {
            &blob.key
        };
        painter.text(
            blob_center,
            egui::Align2::CENTER_CENTER,
            key_label,
            egui::FontId::proportional(6.5 * zoom),
            Color32::from_rgb(0xC0, 0xC8, 0xD0),
        );
    }
}

fn draw_dashed_line(
    painter: &Painter,
    from: Pos2,
    to: Pos2,
    color: Color32,
    width: f32,
    dash_len: f32,
    gap_len: f32,
) {
    let delta = to - from;
    let length = delta.length();
    if length < 1.0 {
        return;
    }
    let dir = delta / length;
    let cycle = dash_len + gap_len;

    let mut d = 0.0;
    while d < length {
        let start = from + dir * d;
        let end_d = (d + dash_len).min(length);
        let end = from + dir * end_d;
        painter.line_segment([start, end], Stroke::new(width, color));
        d += cycle;
    }
}

fn draw_arrowhead(painter: &Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    let delta = to - from;
    let len = delta.length();
    if len < 1.0 {
        return;
    }
    let dir = delta / len;
    let perp = Vec2::new(-dir.y, dir.x);

    let arrow_size = (width * 3.0).max(6.0);
    let tip = to;
    let left = tip - dir * arrow_size + perp * arrow_size * 0.4;
    let right = tip - dir * arrow_size - perp * arrow_size * 0.4;

    painter.add(egui::Shape::convex_polygon(
        vec![tip, left, right],
        color,
        Stroke::NONE,
    ));
}

fn draw_grid(painter: &Painter, rect: Rect, pan: Vec2, _zoom: f32) {
    let grid_color = Color32::from_rgba_premultiplied(0x30, 0x36, 0x3D, 30);
    let spacing = 60.0;

    let start_x = rect.left() + (pan.x % spacing);
    let start_y = rect.top() + (pan.y % spacing);

    let mut x = start_x;
    while x < rect.right() {
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(0.5, grid_color),
        );
        x += spacing;
    }

    let mut y = start_y;
    while y < rect.bottom() {
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(0.5, grid_color),
        );
        y += spacing;
    }
}

fn draw_connecting(painter: &Painter, rect: Rect) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "⬡ Connecting to HiveMind...",
        egui::FontId::proportional(20.0),
        AMBER,
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 30.0),
        egui::Align2::CENTER_CENTER,
        "Waiting for kernel at localhost:8080",
        egui::FontId::proportional(13.0),
        DIM_TEXT,
    );
}

fn draw_hud(painter: &Painter, rect: Rect, snap: &HiveSnapshot, settled: bool, zoom: f32) {
    // Top-left status
    let text = format!(
        "⬡ {} memories │ {} agents │ {:.0} sig/s │ zoom: {:.1}x",
        snap.stats.total_memories, snap.stats.total_agents, snap.stats.signals_per_second, zoom,
    );
    let hud_pos = rect.left_top() + Vec2::new(12.0, 12.0);
    painter.text(
        hud_pos,
        egui::Align2::LEFT_TOP,
        &text,
        egui::FontId::proportional(11.0),
        DIM_TEXT,
    );

    // Layout status indicator
    if !settled {
        let status_pos = rect.right_top() + Vec2::new(-12.0, 12.0);
        painter.text(
            status_pos,
            egui::Align2::RIGHT_TOP,
            "● Layout settling...",
            egui::FontId::proportional(10.0),
            AMBER,
        );
    }
}
