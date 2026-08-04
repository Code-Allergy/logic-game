//! Workspace/catalog file I/O helpers for the editor.

use std::fs;
use std::io;
use std::path::Path;

use bevy::prelude::ResMut;
use crate::{DEFAULT_WORKSPACE_EXT, WORKSPACE_DIR};
use crate::sim::GameCircuit;
use crate::sim::GameNodeKind;

use crate::sim::fixtures::fixture_workspace_catalog;
use crate::editor::EditorState;

pub fn catalog_init_system(mut editor_state: ResMut<EditorState>) {
    if editor_state.workspace_catalog.is_empty() {
        editor_state.workspace_catalog = fixture_workspace_catalog();
    }
}

pub fn load_saved_workspace_names() -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    if !Path::new(WORKSPACE_DIR).exists() {
        return Ok(names);
    }

    for entry in fs::read_dir(WORKSPACE_DIR)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str())
            == Some(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'))
        {
            if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort_unstable();
    Ok(names)
}

pub fn read_workspace_from_disk(name: &str) -> io::Result<GameCircuit> {
    let path = workspace_path(name);
    let file = std::fs::File::open(path)?;
    let saved: crate::sim::SavedSimulation = serde_json::from_reader(file)?;
    if saved.version != crate::sim::SavedSimulation::CURRENT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported save format version",
        ));
    }
    let mut circuit = saved.circuit;
    enforce_required_io_labels(&mut circuit);
    Ok(circuit)
}

fn enforce_required_io_labels(circuit: &mut GameCircuit) {
    let mut next_input = 1usize;
    let mut next_input8 = 1usize;
    let mut next_input64 = 1usize;
    let mut next_output = 1usize;
    let mut next_output8 = 1usize;
    let mut next_output64 = 1usize;

    for node in circuit.nodes.values_mut() {
        let needs_label = matches!(
            node.kind,
            GameNodeKind::Input
                | GameNodeKind::Input8
                | GameNodeKind::Input64
                | GameNodeKind::Output
                | GameNodeKind::Output8
                | GameNodeKind::Output64
        ) && node.label.as_deref().is_none_or(|value| value.trim().is_empty());

        if !needs_label {
            continue;
        }

        node.label = Some(match node.kind {
            GameNodeKind::Input => {
                let label = format!("Input {next_input}");
                next_input += 1;
                label
            }
            GameNodeKind::Input8 => {
                let label = format!("Input8 {next_input8}");
                next_input8 += 1;
                label
            }
            GameNodeKind::Input64 => {
                let label = format!("Input64 {next_input64}");
                next_input64 += 1;
                label
            }
            GameNodeKind::Output => {
                let label = format!("Output {next_output}");
                next_output += 1;
                label
            }
            GameNodeKind::Output8 => {
                let label = format!("Output8 {next_output8}");
                next_output8 += 1;
                label
            }
            _ => {
                let label = format!("Output64 {next_output64}");
                next_output64 += 1;
                label
            }
        });
    }
}

pub(crate) fn workspace_path(name: &str) -> String {
    format!(
        "{}/{}{}",
        WORKSPACE_DIR,
        sanitize_workspace_name(name),
        DEFAULT_WORKSPACE_EXT
    )
}

pub(crate) fn sanitize_workspace_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}
