//! Flat 2D shapes for the canvas, built as triangle lists: rounded
//! rectangles, circles, rings and stroked paths (with round joins and
//! caps). Pure geometry, so it is unit-tested without a renderer;
//! [`Shape::into_mesh`] turns a shape into a Bevy [`Mesh`] for `Mesh2d`.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

/// Triangles in the plane.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shape {
    pub positions: Vec<Vec2>,
    pub indices: Vec<u32>,
}

/// Segments per quarter circle for a radius: enough to look round at
/// the sizes the canvas uses, few enough to stay cheap.
fn quarter_segments(radius: f32) -> usize {
    (radius * 0.6).ceil().clamp(2.0, 8.0) as usize
}

impl Shape {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Add `other`'s triangles.
    pub fn append(&mut self, other: &Shape) -> &mut Self {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.indices.extend(other.indices.iter().map(|i| i + base));
        self
    }

    pub fn translated(mut self, by: Vec2) -> Self {
        for p in &mut self.positions {
            *p += by;
        }
        self
    }

    /// Bounding box `(min, max)`; `None` for an empty shape.
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let first = *self.positions.first()?;
        Some(
            self.positions
                .iter()
                .fold((first, first), |(lo, hi), p| (lo.min(*p), hi.max(*p))),
        )
    }

    /// Total area of the triangles (overlaps counted twice).
    pub fn area(&self) -> f32 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| self.positions[t[k] as usize]);
                (b - a).perp_dot(c - a).abs() * 0.5
            })
            .sum()
    }

    /// A convex polygon, fanned from its first point.
    pub fn convex(outline: &[Vec2]) -> Shape {
        let mut shape = Shape {
            positions: outline.to_vec(),
            indices: Vec::with_capacity(outline.len().saturating_sub(2) * 3),
        };
        for i in 1..outline.len().saturating_sub(1) as u32 {
            shape.indices.extend([0, i, i + 1]);
        }
        shape
    }

    /// A rectangle of `size` centred on the origin with corners of
    /// `radius` (clamped to half the shorter side).
    pub fn rounded_rect(size: Vec2, radius: f32) -> Shape {
        Shape::convex(&rounded_rect_outline(size, radius))
    }

    pub fn circle(radius: f32) -> Shape {
        let n = quarter_segments(radius) * 4;
        let outline: Vec<Vec2> = (0..n)
            .map(|i| Vec2::from_angle(i as f32 / n as f32 * TAU) * radius)
            .collect();
        Shape::convex(&outline)
    }

    /// A circle at `centre`.
    pub fn dot(centre: Vec2, radius: f32) -> Shape {
        Shape::circle(radius).translated(centre)
    }

    /// A pill (stadium) of `size`, centred on the origin.
    pub fn pill(size: Vec2) -> Shape {
        Shape::rounded_rect(size, size.x.min(size.y) * 0.5)
    }

    /// A stroke of `width` along `points`, with round joins and caps.
    /// Overlaps are harmless for opaque fills.
    pub fn stroke(points: &[Vec2], width: f32, closed: bool) -> Shape {
        let mut shape = Shape::default();
        let half = width * 0.5;
        let n = points.len();
        if n == 0 {
            return shape;
        }
        let segments = if closed { n } else { n - 1 };
        for i in 0..segments {
            let (a, b) = (points[i], points[(i + 1) % n]);
            let d = b - a;
            if d.length_squared() < 1e-8 {
                continue;
            }
            let normal = d.normalize().perp() * half;
            shape.append(&Shape {
                positions: vec![a + normal, b + normal, b - normal, a - normal],
                indices: vec![0, 1, 2, 0, 2, 3],
            });
        }
        // Round joins and caps: a disc at every vertex. Only where the path
        // turns (or ends), to keep straight runs cheap.
        for i in 0..n {
            let end = !closed && (i == 0 || i == n - 1);
            let turns = if end {
                true
            } else {
                let prev = points[(i + n - 1) % n];
                let next = points[(i + 1) % n];
                let (u, v) = (points[i] - prev, next - points[i]);
                u.normalize_or_zero().dot(v.normalize_or_zero()) < 0.9999
            };
            if turns {
                shape.append(&Shape::dot(points[i], half));
            }
        }
        shape
    }
}

/// Outline of a rounded rectangle, counter-clockwise from the right edge.
pub fn rounded_rect_outline(size: Vec2, radius: f32) -> Vec<Vec2> {
    let half = size * 0.5;
    let r = radius.clamp(0.0, half.x.min(half.y));
    if r <= 0.0 {
        return vec![
            Vec2::new(half.x, -half.y),
            Vec2::new(half.x, half.y),
            Vec2::new(-half.x, half.y),
            Vec2::new(-half.x, -half.y),
        ];
    }
    let segs = quarter_segments(r);
    let corners = [
        (Vec2::new(half.x - r, -half.y + r), -FRAC_PI_2),
        (Vec2::new(half.x - r, half.y - r), 0.0),
        (Vec2::new(-half.x + r, half.y - r), FRAC_PI_2),
        (Vec2::new(-half.x + r, -half.y + r), PI),
    ];
    let mut out = Vec::with_capacity(4 * (segs + 1));
    for (centre, start) in corners {
        out.extend(arc(centre, r, start, start + FRAC_PI_2, segs));
    }
    out
}

