//! Frame time of the headless app (simulation runtime and visuals, no
//! window, no renderer): the cost of one `App::update` in the systems the
//! game runs every frame, without drawing.
//!
//! - `frame/idle/<id>`: a large gallery circuit open and paused; nothing
//!   changes, so this is the floor of every frame.
//! - `frame/drag/cpu16`: a part of the `cpu16` moved every frame (as while
//!   dragging), with its wires re-routed incrementally.
//! - `frame/running/<cpu>`: the CPU running unlimited on the engine's
//!   worker thread; each frame projects the latest probe frame into the
//!   gates and wires.

use std::time::Duration;

use bevy::prelude::*;
use criterion::{Criterion, criterion_group, criterion_main};
use gate_quest::sim::{GateMoved, SimBridge, SimRuntime, SimulationConfig};

fn ids() -> Vec<&'static str> {
    if perf::quick() {
        perf::CPUS.to_vec()
    } else {
        perf::LARGE.to_vec()
    }
}

fn idle(c: &mut Criterion) {
    let mut g = c.benchmark_group("frame/idle");
    g.sample_size(20).measurement_time(Duration::from_secs(3));
    for id in ids() {
        let mut app = perf::headless_app(SimRuntime::Threadless);
        perf::load(&mut app, perf::workspace(id));
        g.bench_function(id, |b| b.iter(|| app.update()));
    }
    g.finish();
}

fn drag(c: &mut Criterion) {
    let mut g = c.benchmark_group("frame/drag");
    g.sample_size(20).measurement_time(Duration::from_secs(3));
    let ws = perf::workspace("cpu16");
    // The first non-port part of the root, moved back and forth by two
    // grid cells.
    let node = ws
        .root
        .nodes
        .iter()
        .find(|n| !n.kind.is_port())
        .expect("a part")
        .clone();
    let mut app = perf::headless_app(SimRuntime::Threadless);
    perf::load(&mut app, ws);
    let mut right = false;
    g.bench_function("cpu16", |b| {
        b.iter(|| {
            right = !right;
            let dx = if right { 40.0 } else { 0.0 };
            app.world_mut().write_message(GateMoved {
                gate_uuid: node.id,
                position: Vec2::new(node.x + dx, node.y),
            });
            app.update();
        })
    });
    g.finish();
}

fn running(c: &mut Criterion) {
    let mut g = c.benchmark_group("frame/running");
    g.sample_size(20).measurement_time(Duration::from_secs(3));
    for id in perf::CPUS {
        let mut app = perf::headless_app(SimRuntime::Threaded);
        perf::load(&mut app, perf::workspace(id));
        app.world_mut()
            .resource_mut::<SimulationConfig>()
            .unlimited_tick_rate = true;
        app.world_mut().resource_mut::<SimBridge>().resume();
        perf::settle(&mut app);
        g.bench_function(id, |b| b.iter(|| app.update()));
        app.world_mut().resource_mut::<SimBridge>().pause();
        app.update();
    }
    g.finish();
}

criterion_group!(benches, idle, drag, running);
criterion_main!(benches);
