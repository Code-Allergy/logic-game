use bevy::prelude::*;
use bevy::render::RenderApp;
use bevy::render::batching::gpu_preprocessing::{GpuPreprocessingMode, GpuPreprocessingSupport};
use bevy_egui::EguiPlugin;
use bevy_inspector_egui::quick::WorldInspectorPlugin;
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
    .add_plugins(EguiPlugin {
        enable_multipass_for_primary_context: false,
    });
    // Extra debugging UI tools
    #[cfg(debug_assertions)]
    app.add_plugins(WorldInspectorPlugin::new());
    // .add_plugins(bevy_mod_debugdump::CommandLineArgs)
    // Setup our game state
    app.init_state::<GameState>()
        .add_plugins(MainMenuPlugin::new(GameState::MainMenu))
        .add_plugins(SimulationPlugin::new(GameState::Simulating))
        // set colour of the background
        // Startup and update systems
        .add_systems(Startup, initial_startup_system)
        // .add_systems(Last, save_on_exit_system)
        // RUN ALWAYS
        .add_systems(Update, (show_settings_menu, settings_menu_button_system))
        // Run the game only when there is input
        .init_resource::<SettingsMenuOpen>()
        .init_resource::<GameSettings>();

    // TODO: bevy 0.16.0 workaround for crash when starting the game
    app.sub_app_mut(RenderApp)
        .insert_resource(GpuPreprocessingSupport {
            max_supported_mode: GpuPreprocessingMode::None,
        });

    app.run();
}
