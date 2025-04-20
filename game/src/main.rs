use bevy::input::mouse::MouseWheel;
use bevy::utils::HashMap;
use bevy::window::PrimaryWindow;
use bevy::{prelude::*, render::camera::ScalingMode};
use bevy_egui::{EguiContexts, EguiPlugin};
use bevy_inspector_egui::quick::WorldInspectorPlugin;
use main_menu::MainMenuPlugin;

mod systems;
use systems::*;

mod main_menu;

mod sim;
use sim::*;

mod settings_overlay;
use settings_overlay::*;

mod spatial_grid;
use spatial_grid::SpatialGrid;

pub const NORMAL_BUTTON: Color = Color::srgb(0.15, 0.15, 0.15);
pub const HOVERED_BUTTON: Color = Color::srgb(0.25, 0.25, 0.25);
pub const PRESSED_BUTTON: Color = Color::srgb(0.35, 0.75, 0.35);

const SIMULATION_TICK_RATE: f64 = 10.0; // 10 ticks per second
pub const CONNECTION_Z_LEVEL: f32 = 10.0;

pub const SAVE_FILE_NAME: &str = "./simulation.json";

pub const GAME_TITLE: &str = "GateQuest";
pub const GAME_WEB_TAG: &str = "#gatequest";

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

#[derive(Event)]
pub struct GateComponentSelectionEvent {
    pub entity: Entity,
    pub component: GateComponent,
}

/* First system run before any system state is ran */
pub fn initial_startup_system(mut commands: Commands, mut ctx: EguiContexts) {
    // Set 2D camera
    commands.spawn((
        Camera2d,
        MainCamera,
        // Transform::from_xyz(0.0, 0.0, 500.0), // Z must be positive so it looks toward origin
    ));
    catppuccin_egui::set_theme(ctx.ctx_mut(), catppuccin_egui::MOCHA);
}

/// Read the primary window’s logical height (in pixels) and make 1 world unit == 1 px
fn configure_camera_scaling(
    windows: Query<&Window, With<PrimaryWindow>>, // ← query the window component :contentReference[oaicite:2]{index=2}
    mut cameras: Query<&mut OrthographicProjection, With<MainCamera>>,
) {
    let window = windows.single(); // should exist exactly once
    let height = window.height(); // logical height in pixels :contentReference[oaicite:3]{index=3}

    let mut proj = cameras.single_mut();
    proj.scaling_mode = ScalingMode::FixedVertical {
        viewport_height: height, // map 1 world unit → 1 px
    };
}

fn main() {
    App::new()
        // Add Bevy's default plugins
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: GAME_TITLE.to_string(),
                canvas: Some(GAME_WEB_TAG.to_owned()),
                fit_canvas_to_parent: true,
                prevent_default_event_handling: true, // for WASM build
                ..default()
            }),
            ..default()
        }))
        // .add_plugins(bevy_mod_debugdump::CommandLineArgs)
        .add_plugins(EguiPlugin)
        // Extra debugging UI tools
        .add_plugins(WorldInspectorPlugin::new())
        .add_plugins(MainMenuPlugin {
            state: GameState::MainMenu,
        })
        // Setup our game state
        .init_state::<GameState>()
        .add_event::<GateComponentSelectionEvent>()
        .insert_resource(ClearColor(Color::srgba(0.050, 0.229, 0.793, 1.000)))
        // Run the game only when there is input
        .insert_resource(SettingsMenuOpen(false))
        .insert_resource(GameSettings::default())
        // Insert our simulation as a resource
        .insert_resource(Simulation::default())
        .insert_resource(SimulationConfig {
            tick_rate: SIMULATION_TICK_RATE,
        })
        .insert_resource(SelectedGate::default())
        .insert_resource(SpatialGrid::new(100.0))
        // TIMESTEP
        .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE)) // Set the tick rate of the simulation
        // Startup and update systems
        .add_systems(
            Startup,
            (
                initial_startup_system.before(configure_camera_scaling),
                configure_camera_scaling,
            ),
        )
        /* GAME STATE */
        .add_systems(
            OnEnter(GameState::Simulating),
            (
                draw_gates_system,
                draw_connections_system.after(draw_gates_system),
                rebuild_spatial_grid_system,
            ),
        )
        .add_systems(
            FixedUpdate,
            (simulation_update_system).run_if(in_state(GameState::Simulating)),
        )
        // Update systems
        .add_systems(
            Update,
            (
                update_input_colours_system,
                gate_selection_system.before(drag_selected_system),
                drag_selected_system,
                update_connections_system,
                update_spatial_grid_system,
                update_inputs_on_left_click,
                update_selected_gate,
                simulation_config_ui_system,
                // mouse_selection_input,
                // finalize_selection,
                zoom_camera,
                update_tickrate_system,
                ui_for_selected_gate,
            )
                .run_if(in_state(GameState::Simulating)),
        )
        // .add_systems(Last, save_on_exit_system)
        // RUN ALWAYS
        .add_systems(Update, (show_settings_menu, settings_menu_button_system))
        // .add_observer(down_handler)
        // .add_observer(up_handler)
        // .add_observer(drag_start_handler)
        // .add_observer(drag_handler)
        // .add_observer(drag_end_handler)
        .run();
}

fn zoom_camera(
    mut scroll_evr: EventReader<MouseWheel>,
    mut cameras: Query<&mut OrthographicProjection, With<MainCamera>>,
) {
    let mut proj = cameras.single_mut();
    for ev in scroll_evr.read() {
        // scale >1.0 zooms out; <1.0 zooms in
        proj.scale = (proj.scale * (1.0 - ev.y * 0.1)).clamp(0.1, 10.0);
    }
}

fn down_handler(_: Trigger<Pointer<Down>>) {
    info!("Down");
}

fn up_handler(_: Trigger<Pointer<Up>>) {
    info!("Up");
}

fn drag_start_handler(_: Trigger<Pointer<DragStart>>) {
    info!("DragStart");
}

fn drag_handler(_: Trigger<Pointer<Drag>>) {
    info!("Drag");
}

fn drag_end_handler(_: Trigger<Pointer<DragEnd>>) {
    info!("DragEnd");
}
