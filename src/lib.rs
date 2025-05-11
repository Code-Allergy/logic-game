use bevy::{platform::collections::HashMap, prelude::*};
use bevy_egui::EguiContexts;

pub mod editor;
pub mod misc;
pub mod selection;
pub mod settings_overlay;
pub mod sim;
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

// TODO 0.16 update
// Refactor and simplify custom projections #
// Areas:Rendering.
// PRs:#17063
// Read the primary window’s logical height (in pixels) and make 1 world unit == 1 px
// pub fn configure_camera_scaling(
//     windows: Query<&Window, With<PrimaryWindow>>, // ← query the window component :contentReference[oaicite:2]{index=2}
//     mut cameras: Query<&mut OrthographicProjection, With<MainCamera>>,
// ) {
//     let window = windows.single(); // should exist exactly once
//     let height = window.height(); // logical height in pixels :contentReference[oaicite:3]{index=3}

//     let mut proj = cameras.single_mut();
//     proj.scaling_mode = ScalingMode::FixedVertical {
//         viewport_height: height, // map 1 world unit → 1 px
//     };
// }

// pub fn zoom_camera(
//     mut scroll_evr: EventReader<MouseWheel>,
//     mut cameras: Query<&mut OrthographicProjection, With<MainCamera>>,
// ) {
//     let mut proj = cameras.single_mut();
//     for ev in scroll_evr.read() {
//         // scale >1.0 zooms out; <1.0 zooms in
//         proj.scale = (proj.scale * (1.0 - ev.y * 0.1)).clamp(0.1, 10.0);
//     }
// }
