//! Geometry of the demo map: where its cards go for a window size
//! ([`MapLayout`]) and the winding road through the stops ([`Road`]).
//!
//! Everything is in logical pixels, y down. Sizes follow the mockup at
//! 1440 x 900 and scale with [`MapLayout::scale`].

use bevy::math::{Rect, Vec2};

/// The mockup's page size.
pub const DESIGN: Vec2 = Vec2::new(1440.0, 900.0);

/// A stop tile (the last stop is [`BIG_STOP`]); the current stop is
/// [`CURRENT_STOP`].
pub const STOP: Vec2 = Vec2::new(52.0, 50.0);
pub const CURRENT_STOP: Vec2 = Vec2::new(60.0, 58.0);
pub const BIG_STOP: Vec2 = Vec2::new(96.0, 82.0);
/// The progress card in the map's lower right corner.
pub const PROGRESS_CARD: Vec2 = Vec2::new(210.0, 150.0);

/// Where the demo map's parts sit in a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapLayout {
    pub screen: Vec2,
    /// Mockup units to pixels: 1 at 1440 x 900, 0.8 at 1280 x 720, 1.6 at
    /// 2560 x 1440.
    pub scale: f32,
    /// Title, line and the Back button.
    pub header: Rect,
    /// The road map card.
    pub map: Rect,
    /// The level panel, right of the map.
    pub panel: Rect,
    /// The gallery shelf along the bottom.
    pub gallery: Rect,
}

impl MapLayout {
    pub fn new(screen: Vec2) -> Self {
        let s = (screen.x / DESIGN.x)
            .min(screen.y / DESIGN.y)
            .clamp(0.75, 1.6);
        let (top, side, bottom) = (28.0 * s, 40.0 * s, 32.0 * s);
        let (gap_y, gap_x) = (18.0 * s, 24.0 * s);
        let header_h = 44.0 * s;
        let gallery_h = Self::gallery_height(s);
        let content = Rect::new(side, top, screen.x - side, screen.y - bottom);
        let header = Rect::new(
            content.min.x,
            content.min.y,
            content.max.x,
            content.min.y + header_h,
        );
        let gallery = Rect::new(
            content.min.x,
            content.max.y - gallery_h,
            content.max.x,
            content.max.y,
        );
        let row = (header.max.y + gap_y, gallery.min.y - gap_y);
        let panel_w = (content.width() * 0.38).max(360.0 * s);
        let map = Rect::new(content.min.x, row.0, content.max.x - panel_w - gap_x, row.1);
        let panel = Rect::new(map.max.x + gap_x, row.0, content.max.x, row.1);
        Self {
            screen,
            scale: s,
            header,
            map,
            panel,
            gallery,
        }
    }

    /// The shelf: a title row, then a card with its thumbnail, title and
    /// bottom edge.
    fn gallery_height(s: f32) -> f32 {
        (30.0 + 10.0 + Self::GALLERY_CARD_H + 4.0) * s
    }

    /// Height of a gallery card at scale 1 (8 + 56 thumbnail + 6 + title
    /// + 8).
    pub const GALLERY_CARD_H: f32 = 100.0;

    /// The road's well inside the map card.
    pub fn well(&self) -> Rect {
        self.map.inflate(-10.0 * self.scale)
    }

    /// The progress card, in the well's lower right corner.
    pub fn progress_card(&self) -> Rect {
        let s = self.scale;
        let well = self.well();
        let max = Vec2::new(well.max.x - 30.0 * s, well.max.y - 36.0 * s);
        Rect::from_corners(max - PROGRESS_CARD * s, max)
    }

    /// The road through `stops` stops in this layout.
    pub fn road(&self, stops: usize) -> Road {
        let s = self.scale;
        let well = self.well();
        // Room for the pin above the top row, labels below the bottom row,
        // and the big last tile at the right.
        let area = Rect::new(
            well.min.x + 60.0 * s,
            well.min.y + 92.0 * s,
            well.max.x - 76.0 * s,
            well.max.y - 72.0 * s,
        );
        // Lower rows stop short of the progress card, leaving room for a
        // turn's bulge and the label beside it.
        let lower_right = self.progress_card().min.x - 150.0 * s;
        Road::new(stops, area, lower_right, s)
    }
}

