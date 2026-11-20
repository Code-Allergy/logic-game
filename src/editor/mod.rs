//! Logic-circuit editor plugin, UI, and interaction systems.
//!
//! Split into small modules:
//! - `ui`: UI layout and workspace/sub-circuit selection logic.
//! - `systems`: wire-building selection.
//! - `workspace`: catalog initialization and on-disk workspace helpers.
//!
//! The editor never mutates the circuit directly: it sends the authoring
//! messages of [`crate::sim::authoring`].

mod systems;
mod ui;

pub mod workspace;

use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::EguiPrimaryContextPass;

use crate::sim::{GateKind, WorkspaceDocument};

pub use crate::sim::{CloseView, LoadWorkspaceEvent, OpenInstanceView, SaveWorkspaceEvent};
pub use systems::handle_connection_creation;
pub use ui::editor_ui_system;
pub use workspace::catalog_init_system;

// ─────────────────────────────────────────────────────────────────────────────
// Editor resource
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource)]
pub struct EditorState {
    pub visible: bool,
    pub selected_gate_type: Option<GateKind>,
    pub selected_gate_label: Option<String>,
    /// Width used for width-parametric kinds placed from the palette.
    pub palette_width: u8,
    pub connection_mode: bool,
    pub current_workspace: Option<String>,
    pub workspace_file_name: String,
    pub workspace_catalog: Vec<(String, Arc<WorkspaceDocument>)>,
    /// Saved workspace names, read from disk once and refreshed after a save
    /// or on request (never per frame).
    pub saved_workspaces: Option<Vec<String>>,
    /// Last workspace load/save message (e.g. an unsupported old format).
    pub status: Option<String>,
    /// First gate picked in connection mode.
    pub connection_source: Option<Entity>,
    /// Output pin on the source gate.
    pub connection_source_pin: u8,
    /// Destination gate in connection mode.
    pub connection_sink: Option<Entity>,
    /// Input pin on the sink gate.
    pub connection_sink_pin: u8,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            visible: true,
            selected_gate_type: None,
            selected_gate_label: None,
            palette_width: 1,
            connection_mode: false,
            current_workspace: None,
            workspace_file_name: "workspace".to_string(),
            workspace_catalog: Vec::new(),
            saved_workspaces: None,
            status: None,
            connection_source: None,
            connection_source_pin: 0,
            connection_sink: None,
            connection_sink_pin: 0,
        }
    }
}

impl EditorState {
    pub fn clear_connection(&mut self) {
        self.connection_source = None;
        self.connection_sink = None;
        self.connection_source_pin = 0;
        self.connection_sink_pin = 0;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_systems(
                EguiPrimaryContextPass,
                (catalog_init_system, editor_ui_system, handle_connection_creation),
            );
    }
}
