//! Automatic layout of a circuit: layered (Sugiyama-style) placement.
//!
//! Used by the RTL importer ([`super::rtl`]) and available for any circuit
//! document ([`layout_circuit`]). See `docs/rtl-import.md` ("Auto-layout").
//!
//! 1. **Cut feedback.** Edges out of state elements (`Delay`, `Register`,
//!    `SrLatch`) are left out of the ranking, as the compiler does for loop
//!    detection. Any cycle left (through instances, or a combinational
//!    loop the compiler will report) is broken by dropping the back edges
//!    of a depth-first search in node order.
//! 2. **Rank by logic depth.** Inputs are rank 0; every other node is one
//!    more than its deepest predecessor (at least 1). A node without
//!    predecessors (a constant) is moved right, next to its first consumer.
//!    Outputs get the last rank.
//! 3. **Order within ranks.** Starting from node order, alternating sweeps
//!    sort each rank by the barycenter of its neighbours in the ranks
//!    before (downward sweep) or after (upward sweep). The order with the
//!    fewest crossings between adjacent ranks is kept.
//! 4. **Coordinates.** One column per rank, left to right; nodes stacked
//!    top to bottom in their order, each column centred on y = 0. Centres
//!    are on the 20-unit editor grid and boxes ([`GateKind::size`]) never
//!    overlap.
//!
//! Everything is deterministic: the result depends only on the node and
//! edge lists, in order.

use std::collections::{HashMap, HashSet};

use bevy::math::Vec2;

use super::{CircuitDocument, GateKind};

/// The editor grid: every laid-out centre is a multiple of this.
pub const GRID: f32 = 20.0;
/// Horizontal space between columns.
pub const COLUMN_GAP: f32 = 100.0;
/// Vertical space between nodes of a column.
pub const ROW_GAP: f32 = 30.0;
/// Number of crossing-reduction sweeps (each down then up).
const SWEEPS: usize = 6;

/// What a node is, for the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// An input port: first column.
    Input,
    /// An output port or display: last column.
    Output,
    /// A state element: its outgoing edges do not count for ranking.
    State,
    /// Everything else.
    Logic,
}

impl Role {
    pub fn of(kind: &GateKind) -> Role {
        match kind {
            GateKind::Input { .. } => Role::Input,
            GateKind::Output { .. } | GateKind::Display { .. } => Role::Output,
            GateKind::Delay { .. } | GateKind::Register { .. } | GateKind::SrLatch => Role::State,
            _ => Role::Logic,
        }
    }
}

/// A node to place.
#[derive(Debug, Clone, Copy)]
pub struct LayoutNode {
    pub role: Role,
    /// Box size; the position is its centre.
    pub size: Vec2,
}

impl LayoutNode {
    pub fn of(kind: &GateKind) -> LayoutNode {
        LayoutNode {
            role: Role::of(kind),
            size: kind.size(),
        }
    }
}

/// The result of [`layout`].
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Centre of each node, in input order.
    pub positions: Vec<Vec2>,
    /// Rank (column) of each node.
    pub ranks: Vec<usize>,
    /// Nodes of each rank, top to bottom.
    pub columns: Vec<Vec<usize>>,
    /// Crossings between adjacent ranks before and after ordering.
    pub crossings: (usize, usize),
}

fn snap_down(v: f32) -> f32 {
    (v / GRID).floor() * GRID
}

fn snap_up(v: f32) -> f32 {
    (v / GRID).ceil() * GRID
}

