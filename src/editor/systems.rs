//! Editor systems: wire-building selection and subcircuit navigation.

use std::sync::Arc;

use bevy::prelude::*;

use crate::sim::{
    CircuitLibrary, CircuitReader, GateKind, GateType, LoadWorkspaceEvent, WorkspaceDocument,
};
use crate::temp::GateComponentSelectionEvent;

use super::{CloseSubcircuitViewEvent, EditorState, OpenSubcircuitEvent, SubcircuitNavigationFrame};

/// In connection mode, the first clicked gate is the source and the second
/// the sink; pins are then chosen in the wire builder window.
pub fn handle_connection_creation(
    mut editor_state: ResMut<EditorState>,
    library: Res<CircuitLibrary>,
    gates: Query<&GateType>,
    mut selection_events: MessageReader<GateComponentSelectionEvent>,
    game_state: Res<State<crate::GameState>>,
) {
    if game_state.get() != &crate::GameState::Simulating || !editor_state.connection_mode {
        return;
    }
    for event in selection_events.read() {
        let clicked = event.entity;
        let Ok(ty) = gates.get(clicked) else { continue };
        let (ins, outs) = library.pins(&ty.kind).unwrap_or_default();
        match editor_state.connection_source {
            None => {
                if outs.is_empty() {
                    warn!("Cannot use {} as connection source: no output pins", ty.kind.display_name());
                    continue;
                }
                editor_state.clear_connection();
                editor_state.connection_source = Some(clicked);
            }
            Some(source) if source == clicked && editor_state.connection_sink.is_none() => {
                // A second click on the source picks it as the sink too
                // (feedback through a state element), unless it has no inputs.
                if ins.is_empty() {
                    editor_state.clear_connection();
                } else {
                    editor_state.connection_sink = Some(clicked);
                    editor_state.connection_sink_pin = 0;
                }
            }
            Some(_) => {
                if ins.is_empty() {
                    warn!("Cannot use {} as connection sink: no input pins", ty.kind.display_name());
                    continue;
                }
                editor_state.connection_sink = Some(clicked);
                editor_state.connection_sink_pin = 0;
            }
        }
    }
}

/// Open a subcircuit's definition as the edited circuit, and write it back
/// to the library when returning to the parent.
///
/// The child view is a separate simulation of the definition (live instance
/// views are #29).
pub fn subcircuit_navigation_system(
    mut editor_state: ResMut<EditorState>,
    mut open_events: MessageReader<OpenSubcircuitEvent>,
    mut close_events: MessageReader<CloseSubcircuitViewEvent>,
    mut load: MessageWriter<LoadWorkspaceEvent>,
    reader: CircuitReader,
    gates: Query<&GateType>,
) {
    if close_events.read().last().is_some() {
        let Some(frame) = editor_state.subcircuit_navigation_stack.pop() else {
            warn!("CloseSubcircuitViewEvent ignored: already at root workspace");
            return;
        };
        let mut child = reader.document();
        child.id = frame.definition;
        let mut parent = (*frame.parent).clone();
        parent.definitions.extend(reader.library.reachable_from(&child));
        parent.definitions.insert(frame.definition, Arc::new(child));
        editor_state.current_workspace = frame.parent_workspace;
        editor_state.workspace_file_name = frame.parent_workspace_file_name;
        editor_state.workspace_breadcrumb = frame.parent_breadcrumb;
        let name = editor_state
            .current_workspace
            .clone()
            .unwrap_or_else(|| parent.root.name.clone());
        load.write(LoadWorkspaceEvent { name, workspace: Arc::new(parent) });
        return;
    }

    let Some(event) = open_events.read().last() else { return };
    let Ok(GateType { kind: GateKind::SubCircuit { definition } }) = gates.get(event.entity) else {
        warn!("OpenSubcircuitEvent ignored for a non-subcircuit gate");
        return;
    };
    let Some(def) = reader.library.get(*definition) else {
        warn!("OpenSubcircuitEvent: definition {definition} is missing");
        return;
    };
    let mut root = (**def).clone();
    root.id = *definition;
    let child = WorkspaceDocument {
        definitions: reader.library.reachable_from(&root),
        root,
    };
    let child_name = child.root.name.clone();
    let frame = SubcircuitNavigationFrame {
        definition: *definition,
        parent: Arc::new(reader.workspace()),
        parent_workspace: editor_state.current_workspace.clone(),
        parent_workspace_file_name: editor_state.workspace_file_name.clone(),
        parent_breadcrumb: editor_state.workspace_breadcrumb.clone(),
    };
    editor_state.subcircuit_navigation_stack.push(frame);
    editor_state.current_workspace = Some(child_name.clone());
    editor_state.workspace_file_name = child_name.clone();
    editor_state.workspace_breadcrumb = format!("{} / {}", editor_state.workspace_breadcrumb, child_name);
    load.write(LoadWorkspaceEvent { name: child_name, workspace: Arc::new(child) });
}