/// Where a stop's name goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSide {
    Below,
    /// Beside the stop, on a turn: right of it.
    Right,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoadStop {
    /// Centre of the tile.
    pub pos: Vec2,
    /// Index of the road point at the stop.
    pub point: usize,
    pub label: LabelSide,
}

/// A winding road: a polyline and the stops along it at equal distances,
/// the first at its start and the last at its end.
#[derive(Debug, Clone, PartialEq)]
pub struct Road {
    pub points: Vec<Vec2>,
    pub stops: Vec<RoadStop>,
}

/// Points per row and per turn.
const ROW_SAMPLES: usize = 48;
const TURN_SAMPLES: usize = 24;

impl Road {
    /// A snake of rows inside `area`, from the bottom up, ending at the
    /// top right: the top row spans the area, the rows below it end at
    /// `lower_right`. One row per four stops (fewer if the area is short),
    /// joined by half-ellipse turns; each row has a gentle wave. The same
    /// arguments always give the same road.
    pub fn new(stops: usize, area: Rect, lower_right: f32, scale: f32) -> Self {
        if stops == 0 {
            return Self {
                points: Vec::new(),
                stops: Vec::new(),
            };
        }
        let min_gap = 120.0 * scale;
        let fit = (area.height() / min_gap).floor() as usize + 1;
        let rows = stops.div_ceil(4).clamp(1, 4).min(fit.max(1));
        let gap = if rows > 1 {
            area.height() / (rows - 1) as f32
        } else {
            0.0
        };
        // Half-ellipse turns, half as wide as they are tall.
        let rx = gap * 0.25;
        // Left turns sit in from the edge: their stops' names go beside
        // them, on the outside.
        let left = area.min.x + if rows > 1 { 80.0 * scale } else { 0.0 };
        let lower_right = lower_right.clamp(left + 4.0 * rx + 1.0, area.max.x);
        let wave = (gap * 0.09).min(16.0 * scale);

        // Rows from the top (row 0 runs right; the next one left, ...), in
        // travel order from the bottom.
        let mut points = Vec::new();
        for i in (0..rows).rev() {
            let y = area.min.y + gap * i as f32;
            let rightwards = i % 2 == 0;
            let start_side_x = |right: bool| {
                if right {
                    if i == 0 { area.max.x } else { lower_right - rx }
                } else {
                    left + rx
                }
            };
            // The first row starts at the area's edge, the last ends there.
            let (mut from, mut to) = (start_side_x(!rightwards), start_side_x(rightwards));
            if i == rows - 1 {
                from = if rightwards { area.min.x } else { lower_right };
            }
            if i == 0 {
                to = if rightwards { area.max.x } else { area.min.x };
            }
            for k in 0..=ROW_SAMPLES {
                if k == 0 && !points.is_empty() {
                    continue;
                }
                let t = k as f32 / ROW_SAMPLES as f32;
                let phase = std::f32::consts::PI * t;
                // Zero, with zero slope, at both ends: rows meet the turns
                // smoothly.
                let dy =
                    wave * (2.0 * phase).sin() * phase.sin() * if i % 2 == 0 { 1.0 } else { -1.0 };
                points.push(Vec2::new(from + (to - from) * t, y + dy));
            }
            if i > 0 {
                // Turn up to the row above, bulging outwards.
                let side = if rightwards { 1.0 } else { -1.0 };
                let centre = Vec2::new(to, y - gap * 0.5);
                for k in 1..TURN_SAMPLES {
                    let a = std::f32::consts::FRAC_PI_2
                        - std::f32::consts::PI * k as f32 / TURN_SAMPLES as f32;
                    points.push(centre + Vec2::new(side * rx * a.cos(), gap * 0.5 * a.sin()));
                }
            }
        }

        // Even spacing between points, so stops land where they should.
        let points = resample(&points, 2.0);

        // Stops at equal distances along the road.
        let mut along = vec![0.0f32];
        for w in points.windows(2) {
            along.push(along.last().copied().unwrap_or(0.0) + w[0].distance(w[1]));
        }
        let length = along.last().copied().unwrap_or(0.0);
        let mid_x = (left + lower_right) * 0.5;
        let road_stops = (0..stops)
            .map(|k| {
                let target = if stops > 1 {
                    length * k as f32 / (stops - 1) as f32
                } else {
                    0.0
                };
                let point = along
                    .iter()
                    .position(|&d| d >= target - 1e-3)
                    .unwrap_or(points.len() - 1);
                let pos = points[point];
                let before = points[point.saturating_sub(2)];
                let after = points[(point + 2).min(points.len() - 1)];
                let tangent = after - before;
                let label = if tangent.y.abs() > tangent.x.abs() {
                    if pos.x > mid_x {
                        LabelSide::Right
                    } else {
                        LabelSide::Left
                    }
                } else {
                    LabelSide::Below
                };
                RoadStop { pos, point, label }
            })
            .collect();
        Self {
            points,
            stops: road_stops,
        }
    }

