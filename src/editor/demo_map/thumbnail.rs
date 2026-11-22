//! A small drawing of a circuit from its document: a box per node,
//! coloured by family, and a straight line per wire. No simulator; the
//! mapping into the thumbnail is the minimap's ([`MinimapTransform`]).

use std::collections::HashMap;

use bevy::math::{Rect, Vec2};
use bevy_egui::egui::{self, CornerRadius, StrokeKind};

use crate::editor::hud::MinimapTransform;
use crate::sim::{CircuitDocument, art};
use crate::theme::{Family, Palette};
use crate::ui_style::{egui_color, egui_rect, stroke};

/// A circuit mapped into a thumbnail rectangle (screen space, y down).
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub boxes: Vec<(Rect, Family)>,
    pub wires: Vec<[Vec2; 2]>,
    /// Map pixels per world unit.
    pub scale: f32,
}

/// The world bounds a thumbnail shows: the circuit with a thin margin
/// (thumbnails are small, so less than the minimap's).
pub fn thumbnail_world(content: Rect) -> Rect {
    content.inflate(content.size().max_element() * 0.04 + 20.0)
}

/// Distance between pins on a tile side, in world units.
const PIN_PITCH: f32 = 20.0;

/// Where pin `pin` of `count` sits on a side of a tile of height `h`,
/// relative to the centre (y up): pin 0 at the top.
fn pin_offset(pin: u8, count: usize, h: f32) -> f32 {
    if count <= 1 {
        return 0.0;
    }
    let pitch = ((h - 16.0).max(0.0) / (count - 1) as f32).min(PIN_PITCH);
    let top = (count - 1) as f32 * pitch * 0.5;
    top - f32::from(pin) * pitch
}

/// `doc` drawn into `map`: the document's node rectangles at one scale,
/// centred, with a thin margin around them; wires from an output
/// pin on the right edge to an input pin on the left edge.
pub fn thumbnail(doc: &CircuitDocument, map: Rect) -> Thumbnail {
    let bounds = doc
        .nodes
        .iter()
        .map(|n| Rect::from_center_size(n.position(), n.kind.size()))
        .reduce(|a, b| a.union(b));
    let Some(bounds) = bounds else {
        return Thumbnail {
            boxes: Vec::new(),
            wires: Vec::new(),
            scale: 0.0,
        };
    };
    let t = MinimapTransform::new(thumbnail_world(bounds), map);
    // Pins per side, from the wires (the document has no pin counts for
    // subcircuits without the library).
    let mut pins: HashMap<_, (usize, usize)> = HashMap::new();
    for w in &doc.wires {
        let out = &mut pins.entry(w.from.node).or_default().1;
        *out = (*out).max(usize::from(w.from.pin) + 1);
        let inp = &mut pins.entry(w.to.node).or_default().0;
        *inp = (*inp).max(usize::from(w.to.pin) + 1);
    }
    let pin = |node: uuid::Uuid, index: u8, output: bool| {
        let n = doc.node(node)?;
        let size = n.kind.size();
        let (ins, outs) = pins.get(&node).copied().unwrap_or_default();
        let count = if output { outs } else { ins };
        let x = if output { size.x } else { -size.x } * 0.5;
        Some(t.to_map(n.position() + Vec2::new(x, pin_offset(index, count, size.y))))
    };
    let wires = doc
        .wires
        .iter()
        .filter_map(|w| {
            Some([
                pin(w.from.node, w.from.pin, true)?,
                pin(w.to.node, w.to.pin, false)?,
            ])
        })
        .collect();
    let boxes = doc
        .nodes
        .iter()
        .map(|n| {
            let r = t.rect_to_map(Rect::from_center_size(n.position(), n.kind.size()));
            (r, art::family(&n.kind))
        })
        .collect();
    Thumbnail {
        boxes,
        wires,
        scale: t.scale(),
    }
}

