//! Open a demo or gallery item in the full app, run it for a few ticks and
//! save a screenshot of the window (a development tool for checking the
//! look, e.g. under a virtual framebuffer).
//!
//! ```sh
//! cargo run --example screenshot -- counter out.png            # day
//! cargo run --example screenshot -- counter out.png --night
//! cargo run --example screenshot -- traffic_light out.png --ticks 40 --zoom 0.8
//! cargo run --example screenshot -- counter out.png --size 1280x720 --select Count
//! cargo run --example screenshot -- counter out.png --library   # Library drawer open
//! ```
//!
//! `--size WxH` sets the window size (default 1600x1000); `--select LABEL`
//! selects the first part with that label, so the inspector shows it;
//! `--set LABEL=VALUE` drives an input before the ticks run; `--tab NAME`
//! opens a parts-bar tab (`logic`, `memory`, `chips`, ...).

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;

use gate_quest::editor::{EditorPlugin, EditorState, PartTab};
use gate_quest::sim::{
    GateLabel, LoadWorkspaceEvent, SelectedGate, SetInputValue, SimBridge, SimulationPlugin, demos,
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
    select: Option<String>,
    library: bool,
    set: Vec<(String, u64)>,
    tab: Option<PartTab>,
    frame: u32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "counter".into());
    let out = PathBuf::from(args.next().unwrap_or_else(|| "screenshot.png".into()));
    let (mut night, mut ticks, mut zoom) = (false, 12, None);
    let (mut size, mut select, mut library) = ((1600, 1000), None, false);
    let mut set = Vec::new();
    let mut tab = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--night" => night = true,
            "--library" => library = true,
            "--select" => select = args.next(),
            "--tab" => {
                let name = args.next().unwrap_or_default().to_lowercase();
                tab = PartTab::ALL
                    .into_iter()
                    .find(|t| t.title().to_lowercase().starts_with(&name));
            }
            "--set" => {
                let spec = args.next().unwrap_or_default();
                let parsed = spec
                    .split_once('=')
                    .and_then(|(l, v)| Some((l.to_string(), v.parse().ok()?)));
                set.push(parsed.unwrap_or_else(|| panic!("--set wants LABEL=VALUE, got {spec}")));
            }
            "--size" => {
                let spec = args.next().unwrap_or_default();
                let parsed = spec
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                size = parsed.unwrap_or_else(|| panic!("--size wants WxH, got {spec}"));
            }
            "--ticks" => ticks = args.next().and_then(|t| t.parse().ok()).unwrap_or(ticks),
            "--zoom" => zoom = args.next().and_then(|z| z.parse().ok()),
            other => panic!("unknown argument {other}"),
        }
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(size.0, size.1),
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
            select,
            library,
            set,
            tab,
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
    mut selected: ResMut<SelectedGate>,
    mut editor: ResMut<EditorState>,
    labels: Query<(Entity, &GateLabel)>,
    mut inputs: MessageWriter<SetInputValue>,
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
        15 => {
            for (label, value) in &shot.set {
                for (entity, l) in &labels {
                    if &l.value == label {
                        inputs.write(SetInputValue {
                            entity,
                            value: *value,
                        });
                    }
                }
            }
        }
        20 => bridge.step(shot.ticks),
        30 => {
            if let Some(label) = &shot.select {
                let found = labels.iter().find(|(_, l)| &l.value == label);
                selected.0 = Some(found.expect("a part with that label").0);
            }
            editor.library_open = shot.library;
            if let Some(tab) = shot.tab {
                editor.part_tab = tab;
            }
        }
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
