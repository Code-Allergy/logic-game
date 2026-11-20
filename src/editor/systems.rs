//! Editor systems: wire-building selection. (Subcircuit views are handled
//! by [`crate::sim::view`].)

use bevy::prelude::*;

use crate::sim::{CircuitLibrary, GateType};
use crate::temp::GateComponentSelectionEvent;

use super::EditorState;

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
