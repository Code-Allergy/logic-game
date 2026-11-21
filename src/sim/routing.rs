//! Orthogonal wire routing on the 20-unit grid.
//!
//! [`route`] takes the boxes of the gates (with their positions and sizes)
//! and the nets (one driver pin and any number of sink pins) and returns,
//! for every net, a tree of horizontal and vertical polylines: one
//! polyline per sink, starting at the driver pin (the net's first sink) or
//! at a junction on an earlier sink's polyline, and ending at the sink pin.
//! It is pure and deterministic: no ECS, no randomness, no hashing.
//!
//! # Model
//!
//! - **Grid graph.** Columns are the multiples of [`GRID`]; rows are the
//!   multiples of [`GRID`] plus the rows of off-grid pins. An off-grid row
//!   carries horizontal wire only for the nets that have a pin on it, so a
//!   wire can run straight into a pin between two grid rows without
//!   claiming a track next to another one.
//! - **Pins.** A wire leaves an output pin to the right and enters an input
//!   pin from the left: a short horizontal stub runs from the pin to its
//!   *access node*, the first grid column at least [`CLEARANCE`] beyond the
//!   tile. Access nodes are reserved for their net before any routing.
//! - **Obstacles.** Graph nodes inside a tile cost a lot to enter
//!   ([`IN_BOX`]) and nodes in its clearance some ([`NEAR_BOX`]), so wires
//!   go around tiles with room to spare whenever there is a way, squeeze
//!   past where tiles are close, and overlapping tiles (a freshly placed
//!   gate) still get a route. An access node that would land in the
//!   clearance of a tile right opposite moves towards its pin
//!   ([`access_for`]).
//! - **Sharing.** Two nets never share an edge (no collinear overlap) and
//!   meet at a node only where both run straight across each other: a
//!   crossing, drawn as an overpass. A net never bends or ends on another
//!   net, so only a junction dot means connected.
//! - **Search.** Each sink is an A* search over (node, heading) states
//!   with costs for length, bends ([`BEND`]) and crossings ([`CROSS`]).
//!   The search starts from every node of the net's tree routed so far
//!   (multi-source; leaving the tree sideways is a T junction), so later
//!   sinks join the nearest part of the tree: a Steiner-like tree with
//!   shared trunks. There are no four-way joins: a node of the tree has at
//!   most three branches.
//! - **Order.** Nets are routed shortest first (half-perimeter of their
//!   access nodes), sinks nearest the driver first; ties go by index.
//! - **Fallback.** When a search fails or exceeds its budget the sink gets
//!   a simple Z (or a loop below, for a backward wire) from the driver
//!   ([`SinkPath::routed`] is false); it is not reserved on the grid.
//!
//! # Incremental routing
//!
//! [`route`] can keep earlier paths ([`Keep`]): a kept path is checked
//! against the grid and reserved before anything is routed, and only the
//! other sinks are searched. The editor keeps every sink whose pins did not
//! move, so dragging a gate re-routes only the wires of that gate. With
//! `strict` false (during a drag) a kept path that now runs through a box
//! is still kept; with `strict` true it is routed again.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bevy::math::{Rect, Vec2};

use super::art::{EDGE, PinLayout};
use super::{CircuitDocument, CircuitLibrary, GateKind};

/// The routing grid (the editor grid).
pub const GRID: f32 = 20.0;
/// Space kept free around every tile.
pub const CLEARANCE: f32 = 10.0;
/// Room around the circuit for wires that go around it.
const PAD: f32 = 6.0 * GRID;
/// Height of the name tag under a port's tile (it counts as part of the
/// tile for routing).
const NAME_TAG: f32 = 22.0;

/// Cost of a bend, in world units of wire.
pub const BEND: u32 = 50;
/// Cost of crossing another net.
pub const CROSS: u32 = 30;
/// Cost of entering a node inside a (grown) box.
pub const IN_BOX: u32 = 4000;
/// Distance from a tile within which a wire costs nearly as much as in it
/// (about half a 1-bit casing): the least clearance where tiles leave no
/// room for [`CLEARANCE`].
pub const MIN_GAP: f32 = 4.0;
/// Cost of entering a node in a tile's clearance.
pub const NEAR_BOX: u32 = 40;
/// Cost of using the approach to another net's pin.
const APPROACH: u32 = 60;
/// Rounds of rip-up and reroute for sinks that fell back.
const RIP_UP_ROUNDS: usize = 4;
/// Cost of a junction (branching off the net's tree).
const JUNCTION: u32 = 10;
/// Extra cost of a junction at a corner of the tree.
const JUNCTION_AT_BEND: u32 = 30;
/// Most states one search may expand before it falls back.
const MAX_POPS: usize = 250_000;

// ─────────────────────────────────────────────────────────────────────────────
// Problem and result
// ─────────────────────────────────────────────────────────────────────────────

/// A net: one driver (output) pin and its sink (input) pins.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetSpec {
    pub driver: Vec2,
    pub sinks: Vec<Vec2>,
}

/// What to route: the boxes to keep clear of and the nets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RouteProblem {
    pub boxes: Vec<Rect>,
    pub nets: Vec<NetSpec>,
}

/// The wire of one sink.
#[derive(Debug, Clone, PartialEq)]
pub struct SinkPath {
    /// Corner points, horizontal and vertical segments only. Starts at the
    /// driver pin (no parent) or at a junction on the parent's path, ends
    /// at the sink pin.
    pub points: Vec<Vec2>,
    /// The sink whose path this one branches off.
    pub parent: Option<usize>,
    /// False for a fallback path (not searched, not reserved).
    pub routed: bool,
}

impl SinkPath {
    /// The junction this path starts at, if it branches off another.
    pub fn junction(&self) -> Option<Vec2> {
        self.parent.and(self.points.first().copied())
    }

    /// Segments as point pairs.
    pub fn segments(&self) -> impl Iterator<Item = (Vec2, Vec2)> + '_ {
        self.points.windows(2).map(|w| (w[0], w[1]))
    }
}

/// The routed tree of one net: a path per sink, in the net's sink order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetRoute {
    pub sinks: Vec<SinkPath>,
}

impl NetRoute {
    /// Junction points (where a branch leaves the tree).
    pub fn junctions(&self) -> impl Iterator<Item = Vec2> + '_ {
        self.sinks.iter().filter_map(SinkPath::junction)
    }

    /// The whole way from the driver pin to sink `sink`: its own path
    /// prefixed with the part of its ancestors' paths up to the junction.
    pub fn full_path(&self, sink: usize) -> Vec<Vec2> {
        let mut chain = vec![sink];
        let mut at = sink;
        while let Some(p) = self.sinks[at].parent {
            if chain.contains(&p) || chain.len() > self.sinks.len() {
                break;
            }
            chain.push(p);
            at = p;
        }
        let mut out: Vec<Vec2> = Vec::new();
        for (k, &s) in chain.iter().enumerate().rev() {
            let points = &self.sinks[s].points;
            let next = k.checked_sub(1).map(|j| chain[j]);
            match next.and_then(|n| self.sinks[n].junction()) {
                Some(stop) => {
                    let cut = cut_at(points, stop);
                    extend_path(&mut out, &cut);
                }
                None => extend_path(&mut out, points),
            }
        }
        simplify(&mut out);
        out
    }

    /// The longest horizontal segment of the net: `(sink, from, to)`.
    pub fn longest_horizontal(&self) -> Option<(usize, Vec2, Vec2)> {
        let mut best: Option<(usize, Vec2, Vec2)> = None;
        for (i, s) in self.sinks.iter().enumerate() {
            for (a, b) in s.segments() {
                if a.y == b.y && best.is_none_or(|(_, x, y)| (b.x - a.x).abs() > (y.x - x.x).abs())
                {
                    best = Some((i, a, b));
                }
            }
        }
        best
    }
}

/// Where two nets cross (both run straight through the point).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub at: Vec2,
    /// `(net, sink)` of the horizontal wire.
    pub horizontal: (usize, usize),
    /// `(net, sink)` of the vertical wire.
    pub vertical: (usize, usize),
}

/// Counters of one [`route`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouteStats {
    /// Sinks searched.
    pub searched: usize,
    /// Sinks kept from earlier.
    pub kept: usize,
    /// Searches that failed (fallback paths).
    pub fallbacks: usize,
    /// States expanded by all searches.
    pub expanded: usize,
    /// Graph nodes.
    pub nodes: usize,
}

