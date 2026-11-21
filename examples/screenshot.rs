//! Open a demo or gallery item in the full app, run it for a few ticks and
//! save a screenshot of the window (a development tool for checking the
//! look, e.g. under a virtual framebuffer).
//!
//! ```sh
//! cargo run --example screenshot -- counter out.png            # day
//! cargo run --example screenshot -- counter out.png --night
//! cargo run --example screenshot -- traffic_light out.png --ticks 40 --zoom 0.8
//! cargo run --example screenshot -- cpu out.png --definition alu8   # a definition
//! cargo run --example screenshot -- path/to/file.json out.png       # any version 6 file
//! ```
//!
//! The first argument is a demo or gallery id, or the path of a version 6
//! file. `--definition NAME` opens the definition of that name (a circuit
//! of the file) instead of the root circuit. `--fit` frames the circuit in
//! the canvas right of the side panel (`--zoom` scales that framing).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;

use gate_quest::editor::EditorPlugin;
use gate_quest::sim::{
    CircuitDocument, GateIdentity, LoadWorkspaceEvent, OpenDefinitionView, SimBridge,
    SimulationPlugin, WorkspaceDocument, demos, source,
};
use gate_quest::temp::GateComponentSelectionEvent;
use gate_quest::theme::{Theme, ThemeMode, ThemePlugin};
use gate_quest::ui_style::WorkshopUiPlugin;
use gate_quest::{
    GameState, MainCamera, UiPointerState, configure_camera_scaling, initial_startup_system,
    update_ui_pointer_state,
};

#[derive(Resource)]
struct Shot {
    id: String,
    out: PathBuf,
    night: bool,
    ticks: u64,
    zoom: Option<f32>,
    definition: Option<String>,
    fit: bool,
    frame: u32,
}

type Circuits = Vec<(String, Arc<CircuitDocument>)>;

/// The workspace and the circuits by name of a demo or gallery id, or of
/// a version 6 file.
fn load(id: &str) -> (String, Arc<WorkspaceDocument>, Circuits) {
    let catalog = demos::catalog().expect("the demos load");
    let path = Path::new(id);
    if path.is_file() {
        let text = std::fs::read_to_string(path).expect("read the file");
        let file = source::parse(&text).expect("a version 6 file");
        let stem = path
            .file_stem()
            .map_or("file".into(), |s| s.to_string_lossy().into_owned());
        let expansion = source::expand(&file, &format!("screenshot/{stem}"), &catalog.externals())
            .expect("the file expands");
        (stem, Arc::new(expansion.workspace), expansion.circuits)
    } else {
        let demo = catalog.get(id).expect("a demo or gallery id, or a file");
        (
            demo.title.clone(),
            Arc::clone(&demo.workspace),
            demo.circuits.clone(),
        )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "counter".into());
    let out = PathBuf::from(args.next().unwrap_or_else(|| "screenshot.png".into()));
    let (mut night, mut ticks, mut zoom, mut definition, mut fit) = (false, 12, None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--night" => night = true,
            "--ticks" => ticks = args.next().and_then(|t| t.parse().ok()).unwrap_or(ticks),
            "--zoom" => zoom = args.next().and_then(|z| z.parse().ok()),
            "--definition" => definition = args.next(),
            "--fit" => fit = true,
            other => panic!("unknown argument {other}"),
        }
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(1600, 1000),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((EguiPlugin::default(), WorkshopUiPlugin, ThemePlugin))
        .add_plugins(SimulationPlugin::new(GameState::Simulating))
        .add_plugins(EditorPlugin)
        .insert_state(GameState::Simulating)
        .insert_resource(UiPointerState::default())
        .insert_resource(ClearColor(Theme::default().palette.bench))
        .add_message::<GateComponentSelectionEvent>()
        .insert_resource(Shot {
            id,
            out,
            night,
            ticks,
            zoom,
            definition,
            fit,
            frame: 0,
        })
        .add_systems(
            Startup,
            (initial_startup_system, configure_camera_scaling).chain(),
        )
        .add_systems(bevy_egui::EguiPrimaryContextPass, update_ui_pointer_state)
        .add_systems(Update, drive)
        .run();
}

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    mut shot: ResMut<Shot>,
    mut theme: ResMut<Theme>,
    bridge: Res<SimBridge>,
    mut load_workspace: MessageWriter<LoadWorkspaceEvent>,
    mut open: MessageWriter<OpenDefinitionView>,
    mut circuits: Local<Circuits>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    gates: Query<&Transform, (With<GateIdentity>, Without<MainCamera>)>,
    windows: Query<&Window>,
    mut exit: MessageWriter<AppExit>,
) {
    shot.frame += 1;
    match shot.frame {
        5 => {
            let (name, workspace, all) = load(&shot.id);
            *circuits = all;
            load_workspace.write(LoadWorkspaceEvent {
                name,
                workspace,
                read_only: true,
            });
            if shot.night {
                theme.set(ThemeMode::Night);
            }
        }
        20 => bridge.step(shot.ticks),
        30 => {
            if let Some(name) = &shot.definition {
                let (_, doc) = circuits
                    .iter()
                    .find(|(n, _)| n == name)
                    .unwrap_or_else(|| panic!("no circuit {name}"));
                open.write(OpenDefinitionView { definition: doc.id });
            }
        }
        40 => {
            let Ok((mut projection, mut camera)) = cameras.single_mut() else {
                return;
            };
            let Projection::Orthographic(ortho) = projection.as_mut() else {
                return;
            };
            if shot.fit
                && let Ok(window) = windows.single()
                && !gates.is_empty()
            {
                // The canvas right of the side panel (about 500 pixels).
                let (lo, hi) = gates.iter().fold(
                    (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
                    |(lo, hi), t| {
                        (
                            lo.min(t.translation.truncate()),
                            hi.max(t.translation.truncate()),
                        )
                    },
                );
                let (lo, hi) = (lo - Vec2::splat(60.0), hi + Vec2::splat(60.0));
                let panel = 500.0;
                let room = Vec2::new(window.width() - panel, window.height() - 120.0);
                let scale = ((hi - lo) / room).max_element() * shot.zoom.unwrap_or(1.0);
                ortho.scale = scale;
                let centre = (lo + hi) * 0.5;
                camera.translation.x = centre.x - panel * 0.5 * scale;
                camera.translation.y = centre.y + 40.0 * scale;
            } else if let Some(zoom) = shot.zoom {
                ortho.scale = zoom;
            }
        }
        70 => {
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(shot.out.clone()));
        }
        100 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
