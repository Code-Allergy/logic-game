//! `gate_quest` library root.
//!
//! Exposes all public modules and shared constants used by `main.rs`.

use bevy::{
    camera::ScalingMode, input::mouse::MouseWheel, platform::collections::HashMap, prelude::*,
    window::PrimaryWindow,
};
use bevy_egui::EguiContexts;

pub mod editor;
pub mod fonts;
pub mod selection;
pub mod settings;
pub mod settings_overlay;
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

#[derive(Resource, Default)]
pub struct UiPointerState {
    pub over_egui: bool,
}

#[derive(States, Debug, Clone, Eq, PartialEq, Hash, Default)]
pub enum GameState {
    #[default]
    MainMenu,
    Loading,
    Simulating,
    Paused,
    Settings,
}

#[derive(Resource)]
pub struct GameSettings {
    pub resolution: (u32, u32),
    pub fullscreen: bool,
    pub vsync: bool,
    pub volume: f32,
    pub controls: HashMap<String, String>,
}

impl Default for GameSettings {
    fn default() -> Self {
        GameSettings {
            resolution: (1920, 1080),
            fullscreen: false,
            vsync: true,
            volume: 0.5,
            controls: HashMap::new(),
        }
    }
}

pub fn initial_startup_system(mut commands: Commands, mut _ctx: EguiContexts) {
    commands.spawn((Camera2d, MainCamera, Transform::from_xyz(0.0, 0.0, 1000.0)));
}

pub fn update_ui_pointer_state(mut state: ResMut<UiPointerState>, mut contexts: EguiContexts) {
    state.over_egui = matches!(contexts.ctx_mut(), Ok(ctx) if ctx.is_pointer_over_egui());
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

pub fn zoom_camera(
    mut scroll_evr: MessageReader<MouseWheel>,
    mut cameras: Query<&mut Projection, With<MainCamera>>,
) -> Result {
    let mut proj = cameras.single_mut()?;
    if let Projection::Orthographic(ortho) = proj.as_mut() {
        for ev in scroll_evr.read() {
            ortho.scale = (ortho.scale * (1.0 - ev.y * 0.1)).clamp(0.1, 10.0);
        }
    }
    Ok(())
}