/// The result of [`route`]: a route per net (in problem order) and the
/// crossings between nets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Routing {
    pub nets: Vec<NetRoute>,
    pub crossings: Vec<Crossing>,
    pub stats: RouteStats,
}

/// Earlier paths to keep: per net, per sink (in the net's current sink
/// order), the path routed before, with `parent` as a current sink index.
/// Shorter lists keep nothing for the rest.
pub type Keep = [Vec<Option<SinkPath>>];

/// Route every net of `problem` from scratch.
pub fn route_all(problem: &RouteProblem) -> Routing {
    route(problem, &[], true)
}

// ─────────────────────────────────────────────────────────────────────────────
// Polyline helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Drop repeated points and the middle points of straight runs.
pub fn simplify(points: &mut Vec<Vec2>) {
    points.dedup_by(|b, a| a.distance_squared(*b) < 1e-6);
    let mut i = 1;
    while i + 1 < points.len() {
        let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
        let straight = ((a.x - b.x).abs() < 1e-4 && (b.x - c.x).abs() < 1e-4)
            || ((a.y - b.y).abs() < 1e-4 && (b.y - c.y).abs() < 1e-4);
        // Only a straight run *through* b: not a U-turn.
        if straight && (b - a).dot(c - b) > 0.0 {
            points.remove(i);
        } else {
            i += 1;
        }
    }
}

fn extend_path(out: &mut Vec<Vec2>, points: &[Vec2]) {
    for &p in points {
        if out.last().is_none_or(|l| l.distance_squared(p) > 1e-6) {
            out.push(p);
        }
    }
}

/// Whether `p` lies on the axis-aligned segment `a`-`b`.
pub fn on_segment(p: Vec2, a: Vec2, b: Vec2) -> bool {
    let eps = 1e-3;
    let (lo, hi) = (a.min(b), a.max(b));
    p.x >= lo.x - eps && p.x <= hi.x + eps && p.y >= lo.y - eps && p.y <= hi.y + eps && {
        let d = b - a;
        (p - a).perp_dot(d).abs() <= eps * d.length().max(1.0)
    }
}

/// `points` up to (and including) the first point where they reach `stop`.
fn cut_at(points: &[Vec2], stop: Vec2) -> Vec<Vec2> {
    let mut out = Vec::new();
    for w in points.windows(2) {
        out.push(w[0]);
        if on_segment(stop, w[0], w[1]) {
            out.push(stop);
            return out;
        }
    }
    points.to_vec()
}

// ─────────────────────────────────────────────────────────────────────────────
// Grid graph
// ─────────────────────────────────────────────────────────────────────────────

const NONE: u32 = u32::MAX;
const MULTI: u32 = u32::MAX - 1;
const E: usize = 0;
const W: usize = 1;
const N: usize = 2;
const S: usize = 3;
const H_MASK: u8 = 0b0011;
const V_MASK: u8 = 0b1100;

fn bit(d: usize) -> u8 {
    1 << d
}

fn opposite(d: usize) -> usize {
    d ^ 1
}

fn horizontal(d: usize) -> bool {
    d < 2
}

/// One net's use of a node.
#[derive(Debug, Clone, Copy)]
struct Occ {
    net: u32,
    /// The sink whose path first used the node.
    sink: u32,
    /// Directions the net's wire leaves the node in.
    dirs: u8,
}

const FREE: Occ = Occ {
    net: NONE,
    sink: NONE,
    dirs: 0,
};

#[derive(Clone)]
struct Grid {
    x0: f32,
    cols: usize,
    ys: Vec<f32>,
    /// Off-grid rows: the nets with a pin on them (empty for grid rows).
    row_nets: Vec<Vec<u32>>,
    row_on_grid: Vec<bool>,
    /// 0 free, 1 in a tile's clearance, 2 in a tile (or nearer than
    /// [`MIN_GAP`]).
    zone: Vec<u8>,
    /// The net whose terminal (access node) this is.
    term: Vec<u32>,
    /// The net whose pin this node is the approach to (the node before a
    /// sink's access node, after a driver's), [`MULTI`] for several.
    approach: Vec<u32>,
    /// Up to two nets per node (two only at a crossing).
    occ: Vec<[Occ; 2]>,
}

fn snap_up(v: f32) -> f32 {
    (v / GRID - 1e-4).ceil() * GRID
}

fn snap_down(v: f32) -> f32 {
    (v / GRID + 1e-4).floor() * GRID
}

fn on_grid(v: f32) -> bool {
    (v / GRID - (v / GRID).round()).abs() < 1e-4
}

/// Default access node of a driver pin (the first grid column at least
/// [`CLEARANCE`] to its right) or a sink pin (to its left).
pub fn access_point(pin: Vec2, driver: bool) -> Vec2 {
    if driver {
        Vec2::new(snap_up(pin.x + CLEARANCE), pin.y)
    } else {
        Vec2::new(snap_down(pin.x - CLEARANCE), pin.y)
    }
}

/// Whether `p` lies strictly inside `r` grown by `by`.
fn inside(r: &Rect, by: f32, p: Vec2) -> bool {
    p.x > r.min.x - by && p.x < r.max.x + by && p.y > r.min.y - by && p.y < r.max.y + by
}

/// Access node of a pin among `boxes`: the default one, unless that lies
/// in another tile's clearance (a tile right opposite the pin); then the
/// grid column nearest the pin that does not, or failing that the one
/// nearest the pin. So a driver and a sink 30 units apart still meet
/// head on.
pub fn access_for(boxes: &[Rect], pin: Vec2, driver: bool) -> Vec2 {
    let own = |b: &Rect| inside(b, 0.5, pin);
    let blocked = |a: Vec2| boxes.iter().any(|b| !own(b) && inside(b, CLEARANCE, a));
    let mut a = access_point(pin, driver);
    let step = if driver { -GRID } else { GRID };
    while blocked(a) {
        let next = a.x + step;
        if (driver && next <= pin.x + 0.5) || (!driver && next >= pin.x - 0.5) {
            break;
        }
        a.x = next;
    }
    a
}

