use bevy::prelude::*;
use bevy_egui::EguiPlugin;

#[cfg(debug_assertions)]
use bevy_inspector_egui::quick::WorldInspectorPlugin;

use gate_quest::editor::EditorPlugin;
use gate_quest::settings_overlay::{
    SettingsMenuOpen, settings_menu_button_system, show_settings_menu,
};

use gate_quest::title::MainMenuPlugin;
use gate_quest::{
    GAME_TITLE, GAME_WEB_TAG, GameSettings, GameState, initial_startup_system, sim::*,
};

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
    .add_plugins(EguiPlugin);
    // Extra debugging UI tools
    #[cfg(debug_assertions)]
    app.add_plugins(WorldInspectorPlugin::new());
    // #[cfg(debug_assertions)]
    // app.add_plugins(bevy_mod_debugdump::CommandLineArgs);

    app.add_plugins(MainMenuPlugin {
        state: GameState::MainMenu,
    })
    .add_plugins(EditorPlugin)
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
    .run();
}
