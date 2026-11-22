//! `gate_quest` library root.
//!
//! Exposes all public modules and shared constants used by `main.rs`.

use bevy::{camera::ScalingMode, prelude::*, window::PrimaryWindow};
use bevy_egui::EguiContexts;

pub mod camera;
pub mod editor;
pub mod fonts;
pub mod mcp;
pub mod settings;
pub mod sim;
pub mod theme;
pub mod title;
pub mod ui_style;

pub mod temp {
    use bevy::prelude::*;

    pub const CONNECTION_Z_LEVEL: f32 = -10.0;

    /// A gate was clicked.
    #[derive(Message, Clone)]
    pub struct GateComponentSelectionEvent {
        pub entity: Entity,
    }
}

pub const WORKSPACE_DIR: &str = "./workspaces";
pub const DEFAULT_WORKSPACE_EXT: &str = ".json";
pub const SIMULATION_TICK_RATE: f64 = 10.0;
pub const GAME_TITLE: &str = "GateQuest";
pub const GAME_WEB_TAG: &str = "#gatequest-canvas";

#[derive(Component)]
pub struct MainCamera;

#[derive(Resource, Default, PartialEq)]
pub struct UiPointerState {
    pub over_egui: bool,
    /// A text field has the keyboard: canvas keys (arrows) are ignored.
    pub wants_keyboard: bool,
}

#[derive(States, Debug, Clone, Eq, PartialEq, Hash, Default)]
pub enum GameState {
    #[default]
    MainMenu,
    Loading,
    Simulating,
    /// The demo map: the curriculum road, a level's demos and the gallery
    /// (`editor::demo_map`). The circuit stays loaded, hidden and paused.
    DemoMap,
    Paused,
    Settings,
}

pub fn initial_startup_system(mut commands: Commands, mut _ctx: EguiContexts) {
    commands.spawn((Camera2d, MainCamera, Transform::from_xyz(0.0, 0.0, 1000.0)));
}

pub fn update_ui_pointer_state(mut state: ResMut<UiPointerState>, mut contexts: EguiContexts) {
    let (over_egui, wants_keyboard) = match contexts.ctx_mut() {
        Ok(ctx) => (ctx.is_pointer_over_egui(), ctx.egui_wants_keyboard_input()),
        Err(_) => (false, false),
    };
    state.set_if_neq(UiPointerState {
        over_egui,
        wants_keyboard,
    });
}

pub fn configure_camera_scaling(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut Projection, With<MainCamera>>,
) -> Result {
    let window = windows.single()?;
    let height = window.height().max(1.0);
    let mut projection = cameras.single_mut()?;

    if let Projection::Orthographic(ortho) = projection.as_mut() {
        ortho.scaling_mode = ScalingMode::FixedVertical {
            viewport_height: height,
        };
    }
    Ok(())
}

pub use camera::{key_pan_camera, pan_camera, zoom_camera};