/// Lay out `nodes` connected by `edges` (`from` drives `to`; duplicates and
/// self-loops are allowed and ignored).
pub fn layout(nodes: &[LayoutNode], edges: &[(usize, usize)]) -> Layout {
    let n = nodes.len();
    // 1. Ranking graph: no self-loops, no duplicates, no edges out of state.
    let mut seen = HashSet::new();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut all: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in edges {
        if a == b || a >= n || b >= n || !seen.insert((a, b)) {
            continue;
        }
        all.push((a, b));
        if nodes[a].role != Role::State && nodes[b].role != Role::Input {
            succ[a].push(b);
        }
    }
    // Break remaining cycles: drop back edges of an iterative DFS.
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); n];
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        while let Some(&mut (v, ref mut i)) = stack.last_mut() {
            if let Some(&w) = succ[v].get(*i) {
                *i += 1;
                match state[w] {
                    0 => {
                        dag[v].push(w);
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => {} // back edge: cut
                    _ => dag[v].push(w),
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, ws) in dag.iter().enumerate() {
        for &w in ws {
            preds[w].push(v);
        }
    }

    // 2. Ranks by longest path (Kahn, in node order).
    let mut indegree: Vec<usize> = preds.iter().map(Vec::len).collect();
    let mut topo: Vec<usize> = (0..n).filter(|&v| indegree[v] == 0).collect();
    let mut head = 0;
    while head < topo.len() {
        let v = topo[head];
        head += 1;
        for &w in &dag[v] {
            indegree[w] -= 1;
            if indegree[w] == 0 {
                topo.push(w);
            }
        }
    }
    debug_assert_eq!(topo.len(), n, "the ranking graph is acyclic");
    let mut rank = vec![0usize; n];
    for &v in &topo {
        if nodes[v].role == Role::Input {
            rank[v] = 0;
            continue;
        }
        rank[v] = preds[v]
            .iter()
            .map(|&p| rank[p] + 1)
            .max()
            .unwrap_or(1)
            .max(1);
    }
    let last = (0..n)
        .filter(|&v| nodes[v].role != Role::Output)
        .map(|v| rank[v])
        .max()
        .unwrap_or(0)
        + 1;
    for v in 0..n {
        if nodes[v].role == Role::Output {
            rank[v] = last;
        }
    }
    // Sources (constants) sit just left of their first consumer. Done in
    // reverse topological order so a chain of sources moves together.
    for &v in topo.iter().rev() {
        if nodes[v].role != Role::Input
            && nodes[v].role != Role::Output
            && preds[v].is_empty()
            && let Some(m) = dag[v].iter().map(|&w| rank[w]).min()
        {
            rank[v] = m.saturating_sub(1).max(1);
        }
    }
    // Drop empty ranks (outputs may leave a gap, e.g. a circuit of ports).
    let mut used: Vec<usize> = rank.clone();
    used.sort_unstable();
    used.dedup();
    let compact: HashMap<usize, usize> = used.iter().enumerate().map(|(i, &r)| (r, i)).collect();
    for r in &mut rank {
        *r = compact[r];
    }
    let layers = used.len();

    // 3. Crossing reduction.
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); layers];
    for v in 0..n {
        columns[rank[v]].push(v);
    }
    // Neighbours over every edge (feedback included) for barycenters.
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in &all {
        if rank[a] < rank[b] {
            down[a].push(b);
            up[b].push(a);
        } else if rank[a] > rank[b] {
            down[b].push(a);
            up[a].push(b);
        }
    }
    let adjacent: Vec<(usize, usize)> = all
        .iter()
        .filter_map(|&(a, b)| match rank[a].abs_diff(rank[b]) {
            1 if rank[a] < rank[b] => Some((a, b)),
            1 => Some((b, a)),
            _ => None,
        })
        .collect();
    let mut pos = vec![0f64; n];
    let place = |columns: &[Vec<usize>], pos: &mut [f64]| {
        for col in columns {
            let mid = (col.len() as f64 - 1.0) / 2.0;
            for (i, &v) in col.iter().enumerate() {
                pos[v] = i as f64 - mid;
            }
        }
    };
    place(&columns, &mut pos);
    let before = count_crossings(&columns, &adjacent, rank.as_slice());
    let mut best = (before, columns.clone());
    for _ in 0..SWEEPS {
        if best.0 == 0 {
            break;
        }
        for downward in [true, false] {
            let order: Vec<usize> = if downward {
                (1..layers).collect()
            } else {
                (0..layers.saturating_sub(1)).rev().collect()
            };
            for r in order {
                let neighbours = if downward { &up } else { &down };
                let mut keyed: Vec<(f64, usize)> = columns[r]
                    .iter()
                    .map(|&v| {
                        let ns = &neighbours[v];
                        let b = if ns.is_empty() {
                            pos[v]
                        } else {
                            ns.iter().map(|&w| pos[w]).sum::<f64>() / ns.len() as f64
                        };
                        (b, v)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
                columns[r] = keyed.into_iter().map(|(_, v)| v).collect();
                let mid = (columns[r].len() as f64 - 1.0) / 2.0;
                for (i, &v) in columns[r].iter().enumerate() {
                    pos[v] = i as f64 - mid;
                }
            }
            let c = count_crossings(&columns, &adjacent, &rank);
            if c < best.0 {
                best = (c, columns.clone());
            }
        }
    }
    let (after, columns) = best;

    // 4. Coordinates on the grid.
    let mut positions = vec![Vec2::ZERO; n];
    let mut x_cursor = 0.0f32;
    for col in &columns {
        let width = col.iter().map(|&v| nodes[v].size.x).fold(0.0f32, f32::max);
        let x = snap_up(x_cursor + width / 2.0);
        x_cursor = x + width / 2.0 + COLUMN_GAP;
        let height: f32 = col.iter().map(|&v| nodes[v].size.y).sum::<f32>()
            + ROW_GAP * col.len().saturating_sub(1) as f32;
        let mut y_cursor = snap_up(height / 2.0);
        for &v in col {
            let h = nodes[v].size.y;
            let y = snap_down(y_cursor - h / 2.0);
            positions[v] = Vec2::new(x, y);
            y_cursor = y - h / 2.0 - ROW_GAP;
        }
    }
    Layout {
        positions,
        ranks: rank,
        columns,
        crossings: (before, after),
    }
}

/// Crossings between edges of adjacent ranks (`edges` go from rank r to
/// r + 1), counted with a Fenwick tree.
fn count_crossings(columns: &[Vec<usize>], edges: &[(usize, usize)], rank: &[usize]) -> usize {
    let mut index: HashMap<usize, usize> = HashMap::new();
    for col in columns {
        for (i, &v) in col.iter().enumerate() {
            index.insert(v, i);
        }
    }
    let mut by_rank: Vec<Vec<(usize, usize)>> = vec![Vec::new(); columns.len()];
    for &(a, b) in edges {
        by_rank[rank[a]].push((index[&a], index[&b]));
    }
    let mut total = 0;
    for (r, list) in by_rank.iter_mut().enumerate() {
        if list.is_empty() {
            continue;
        }
        list.sort_unstable();
        let size = columns.get(r + 1).map_or(0, Vec::len) + 1;
        let mut tree = vec![0usize; size + 1];
        for (seen, &(_, b)) in list.iter().enumerate() {
            // Edges seen so far that end strictly below b cross this one.
            let mut i = b + 1;
            let mut le = 0;
            while i > 0 {
                le += tree[i];
                i -= i.isolate_lowest_one();
            }
            total += seen - le;
            let mut i = b + 1;
            while i <= size {
                tree[i] += 1;
                i += i.isolate_lowest_one();
            }
        }
    }
    total
}

/// Lay out every node of `doc` (its wires give the edges). Node and wire
/// order decide ties, so the same document always gets the same layout.
pub fn layout_circuit(doc: &mut CircuitDocument) {
    let index: HashMap<_, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let nodes: Vec<LayoutNode> = doc.nodes.iter().map(|n| LayoutNode::of(&n.kind)).collect();
    let edges: Vec<(usize, usize)> = doc
        .wires
        .iter()
        .filter_map(|w| Some((*index.get(&w.from.node)?, *index.get(&w.to.node)?)))
        .collect();
    let result = layout(&nodes, &edges);
    for (node, p) in doc.nodes.iter_mut().zip(result.positions) {
        node.x = p.x;
        node.y = p.y;
    }
}

/// Whether any two boxes overlap (touching edges do not count).
pub fn overlaps(boxes: &[(Vec2, Vec2)]) -> Option<(usize, usize)> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| {
        (boxes[a].0.x - boxes[a].1.x / 2.0).total_cmp(&(boxes[b].0.x - boxes[b].1.x / 2.0))
    });
    for (i, &a) in order.iter().enumerate() {
        let (pa, sa) = boxes[a];
        for &b in &order[i + 1..] {
            let (pb, sb) = boxes[b];
            if pb.x - sb.x / 2.0 >= pa.x + sa.x / 2.0 {
                break;
            }
            if (pa.y - pb.y).abs() < (sa.y + sb.y) / 2.0 {
                return Some((a.min(b), a.max(b)));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: Role) -> LayoutNode {
        LayoutNode {
            role,
            size: Vec2::splat(50.0),
        }
    }

    #[test]
    fn ranks_follow_logic_depth_and_cut_state_feedback() {
        // in -> add -> delay -> (back to add), delay -> out; a const into add.
        let nodes = [
            node(Role::Input),
            node(Role::Logic),
            node(Role::State),
            node(Role::Output),
            node(Role::Logic),
        ];
        let edges = [(0, 1), (1, 2), (2, 1), (2, 3), (4, 1)];
        let l = layout(&nodes, &edges);
        assert_eq!(l.ranks, vec![0, 2, 3, 4, 1]);
        assert_eq!(layout(&nodes, &edges), l, "deterministic");
    }

    #[test]
    fn combinational_cycles_do_not_hang() {
        let nodes = [node(Role::Logic), node(Role::Logic), node(Role::Logic)];
        let l = layout(&nodes, &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(l.ranks, vec![0, 1, 2]);
    }

    #[test]
    fn barycenter_removes_a_crossing() {
        // Two inputs wired crosswise to two outputs through two gates.
        let nodes = [
            node(Role::Input),
            node(Role::Input),
            node(Role::Logic),
            node(Role::Logic),
        ];
        let l = layout(&nodes, &[(0, 3), (1, 2)]);
        assert_eq!(l.crossings, (1, 0));
    }

    #[test]
    fn positions_are_on_the_grid_without_overlap() {
        let mut nodes: Vec<LayoutNode> = (0..30).map(|_| node(Role::Logic)).collect();
        nodes.push(LayoutNode {
            role: Role::Output,
            size: Vec2::new(130.0, 70.0),
        });
        let edges: Vec<(usize, usize)> = (0..29).map(|i| (i, (i * 7 + 3) % 31)).collect();
        let l = layout(&nodes, &edges);
        for p in &l.positions {
            assert_eq!(p.x % GRID, 0.0);
            assert_eq!(p.y % GRID, 0.0);
        }
        let boxes: Vec<(Vec2, Vec2)> = l
            .positions
            .iter()
            .zip(&nodes)
            .map(|(p, n)| (*p, n.size))
            .collect();
        assert_eq!(overlaps(&boxes), None);
    }
}
