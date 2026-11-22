use std::env;

use bevy::prelude::*;
use bevy_egui::EguiPlugin;

#[cfg(feature = "inspector")]
use bevy_inspector_egui::quick::WorldInspectorPlugin;

use gate_quest::configure_camera_scaling;
use gate_quest::editor::EditorPlugin;
use gate_quest::selection::SelectionPlugin;
use gate_quest::settings_overlay::{
    SettingsMenuOpen, settings_menu_button_system, show_settings_menu,
};
use gate_quest::sim::SimulationPlugin;
use gate_quest::temp::GateComponentSelectionEvent;
use gate_quest::theme::{Theme, ThemePlugin};
use gate_quest::title::MainMenuPlugin;
use gate_quest::ui_style::WorkshopUiPlugin;
use gate_quest::{
    GAME_TITLE, GAME_WEB_TAG, GameSettings, GameState, UiPointerState, initial_startup_system,
    update_ui_pointer_state,
};
use gate_quest::{pan_camera, zoom_camera};

fn main() {
    let disable_egui = env::var_os("GATEQUEST_DISABLE_EGUI").is_some();
    let disable_editor = env::var_os("GATEQUEST_DISABLE_EDITOR").is_some();
    let disable_selection = env::var_os("GATEQUEST_DISABLE_SELECTION").is_some();

    let mut app = App::new();

    // The game has no gamepad input. Without gilrs, an unplugged pad with
    // rumble can't flood the log with force-feedback errors every 50 ms.
    let mut app = app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: GAME_TITLE.to_string(),
                    canvas: Some(GAME_WEB_TAG.to_owned()),
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: true,
                    ..default()
                }),
                ..default()
            })
            .disable::<bevy::gilrs::GilrsPlugin>(),
    );

    if !disable_egui {
        app = app.add_plugins((EguiPlugin::default(), WorkshopUiPlugin));
    }

    #[cfg(feature = "inspector")]
    if !disable_egui {
        app = app.add_plugins(WorldInspectorPlugin::new());
    }

    app.add_plugins(ThemePlugin)
        .add_plugins(MainMenuPlugin {
            state: GameState::MainMenu,
        })
        .add_plugins(SimulationPlugin::new(GameState::Simulating));

    if !disable_editor {
        app = app.add_plugins(EditorPlugin);
    }
    if !disable_selection {
        app = app.add_plugins(SelectionPlugin);
    }

    app = app
        .init_state::<GameState>()
        .insert_resource(UiPointerState::default())
        .add_message::<GateComponentSelectionEvent>()
        .insert_resource(ClearColor(Theme::default().palette.bench))
        .insert_resource(SettingsMenuOpen(false))
        .insert_resource(GameSettings::default())
        .add_systems(
            Startup,
            (
                initial_startup_system.before(configure_camera_scaling),
                configure_camera_scaling,
            ),
        )
        .add_systems(
            Update,
            (zoom_camera, pan_camera).run_if(in_state(GameState::Simulating)),
        )
        .add_systems(Update, (show_settings_menu, settings_menu_button_system));

    if !disable_egui {
        app = app.add_systems(bevy_egui::EguiPrimaryContextPass, update_ui_pointer_state);
    }

    app.run();
}
