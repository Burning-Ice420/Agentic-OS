// hivemind-observer/src/layout.rs
// Force-directed graph layout using the Fruchterman-Reingold algorithm.

use egui::Pos2;
use std::collections::HashMap;

/// A node in the force-directed layout.
#[derive(Clone, Debug)]
pub struct LayoutNode {
    pub id: String,
    pub pos: Pos2,
    pub vel: egui::Vec2,
    pub pinned: bool,
}

/// An edge in the force-directed layout.
#[derive(Clone, Debug)]
pub struct LayoutEdge {
    pub from: String,
    pub to: String,
}

/// Fruchterman-Reingold force-directed layout engine.
pub struct ForceLayout {
    pub nodes: HashMap<String, LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    temperature: f32,
    max_temperature: f32,
    cooling_factor: f32,
    iteration: usize,
    area_width: f32,
    area_height: f32,
}

impl ForceLayout {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            temperature: 100.0,
            max_temperature: 100.0,
            cooling_factor: 0.97,
            iteration: 0,
            area_width: width,
            area_height: height,
        }
    }

    /// Reset the temperature so the layout becomes "hot" again and re-organizes.
    pub fn reheat(&mut self) {
        self.temperature = self.max_temperature;
        self.iteration = 0;
    }

    /// Update the layout area dimensions.
    pub fn set_area(&mut self, width: f32, height: f32) {
        self.area_width = width;
        self.area_height = height;
    }

    /// Ensure a node exists in the layout; if new, place it pseudo-randomly.
    pub fn ensure_node(&mut self, id: &str) {
        if !self.nodes.contains_key(id) {
            // Deterministic-ish initial placement based on the id hash
            let hash = simple_hash(id);
            let cx = self.area_width / 2.0;
            let cy = self.area_height / 2.0;
            let angle = (hash % 360) as f32 * std::f32::consts::PI / 180.0;
            let radius = 50.0 + (hash % 150) as f32;
            let pos = Pos2::new(cx + angle.cos() * radius, cy + angle.sin() * radius);

            self.nodes.insert(
                id.to_string(),
                LayoutNode {
                    id: id.to_string(),
                    pos,
                    vel: egui::Vec2::ZERO,
                    pinned: false,
                },
            );
        }
    }

    /// Set the edges for the current layout.
    pub fn set_edges(&mut self, edges: Vec<LayoutEdge>) {
        self.edges = edges;
    }

    /// Remove nodes not present in the given set of IDs.
    pub fn retain_nodes(&mut self, ids: &[String]) {
        self.nodes.retain(|k, _| ids.contains(k));
    }

    /// Run one iteration of the Fruchterman-Reingold algorithm.
    pub fn step(&mut self) {
        if self.nodes.len() < 2 || self.temperature < 0.5 {
            return;
        }

        let area = self.area_width * self.area_height;
        let k = (area / self.nodes.len() as f32).sqrt().max(1.0);
        let k_sq = k * k;

        // Collect node ids for indexed access
        let ids: Vec<String> = self.nodes.keys().cloned().collect();

        // Accumulate displacement vectors
        let mut displacements: HashMap<String, egui::Vec2> = HashMap::new();
        for id in &ids {
            displacements.insert(id.clone(), egui::Vec2::ZERO);
        }

        // ── Repulsive forces between all pairs ──
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let pos_i = self.nodes[&ids[i]].pos;
                let pos_j = self.nodes[&ids[j]].pos;

                let mut delta = pos_i - pos_j;
                let dist = delta.length().max(1.0);
                let repulsive_force = k_sq / dist;

                delta = delta / dist * repulsive_force;

                if let Some(d) = displacements.get_mut(&ids[i]) {
                    *d += delta;
                }
                if let Some(d) = displacements.get_mut(&ids[j]) {
                    *d -= delta;
                }
            }
        }

        // ── Attractive forces along edges ──
        for edge in &self.edges {
            if let (Some(n_from), Some(n_to)) = (self.nodes.get(&edge.from), self.nodes.get(&edge.to))
            {
                let delta = n_from.pos - n_to.pos;
                let dist = delta.length().max(1.0);
                let attractive_force = dist * dist / k;

                let force = delta / dist * attractive_force;

                if let Some(d) = displacements.get_mut(&edge.from) {
                    *d -= force;
                }
                if let Some(d) = displacements.get_mut(&edge.to) {
                    *d += force;
                }
            }
        }

        // ── Gravity: gentle pull toward center ──
        let center = Pos2::new(self.area_width / 2.0, self.area_height / 2.0);
        let gravity = 0.3;
        for id in &ids {
            if let Some(node) = self.nodes.get(id) {
                let to_center = center - node.pos;
                if let Some(d) = displacements.get_mut(id) {
                    *d += to_center * gravity;
                }
            }
        }

        // ── Apply displacements, limited by temperature ──
        for id in &ids {
            let node = match self.nodes.get_mut(id) {
                Some(n) => n,
                None => continue,
            };

            if node.pinned {
                continue;
            }

            let disp = displacements[id];
            let disp_len = disp.length().max(0.001);
            let clamped = disp / disp_len * disp_len.min(self.temperature);

            // Apply with damping
            node.vel = (node.vel + clamped) * 0.5;
            node.pos += node.vel;

            // Clamp to bounds with padding
            let pad = 50.0;
            node.pos.x = node.pos.x.clamp(pad, self.area_width - pad);
            node.pos.y = node.pos.y.clamp(pad, self.area_height - pad);
        }

        // ── Cool down ──
        self.temperature *= self.cooling_factor;
        self.iteration += 1;
    }

    /// Returns true if the layout has mostly settled.
    pub fn is_settled(&self) -> bool {
        self.temperature < 0.5
    }

    /// Get the position of a node by id.
    pub fn get_pos(&self, id: &str) -> Option<Pos2> {
        self.nodes.get(id).map(|n| n.pos)
    }

    /// Pin a node at a specific position (e.g., when user drags it).
    pub fn pin_node(&mut self, id: &str, pos: Pos2) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.pos = pos;
            node.pinned = true;
            node.vel = egui::Vec2::ZERO;
        }
    }

    /// Unpin a node so it participates in physics again.
    pub fn unpin_node(&mut self, id: &str) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.pinned = false;
        }
    }
}

/// Simple hash for deterministic initial placement.
fn simple_hash(s: &str) -> u32 {
    let mut hash: u32 = 5381;
    for b in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    hash
}
