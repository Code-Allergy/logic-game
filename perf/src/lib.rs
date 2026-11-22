//! Shared inputs of the app-side benchmarks (`benches/`) and of
//! `perf-metrics`: the built-in catalog, gallery designs, and a headless
//! Bevy app like the one `src/sim/tests` uses (simulation runtime and
//! visuals, no window, no renderer).
//!
//! See `docs/performance.md` for how the suites are run (`cargo xtask perf
//! bench`) and how to add one.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use gate_quest::sim::demos::DemoCatalog;
use gate_quest::sim::netlist::design_of;
use gate_quest::sim::{
    EntityWorkCounter, GateIdentity, LoadWorkspaceEvent, SimBridge, SimRuntime,
    SimulationRuntimePlugin, SimulationVisualsPlugin, WorkspaceDocument,
};
use logi_sim::netlist::Design;
use uuid::Uuid;

/// The two 16-bit CPUs of the gallery: `cpu16` (gates) and `cpu16_rtl`
/// (word primitives), running the same program.
pub const CPUS: [&str; 2] = ["cpu16", "cpu16_rtl"];

/// The largest gallery circuits, for frame-time and document benchmarks.
pub const LARGE: [&str; 5] = [
    "cpu16",
    "cpu16_rtl",
    "cpu_fibonacci",
    "screen_text",
    "multiplier",
];

/// `PERF_QUICK` is set (by `cargo xtask perf bench --quick`): benchmarks
/// leave out their largest cases.
pub fn quick() -> bool {
    std::env::var_os("PERF_QUICK").is_some_and(|v| v != "0")
}

/// The built-in demos and gallery, from the embedded files.
pub fn catalog() -> &'static DemoCatalog {
    static CATALOG: OnceLock<DemoCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| DemoCatalog::embedded().expect("the demos load"))
}

/// The expanded workspace of demo or gallery item `id`.
pub fn workspace(id: &str) -> WorkspaceDocument {
    let demo = catalog()
        .get(id)
        .unwrap_or_else(|| panic!("no demo or gallery item {id}"));
    (*demo.workspace).clone()
}

/// The `logi_sim` design of demo or gallery item `id`.
pub fn design(id: &str) -> Design {
    design_of(&workspace(id)).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// The version 6 text of gallery item `id` (as embedded in the game).
pub fn gallery_text(id: &str) -> &'static str {
    let file = format!("{id}.json");
    gate_quest::sim::demos::GALLERY_EMBEDDED
        .iter()
        .find(|(name, _)| *name == file)
        .unwrap_or_else(|| panic!("no gallery file {file}"))
        .1
}

// ─────────────────────────────────────────────────────────────────────────────
// Headless app
// ─────────────────────────────────────────────────────────────────────────────

/// The simulation runtime and visuals in an `App` with no window and no
/// renderer (as `visual_app()` in `src/sim/tests/visuals.rs`).
pub fn headless_app(runtime: SimRuntime) -> App {
    let mut app = App::new();
    app.add_plugins(SimulationRuntimePlugin { runtime })
        .add_plugins(SimulationVisualsPlugin)
        .init_resource::<EntityWorkCounter>();
    app
}

/// Apply pending commands: authoring in `Update`, design sync in
/// `PostUpdate`, probe projection in the next `PreUpdate`.
pub fn settle(app: &mut App) {
    app.update();
    app.update();
}

/// Send `message` and [`settle`].
pub fn send<M: Message>(app: &mut App, message: M) {
    app.world_mut().write_message(message);
    settle(app);
}

/// Open `workspace` (editable) and wait until the engine runs it.
pub fn load(app: &mut App, workspace: WorkspaceDocument) {
    send(
        app,
        LoadWorkspaceEvent {
            name: workspace.root.name.clone(),
            workspace: Arc::new(workspace),
            read_only: false,
            unsaved: false,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !app
        .world_mut()
        .resource_mut::<SimBridge>()
        .wait_until_current(Duration::from_millis(50))
    {
        app.update();
        assert!(Instant::now() < deadline, "the workspace did not load");
    }
    settle(app);
}

/// The gate entity with document id `uuid`.
pub fn gate(app: &mut App, uuid: Uuid) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &GateIdentity)>();
    q.iter(app.world())
        .find(|(_, id)| id.uuid == uuid)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no gate {uuid}"))
}