/// Paint `doc` into `rect`: a dotted well, the wires, then the boxes
/// (family fill with an ink outline when big enough, solid family colour
/// when tiny, as on the minimap).
pub fn paint_thumbnail(
    painter: &egui::Painter,
    palette: &Palette,
    rect: egui::Rect,
    doc: &CircuitDocument,
    radius: u8,
) {
    painter.rect_filled(rect, CornerRadius::same(radius), egui_color(palette.well));
    let dot = egui_color(palette.bench_dot);
    let step = 12.0;
    let mut y = rect.min.y + step * 0.5;
    while y < rect.max.y {
        let mut x = rect.min.x + step * 0.5;
        while x < rect.max.x {
            painter.circle_filled(egui::pos2(x, y), 1.1, dot);
            x += step;
        }
        y += step;
    }
    let map = Rect::new(rect.min.x, rect.min.y, rect.max.x, rect.max.y).inflate(-4.0);
    let thumb = thumbnail(doc, map);
    let ink = egui_color(palette.ink);
    let wire = stroke((thumb.scale * 4.0).clamp(1.0, 2.0), egui_color(palette.off));
    for [a, b] in &thumb.wires {
        painter.line_segment([egui::pos2(a.x, a.y), egui::pos2(b.x, b.y)], wire);
    }
    for (r, family) in &thumb.boxes {
        let r = egui_rect(*r);
        let r = egui::Rect::from_center_size(r.center(), r.size().max(egui::vec2(3.0, 3.0)));
        if r.width().min(r.height()) >= 7.0 {
            painter.rect(
                r,
                CornerRadius::same(3),
                egui_color(palette.family_fill(*family)),
                stroke(1.5, ink),
                StrokeKind::Inside,
            );
        } else {
            let solid = match family {
                Family::Logic => palette.ink,
                Family::State => palette.state,
                Family::Io => palette.io,
            };
            painter.rect_filled(r, CornerRadius::same(1), egui_color(solid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::demos;

    #[test]
    fn thumbnail_fits_the_circuit_into_the_map_at_one_scale() {
        let catalog = demos::catalog().unwrap();
        let map = Rect::new(100.0, 50.0, 220.0, 128.0);
        for demo in catalog.all() {
            let doc = &demo.workspace.root;
            let thumb = thumbnail(doc, map);
            assert_eq!(thumb.boxes.len(), doc.nodes.len(), "{}", demo.id);
            assert_eq!(thumb.wires.len(), doc.wires.len(), "{}", demo.id);
            assert!(thumb.scale > 0.0);
            let grown = map.inflate(0.5);
            for (r, _) in &thumb.boxes {
                assert!(
                    grown.contains(r.min) && grown.contains(r.max),
                    "{}",
                    demo.id
                );
                // One scale on both axes: a 50 x 50 tile stays square.
                assert!((r.width() - r.height()).abs() < 0.01 || r.width() > r.height());
            }
            for [a, b] in &thumb.wires {
                assert!(grown.contains(*a) && grown.contains(*b), "{}", demo.id);
            }
        }
    }

    #[test]
    fn thumbnail_transform_maps_world_y_up_to_screen_y_down() {
        let mut doc = CircuitDocument::new("t");
        let gate =
            |x, y| crate::sim::DocNode::new(crate::sim::GateKind::Not { w: 1 }, Vec2::new(x, y));
        let a = doc.add(gate(0.0, 0.0));
        let b = doc.add(gate(200.0, 100.0));
        doc.connect(a, 0, b, 0);
        let map = Rect::new(0.0, 0.0, 120.0, 78.0);
        let thumb = thumbnail(&doc, map);
        let (ra, rb) = (thumb.boxes[0].0, thumb.boxes[1].0);
        assert!(rb.center().x > ra.center().x, "right stays right");
        assert!(rb.center().y < ra.center().y, "up becomes up the screen");
        // The middle of the circuit is the middle of the map.
        let mid = (ra.union(rb)).center();
        assert!(mid.distance(map.center()) < 0.01);
        // The wire runs from a's right edge to b's left edge.
        let [from, to] = thumb.wires[0];
        assert!((from.x - ra.max.x).abs() < 0.01 && (to.x - rb.min.x).abs() < 0.01);
        assert!((from.y - ra.center().y).abs() < 0.01);
        // The minimap's transform over the padded bounds.
        let world = thumbnail_world(Rect::new(-25.0, -25.0, 225.0, 125.0));
        assert_eq!(world, Rect::new(-55.0, -55.0, 255.0, 155.0));
        assert_eq!(thumb.scale, MinimapTransform::new(world, map).scale());
        assert!(
            (thumb.scale - 78.0 / 210.0).abs() < 1e-5,
            "the height limits"
        );
        // An empty circuit draws nothing.
        assert!(thumbnail(&CircuitDocument::new("e"), map).boxes.is_empty());
    }

    #[test]
    fn pins_spread_over_the_tile_side() {
        assert_eq!(pin_offset(0, 1, 50.0), 0.0);
        assert_eq!(pin_offset(0, 2, 50.0), 10.0);
        assert_eq!(pin_offset(1, 2, 50.0), -10.0);
        // Crowded sides squeeze inside the tile.
        assert!(pin_offset(0, 8, 50.0) <= 17.0);
        assert!(pin_offset(7, 8, 50.0) >= -17.0);
    }
}
