use bevy::prelude::*;
use bevy::utils::HashMap;
use bevy_egui::{EguiContexts, EguiPlugin};
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
    commands.spawn((Camera2d::default(), MainCamera));
    catppuccin_egui::set_theme(ctx.ctx_mut(), catppuccin_egui::MOCHA);
}

fn main() {
    App::new()
        // Add Bevy's default plugins
        .add_plugins(DefaultPlugins)
        // .add_plugins(bevy_mod_debugdump::CommandLineArgs)
        .add_plugins(EguiPlugin)
        .add_plugins(MainMenuPlugin {
            state: GameState::MainMenu,
        })
        // Setup our game state
        .init_state::<GameState>()
        .add_event::<GateComponentSelectionEvent>()
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
        .add_systems(Startup, initial_startup_system)
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
                update_tickrate_system,
                ui_for_selected_gate,
            )
                .run_if(in_state(GameState::Simulating)),
        )
        // .add_systems(Last, save_on_exit_system)
        // RUN ALWAYS
        .add_systems(Update, (show_settings_menu, settings_menu_button_system))
        .run();
}
