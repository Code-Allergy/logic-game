//! Open a demo or gallery item in the full app, run it for a few ticks and
//! save a screenshot of the window (a development tool for checking the
//! look, e.g. under a virtual framebuffer).
//!
//! ```sh
//! cargo run --example screenshot -- counter out.png            # day
//! cargo run --example screenshot -- counter out.png --night
//! cargo run --example screenshot -- traffic_light out.png --ticks 40 --zoom 0.8
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;

use gate_quest::editor::EditorPlugin;
use gate_quest::sim::{LoadWorkspaceEvent, SimBridge, SimulationPlugin, demos};
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
    frame: u32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "counter".into());
    let out = PathBuf::from(args.next().unwrap_or_else(|| "screenshot.png".into()));
    let (mut night, mut ticks, mut zoom) = (false, 12, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--night" => night = true,
            "--ticks" => ticks = args.next().and_then(|t| t.parse().ok()).unwrap_or(ticks),
            "--zoom" => zoom = args.next().and_then(|z| z.parse().ok()),
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

fn drive(
    mut commands: Commands,
    mut shot: ResMut<Shot>,
    mut theme: ResMut<Theme>,
    bridge: Res<SimBridge>,
    mut load: MessageWriter<LoadWorkspaceEvent>,
    mut cameras: Query<&mut Projection, With<MainCamera>>,
    mut exit: MessageWriter<AppExit>,
) {
    shot.frame += 1;
    match shot.frame {
        5 => {
            let catalog = demos::catalog().expect("the demos load");
            let demo = catalog.get(&shot.id).expect("a demo or gallery id");
            load.write(LoadWorkspaceEvent {
                name: demo.title.clone(),
                workspace: Arc::clone(&demo.workspace),
                read_only: true,
            });
            if shot.night {
                theme.set(ThemeMode::Night);
            }
        }
        20 => bridge.step(shot.ticks),
        40 => {
            if let (Some(zoom), Ok(mut projection)) = (shot.zoom, cameras.single_mut())
                && let Projection::Orthographic(ortho) = projection.as_mut()
            {
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
