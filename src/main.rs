use bevy::prelude::*;
use bevy_egui::EguiPlugin;

#[cfg(debug_assertions)]
use bevy_inspector_egui::quick::WorldInspectorPlugin;

use gate_quest::configure_camera_scaling;
use gate_quest::editor::EditorPlugin;
use gate_quest::selection::SelectionPlugin;
use gate_quest::settings_overlay::{
    SettingsMenuOpen, settings_menu_button_system, show_settings_menu,
};

use gate_quest::sim::SimulationPlugin;
use gate_quest::sim::spatial_grid::SpatialGrid;
use gate_quest::temp::GateComponentSelectionEvent;

use gate_quest::title::MainMenuPlugin;
use gate_quest::{GAME_TITLE, GAME_WEB_TAG, GameSettings, GameState, initial_startup_system};

fn main() {
    let mut app = App::new();
    // Add Bevy's default plugins
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            // Other platforms window title
            title: GAME_TITLE.to_string(),
            // for WASM build
            canvas: Some(GAME_WEB_TAG.to_owned()),
            fit_canvas_to_parent: true,
            prevent_default_event_handling: true,
            ..default()
        }),
        ..default()
    }))
    .add_plugins(EguiPlugin::default());

    // Extra debugging UI tools
    #[cfg(debug_assertions)]
    app.add_plugins(WorldInspectorPlugin::new());

    app
        // Add game plugins
        .add_plugins(MainMenuPlugin {
            state: GameState::MainMenu,
        })
        .add_plugins(EditorPlugin)
        .add_plugins(SelectionPlugin)
        .add_plugins(SimulationPlugin::new(GameState::MainMenu)) // Core simulation logic
        // Setup game state
        .init_state::<GameState>()
        .add_message::<GateComponentSelectionEvent>()
        .insert_resource(ClearColor(Color::srgba(0.050, 0.229, 0.793, 1.000)))
        // Game settings
        .insert_resource(SettingsMenuOpen(false))
        .insert_resource(GameSettings::default())
        // Spatial grid for optimized queries
        .insert_resource(SpatialGrid::new(100.0))
        // Camera setup
        .add_systems(
            Startup,
            (
                initial_startup_system.before(configure_camera_scaling),
                configure_camera_scaling,
            ),
        )
        // Spatial grid system
        // .add_systems(OnEnter(GameState::Simulating), rebuild_spatial_grid_system)
        // Systems that need to run during simulation
        // .add_systems(
        //     Update,
        //     (
        //         // Selection and interaction systems
        //         gate_selection_system.before(drag_selected_system),
        //         drag_selected_system,
        //         update_spatial_grid_system,
        //         update_inputs_on_left_click,
        //         // Camera control
        //         zoom_camera,
        //     )
        //         .run_if(in_state(GameState::Simulating)),
        // )
        // Always-running UI systems
        .add_systems(Update, (show_settings_menu, settings_menu_button_system))
        .run();
}
