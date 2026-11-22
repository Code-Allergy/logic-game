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

/// Radius of the rounded corner at vertex `i` of `points` (an interior
/// vertex): `radius`, but at most half of either neighbouring segment;
/// zero where the path runs straight on.
pub fn corner_radius(points: &[Vec2], i: usize, radius: f32) -> f32 {
    let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
    let (u, v) = (b - a, c - b);
    if u.perp_dot(v).abs() <= 1e-3 * u.length() * v.length() {
        return 0.0;
    }
    radius.min(u.length() * 0.5).min(v.length() * 0.5)
}

/// `points` with every corner replaced by a quadratic curve of up to
/// `radius` (see [`corner_radius`]), as wires turn on the grid.
pub fn round_corners(points: &[Vec2], radius: f32) -> Vec<Vec2> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(n * 4);
    out.push(points[0]);
    for i in 1..n - 1 {
        let r = corner_radius(points, i, radius);
        let b = points[i];
        if r < 0.5 {
            out.push(b);
            continue;
        }
        let start = b + (points[i - 1] - b).normalize() * r;
        let end = b + (points[i + 1] - b).normalize() * r;
        out.extend(quad_bezier(start, b, end, quarter_segments(r)));
    }
    out.push(points[n - 1]);
    out.dedup_by(|b, a| a.distance_squared(*b) < 1e-6);
    out
}

impl Shape {
    /// A stroke of `width` along a smooth open path (gentle turns, such as
    /// [`round_corners`] makes): one strip with mitred joins, and round
    /// caps at both ends.
    pub fn path_stroke(points: &[Vec2], width: f32) -> Shape {
        let mut pts = points.to_vec();
        pts.dedup_by(|b, a| a.distance_squared(*b) < 1e-6);
        let half = width * 0.5;
        let mut shape = Shape::default();
        match pts.len() {
            0 => return shape,
            1 => return Shape::dot(pts[0], half),
            _ => {}
        }
        let n = pts.len();
        let normal = |i: usize| (pts[i + 1] - pts[i]).normalize().perp();
        for (i, &p) in pts.iter().enumerate() {
            let offset = if i == 0 {
                normal(0) * half
            } else if i == n - 1 {
                normal(n - 2) * half
            } else {
                let (a, b) = (normal(i - 1), normal(i));
                let m = (a + b).normalize_or(a);
                m * half / m.dot(a).max(0.25)
            };
            shape.positions.push(p + offset);
            shape.positions.push(p - offset);
        }
        for i in 0..n as u32 - 1 {
            let (l0, r0, l1, r1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
            shape.indices.extend([l0, l1, r1, l0, r1, r0]);
        }
        shape.append(&Shape::dot(pts[0], half));
        shape.append(&Shape::dot(pts[n - 1], half));
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
    ///
    /// A shape with no triangles becomes one degenerate triangle at the
    /// origin (it draws nothing): the renderer's mesh allocator skips a
    /// mesh with no vertices but still tries to upload it, logging "Use-after-free:
    /// attempted to copy element data for an unallocated key" twice
    /// (vertices and indices) each time such a mesh is added or changed.
    pub fn into_mesh(mut self) -> Mesh {
        if self.positions.is_empty() || self.indices.is_empty() {
            self = Shape {
                positions: vec![Vec2::ZERO; 3],
                indices: vec![0, 1, 2],
            };
        }
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
    fn rounded_corners_stay_inside_the_corner() {
        let path = [Vec2::ZERO, Vec2::new(100.0, 0.0), Vec2::new(100.0, 100.0)];
        let round = round_corners(&path, 20.0);
        assert_eq!(round[0], path[0]);
        assert_eq!(*round.last().unwrap(), path[2]);
        assert!(round.contains(&Vec2::new(80.0, 0.0)) && round.contains(&Vec2::new(100.0, 20.0)));
        assert!(!round.contains(&Vec2::new(100.0, 0.0)), "the corner is cut");
        // Short legs limit the radius; straight runs keep their points.
        assert_eq!(
            corner_radius(
                &[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 50.0)],
                1,
                20.0
            ),
            5.0
        );
        let straight = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(30.0, 0.0)];
        assert_eq!(corner_radius(&straight, 1, 20.0), 0.0);
        assert_eq!(round_corners(&straight, 20.0), straight.to_vec());
    }

    #[test]
    fn path_stroke_has_constant_width() {
        let path = round_corners(
            &[Vec2::ZERO, Vec2::new(100.0, 0.0), Vec2::new(100.0, 100.0)],
            20.0,
        );
        let s = Shape::path_stroke(&path, 6.0);
        let (lo, hi) = s.bounds().unwrap();
        assert!(close(lo.x, -3.0, 1e-3) && close(hi.x, 103.0, 1e-3));
        assert!(close(lo.y, -3.0, 1e-3) && close(hi.y, 103.0, 1e-3));
        // Strip area is about length times width (plus the two caps).
        let length: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
        // Two full discs at the ends (overlaps count twice).
        let caps = 2.0 * PI * 9.0;
        assert!(
            close(s.area(), length * 6.0 + caps, length * 6.0 * 0.03),
            "{}",
            s.area()
        );
        assert_eq!(Shape::path_stroke(&[], 2.0).triangle_count(), 0);
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

    /// The renderer cannot upload a mesh with no vertices (it logs a
    /// "Use-after-free" error per upload), so an empty shape (a hidden
    /// drag mark, a drag with no veils) still makes a mesh with vertices
    /// and indices: one degenerate triangle.
    #[test]
    fn an_empty_shape_still_makes_a_mesh_with_vertices() {
        for shape in [
            Shape::default(),
            Shape {
                positions: vec![Vec2::ONE; 4],
                indices: Vec::new(),
            },
        ] {
            let mesh = shape.into_mesh();
            assert_eq!(mesh.count_vertices(), 3);
            assert_eq!(mesh.indices().map(|i| i.len()), Some(3));
        }
    }
}
