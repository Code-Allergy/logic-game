//! The MCP server's document tools on a large workspace (`src/mcp`,
//! `docs/mcp-server.md`): what one call costs when the circuit has ten
//! thousand parts.
//!
//! - `mcp/build`: one `edit` request that makes the circuit (three
//!   arrays of gates, chained and wired to buses, laid out).
//! - `mcp/edit`: a small `edit` (one part, one wire) of that circuit.
//! - `mcp/outline`: its `view` outline (200 lines).
//! - `mcp/make_chip`: 1,000 of its parts (a third from each array) made a
//!   chip.
//! - `mcp/expand`: a 64-bit adder expanded to gates.
//! - `mcp/test`: a stored formula test of 200 samples on it.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use gate_quest::mcp::batch::apply;
use gate_quest::mcp::{testing, view};
use gate_quest::sim::source::Externals;
use gate_quest::sim::{CircuitDocument, WorkspaceDocument};
use serde_json::{Value, json};

/// Gates per array; three arrays and the ports make the circuit.
const N: u32 = 3333;

fn empty() -> WorkspaceDocument {
    let mut root = CircuitDocument::new("bench");
    root.id = uuid::Uuid::from_u128(11);
    WorkspaceDocument::new(root)
}

fn build_request() -> Value {
    json!({"ops": [
        {"add": [
            {"name": "A", "type": "Input", "w": 32},
            {"name": "Go", "type": "Input"},
            {"name": "x", "type": "Xor", "count": N},
            {"name": "a", "type": "And", "count": N},
            {"name": "o", "type": "Or", "count": N},
            {"name": "Y", "type": "Output", "w": 32},
            {"name": "sum", "type": "Add", "w": 64},
            {"name": "P", "type": "Input", "w": 64},
            {"name": "Q", "type": "Input", "w": 64},
            {"name": "S", "type": "Output", "w": 64}
        ]},
        {"wire": [
            ["Go", "x[0].a", format!("a[0..{N}].a")],
            [format!("x[0..{}].y", N - 1), format!("x[1..{N}].a")],
            ["A", "x[0..32].b"],
            [format!("x[0..{N}].y"), format!("a[0..{N}].b"), format!("o[0..{N}].a")],
            [format!("a[0..{N}].y"), format!("o[0..{N}].b")],
            ["o[0..32].y", "Y"],
            ["P", "sum.a"], ["Q", "sum.b"], ["sum.sum", "S"]
        ]},
        {"tests": [{"name": "sums", "formula": {"expect": {"S": "P + Q"}, "samples": 200}}]}
    ]})
}

fn edit(ws: &WorkspaceDocument, request: &Value) -> WorkspaceDocument {
    apply(ws, request, &Externals::default())
        .unwrap_or_else(|e| panic!("{e}"))
        .0
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("mcp");
    g.sample_size(10)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(if perf::quick() { 2 } else { 6 }));
    let build = build_request();
    let ws = edit(&empty(), &build);
    assert!(ws.root.nodes.len() > 10_000);

    g.bench_function("build", |b| b.iter(|| black_box(edit(&empty(), &build))));
    let small =
        json!({"ops": [{"add": [{"name": "n", "type": "Not"}]}, {"wire": [["o[7]", "n"]]}]});
    g.bench_function("edit", |b| b.iter(|| black_box(edit(&ws, &small))));
    g.bench_function("outline", |b| {
        b.iter(|| black_box(view::outline(&ws, &ws.root, None, view::DEFAULT_LIMIT)))
    });
    // The first 334 gates of each array: a thousand parts, a few ports (a
    // chip has at most 256 on a side, `chip::MAX_CHIP_PINS`).
    let chip = json!({"ops": [{"make_chip": {
        "parts": ["x[0..334]", "a[0..334]", "o[0..334]"], "name": "block"}}]});
    g.bench_function("make_chip", |b| b.iter(|| black_box(edit(&ws, &chip))));
    let expand = json!({"ops": [{"expand": ["sum"]}]});
    g.bench_function("expand", |b| b.iter(|| black_box(edit(&ws, &expand))));
    g.bench_function("test", |b| {
        b.iter(|| {
            let report = testing::run_suite(&ws, Some("main"), None).unwrap();
            assert_eq!(report.failed(), 0);
            black_box(report)
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
