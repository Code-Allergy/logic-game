use bevy::{
    camera::ScalingMode, input::mouse::MouseWheel, platform::collections::HashMap, prelude::*,
    window::PrimaryWindow,
};
use bevy_egui::EguiContexts;

pub mod editor;
pub mod misc;
pub mod selection;
pub mod settings;
pub mod settings_overlay;
pub mod sim;

/// MAIN MENU
///
/// This module contains the main menu for the game, which is displayed when the game is first launched.
/// Takes in a target state to be active in
/// Requires the 2dCamera to be setup in the main.rs file
pub mod title;

pub mod temp {
    use bevy::prelude::*;

    use crate::sim::GateComponent;
    pub const CONNECTION_Z_LEVEL: f32 = 10.0;

    // TEMP - lib migration
    #[derive(Message)]
    pub struct GateComponentSelectionEvent {
        pub entity: Entity,
        pub component: GateComponent,
    }
}

pub const SAVE_FILE_NAME: &str = "./simulation.json";
pub const NORMAL_BUTTON: Color = Color::srgb(0.15, 0.15, 0.15);
pub const HOVERED_BUTTON: Color = Color::srgb(0.25, 0.25, 0.25);
pub const PRESSED_BUTTON: Color = Color::srgb(0.35, 0.75, 0.35);
pub const SIMULATION_TICK_RATE: f64 = 10.0; // 10 ticks per second
pub const GAME_TITLE: &str = "GateQuest";
pub const GAME_WEB_TAG: &str = "#gatequest-canvas";

#[derive(Component)]
pub struct MainCamera;

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

/* First system run before any system state is ran */
pub fn initial_startup_system(mut commands: Commands, mut _ctx: EguiContexts) {
    // Set 2D camera
    commands.spawn((
        Camera2d,
        MainCamera,
        // Transform::from_xyz(0.0, 0.0, 500.0), // Z must be positive so it looks toward origin
    ));
    // catppuccin_egui::set_theme(ctx.ctx_mut(), catppuccin_egui::MOCHA);
}

// Refactor and simplify custom projections #
// Areas:Rendering.
// PRs:#17063
// Read the primary window’s logical height (in pixels) and make 1 world unit == 1 px
pub fn configure_camera_scaling(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut Projection, With<MainCamera>>,
) -> Result {
    let window = windows.single()?; // should exist exactly once
    let height = window.height(); // logical height in pixels
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
            // scale >1.0 zooms out; <1.0 zooms in
            ortho.scale = (ortho.scale * (1.0 - ev.y * 0.1)).clamp(0.1, 10.0);
        }
    }

    Ok(())
}
