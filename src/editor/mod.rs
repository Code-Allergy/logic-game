//! Logic-circuit editor plugin, UI, and interaction systems.
//!
//! The editor is a floating HUD over a full-window canvas (see
//! [`crate::ui_style::HudLayout`]):
//!
//! - top-left, the file card (workspace name, view breadcrumb, saved or
//!   read-only state) that opens the Library drawer (workspaces, demos,
//!   gallery, import, save, definitions);
//! - top-centre, the transport card and top-right, the build status pill
//!   ([`crate::sim::systems`] and [`hud`]);
//! - right, the inspector of the selected part ([`crate::sim::systems`]);
//! - bottom: the minimap and zoom, the parts bar, the signals card
//!   ([`hud`]).
//!
//! Modules:
//! - `ui`: the file card, the Library drawer, the wire builder and
//!   placement by click.
//! - `hud`: the parts bar, minimap, zoom, build pill, signals and toasts.
//! - `parts`: the parts bar's catalogue (tabs, number keys, names).
//! - `systems`: wire-building selection.
//! - `undo`: the undo shortcuts and the file card's Undo and Redo buttons.
//! - `workspace`: on-disk workspace helpers (the player's data folder,
//!   safe names, Save As, atomic writes).
//! - `files`: replacing the open circuit, behind the unsaved-changes
//!   guard.
//! - `demo_map`: the full-screen demo map ([`crate::GameState::DemoMap`])
//!   and the record of opened demos.
//!
//! The editor never mutates the circuit directly: it sends the authoring
//! messages of [`crate::sim::authoring`].

pub mod demo_map;
pub mod files;
pub mod hud;
pub mod parts;
mod systems;
mod ui;
pub mod undo;

pub mod workspace;

use bevy::prelude::*;
use bevy_egui::EguiPrimaryContextPass;

use uuid::Uuid;

use crate::sim::GateKind;

pub use parts::PartTab;

pub use crate::sim::{CloseView, LoadWorkspaceEvent, OpenInstanceView, SaveWorkspaceEvent};
pub use systems::handle_connection_creation;
pub use ui::editor_ui_system;

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
    /// Saved workspace names, read from disk once and refreshed after a save
    /// or on request (never per frame).
    pub saved_workspaces: Option<Vec<String>>,
    /// Last workspace load/save message (e.g. an unsupported old format).
    pub status: Option<String>,
    /// Path typed into "Import RTL…" (a Yosys JSON netlist).
    pub rtl_import_path: String,
    /// First gate picked in connection mode.
    pub connection_source: Option<Entity>,
    /// Output pin on the source gate.
    pub connection_source_pin: u8,
    /// Destination gate in connection mode.
    pub connection_sink: Option<Entity>,
    /// Input pin on the sink gate.
    pub connection_sink_pin: u8,
    /// The Library drawer (workspaces, demos, gallery, import) is open.
    pub library_open: bool,
    /// The parts bar's open tab.
    pub part_tab: PartTab,
    /// A part being dragged from the parts bar onto the canvas.
    pub dragging_part: Option<(GateKind, Option<String>)>,
    /// Gates (by id) pinned to the top of the Signals card.
    pub watched: Vec<Uuid>,
    /// A workspace, demo or copy waiting to replace the open circuit until
    /// the player answers the unsaved-changes question.
    pub pending_open: Option<files::PendingOpen>,
    /// The outcome of the last save (set by the save system).
    pub last_save: Option<Result<String, String>>,
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
            saved_workspaces: None,
            status: None,
            rtl_import_path: String::new(),
            connection_source: None,
            connection_source_pin: 0,
            connection_sink: None,
            connection_sink_pin: 0,
            library_open: false,
            part_tab: PartTab::default(),
            dragging_part: None,
            watched: Vec::new(),
            pending_open: None,
            last_save: None,
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

    /// Arm a part for placement by click (leaves wire mode).
    pub fn arm(&mut self, kind: GateKind, label: Option<String>) {
        self.selected_gate_type = Some(kind);
        self.selected_gate_label = label;
        self.connection_mode = false;
        self.clear_connection();
    }

    /// Watch a gate in the Signals card, or stop watching it.
    pub fn toggle_watch(&mut self, gate: Uuid) {
        if let Some(i) = self.watched.iter().position(|g| *g == gate) {
            self.watched.remove(i);
        } else {
            self.watched.push(gate);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_plugins(demo_map::DemoMapPlugin)
            .add_systems(
                EguiPrimaryContextPass,
                (
                    editor_ui_system,
                    handle_connection_creation,
                    hud::parts_bar_system,
                    hud::minimap_system,
                    hud::build_pill_system,
                    hud::signals_card_system,
                    hud::toast_system,
                    hud::editor_keys_system,
                    undo::undo_keys_system,
                ),
            );
    }
}
