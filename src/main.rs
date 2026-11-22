use std::env;

use bevy::prelude::*;
use bevy_egui::EguiPlugin;

#[cfg(feature = "inspector")]
use bevy_inspector_egui::quick::WorldInspectorPlugin;

use gate_quest::configure_camera_scaling;
use gate_quest::editor::EditorPlugin;
use gate_quest::settings::SettingsPlugin;
use gate_quest::sim::SimulationPlugin;
use gate_quest::temp::GateComponentSelectionEvent;
use gate_quest::theme::{Theme, ThemePlugin};
use gate_quest::title::MainMenuPlugin;
use gate_quest::ui_style::WorkshopUiPlugin;
use gate_quest::{
    GAME_TITLE, GAME_WEB_TAG, GameState, UiPointerState, initial_startup_system,
    update_ui_pointer_state,
};
use gate_quest::{key_pan_camera, pan_camera, zoom_camera};

fn main() {
    let disable_egui = env::var_os("GATEQUEST_DISABLE_EGUI").is_some();
    let disable_editor = env::var_os("GATEQUEST_DISABLE_EDITOR").is_some();

    let mut app = App::new();

    // The game has no gamepad input, so Bevy is built without gilrs (see
    // Cargo.toml): an unplugged pad with rumble can't flood the log with
    // force-feedback errors every 50 ms.
    let mut app = app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: GAME_TITLE.to_string(),
            canvas: Some(GAME_WEB_TAG.to_owned()),
            fit_canvas_to_parent: true,
            prevent_default_event_handling: true,
            ..default()
        }),
        ..default()
    }));

    if !disable_egui {
        app = app.add_plugins((EguiPlugin::default(), WorkshopUiPlugin));
    }

    #[cfg(feature = "inspector")]
    if !disable_egui {
        app = app.add_plugins(WorldInspectorPlugin::new());
    }

    app.add_plugins((ThemePlugin, SettingsPlugin))
        .add_plugins(MainMenuPlugin {
            state: GameState::MainMenu,
        })
        .add_plugins(SimulationPlugin::new(GameState::Simulating));

    if !disable_editor {
        app = app.add_plugins(EditorPlugin);
    }

    app = app
        .init_state::<GameState>()
        .insert_resource(UiPointerState::default())
        .add_message::<GateComponentSelectionEvent>()
        .insert_resource(ClearColor(Theme::default().palette.bench))
        .add_systems(
            Startup,
            (
                initial_startup_system.before(configure_camera_scaling),
                configure_camera_scaling,
            ),
        )
        .add_systems(
            Update,
            (zoom_camera, pan_camera, key_pan_camera).run_if(in_state(GameState::Simulating)),
        );

    if !disable_egui {
        app = app.add_systems(bevy_egui::EguiPrimaryContextPass, update_ui_pointer_state);
    }

    #[cfg(feature = "mcp")]
    if env::args().any(|arg| arg == "--mcp") {
        serve_mcp(app);
    }

    app.run();
}

/// `--mcp`: serve the MCP tools on the open workspace, over HTTP on this
/// machine (`docs/mcp-server.md`), and start in the editor.
#[cfg(feature = "mcp")]
fn serve_mcp(app: &mut App) {
    use gate_quest::editor::workspace::{WorkspaceStore, player_data_dir};
    use gate_quest::mcp::app::{McpAppPlugin, link};
    use gate_quest::mcp::http::serve;
    use gate_quest::mcp::server::Server;
    use gate_quest::mcp::session::Session;

    let (link, inbox) = link();
    let store = WorkspaceStore::player().unwrap_or_else(|| WorkspaceStore::in_dir("./workspaces"));
    let server = Server::new(Session::with_app(store, Box::new(link)));
    match serve(server, player_data_dir()) {
        Ok(endpoint) => {
            println!(
                "MCP server: {} (token in {})",
                endpoint.url,
                match &endpoint.file {
                    Some(file) => file.display().to_string(),
                    None => "no file: it could not be written".to_string(),
                }
            );
            app.add_plugins(McpAppPlugin::new(inbox))
                .insert_state(GameState::Simulating);
        }
        Err(e) => eprintln!("MCP server: not started: {e}"),
    }
}