/// Points along a circular arc from angle `a0` to `a1` (radians),
/// `segments` pieces, both ends included.
pub fn arc(centre: Vec2, radius: f32, a0: f32, a1: f32, segments: usize) -> Vec<Vec2> {
    let segments = segments.max(1);
    (0..=segments)
        .map(|i| centre + Vec2::from_angle(a0 + (a1 - a0) * i as f32 / segments as f32) * radius)
        .collect()
}

/// Points along a quadratic Bézier curve, both ends included.
pub fn quad_bezier(p0: Vec2, control: Vec2, p1: Vec2, segments: usize) -> Vec<Vec2> {
    let segments = segments.max(1);
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let u = 1.0 - t;
            p0 * u * u + control * 2.0 * u * t + p1 * t * t
        })
        .collect()
}

impl Shape {
    /// A `Mesh2d` mesh: positions at z = 0, normals facing the camera, and
    /// UVs across the bounding box.
    pub fn into_mesh(self) -> Mesh {
        let (lo, hi) = self.bounds().unwrap_or((Vec2::ZERO, Vec2::ONE));
        let span = (hi - lo).max(Vec2::splat(1e-6));
        let uvs: Vec<[f32; 2]> = self
            .positions
            .iter()
            .map(|p| {
                let uv = (*p - lo) / span;
                [uv.x, 1.0 - uv.y]
            })
            .collect();
        let normals = vec![[0.0, 0.0, 1.0]; self.positions.len()];
        let positions: Vec<[f32; 3]> = self.positions.iter().map(|p| [p.x, p.y, 0.0]).collect();
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn rounded_rect_fills_its_box_minus_the_corners() {
        let (size, r) = (Vec2::new(100.0, 60.0), 12.0);
        let shape = Shape::rounded_rect(size, r);
        let (lo, hi) = shape.bounds().unwrap();
        assert!(lo.abs_diff_eq(-size / 2.0, 1e-4) && hi.abs_diff_eq(size / 2.0, 1e-4));
        // Area of a rounded rectangle: w*h - (4 - pi) r^2 (polygonal arcs
        // are a little smaller).
        let exact = size.x * size.y - (4.0 - PI) * r * r;
        assert!(close(shape.area(), exact, exact * 0.01), "{}", shape.area());
        assert!(
            shape
                .indices
                .iter()
                .all(|&i| (i as usize) < shape.positions.len())
        );
    }

    #[test]
    fn corner_radius_is_clamped() {
        let shape = Shape::rounded_rect(Vec2::new(20.0, 10.0), 50.0);
        let (lo, hi) = shape.bounds().unwrap();
        assert!(close(hi.y - lo.y, 10.0, 1e-4) && close(hi.x - lo.x, 20.0, 1e-4));
        let square = Shape::rounded_rect(Vec2::splat(10.0), 0.0);
        assert_eq!(square.triangle_count(), 2);
        assert!(close(square.area(), 100.0, 1e-3));
    }

    #[test]
    fn circle_and_pill() {
        let c = Shape::circle(10.0);
        assert!(close(c.area(), PI * 100.0, PI * 100.0 * 0.03));
        let pill = Shape::pill(Vec2::new(40.0, 24.0));
        let (lo, hi) = pill.bounds().unwrap();
        assert!(close(hi.x - lo.x, 40.0, 1e-4) && close(hi.y - lo.y, 24.0, 1e-4));
    }

    #[test]
    fn stroke_covers_its_path_with_round_caps() {
        let s = Shape::stroke(&[Vec2::ZERO, Vec2::new(30.0, 0.0)], 4.0, false);
        let (lo, hi) = s.bounds().unwrap();
        assert!(close(lo.x, -2.0, 1e-3) && close(hi.x, 32.0, 1e-3));
        assert!(close(lo.y, -2.0, 1e-3) && close(hi.y, 2.0, 1e-3));
        // A straight run with a middle point adds no join disc.
        let straight = Shape::stroke(
            &[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(30.0, 0.0)],
            4.0,
            false,
        );
        let corner = Shape::stroke(
            &[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 20.0)],
            4.0,
            false,
        );
        assert!(corner.triangle_count() > straight.triangle_count());
        assert_eq!(Shape::stroke(&[], 3.0, false).triangle_count(), 0);
    }

    #[test]
    fn curves_hit_their_ends() {
        let a = arc(Vec2::ZERO, 5.0, 0.0, PI, 6);
        assert_eq!(a.len(), 7);
        assert!(a[0].abs_diff_eq(Vec2::new(5.0, 0.0), 1e-4));
        assert!(a[6].abs_diff_eq(Vec2::new(-5.0, 0.0), 1e-4));
        let q = quad_bezier(Vec2::ZERO, Vec2::new(5.0, 10.0), Vec2::new(10.0, 0.0), 8);
        assert!(q[0].abs_diff_eq(Vec2::ZERO, 1e-5) && q[8].abs_diff_eq(Vec2::new(10.0, 0.0), 1e-5));
        assert!(
            close(q[4].y, 5.0, 1e-4),
            "the midpoint is halfway to the control"
        );
    }

    #[test]
    fn append_offsets_indices_and_mesh_has_attributes() {
        let mut s = Shape::rounded_rect(Vec2::splat(10.0), 2.0);
        let n = s.positions.len() as u32;
        s.append(&Shape::dot(Vec2::new(20.0, 0.0), 3.0));
        assert!(s.indices.iter().any(|&i| i >= n));
        let count = s.positions.len();
        let mesh = s.into_mesh();
        assert_eq!(mesh.count_vertices(), count);
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some());
    }
}