impl Grid {
    fn new(problem: &RouteProblem) -> Grid {
        let mut lo = Vec2::splat(f32::MAX);
        let mut hi = Vec2::splat(f32::MIN);
        for b in &problem.boxes {
            lo = lo.min(b.min);
            hi = hi.max(b.max);
        }
        let mut pin_rows: Vec<(f32, u32)> = Vec::new();
        for (i, net) in problem.nets.iter().enumerate() {
            for (pin, driver) in
                std::iter::once((net.driver, true)).chain(net.sinks.iter().map(|&s| (s, false)))
            {
                let a = access_point(pin, driver);
                lo = lo.min(pin.min(a));
                hi = hi.max(pin.max(a));
                pin_rows.push((pin.y, i as u32));
            }
        }
        if lo.x > hi.x {
            lo = Vec2::ZERO;
            hi = Vec2::ZERO;
        }
        let x0 = snap_down(lo.x - PAD);
        let x1 = snap_up(hi.x + PAD);
        let cols = ((x1 - x0) / GRID).round() as usize + 1;
        let (y0, y1) = (snap_down(lo.y - PAD), snap_up(hi.y + PAD));
        let mut rows: Vec<(f32, Option<u32>)> = (0..=((y1 - y0) / GRID).round() as usize)
            .map(|i| (y0 + i as f32 * GRID, None))
            .collect();
        for &(y, net) in &pin_rows {
            if !on_grid(y) {
                rows.push((y, Some(net)));
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut ys: Vec<f32> = Vec::with_capacity(rows.len());
        let mut row_nets: Vec<Vec<u32>> = Vec::with_capacity(rows.len());
        let mut row_on_grid = Vec::with_capacity(rows.len());
        for (y, net) in rows {
            if let Some(&last) = ys.last()
                && (y - last).abs() < 1e-3
            {
                let i = ys.len() - 1;
                if let Some(net) = net
                    && !row_on_grid[i]
                    && !row_nets[i].contains(&net)
                {
                    row_nets[i].push(net);
                }
                continue;
            }
            ys.push(y);
            row_on_grid.push(net.is_none());
            row_nets.push(net.into_iter().collect());
        }
        let nodes = cols * ys.len();
        let mut grid = Grid {
            x0,
            cols,
            ys,
            row_nets,
            row_on_grid,
            zone: vec![0; nodes],
            term: vec![NONE; nodes],
            approach: vec![NONE; nodes],
            occ: vec![[FREE; 2]; nodes],
        };
        for b in &problem.boxes {
            for (grow, level) in [(CLEARANCE, 1), (MIN_GAP, 2)] {
                let (lo, hi) = (b.min - grow, b.max + grow);
                let c0 = ((lo.x - x0) / GRID + 1e-4).floor() as i64 + 1;
                let c1 = ((hi.x - x0) / GRID - 1e-4).ceil() as i64 - 1;
                let r0 = grid.ys.partition_point(|&y| y <= lo.y + 1e-3);
                let r1 = grid.ys.partition_point(|&y| y < hi.y - 1e-3);
                for c in c0.max(0)..=c1.min(cols as i64 - 1) {
                    for r in r0..r1 {
                        let n = grid.node(c as usize, r);
                        grid.zone[n] = grid.zone[n].max(level);
                    }
                }
            }
        }
        grid
    }

    /// [`access_for`], with a quick answer where the default is clear.
    fn access(&self, boxes: &[Rect], pin: Vec2, driver: bool) -> Vec2 {
        let a = access_point(pin, driver);
        match self.node_at(a) {
            Some(n) if self.zone[n] == 0 => a,
            _ => access_for(boxes, pin, driver),
        }
    }

    fn rows(&self) -> usize {
        self.ys.len()
    }

    fn node(&self, col: usize, row: usize) -> usize {
        col * self.rows() + row
    }

    fn col_row(&self, n: usize) -> (usize, usize) {
        (n / self.rows(), n % self.rows())
    }

    fn pos(&self, n: usize) -> Vec2 {
        let (c, r) = self.col_row(n);
        Vec2::new(self.x0 + c as f32 * GRID, self.ys[r])
    }

    fn col_of(&self, x: f32) -> Option<usize> {
        let c = (x - self.x0) / GRID;
        let rc = c.round();
        ((c - rc).abs() < 1e-3 && rc >= 0.0 && (rc as usize) < self.cols).then_some(rc as usize)
    }

    fn row_of(&self, y: f32) -> Option<usize> {
        let i = self.ys.partition_point(|&v| v < y - 1e-3);
        (i < self.ys.len() && (self.ys[i] - y).abs() < 1e-3).then_some(i)
    }

    fn node_at(&self, p: Vec2) -> Option<usize> {
        Some(self.node(self.col_of(p.x)?, self.row_of(p.y)?))
    }

    fn neighbour(&self, n: usize, d: usize) -> Option<usize> {
        let (c, r) = self.col_row(n);
        match d {
            E => (c + 1 < self.cols).then(|| n + self.rows()),
            W => (c > 0).then(|| n - self.rows()),
            N => (r + 1 < self.rows()).then_some(n + 1),
            S => (r > 0).then(|| n - 1),
            _ => None,
        }
    }

    fn step_len(&self, n: usize, d: usize) -> u32 {
        if horizontal(d) {
            GRID as u32
        } else {
            let r = n % self.rows();
            let other = if d == N { r + 1 } else { r - 1 };
            ((self.ys[other] - self.ys[r]).abs().round() as u32).max(1)
        }
    }

    fn row_allows(&self, n: usize, net: u32) -> bool {
        let r = n % self.rows();
        self.row_on_grid[r] || self.row_nets[r].contains(&net)
    }

    fn own(&self, n: usize, net: u32) -> Option<Occ> {
        self.occ[n].iter().copied().find(|o| o.net == net)
    }

    /// The other nets' uses of node `n`.
    fn others(&self, n: usize, net: u32) -> impl Iterator<Item = Occ> + '_ {
        self.occ[n]
            .iter()
            .copied()
            .filter(move |o| o.net != NONE && o.net != net)
    }

    fn stamp(&mut self, n: usize, net: u32, sink: u32, dirs: u8) {
        let slot = &mut self.occ[n];
        if let Some(o) = slot.iter_mut().find(|o| o.net == net) {
            o.dirs |= dirs;
        } else if let Some(o) = slot.iter_mut().find(|o| o.net == NONE) {
            *o = Occ { net, sink, dirs };
        }
    }

    /// Whether net `net` may step from `n` in direction `d` onto `m`
    /// (`target` may be a terminal); the extra cost of the step, or `None`.
    /// `strict` makes boxes forbidden instead of costly.
    fn step_cost(
        &self,
        n: usize,
        d: usize,
        m: usize,
        net: u32,
        target: usize,
        boxes: BoxRule,
    ) -> Option<u32> {
        if horizontal(d) && !self.row_allows(n, net) {
            return None;
        }
        // An edge another net uses.
        if self.others(n, net).any(|o| o.dirs & bit(d) != 0) {
            return None;
        }
        if m != target && (self.own(m, net).is_some() || self.term[m] != NONE) {
            return None;
        }
        let mut extra = 0;
        let mut users = 0;
        for o in self.others(m, net) {
            users += 1;
            let across = if horizontal(d) { V_MASK } else { H_MASK };
            if o.dirs != across || m == target {
                return None;
            }
            extra += CROSS;
        }
        if users > 1 || (users == 1 && self.own(m, net).is_some()) {
            return None;
        }
        if m != target && self.approach[m] != NONE && self.approach[m] != net {
            extra += APPROACH;
        }
        if m != target {
            match (self.zone[m], boxes) {
                (2, BoxRule::Avoid) => extra += IN_BOX,
                (2, BoxRule::Forbid) => return None,
                (1, BoxRule::Avoid) => extra += NEAR_BOX,
                _ => {}
            }
        }
        Some(extra)
    }

    /// The graph nodes along a path, from the driver's access node (no
    /// parent) or the junction to the sink's access node.
    fn path_nodes(
        &self,
        path: &SinkPath,
        driver_access: Vec2,
        sink_access: Vec2,
    ) -> Option<Vec<usize>> {
        let mut nodes: Vec<usize> = Vec::new();
        for (a, b) in path.segments() {
            if (a.y - b.y).abs() < 1e-4 {
                let r = self.row_of(a.y)?;
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                let c0 = ((lo - self.x0) / GRID - 1e-3).ceil().max(0.0) as usize;
                let c1 = ((hi - self.x0) / GRID + 1e-3).floor();
                if c1 < 0.0 {
                    return None;
                }
                let c1 = (c1 as usize).min(self.cols - 1);
                let mut run: Vec<usize> = (c0..=c1).map(|c| self.node(c, r)).collect();
                if b.x < a.x {
                    run.reverse();
                }
                nodes.extend(run);
            } else if (a.x - b.x).abs() < 1e-4 {
                let c = self.col_of(a.x)?;
                let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                let r0 = self.ys.partition_point(|&y| y < lo - 1e-3);
                let r1 = self.ys.partition_point(|&y| y <= hi + 1e-3);
                let mut run: Vec<usize> = (r0..r1).map(|r| self.node(c, r)).collect();
                if b.y < a.y {
                    run.reverse();
                }
                nodes.extend(run);
            } else {
                return None;
            }
            nodes.dedup();
        }
        let start = if path.parent.is_none() {
            let at = self.node_at(driver_access)?;
            nodes.iter().position(|&n| n == at)?
        } else {
            let at = self.node_at(*path.points.first()?)?;
            (nodes.first() == Some(&at)).then_some(0)?
        };
        let end = self.node_at(sink_access)?;
        let stop = nodes.iter().rposition(|&n| n == end)?;
        if stop < start {
            return None;
        }
        let nodes = nodes[start..=stop].to_vec();
        for w in nodes.windows(2) {
            if !(0..4).any(|d| self.neighbour(w[0], d) == Some(w[1])) {
                return None;
            }
        }
        Some(nodes)
    }
}

/// How a step into a grown box is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoxRule {
    Avoid,
    Forbid,
    Ignore,
}

fn direction(grid: &Grid, a: usize, b: usize) -> usize {
    (0..4)
        .find(|&d| grid.neighbour(a, d) == Some(b))
        .expect("adjacent nodes")
}

// ─────────────────────────────────────────────────────────────────────────────
// Search
// ─────────────────────────────────────────────────────────────────────────────

struct Search {
    g: Vec<u32>,
    from: Vec<u32>,
    seen: Vec<u32>,
    stamp: u32,
    heap: BinaryHeap<Reverse<(u32, u32, u32)>>,
}

impl Search {
    fn new(nodes: usize) -> Search {
        Search {
            g: vec![0; nodes * 4],
            from: vec![NONE; nodes * 4],
            seen: vec![0; nodes * 4],
            stamp: 0,
            heap: BinaryHeap::new(),
        }
    }