    /// The road from its start to stop `stop` (inclusive).
    pub fn points_to(&self, stop: usize) -> &[Vec2] {
        let end = self.stops.get(stop).map_or(0, |s| s.point);
        &self.points[..=end.min(self.points.len().saturating_sub(1))]
    }

    /// The road from stop `stop` to its end.
    pub fn points_from(&self, stop: usize) -> &[Vec2] {
        let start = self.stops.get(stop).map_or(0, |s| s.point);
        &self.points[start.min(self.points.len())..]
    }
}

/// `points` again, every `step` along the line (and its last point).
fn resample(points: &[Vec2], step: f32) -> Vec<Vec2> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut out = vec![first];
    let mut carry = 0.0;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        let mut d = step - carry;
        while d <= len {
            out.push(a.lerp(b, d / len));
            d += step;
        }
        carry = len - (d - step);
    }
    if let Some(&last) = points.last()
        && out.last().is_some_and(|p| p.distance(last) > 1e-3)
    {
        out.push(last);
    }
    out
}

/// The size of stop `index` of `count`, as drawn (before scaling).
pub fn stop_size(index: usize, count: usize, current: bool) -> Vec2 {
    if index + 1 == count {
        BIG_STOP
    } else if current {
        CURRENT_STOP
    } else {
        STOP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [(f32, f32); 5] = [
        (1280.0, 720.0),
        (1366.0, 768.0),
        (1600.0, 900.0),
        (1920.0, 1080.0),
        (2560.0, 1440.0),
    ];

    fn overlaps(a: Rect, b: Rect) -> bool {
        !a.intersect(b).is_empty()
    }

    #[test]
    fn map_layout_fits_from_720p_to_1440p() {
        for (w, h) in SIZES {
            let screen = Vec2::new(w, h);
            let l = MapLayout::new(screen);
            let window = Rect::from_corners(Vec2::ZERO, screen);
            let parts = [
                ("header", l.header),
                ("map", l.map),
                ("panel", l.panel),
                ("gallery", l.gallery),
            ];
            for (name, r) in parts {
                assert!(
                    window.contains(r.min) && window.contains(r.max),
                    "{name} at {w}x{h}"
                );
                assert!(r.width() > 0.0 && r.height() > 0.0, "{name} at {w}x{h}");
            }
            for (i, (a, ra)) in parts.iter().enumerate() {
                for (b, rb) in &parts[i + 1..] {
                    assert!(!overlaps(*ra, *rb), "{a} overlaps {b} at {w}x{h}");
                }
            }
            assert!(l.map.width() > 0.55 * w, "map width at {w}x{h}");
            assert!(l.map.height() > 0.5 * h, "map height at {w}x{h}");
            assert!(l.panel.width() >= 340.0, "panel at {w}x{h}");
            assert!(l.well().contains(l.progress_card().min));
            assert!(l.well().contains(l.progress_card().max));
        }
        let l = MapLayout::new(DESIGN);
        assert_eq!(l.scale, 1.0);
        assert!((l.map.width() - 820.0).abs() < 25.0, "{}", l.map.width());
        assert_eq!(MapLayout::new(Vec2::new(1280.0, 720.0)).scale, 0.8);
        assert_eq!(MapLayout::new(Vec2::new(2560.0, 1440.0)).scale, 1.6);
    }

    #[test]
    fn road_is_deterministic() {
        let l = MapLayout::new(Vec2::new(1920.0, 1080.0));
        assert_eq!(l.road(10), l.road(10));
        assert_eq!(
            MapLayout::new(Vec2::new(1280.0, 720.0)).road(7),
            MapLayout::new(Vec2::new(1280.0, 720.0)).road(7)
        );
        assert!(l.road(0).stops.is_empty());
    }

    #[test]
    fn road_runs_from_the_first_stop_to_the_last() {
        let l = MapLayout::new(Vec2::new(1440.0, 900.0));
        let road = l.road(10);
        assert_eq!(road.stops.len(), 10);
        assert_eq!(road.stops[0].pos, road.points[0]);
        assert_eq!(road.stops[9].pos, *road.points.last().unwrap());
        // It starts low on the left and ends at the top right.
        let (first, last) = (road.stops[0].pos, road.stops[9].pos);
        assert!(first.y > last.y + 200.0);
        assert!(last.x > first.x + 300.0);
        assert!(road.stops.windows(2).all(|w| w[0].point <= w[1].point));
        assert_eq!(
            road.points_to(9).len() + road.points_from(9).len(),
            road.points.len() + 1
        );
    }

    /// Stops are evenly spaced along the road, never crowd each other, stay
    /// in the well and clear of the progress card, for any level count.
    #[test]
    fn stops_are_evenly_spaced_and_clear_of_each_other() {
        for (w, h) in SIZES {
            let l = MapLayout::new(Vec2::new(w, h));
            let s = l.scale;
            for n in 2..=16 {
                let road = l.road(n);
                assert_eq!(road.stops.len(), n);
                // Distance along the road between neighbours.
                let mut along = vec![0.0f32];
                for p in road.points.windows(2) {
                    along.push(along.last().unwrap() + p[0].distance(p[1]));
                }
                let gaps: Vec<f32> = road
                    .stops
                    .windows(2)
                    .map(|p| along[p[1].point] - along[p[0].point])
                    .collect();
                let (lo, hi) = gaps
                    .iter()
                    .fold((f32::MAX, 0.0f32), |(lo, hi), &g| (lo.min(g), hi.max(g)));
                let step = along.last().unwrap() / (n - 1) as f32;
                assert!(hi - lo < 0.1 * step + 2.0, "even spacing, n={n} at {w}x{h}");
                for (i, a) in road.stops.iter().enumerate() {
                    let size = stop_size(i, n, false) * s;
                    let tile = Rect::from_center_size(a.pos, size);
                    assert!(
                        l.well().contains(tile.min) && l.well().contains(tile.max),
                        "stop {i} of {n} in the well at {w}x{h}"
                    );
                    assert!(
                        !overlaps(tile.inflate(8.0 * s), l.progress_card()),
                        "stop {i} of {n} on the progress card at {w}x{h}"
                    );
                    for (j, b) in road.stops.iter().enumerate().skip(i + 1) {
                        let other = Rect::from_center_size(b.pos, stop_size(j, n, false) * s);
                        assert!(
                            !overlaps(tile.inflate(10.0 * s), other),
                            "stops {i} and {j} of {n} touch at {w}x{h}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ten_stops_wind_in_three_rows_with_labels_beside_the_turns() {
        let road = MapLayout::new(DESIGN).road(10);
        // Three rows: the road changes horizontal direction twice.
        let mut turns = 0;
        let mut heading = 0.0f32;
        for w in road.points.windows(2) {
            let dx = w[1].x - w[0].x;
            if dx.abs() > 1.0 {
                if heading != 0.0 && dx.signum() != heading {
                    turns += 1;
                }
                heading = dx.signum();
            }
        }
        assert_eq!(turns, 2);
        // Stops on straights are named below; stops on the turns beside
        // them, outwards (right turn: right, left turn: left).
        assert_eq!(road.stops[0].label, LabelSide::Below);
        assert_eq!(road.stops[9].label, LabelSide::Below);
        let beside: Vec<_> = road
            .stops
            .iter()
            .filter(|s| s.label != LabelSide::Below)
            .collect();
        assert!(!beside.is_empty());
        let centre = MapLayout::new(DESIGN).well().center().x;
        for stop in beside {
            match stop.label {
                LabelSide::Right => assert!(stop.pos.x > centre - 200.0),
                LabelSide::Left => assert!(stop.pos.x < centre),
                LabelSide::Below => unreachable!(),
            }
        }
    }
}
