//! Logic-circuit editor plugin, UI, and interaction systems.
//!
//! Split into small modules:
//! - `ui`: UI layout and workspace/sub-circuit selection logic.
//! - `systems`: node placement, wire creation, and selection-state handling.
//! - `workspace`: catalog initialization and on-disk workspace helpers.

mod systems;
mod ui;

pub mod workspace;

use bevy::prelude::*;
use bevy_egui::EguiPrimaryContextPass;
use uuid::Uuid;

use crate::sim::{GameCircuit, GameNodeKind, WireKind};

pub use systems::{
    apply_connection_events_system, handle_connection_creation, process_gate_placement,
};
pub use ui::editor_ui_system;
pub use workspace::catalog_init_system;

#[derive(Clone)]
pub(crate) struct SubcircuitNavigationFrame {
    pub parent_subcircuit_node_id: Uuid,
    pub parent_circuit: GameCircuit,
    pub parent_workspace: Option<String>,
    pub parent_workspace_file_name: String,
    pub parent_breadcrumb: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Editor resource
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource)]
pub struct EditorState {
    pub visible: bool,
    pub selected_gate_type: Option<GameNodeKind>,
    pub selected_gate_label: Option<String>,
    pub connection_mode: bool,
    pub current_workspace: Option<String>,
    pub workspace_file_name: String,
    pub workspace_catalog: Vec<(String, GameCircuit)>,
    pub workspace_breadcrumb: String,
    pub(crate) subcircuit_navigation_stack: Vec<SubcircuitNavigationFrame>,
    /// First gate picked in connection mode.
    pub connection_source: Option<Uuid>,
    /// Output pin on the source gate.
    pub connection_source_pin: u8,
    /// Optional destination gate in connection mode.
    pub connection_sink: Option<Uuid>,
    /// Input pin on the sink gate.
    pub connection_sink_pin: u8,
    /// The wire kind selected in connection mode.
    pub connection_wire_kind: WireKind,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            visible: true,
            selected_gate_type: None,
            selected_gate_label: None,
            connection_mode: false,
            current_workspace: None,
            workspace_file_name: "workspace".to_string(),
            workspace_catalog: Vec::new(),
            workspace_breadcrumb: "workspace".to_string(),
            subcircuit_navigation_stack: Vec::new(),
            connection_source: None,
            connection_source_pin: 0,
            connection_sink: None,
            connection_sink_pin: 0,
            connection_wire_kind: WireKind::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Messages
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Message)]
pub struct CreateGateEvent {
    pub kind: GameNodeKind,
    pub position: Vec2,
    pub label: Option<String>,
}

#[derive(Message)]
pub struct CreateConnectionEvent {
    pub source_node: Uuid,
    pub source_pin: u8,
    pub sink_node: Uuid,
    pub sink_pin: u8,
    pub wire_kind: WireKind,
}

#[derive(Message, Clone)]
pub struct LoadWorkspaceEvent {
    pub name: String,
    pub circuit: GameCircuit,
}

#[derive(Message)]
pub struct OpenSubcircuitEvent {
    pub node_id: Uuid,
}

#[derive(Message)]
pub struct CloseSubcircuitViewEvent;

#[derive(Message)]
pub struct SaveWorkspaceEvent {
    pub name: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_message::<CreateGateEvent>()
            .add_message::<CreateConnectionEvent>()
            .add_message::<OpenSubcircuitEvent>()
            .add_message::<CloseSubcircuitViewEvent>()
            .add_systems(
                EguiPrimaryContextPass,
                (
                    catalog_init_system,
                    editor_ui_system,
                    process_gate_placement,
                    handle_connection_creation,
                    apply_connection_events_system,
                ),
            );
    }
}