    fn heuristic(grid: &Grid, n: usize, target: Vec2) -> u32 {
        let d = (target - grid.pos(n)).abs();
        let bends = if d.x > 1e-3 && d.y > 1e-3 { BEND } else { 0 };
        (d.x + d.y).round() as u32 + bends
    }

    /// Cheapest path for `net` from any of `sources` (node, heading, cost)
    /// to `target`, as nodes from a source to the target.
    fn run(
        &mut self,
        grid: &Grid,
        net: u32,
        sources: &[(usize, usize, u32)],
        target: usize,
        expanded: &mut usize,
    ) -> Option<Vec<usize>> {
        self.stamp += 1;
        self.heap.clear();
        let goal = grid.pos(target);
        for &(n, d, g) in sources {
            let s = n * 4 + d;
            if self.seen[s] == self.stamp && self.g[s] <= g {
                continue;
            }
            self.seen[s] = self.stamp;
            self.g[s] = g;
            self.from[s] = NONE;
            let h = Self::heuristic(grid, n, goal);
            self.heap.push(Reverse((g + h, h, s as u32)));
        }
        let mut pops = 0;
        while let Some(Reverse((_, _, s))) = self.heap.pop() {
            let s = s as usize;
            let (n, din) = (s / 4, s % 4);
            let g = self.g[s];
            pops += 1;
            if pops > MAX_POPS {
                break;
            }
            if n == target {
                *expanded += pops;
                let mut path = vec![n];
                let mut at = s;
                while self.from[at] != NONE {
                    at = self.from[at] as usize;
                    path.push(at / 4);
                }
                path.reverse();
                return Some(path);
            }
            // Straight on across another net.
            let crossing = grid.others(n, net).next().is_some();
            for d in 0..4 {
                if d == opposite(din) || (crossing && d != din) {
                    continue;
                }
                let Some(m) = grid.neighbour(n, d) else {
                    continue;
                };
                let Some(extra) = grid.step_cost(n, d, m, net, target, BoxRule::Avoid) else {
                    continue;
                };
                let mut cost = g + grid.step_len(n, d) + extra;
                if d != din {
                    cost += BEND;
                }
                if m == target {
                    // The wire goes on east into the pin.
                    if d == W {
                        continue;
                    }
                    if d != E {
                        cost += BEND;
                    }
                }
                let t = m * 4 + d;
                if self.seen[t] == self.stamp && self.g[t] <= cost {
                    continue;
                }
                self.seen[t] = self.stamp;
                self.g[t] = cost;
                self.from[t] = s as u32;
                let h = Self::heuristic(grid, m, goal);
                self.heap.push(Reverse((cost + h, h, t as u32)));
            }
        }
        *expanded += pops;
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Routing
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct NetState {
    driver_access: Vec2,
    sink_access: Vec<Vec2>,
    /// Graph nodes of the net's tree.
    tree: Vec<usize>,
    paths: Vec<Option<SinkPath>>,
    /// Per sink: the nodes where its searched path runs through a tile.
    through: Vec<Vec<usize>>,
}

/// Route `problem`, keeping the valid paths of `keep` (see [`Keep`]).
/// `strict` re-routes kept paths that run through a box; without it they
/// are kept as they are (a provisional route while a gate is dragged).
pub fn route(problem: &RouteProblem, keep: &Keep, strict: bool) -> Routing {
    let mut grid = Grid::new(problem);
    let mut stats = RouteStats {
        nodes: grid.occ.len(),
        ..RouteStats::default()
    };
    let mut nets: Vec<NetState> = problem
        .nets
        .iter()
        .map(|net| NetState {
            driver_access: grid.access(&problem.boxes, net.driver, true),
            sink_access: net
                .sinks
                .iter()
                .map(|&s| grid.access(&problem.boxes, s, false))
                .collect(),
            tree: Vec::new(),
            paths: vec![None; net.sinks.len()],
            through: vec![Vec::new(); net.sinks.len()],
        })
        .collect();

    // 1. Reserve every terminal (with the nodes on its stub), and mark the
    // approaches to the pins.
    for (i, (net, spec)) in nets.iter().zip(&problem.nets).enumerate() {
        let ends = std::iter::once((spec.driver, net.driver_access, E)).chain(
            spec.sinks
                .iter()
                .zip(&net.sink_access)
                .map(|(&pin, &a)| (pin, a, W)),
        );
        for (pin, a, outward) in ends {
            let Some(n) = grid.node_at(a) else { continue };
            let mut m = n;
            loop {
                if grid.term[m] == NONE {
                    grid.term[m] = i as u32;
                }
                match grid.neighbour(m, opposite(outward)) {
                    Some(next) if (grid.pos(next).x - pin.x) * (a.x - pin.x) > 1e-3 => m = next,
                    _ => break,
                }
            }
            if let Some(m) = grid.neighbour(n, outward) {
                let mark = &mut grid.approach[m];
                *mark = if *mark == NONE || *mark == i as u32 {
                    i as u32
                } else {
                    MULTI
                };
            }
        }
    }

    // 2. Kept paths, parents first.
    for (i, kept) in keep.iter().enumerate().take(nets.len()) {
        let net = i as u32;
        let mut done = vec![false; nets[i].paths.len()];
        let mut progress = true;
        while progress {
            progress = false;
            for (s, path) in kept.iter().enumerate().take(done.len()) {
                let Some(path) = path else { continue };
                if done[s] || path.points.len() < 2 {
                    continue;
                }
                let sink_pin = problem.nets[i].sinks[s];
                let parent_ok = match path.parent {
                    None => path.points[0].distance(problem.nets[i].driver) < 1e-3,
                    Some(p) => p < done.len() && nets[i].paths[p].is_some(),
                };
                if let Some(p) = path.parent
                    && p < done.len()
                    && !done[p]
                    && kept.get(p).is_some_and(Option::is_some)
                {
                    // Wait for the parent.
                    continue;
                }
                done[s] = true;
                progress = true;
                if !parent_ok || path.points.last().unwrap().distance(sink_pin) > 1e-3 {
                    continue;
                }
                let state = &nets[i];
                // A fallback is never reserved: searched again when strict.
                let nodes = path
                    .routed
                    .then(|| grid.path_nodes(path, state.driver_access, state.sink_access[s]))
                    .flatten();
                let fits = nodes.as_ref().is_some_and(|nodes| {
                    kept_fits(&grid, net, nodes, path.parent.is_none(), strict)
                });
                match nodes {
                    Some(nodes) if fits => {
                        stamp_path(
                            &mut grid,
                            &mut nets[i],
                            net,
                            s,
                            &nodes,
                            path.parent.is_none(),
                        );
                        nets[i].paths[s] = Some(path.clone());
                        stats.kept += 1;
                    }
                    _ if !strict => {
                        // Provisional: keep it as drawn, unreserved.
                        nets[i].paths[s] = Some(path.clone());
                        stats.kept += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    // 3. Route the rest: shortest nets first, nearest sinks first.
    let mut order: Vec<usize> = (0..nets.len())
        .filter(|&i| nets[i].paths.iter().any(Option::is_none))
        .collect();
    order.sort_by_key(|&i| (nets[i].span(), i));
    let mut search = Search::new(grid.occ.len());
    route_nets(
        &mut grid,
        &mut search,
        &mut nets,
        problem,
        &order,
        &mut stats,
    );

    // 4. Rip up and reroute: a sink that fell back, or whose path had to
    // run through a tile, is routed first, with the nets around its pins
    // (and around the tiles it crossed) routed again after it.
    let badness = |nets: &[NetState]| -> (usize, usize) {
        let fallbacks = nets
            .iter()
            .flat_map(|n| &n.paths)
            .filter(|p| p.as_ref().is_some_and(|p| !p.routed))
            .count();
        let through = nets.iter().flat_map(|n| &n.through).map(Vec::len).sum();
        (fallbacks, through)
    };
    for _ in 0..RIP_UP_ROUNDS {
        let before = badness(&nets);
        if !strict || before == (0, 0) {
            break;
        }
        let mut failed: Vec<usize> = Vec::new();
        let mut victims: Vec<usize> = Vec::new();
        for (i, n) in nets.iter().enumerate() {
            for (s, p) in n.paths.iter().enumerate() {
                let fell_back = p.as_ref().is_some_and(|p| !p.routed);
                if !fell_back && n.through[s].is_empty() {
                    continue;
                }
                failed.push(i);
                for a in [n.driver_access, n.sink_access[s]] {
                    victims.extend(grid.nets_near(a, 3));
                }
                for &t in &n.through[s] {
                    victims.extend(grid.nets_near(grid.pos(t), 2));
                }
            }
        }
        failed.sort_by_key(|&i| (nets[i].span(), i));
        failed.dedup();
        victims.sort_by_key(|&i| (nets[i].span(), i));
        victims.dedup();
        victims.retain(|v| !failed.contains(v));
        let snapshot = (grid.clone(), nets.clone(), stats);
        for &i in failed.iter().chain(&victims) {
            unroute(&mut grid, &mut nets[i], i as u32);
        }
        let again: Vec<usize> = failed.iter().chain(&victims).copied().collect();
        route_nets(
            &mut grid,
            &mut search,
            &mut nets,
            problem,
            &again,
            &mut stats,
        );
        if badness(&nets) >= before {
            (grid, nets, stats) = snapshot;
            break;
        }
    }
    stats.fallbacks = badness(&nets).0;

    // 5. Crossings.
    let mut crossings = Vec::new();
    for n in 0..grid.occ.len() {
        let [a, b] = grid.occ[n];
        if a.net == NONE || b.net == NONE || a.net == b.net {
            continue;
        }
        let (h, v) = if a.dirs == H_MASK { (a, b) } else { (b, a) };
        if h.dirs != H_MASK || v.dirs != V_MASK {
            continue;
        }
        crossings.push(Crossing {
            at: grid.pos(n),
            horizontal: (h.net as usize, h.sink as usize),
            vertical: (v.net as usize, v.sink as usize),
        });
    }

    Routing {
        nets: nets
            .into_iter()
            .map(|n| NetRoute {
                sinks: n
                    .paths
                    .into_iter()
                    .map(|p| p.expect("every sink"))
                    .collect(),
            })
            .collect(),
        crossings,
        stats,
    }
}

impl NetState {
    /// Half-perimeter of the net's access nodes.
    fn span(&self) -> u32 {
        let (mut lo, mut hi) = (self.driver_access, self.driver_access);
        for &a in &self.sink_access {
            lo = lo.min(a);
            hi = hi.max(a);
        }
        ((hi - lo).x + (hi - lo).y).round() as u32
    }
}

/// Search the unrouted sinks of `order`'s nets, in that order, nearest
/// sinks first.
fn route_nets(
    grid: &mut Grid,
    search: &mut Search,
    nets: &mut [NetState],
    problem: &RouteProblem,
    order: &[usize],
    stats: &mut RouteStats,
) {
    for &i in order {
        let state = &nets[i];
        let mut sinks: Vec<(u32, usize)> = (0..state.paths.len())
            .filter(|&s| state.paths[s].is_none())
            .map(|s| {
                let d = (state.sink_access[s] - state.driver_access).abs();
                ((d.x + d.y).round() as u32, s)
            })
            .collect();
        sinks.sort_unstable();
        for (_, s) in sinks {
            stats.searched += 1;
            let path = route_sink(grid, search, &mut nets[i], problem, i, s, stats);
            nets[i].paths[s] = Some(path);
        }
    }
}

/// Take a net off the grid: every path of it is to be routed again.
fn unroute(grid: &mut Grid, state: &mut NetState, net: u32) {
    for &t in &state.tree {
        for o in grid.occ[t].iter_mut() {
            if o.net == net {
                *o = FREE;
            }
        }
    }
    state.tree.clear();
    state.paths.iter_mut().for_each(|p| *p = None);
    state.through.iter_mut().for_each(Vec::clear);
}

impl Grid {
    /// Nets with a wire within `reach` grid steps of `p` (either way).
    fn nets_near(&self, p: Vec2, reach: usize) -> Vec<usize> {
        let Some(n) = self.node_at(p) else {
            return Vec::new();
        };
        let (c, _) = self.col_row(n);
        let span = reach as f32 * GRID + 1e-3;
        let r0 = self.ys.partition_point(|&y| y < p.y - span);
        let r1 = self.ys.partition_point(|&y| y <= p.y + span);
        let mut out = Vec::new();
        for cc in c.saturating_sub(reach)..=(c + reach).min(self.cols - 1) {
            for rr in r0..r1 {
                for o in &self.occ[self.node(cc, rr)] {
                    if o.net != NONE {
                        out.push(o.net as usize);
                    }
                }
            }
        }
        out
    }
}

/// Whether a kept path still fits the grid as reserved so far.
fn kept_fits(grid: &Grid, net: u32, nodes: &[usize], from_driver: bool, strict: bool) -> bool {
    let boxes = if strict {
        BoxRule::Forbid
    } else {
        BoxRule::Ignore
    };
    let target = *nodes.last().unwrap();
    let first = nodes[0];
    if from_driver {
        if grid.own(first, net).is_some() || grid.others(first, net).next().is_some() {
            return false;
        }
    } else {
        // A branch leaves the tree from a node with at most two branches.
        match grid.own(first, net) {
            Some(o) if o.dirs.count_ones() <= 2 && grid.others(first, net).next().is_none() => {}
            _ => return false,
        }
    }
    for w in nodes.windows(2) {
        let d = direction(grid, w[0], w[1]);
        if !from_driver
            && w[0] == first
            && grid.own(first, net).is_some_and(|o| o.dirs & bit(d) != 0)
        {
            return false;
        }
        if grid.step_cost(w[0], d, w[1], net, target, boxes).is_none() {
            return false;
        }
    }
    // Paths cross only straight on.
    for k in 1..nodes.len().saturating_sub(1) {
        if grid.others(nodes[k], net).next().is_some() {
            let (a, b) = (
                direction(grid, nodes[k - 1], nodes[k]),
                direction(grid, nodes[k], nodes[k + 1]),
            );
            if a != b {
                return false;
            }
        }
    }
    true
}

/// Reserve a path's nodes for its net.
fn stamp_path(
    grid: &mut Grid,
    state: &mut NetState,
    net: u32,
    sink: usize,
    nodes: &[usize],
    from_driver: bool,
) {
    let sink = sink as u32;
    let first = nodes[0];
    if from_driver {
        grid.stamp(first, net, sink, bit(W));
        state.tree.push(first);
    }
    for w in nodes.windows(2) {
        let d = direction(grid, w[0], w[1]);
        grid.stamp(w[0], net, sink, bit(d));
        if grid.own(w[1], net).is_none() {
            state.tree.push(w[1]);
        }
        grid.stamp(w[1], net, sink, bit(opposite(d)));
    }
    grid.stamp(*nodes.last().unwrap(), net, sink, bit(E));
}

/// Search (or fall back) for sink `s` of net `i`, and reserve the result.
fn route_sink(
    grid: &mut Grid,
    search: &mut Search,
    state: &mut NetState,
    problem: &RouteProblem,
    i: usize,
    s: usize,
    stats: &mut RouteStats,
) -> SinkPath {
    let net = i as u32;
    let spec = &problem.nets[i];
    let (Some(start), Some(target)) = (
        grid.node_at(state.driver_access),
        grid.node_at(state.sink_access[s]),
    ) else {
        return fallback(spec, s, state);
    };
    let fresh = grid.own(start, net).is_none();
    let mut sources: Vec<(usize, usize, u32)> = Vec::new();
    if fresh {
        sources.push((start, E, 0));
    } else {
        for &t in &state.tree {
            let Some(o) = grid.own(t, net) else { continue };
            if o.dirs.count_ones() >= 3 || grid.others(t, net).next().is_some() {
                continue;
            }
            let straight = o.dirs == H_MASK || o.dirs == V_MASK;
            let cost = JUNCTION + if straight { 0 } else { JUNCTION_AT_BEND };
            for d in 0..4 {
                if o.dirs & bit(d) == 0 {
                    sources.push((t, d, cost));
                }
            }
        }
    }
    let found = if sources.iter().any(|&(n, ..)| n == target) {
        // The sink's access node is on the tree (a pin right opposite).
        Some(vec![target])
    } else {
        search.run(grid, net, &sources, target, &mut stats.expanded)
    };
    let Some(nodes) = found else {
        return fallback(spec, s, state);
    };
    stamp_path(grid, state, net, s, &nodes, fresh);
    let inner = nodes
        .get(1..nodes.len().saturating_sub(1))
        .unwrap_or_default();
    state.through[s] = inner
        .iter()
        .copied()
        .filter(|&n| grid.zone[n] == 2)
        .collect();
    let mut points: Vec<Vec2> = Vec::with_capacity(nodes.len() + 2);
    let parent = if fresh {
        points.push(spec.driver);
        None
    } else {
        grid.own(nodes[0], net).map(|o| o.sink as usize)
    };
    points.extend(nodes.iter().map(|&n| grid.pos(n)));
    points.push(spec.sinks[s]);
    simplify(&mut points);
    SinkPath {
        points,
        parent: parent.filter(|&p| p != s),
        routed: true,
    }
}

/// A simple path from the driver to sink `s`, ignoring everything: a Z
/// for a forward wire, a loop below both pins for a backward one.
fn fallback(spec: &NetSpec, s: usize, state: &NetState) -> SinkPath {
    let (a, b) = (state.driver_access, state.sink_access[s]);
    let mut points = vec![spec.driver, a];
    if b.x >= a.x {
        let mid = snap_down((a.x + b.x) * 0.5).clamp(a.x, b.x);
        points.extend([Vec2::new(mid, a.y), Vec2::new(mid, b.y)]);
    } else {
        let below = snap_down(a.y.min(b.y)) - 3.0 * GRID;
        points.extend([Vec2::new(a.x, below), Vec2::new(b.x, below)]);
    }
    points.extend([b, spec.sinks[s]]);
    simplify(&mut points);
    SinkPath {
        points,
        parent: None,
        routed: false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Documents
// ─────────────────────────────────────────────────────────────────────────────

/// The box a gate's tile takes up for routing: the tile with its bottom
/// edge, and for ports the name tag below.
pub fn gate_box(kind: &GateKind, centre: Vec2) -> Rect {
    let half = kind.size() * 0.5;
    let mut min = centre - half - Vec2::new(0.0, EDGE);
    if kind.is_port() {
        min.y -= NAME_TAG;
    }
    Rect {
        min,
        max: centre + half,
    }
}

/// The pin layout of a gate's tile, as the canvas builds it.
pub fn pin_layout(kind: &GateKind, library: &CircuitLibrary) -> PinLayout {
    let (ins, outs) = library.pins(kind).unwrap_or_default();
    PinLayout::new(kind.size(), ins.len(), outs.len())
}

/// A document as a routing problem.
#[derive(Debug, Clone, Default)]
pub struct DocumentNets {
    pub problem: RouteProblem,
    /// Per net: the driver `(node index, pin)` and per sink `(node index,
    /// pin, wire index)`, in the problem's order.
    pub nets: Vec<((usize, u8), Vec<(usize, u8, usize)>)>,
    /// Pin width of each net's driver.
    pub widths: Vec<u8>,
}

/// The routing problem of a circuit document: a box per node, a net per
/// driven output pin. Nets are ordered by driver id and pin, sinks by node
/// id and pin, as the editor orders them, so a document routes the same
/// here and on the canvas.
pub fn document_nets(doc: &CircuitDocument, library: &CircuitLibrary) -> DocumentNets {
    use std::collections::BTreeMap;
    let index: std::collections::HashMap<_, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let layouts: Vec<PinLayout> = doc
        .nodes
        .iter()
        .map(|n| pin_layout(&n.kind, library))
        .collect();
    let centre = |i: usize| Vec2::new(doc.nodes[i].x, doc.nodes[i].y);
    let mut grouped: BTreeMap<(uuid::Uuid, u8), Vec<(uuid::Uuid, u8, usize)>> = BTreeMap::new();
    for (w, wire) in doc.wires.iter().enumerate() {
        if index.contains_key(&wire.from.node) && index.contains_key(&wire.to.node) {
            grouped
                .entry((wire.from.node, wire.from.pin))
                .or_default()
                .push((wire.to.node, wire.to.pin, w));
        }
    }
    let mut out = DocumentNets {
        problem: RouteProblem {
            boxes: doc
                .nodes
                .iter()
                .map(|n| gate_box(&n.kind, Vec2::new(n.x, n.y)))
                .collect(),
            nets: Vec::new(),
        },
        ..DocumentNets::default()
    };
    for ((driver, pin), mut sinks) in grouped {
        sinks.sort_by_key(|&(node, pin, _)| (node, pin));
        let d = index[&driver];
        out.problem.nets.push(NetSpec {
            driver: centre(d) + layouts[d].output(pin),
            sinks: sinks
                .iter()
                .map(|&(node, pin, _)| centre(index[&node]) + layouts[index[&node]].input(pin))
                .collect(),
        });
        out.widths.push(
            library
                .pins(&doc.nodes[d].kind)
                .and_then(|(_, outs)| outs.get(usize::from(pin)).copied())
                .unwrap_or(1),
        );
        out.nets.push((
            (d, pin),
            sinks
                .iter()
                .map(|&(node, pin, w)| (index[&node], pin, w))
                .collect(),
        ));
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Invariants (shared by the tests)
// ─────────────────────────────────────────────────────────────────────────────

/// Check the invariants of a routing of `problem`; the first violation.
///
/// - every segment is horizontal or vertical; vertical segments and every
///   corner away from the pins lie on grid columns, horizontal segments on
///   grid rows or on the row of one of the net's pins;
/// - no searched segment passes through a box (the stubs into their own
///   pins aside; the router also keeps clear of the boxes, by [`CLEARANCE`]
///   where there is room and by [`MIN_GAP`] where tiles are close);
/// - every sink's path ends at its pin and starts at the driver pin or on
///   its parent's path, so every sink is connected to the driver;
/// - a branching net has a junction where each branch leaves;
/// - segments of different nets never overlap collinearly.
pub fn check_invariants(problem: &RouteProblem, routing: &Routing) -> Result<(), String> {
    check(problem, routing, true, true)
}

/// [`check_invariants`] without the check against the boxes; see
/// [`tile_crossings`].
pub fn check_invariants_but_boxes(problem: &RouteProblem, routing: &Routing) -> Result<(), String> {
    check(problem, routing, true, false)
}

/// Searched segments (stubs aside) that pass through a box, as `(net,
/// sink, box)`.
pub fn tile_crossings(problem: &RouteProblem, routing: &Routing) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for (i, (spec, net)) in problem.nets.iter().zip(&routing.nets).enumerate() {
        for (s, path) in net.sinks.iter().enumerate() {
            if !path.routed || path.points.len() < 2 {
                continue;
            }
            for (a, b) in searched_segments(problem, spec, s, path) {
                for (bi, bx) in problem.boxes.iter().enumerate() {
                    if segment_hits(a, b, *bx) {
                        out.push((i, s, bi));
                    }
                }
            }
        }
    }
    out
}

/// The segments of a path with the stubs cut back to their access nodes.
fn searched_segments(
    problem: &RouteProblem,
    spec: &NetSpec,
    s: usize,
    path: &SinkPath,
) -> Vec<(Vec2, Vec2)> {
    let last = path.points.len() - 2;
    path.segments()
        .enumerate()
        .map(|(k, (a, b))| {
            let a = if k == 0 && path.parent.is_none() {
                access_for(&problem.boxes, spec.driver, true)
            } else {
                a
            };
            let b = if k == last {
                access_for(&problem.boxes, spec.sinks[s], false)
            } else {
                b
            };
            (a, b)
        })
        .filter(|(a, b)| a.distance(*b) > 1e-3)
        .collect()
}

/// The connectivity part of [`check_invariants`] only: every path ends at
/// its sink pin and starts at the driver pin or on its parent's path, and
/// every branch has a junction. For layouts whose tiles overlap, where the
/// geometric invariants cannot hold.
pub fn check_trees(problem: &RouteProblem, routing: &Routing) -> Result<(), String> {
    check(problem, routing, false, false)
}

fn check(
    problem: &RouteProblem,
    routing: &Routing,
    geometry: bool,
    boxes: bool,
) -> Result<(), String> {
    if routing.nets.len() != problem.nets.len() {
        return Err("one route per net".into());
    }
    let mut segments: Vec<(usize, Vec2, Vec2)> = Vec::new();
    for (i, (spec, net)) in problem.nets.iter().zip(&routing.nets).enumerate() {
        if net.sinks.len() != spec.sinks.len() {
            return Err(format!("net {i}: one path per sink"));
        }
        let pin_rows: Vec<f32> = std::iter::once(spec.driver.y)
            .chain(spec.sinks.iter().map(|s| s.y))
            .collect();
        for (s, path) in net.sinks.iter().enumerate() {
            let p = &path.points;
            if p.len() < 2 {
                return Err(format!("net {i} sink {s}: empty path"));
            }
            if p.last().unwrap().distance(spec.sinks[s]) > 1e-3 {
                return Err(format!("net {i} sink {s}: does not end at its pin"));
            }
            match path.parent {
                None => {
                    if p[0].distance(spec.driver) > 1e-3 {
                        return Err(format!("net {i} sink {s}: does not start at the driver"));
                    }
                }
                Some(parent) => {
                    let on_parent = net
                        .sinks
                        .get(parent)
                        .is_some_and(|pp| pp.segments().any(|(a, b)| on_segment(p[0], a, b)));
                    if !on_parent {
                        return Err(format!("net {i} sink {s}: junction not on its parent"));
                    }
                    let mut seen = vec![s];
                    let mut at = parent;
                    while let Some(next) = net.sinks[at].parent {
                        if seen.contains(&at) {
                            return Err(format!("net {i}: parent cycle"));
                        }
                        seen.push(at);
                        at = next;
                    }
                }
            }
            if !path.routed || !geometry {
                continue;
            }
            let last = p.len() - 2;
            if boxes {
                for (a, b) in searched_segments(problem, spec, s, path) {
                    if let Some(bi) = problem.boxes.iter().position(|bx| segment_hits(a, b, *bx)) {
                        return Err(format!(
                            "net {i} sink {s}: segment {a} {b} passes through box {bi}"
                        ));
                    }
                }
            }
            for (k, (a, b)) in path.segments().enumerate() {
                if (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3 {
                    return Err(format!("net {i} sink {s}: diagonal segment {a} {b}"));
                }
                if (a.x - b.x).abs() < 1e-3 {
                    if !on_grid(a.x) {
                        return Err(format!(
                            "net {i} sink {s}: vertical off the grid at x {}",
                            a.x
                        ));
                    }
                } else if !on_grid(a.y) && !pin_rows.iter().any(|y| (y - a.y).abs() < 1e-3) {
                    return Err(format!(
                        "net {i} sink {s}: horizontal off the grid at y {}",
                        a.y
                    ));
                }
                // Corners away from the pin stubs sit on grid columns.
                for (q, end) in [(a, k == 0 && path.parent.is_none()), (b, k == last)] {
                    if !end && !on_grid(q.x) {
                        return Err(format!("net {i} sink {s}: corner {q} off the grid"));
                    }
                }
                segments.push((i, a, b));
            }
        }
        // A branch leaves its parent at a junction.
        for (s, path) in net.sinks.iter().enumerate() {
            if path.parent.is_some() && path.junction().is_none() {
                return Err(format!("net {i} sink {s}: branch without junction"));
            }
        }
    }
    if !geometry {
        return Ok(());
    }
    // Collinear overlaps between different nets.
    let mut by_line: std::collections::HashMap<(bool, i64), Vec<(usize, f32, f32)>> =
        std::collections::HashMap::new();
    for &(net, a, b) in &segments {
        let vertical = (a.x - b.x).abs() < 1e-3;
        let (key, lo, hi) = if vertical {
            ((a.x * 100.0).round() as i64, a.y.min(b.y), a.y.max(b.y))
        } else {
            ((a.y * 100.0).round() as i64, a.x.min(b.x), a.x.max(b.x))
        };
        by_line
            .entry((vertical, key))
            .or_default()
            .push((net, lo, hi));
    }
    for ((vertical, key), mut list) in by_line {
        list.sort_by(|a, b| a.1.total_cmp(&b.1));
        for (k, &(na, _, ha)) in list.iter().enumerate() {
            for &(nb, lb, _) in &list[k + 1..] {
                if lb >= ha - 1e-3 {
                    break;
                }
                if na != nb {
                    return Err(format!(
                        "nets {na} and {nb} overlap on the {} line {}",
                        if vertical { "vertical" } else { "horizontal" },
                        key as f32 / 100.0
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Whether the axis-aligned segment `a`-`b` enters the interior of `r`.
fn segment_hits(a: Vec2, b: Vec2, r: Rect) -> bool {
    let (lo, hi) = (a.min(b), a.max(b));
    lo.x < r.max.x && hi.x > r.min.x && lo.y < r.max.y && hi.y > r.min.y
        || ((lo.x - hi.x).abs() < 1e-6
            && lo.x > r.min.x
            && lo.x < r.max.x
            && lo.y < r.max.y
            && hi.y > r.min.y)
        || ((lo.y - hi.y).abs() < 1e-6
            && lo.y > r.min.y
            && lo.y < r.max.y
            && lo.x < r.max.x
            && hi.x > r.min.x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(centre: Vec2) -> Rect {
        Rect::from_center_size(centre, Vec2::splat(50.0))
    }

    /// Output pin `k` of `n` at a 50-unit tile at `c`.
    fn out_pin(c: Vec2) -> Vec2 {
        c + Vec2::new(25.0, 0.0)
    }

    fn in_pin(c: Vec2, k: usize, n: usize) -> Vec2 {
        c + PinLayout::new(Vec2::splat(50.0), n, 1).inputs[k]
    }

    /// A few gates: two inputs into an AND, one input also fanning out to
    /// three more gates, and a feedback wire.
    fn sample() -> RouteProblem {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(0.0, -100.0);
        let g = Vec2::new(160.0, -40.0);
        let h = Vec2::new(160.0, 80.0);
        let k = Vec2::new(160.0, -200.0);
        let o = Vec2::new(320.0, -40.0);
        RouteProblem {
            boxes: [a, b, g, h, k, o].map(tile).to_vec(),
            nets: vec![
                NetSpec {
                    driver: out_pin(a),
                    sinks: vec![in_pin(g, 0, 2), in_pin(h, 0, 2), in_pin(k, 0, 2)],
                },
                NetSpec {
                    driver: out_pin(b),
                    sinks: vec![in_pin(g, 1, 2), in_pin(k, 1, 2)],
                },
                NetSpec {
                    driver: out_pin(g),
                    sinks: vec![in_pin(o, 0, 1), in_pin(h, 1, 2)],
                },
            ],
        }
    }

    #[test]
    fn routes_are_deterministic_and_valid() {
        let p = sample();
        let r = route_all(&p);
        assert_eq!(route_all(&p), r, "deterministic");
        check_invariants(&p, &r).unwrap();
        assert_eq!(r.stats.fallbacks, 0);
        // Every segment is axis-aligned.
        for net in &r.nets {
            for s in &net.sinks {
                for (a, b) in s.segments() {
                    assert!(a.x == b.x || a.y == b.y, "{a} {b}");
                }
            }
        }
    }

    #[test]
    fn a_straight_wire_has_no_bends() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(200.0, 0.0);
        let p = RouteProblem {
            boxes: vec![tile(a), tile(b)],
            nets: vec![NetSpec {
                driver: out_pin(a),
                sinks: vec![in_pin(b, 0, 1)],
            }],
        };
        let r = route_all(&p);
        assert_eq!(r.nets[0].sinks[0].points, vec![out_pin(a), in_pin(b, 0, 1)]);
    }

    #[test]
    fn pins_are_left_and_entered_horizontally() {
        let p = sample();
        let r = route_all(&p);
        for (spec, net) in p.nets.iter().zip(&r.nets) {
            for (s, path) in net.sinks.iter().enumerate() {
                let n = path.points.len();
                let (a, b) = (path.points[n - 2], path.points[n - 1]);
                assert_eq!(a.y, b.y, "enters horizontally");
                assert!(a.x < b.x, "from the left");
                assert_eq!(b, spec.sinks[s]);
                if path.parent.is_none() {
                    let (a, b) = (path.points[0], path.points[1]);
                    assert_eq!(a.y, b.y, "leaves horizontally");
                    assert!(b.x > a.x, "to the right");
                }
            }
        }
    }

    #[test]
    fn fanout_shares_a_trunk_with_junctions() {
        let p = sample();
        let r = route_all(&p);
        let net = &r.nets[0];
        let junctions: Vec<Vec2> = net.junctions().collect();
        assert_eq!(junctions.len(), 2, "three sinks: two branches");
        // Each sink's full path is connected and ends at its pin.
        for s in 0..3 {
            let full = net.full_path(s);
            assert_eq!(full[0], p.nets[0].driver);
            assert_eq!(*full.last().unwrap(), p.nets[0].sinks[s]);
            for w in full.windows(2) {
                assert!(w[0].x == w[1].x || w[0].y == w[1].y);
            }
        }
        // Sharing: the tree is shorter than three separate wires.
        let tree: f32 = net
            .sinks
            .iter()
            .flat_map(|s| s.segments())
            .map(|(a, b)| a.distance(b))
            .sum();
        let separate: f32 = (0..3)
            .map(|s| {
                let f = net.full_path(s);
                f.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>()
            })
            .sum();
        assert!(tree < separate * 0.9, "{tree} vs {separate}");
        // No junction is a four-way join: at most three branches meet.
        for j in junctions {
            let mut dirs = std::collections::HashSet::new();
            for s in &net.sinks {
                for (a, b) in s.segments() {
                    for (from, to) in [(a, b), (b, a)] {
                        if from.distance(j) < 1e-3 && to.distance(j) > 1e-3 {
                            let d = (to - from).normalize();
                            dirs.insert((d.x.round() as i32, d.y.round() as i32));
                        } else if on_segment(j, a, b)
                            && a.distance(j) > 1e-3
                            && b.distance(j) > 1e-3
                        {
                            let d = (b - a).normalize();
                            dirs.insert((d.x.round() as i32, d.y.round() as i32));
                            dirs.insert((-d.x.round() as i32, -d.y.round() as i32));
                        }
                    }
                }
            }
            assert!(dirs.len() <= 3, "junction {j}: {dirs:?}");
        }
    }

    #[test]
    fn routes_go_around_a_blocking_tile() {
        let a = Vec2::new(0.0, 0.0);
        let wall = Rect::from_center_size(Vec2::new(120.0, 0.0), Vec2::new(50.0, 200.0));
        let b = Vec2::new(260.0, 0.0);
        let p = RouteProblem {
            boxes: vec![tile(a), wall, tile(b)],
            nets: vec![NetSpec {
                driver: out_pin(a),
                sinks: vec![in_pin(b, 0, 1)],
            }],
        };
        let r = route_all(&p);
        check_invariants(&p, &r).unwrap();
        let path = &r.nets[0].sinks[0];
        assert!(path.routed);
        assert_eq!(
            path.points.len(),
            6,
            "around the wall: four corners {:?}",
            path.points
        );
    }

    #[test]
    fn backward_wires_loop_around() {
        // A register's output fed back to its own input.
        let a = Vec2::new(0.0, 0.0);
        let p = RouteProblem {
            boxes: vec![tile(a)],
            nets: vec![NetSpec {
                driver: out_pin(a),
                sinks: vec![in_pin(a, 0, 1)],
            }],
        };
        let r = route_all(&p);
        check_invariants(&p, &r).unwrap();
        assert!(r.nets[0].sinks[0].routed);
        assert_eq!(r.nets[0].sinks[0].points.len(), 6);
    }

    #[test]
    fn parallel_wires_take_separate_tracks_and_cross_straight() {
        // Three sources stacked on the left, three sinks stacked in reverse
        // order on the right: the wires must cross.
        let srcs: Vec<Vec2> = (0..3).map(|i| Vec2::new(0.0, -80.0 * i as f32)).collect();
        let dsts: Vec<Vec2> = (0..3)
            .map(|i| Vec2::new(300.0, -160.0 + 80.0 * i as f32))
            .collect();
        let p = RouteProblem {
            boxes: srcs.iter().chain(&dsts).map(|&c| tile(c)).collect(),
            nets: (0..3)
                .map(|i| NetSpec {
                    driver: out_pin(srcs[i]),
                    sinks: vec![in_pin(dsts[i], 0, 1)],
                })
                .collect(),
        };
        let r = route_all(&p);
        check_invariants(&p, &r).unwrap();
        assert!(!r.crossings.is_empty(), "the wires cross");
        for c in &r.crossings {
            assert_ne!(c.horizontal.0, c.vertical.0);
            let (h, v) = (
                &r.nets[c.horizontal.0].sinks[c.horizontal.1],
                &r.nets[c.vertical.0].sinks[c.vertical.1],
            );
            assert!(
                h.segments()
                    .any(|(a, b)| a.y == b.y && on_segment(c.at, a, b))
            );
            assert!(
                v.segments()
                    .any(|(a, b)| a.x == b.x && on_segment(c.at, a, b))
            );
        }
    }

    #[test]
    fn kept_paths_stay_and_new_sinks_join_them() {
        let p = sample();
        let full = route_all(&p);
        let keep: Vec<Vec<Option<SinkPath>>> = full
            .nets
            .iter()
            .map(|n| n.sinks.iter().cloned().map(Some).collect())
            .collect();
        let again = route(&p, &keep, true);
        assert_eq!(again.nets, full.nets);
        assert_eq!(again.stats.kept, 7);
        assert_eq!(again.stats.searched, 0);

        // Move the gate `o` (sink of net 2): only that sink is searched, and
        // net 2's other sink keeps its path if it does not depend on it.
        let mut moved = p.clone();
        moved.boxes[5] = tile(Vec2::new(340.0, 60.0));
        moved.nets[2].sinks[0] = in_pin(Vec2::new(340.0, 60.0), 0, 1);
        let mut keep2 = keep.clone();
        keep2[2][0] = None;
        for (s, path) in full.nets[2].sinks.iter().enumerate() {
            if path.parent == Some(0) {
                keep2[2][s] = None;
            }
        }
        let r = route(&moved, &keep2, false);
        check_invariants(&moved, &r).unwrap();
        for i in 0..2 {
            assert_eq!(r.nets[i], full.nets[i], "untouched nets keep their paths");
        }
        assert!(r.stats.searched >= 1 && r.stats.kept >= 5);
    }

    #[test]
    fn overlapping_tiles_still_get_paths() {
        // Every gate at the origin, as a freshly built test circuit has it.
        let c = Vec2::ZERO;
        let p = RouteProblem {
            boxes: vec![tile(c); 4],
            nets: vec![
                NetSpec {
                    driver: out_pin(c),
                    sinks: vec![in_pin(c, 0, 2), in_pin(c, 0, 1)],
                },
                NetSpec {
                    driver: out_pin(c),
                    sinks: vec![in_pin(c, 1, 2)],
                },
            ],
        };
        let r = route_all(&p);
        for (spec, net) in p.nets.iter().zip(&r.nets) {
            for (s, path) in net.sinks.iter().enumerate() {
                assert_eq!(*path.points.last().unwrap(), spec.sinks[s]);
                for (a, b) in path.segments() {
                    assert!(a.x == b.x || a.y == b.y);
                }
            }
        }
    }

    /// A large layered circuit: `cols` columns of `rows` two-input gates,
    /// each fed by two gates of the column before, some long and some
    /// backward wires.
    fn big(cols: usize, rows: usize) -> RouteProblem {
        let centre = |c: usize, r: usize| Vec2::new(c as f32 * 160.0, -(r as f32) * 120.0);
        let mut p = RouteProblem::default();
        for c in 0..cols {
            for r in 0..rows {
                p.boxes.push(tile(centre(c, r)));
            }
        }
        let mut sinks: Vec<Vec<Vec2>> = vec![Vec::new(); cols * rows];
        let mut seed = 12345u64;
        let mut next = |m: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % m
        };
        for c in 1..cols {
            for r in 0..rows {
                for k in 0..2 {
                    let from_col = if next(20) == 0 { next(cols) } else { c - 1 };
                    let from = from_col * rows + (r + next(5)).saturating_sub(2).min(rows - 1);
                    sinks[from].push(in_pin(centre(c, r), k, 2));
                }
            }
        }
        for (i, s) in sinks.into_iter().enumerate() {
            if !s.is_empty() {
                p.nets.push(NetSpec {
                    driver: out_pin(centre(i / rows, i % rows)),
                    sinks: s,
                });
            }
        }
        p
    }

    #[test]
    fn a_large_circuit_routes_quickly() {
        let p = big(20, 50);
        let start = std::time::Instant::now();
        let r = route_all(&p);
        let took = start.elapsed();
        let sinks: usize = p.nets.iter().map(|n| n.sinks.len()).sum();
        eprintln!(
            "{} gates, {} nets, {sinks} sinks, {} nodes: {took:?}, {} expanded, {} fallbacks",
            p.boxes.len(),
            p.nets.len(),
            r.stats.nodes,
            r.stats.expanded,
            r.stats.fallbacks
        );
        assert_eq!(r.nets.len(), p.nets.len());
        check_invariants(&p, &r).unwrap();
        assert!(
            r.stats.fallbacks * 50 <= sinks,
            "few fallbacks: {}",
            r.stats.fallbacks
        );
        // Debug builds are slow; this is a smoke test, the release number is
        // in the report.
        assert!(took.as_secs_f32() < 30.0, "{took:?}");
    }
}
