//! Workspace/catalog file I/O helpers for the editor.
//!
//! Workspace persistence now uses a package folder structure:
//! - `workspaces/<name>/manifest.json` contains package metadata.
//! - `workspaces/<name>/<circuit-id>.json` stores each circuit separately.
//! - Root/sub-circuit references are written as UUID links to avoid nesting
//!   duplicate circuit definitions.

use bevy::prelude::Vec2;
use bevy::log::warn;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use bevy::prelude::ResMut;
use crate::{DEFAULT_WORKSPACE_EXT, WORKSPACE_DIR};
use crate::sim::GameCircuit;
use crate::sim::GameNodeKind;

use crate::sim::fixtures::fixture_workspace_catalog;
use crate::editor::EditorState;

const WORKSPACE_MANIFEST_FILE: &str = "manifest.json";
const BUNDLE_VERSION: u32 = 4;

pub fn catalog_init_system(mut editor_state: ResMut<EditorState>) {
    if editor_state.workspace_catalog.is_empty() {
        editor_state.workspace_catalog = fixture_workspace_catalog();
    }
}

pub fn load_saved_workspace_names() -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    if !Path::new(WORKSPACE_DIR).exists() {
        return Ok(names);
    }

    for entry in fs::read_dir(WORKSPACE_DIR)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.join(WORKSPACE_MANIFEST_FILE).exists() {
                if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                    if seen.insert(name.to_string()) {
                        names.push(name.to_string());
                    }
                }
            }
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str())
            == Some(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'))
        {
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                if seen.insert(stem.to_string()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort_unstable();
    Ok(names)
}

pub fn read_workspace_from_disk(name: &str) -> io::Result<GameCircuit> {
    let workspace_path = workspace_path(name);
    let legacy_path = workspace_path.with_extension(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'));
    if legacy_path.exists() {
        let file = File::open(legacy_path)?;
        let saved: crate::sim::SavedSimulation = serde_json::from_reader(file)?;
        if saved.version != crate::sim::SavedSimulation::CURRENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported save format version",
            ));
        }
        let circuit = saved.circuit;
        validate_required_io_labels(&circuit)?;
        return Ok(circuit);
    }

    if !workspace_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "workspace directory not found",
        ));
    }

    let manifest_path = workspace_path.join(WORKSPACE_MANIFEST_FILE);
    let manifest_file = File::open(&manifest_path)?;
    let manifest: WorkspaceManifest = serde_json::from_reader(manifest_file)?;
    if manifest.version > BUNDLE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported workspace bundle version: {}", manifest.version),
        ));
    }

    let mut loaded = HashMap::new();
    let mut loading = HashSet::new();
    let root_template_id = manifest
        .root_circuit_template_id
        .unwrap_or(manifest.root_circuit_id);
    let circuit = restore_circuit(
        &workspace_path,
        manifest.root_circuit_id,
        root_template_id,
        &mut loaded,
        &mut loading,
    )?;
    report_duplicate_structural_circuits(&circuit);
    validate_required_io_labels(&circuit)?;
    Ok(circuit)
}

pub fn save_workspace_to_disk(name: &str, circuit: &GameCircuit) -> io::Result<()> {
    validate_required_io_labels(circuit)?;
    report_duplicate_structural_circuits(circuit);

    let workspace_dir = workspace_path(name);
    if !workspace_dir.exists() {
        fs::create_dir_all(&workspace_dir)?;
    } else if workspace_dir.is_file() {
        fs::remove_file(&workspace_dir)?;
        fs::create_dir_all(&workspace_dir)?;
    }
    let _ = fs::remove_file(workspace_dir.with_extension(DEFAULT_WORKSPACE_EXT.trim_start_matches('.')));

    let mut circuits = HashMap::new();
    collect_circuit_bundle(circuit, &mut circuits);
    let mut ids: Vec<_> = circuits.keys().copied().collect();
    ids.sort_unstable();

    for (id, saved) in circuits {
        let path = workspace_dir.join(circuit_file_name(id));
        let file = File::create(path)?;
        serde_json::to_writer_pretty(file, &saved).map_err(io::Error::other)?;
    }

    let manifest = WorkspaceManifest {
        version: BUNDLE_VERSION,
        root_circuit_id: circuit.id,
        root_circuit_template_id: Some(circuit.template_uuid()),
        circuit_ids: ids,
    };
    let manifest_path = workspace_dir.join(WORKSPACE_MANIFEST_FILE);
    let manifest_file = File::create(manifest_path)?;
    serde_json::to_writer_pretty(manifest_file, &manifest).map_err(io::Error::other)?;
    Ok(())
}

fn report_duplicate_structural_circuits(circuit: &GameCircuit) {
    let mut signatures: HashMap<Uuid, DuplicateSignatureGroup> = HashMap::new();
    let mut seen = HashSet::new();

    collect_circuit_structural_signatures(circuit, &mut signatures, &mut seen);

    for (signature_id, group) in signatures {
        if group.ids.len() <= 1 {
            continue;
        }

        let preview = if group.signature.len() > 180 {
            format!("{}…", &group.signature[..180])
        } else {
            group.signature
        };

        warn!(
            "Detected {} circuits with identical structure: template_id={}; nodes={}, wires={}; sample signature=\"{}\"; ids={:?}",
            group.ids.len(),
            signature_id,
            group.node_count,
            group.wire_count,
            preview,
            group.ids
        );
    }
}

#[derive(Default)]
struct DuplicateSignatureGroup {
    signature: String,
    ids: Vec<String>,
    node_count: usize,
    wire_count: usize,
}

fn collect_circuit_structural_signatures(
    circuit: &GameCircuit,
    signatures: &mut HashMap<Uuid, DuplicateSignatureGroup>,
    seen: &mut HashSet<Uuid>,
) {
    if !seen.insert(circuit.id) {
        return;
    }

    let signature_id = circuit.template_uuid();
    let signature = circuit.structural_signature();
    let group = signatures.entry(signature_id).or_default();
    if group.signature.is_empty() {
        group.signature = signature;
        group.node_count = circuit.nodes.len();
        group.wire_count = circuit.wires.len();
    }
    group.ids.push(format!("{}@{}", circuit.id, circuit.name));

    for node in circuit.nodes.values() {
        if let GameNodeKind::SubCircuit { circuit: nested } = &node.kind {
            collect_circuit_structural_signatures(nested, signatures, seen);
        }
    }
}

fn validate_required_io_labels(circuit: &GameCircuit) -> io::Result<()> {
    let mut missing = Vec::new();
    collect_missing_required_io_labels(circuit, &mut missing);
    if missing.is_empty() {
        return Ok(());
    }

    let preview = if missing.len() > 12 {
        format!("{}, ...", missing[..12].join(", "))
    } else {
        missing.join(", ")
    };

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "workspace requires labels for Input/Input8/Input64/Output/Output8/Output64/Display nodes; {} unlabeled node(s): {preview}",
            missing.len()
        ),
    ))
}

fn collect_missing_required_io_labels(
    circuit: &GameCircuit,
    missing: &mut Vec<String>,
) {
    for node in circuit.nodes.values() {
        if let GameNodeKind::SubCircuit { circuit: nested } = &node.kind {
            collect_missing_required_io_labels(nested, missing);
            continue;
        }

        let needs_label = matches!(
            node.kind,
            GameNodeKind::Input
                | GameNodeKind::Input8
                | GameNodeKind::Input64
                | GameNodeKind::Output
                | GameNodeKind::Output8
                | GameNodeKind::Output64
                | GameNodeKind::Display { .. }
        ) && node.label.as_deref().is_none_or(|value| value.trim().is_empty());

        if !needs_label {
            continue;
        }

        missing.push(format!(
            "{kind} node {id} in circuit \"{circuit}\"",
            kind = node.kind.display_name(),
            id = node.id,
            circuit = circuit.name
        ));
    }
}

pub(crate) fn workspace_path(name: &str) -> PathBuf {
    Path::new(WORKSPACE_DIR).join(sanitize_workspace_name(name))
}

pub(crate) fn sanitize_workspace_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceManifest {
    pub version: u32,
    pub root_circuit_id: Uuid,
    #[serde(default)]
    pub root_circuit_template_id: Option<Uuid>,
    pub circuit_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedCircuit {
    pub id: Uuid,
    #[serde(default)]
    pub template_id: Uuid,
    pub name: String,
    pub nodes: HashMap<Uuid, PersistedNode>,
    pub wires: HashMap<Uuid, crate::sim::GameWire>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedNode {
    pub id: Uuid,
    pub kind: PersistedNodeKind,
    pub position: Vec2,
    pub label: Option<String>,
    pub custom_properties: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
enum PersistedNodeKind {
    Input,
    Input8,
    Input64,
    Output,
    Output8,
    Output64,
    Display {
        bits: u8,
    },
    Expand8,
    Expand64,
    ReduceOr8,
    ReduceOr64,
    And,
    Or,
    Not,
    Xor,
    Nand,
    Nor,
    Xnor,
    Buffer,
    Mux,
    Demux,
    TriStateBuffer,
    Counter8,
    Clock {
        period_ticks: u32,
        duty_ticks: u32,
    },
    SubCircuit {
        circuit_id: Uuid,
        #[serde(default)]
        template_id: Uuid,
    },
}

fn circuit_file_name(circuit_id: Uuid) -> String {
    format!("{circuit_id}.json")
}

fn collect_circuit_bundle(
    circuit: &GameCircuit,
    circuits: &mut HashMap<Uuid, PersistedCircuit>,
) {
    let template_id = circuit.template_uuid();
    if circuits.contains_key(&template_id) {
        return;
    }

    let mut nodes = HashMap::new();
    for node in circuit.nodes.values() {
        let persisted_kind = match &node.kind {
            GameNodeKind::SubCircuit { circuit: nested } => {
                collect_circuit_bundle(nested, circuits);
                let nested_template_id = nested.template_uuid();
                PersistedNodeKind::SubCircuit {
                    circuit_id: nested.id,
                    template_id: nested_template_id,
                }
            }
            GameNodeKind::Input => PersistedNodeKind::Input,
            GameNodeKind::Input8 => PersistedNodeKind::Input8,
            GameNodeKind::Input64 => PersistedNodeKind::Input64,
            GameNodeKind::Output => PersistedNodeKind::Output,
            GameNodeKind::Output8 => PersistedNodeKind::Output8,
            GameNodeKind::Output64 => PersistedNodeKind::Output64,
            GameNodeKind::Display { bits } => PersistedNodeKind::Display { bits: *bits },
            GameNodeKind::Expand8 => PersistedNodeKind::Expand8,
            GameNodeKind::Expand64 => PersistedNodeKind::Expand64,
            GameNodeKind::ReduceOr8 => PersistedNodeKind::ReduceOr8,
            GameNodeKind::ReduceOr64 => PersistedNodeKind::ReduceOr64,
            GameNodeKind::And => PersistedNodeKind::And,
            GameNodeKind::Or => PersistedNodeKind::Or,
            GameNodeKind::Not => PersistedNodeKind::Not,
            GameNodeKind::Xor => PersistedNodeKind::Xor,
            GameNodeKind::Nand => PersistedNodeKind::Nand,
            GameNodeKind::Nor => PersistedNodeKind::Nor,
            GameNodeKind::Xnor => PersistedNodeKind::Xnor,
            GameNodeKind::Buffer => PersistedNodeKind::Buffer,
            GameNodeKind::Mux => PersistedNodeKind::Mux,
            GameNodeKind::Demux => PersistedNodeKind::Demux,
            GameNodeKind::TriStateBuffer => PersistedNodeKind::TriStateBuffer,
            GameNodeKind::Counter8 => PersistedNodeKind::Counter8,
            GameNodeKind::Clock {
                period_ticks,
                duty_ticks,
            } => PersistedNodeKind::Clock {
                period_ticks: *period_ticks,
                duty_ticks: *duty_ticks,
            },
        };
        nodes.insert(
            node.id,
            PersistedNode {
                id: node.id,
                kind: persisted_kind,
                position: node.position,
                label: node.label.clone(),
                custom_properties: node.custom_properties.clone(),
            },
        );
    }

    circuits.insert(
        template_id,
        PersistedCircuit {
            id: template_id,
            template_id,
            name: circuit.name.clone(),
            nodes,
            wires: circuit.wires.clone(),
        },
    );
}

fn restore_circuit(
    workspace_dir: &Path,
    circuit_instance_id: Uuid,
    circuit_template_id: Uuid,
    loaded: &mut HashMap<Uuid, GameCircuit>,
    loading: &mut HashSet<Uuid>,
) -> io::Result<GameCircuit> {
    let mut circuit = restore_circuit_template(
        workspace_dir,
        circuit_template_id,
        loaded,
        loading,
    )?;
    circuit.id = circuit_instance_id;
    Ok(circuit)
}

fn restore_circuit_template(
    workspace_dir: &Path,
    circuit_template_id: Uuid,
    loaded: &mut HashMap<Uuid, GameCircuit>,
    loading: &mut HashSet<Uuid>,
) -> io::Result<GameCircuit> {
    if let Some(circuit) = loaded.get(&circuit_template_id) {
        return Ok(circuit.clone());
    }
    if !loading.insert(circuit_template_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("recursive subcircuit reference detected: {circuit_template_id}"),
        ));
    }

    let path = workspace_dir.join(circuit_file_name(circuit_template_id));
    let file = File::open(path)?;
    let saved: PersistedCircuit = serde_json::from_reader(file)?;
    let saved_template_id = if saved.template_id.is_nil() {
        saved.id
    } else {
        saved.template_id
    };
    if saved_template_id != circuit_template_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "circuit template id mismatch in workspace bundle: expected {circuit_template_id}, got {}",
                saved_template_id
            ),
        ));
    }

    let mut nodes = HashMap::new();
    for persisted in saved.nodes.into_values() {
        let kind = match persisted.kind {
            PersistedNodeKind::SubCircuit {
                circuit_id: nested_id,
                template_id: nested_template_id,
            } => {
                let nested_template_id = if nested_template_id.is_nil() {
                    nested_id
                } else {
                    nested_template_id
                };
                GameNodeKind::SubCircuit {
                    circuit: Box::new(restore_circuit(
                        workspace_dir,
                        nested_id,
                        nested_template_id,
                        loaded,
                        loading,
                    )?),
                }
            }
            PersistedNodeKind::Input => GameNodeKind::Input,
            PersistedNodeKind::Input8 => GameNodeKind::Input8,
            PersistedNodeKind::Input64 => GameNodeKind::Input64,
            PersistedNodeKind::Output => GameNodeKind::Output,
            PersistedNodeKind::Output8 => GameNodeKind::Output8,
            PersistedNodeKind::Output64 => GameNodeKind::Output64,
            PersistedNodeKind::Display { bits } => GameNodeKind::Display { bits },
            PersistedNodeKind::Expand8 => GameNodeKind::Expand8,
            PersistedNodeKind::Expand64 => GameNodeKind::Expand64,
            PersistedNodeKind::ReduceOr8 => GameNodeKind::ReduceOr8,
            PersistedNodeKind::ReduceOr64 => GameNodeKind::ReduceOr64,
            PersistedNodeKind::And => GameNodeKind::And,
            PersistedNodeKind::Or => GameNodeKind::Or,
            PersistedNodeKind::Not => GameNodeKind::Not,
            PersistedNodeKind::Xor => GameNodeKind::Xor,
            PersistedNodeKind::Nand => GameNodeKind::Nand,
            PersistedNodeKind::Nor => GameNodeKind::Nor,
            PersistedNodeKind::Xnor => GameNodeKind::Xnor,
            PersistedNodeKind::Buffer => GameNodeKind::Buffer,
            PersistedNodeKind::Mux => GameNodeKind::Mux,
            PersistedNodeKind::Demux => GameNodeKind::Demux,
            PersistedNodeKind::TriStateBuffer => GameNodeKind::TriStateBuffer,
            PersistedNodeKind::Counter8 => GameNodeKind::Counter8,
            PersistedNodeKind::Clock {
                period_ticks,
                duty_ticks,
            } => GameNodeKind::Clock {
                period_ticks,
                duty_ticks,
            },
        };
        nodes.insert(
            persisted.id,
            crate::sim::GameNode {
                id: persisted.id,
                kind,
                position: persisted.position,
                label: persisted.label,
                custom_properties: persisted.custom_properties,
            },
        );
    }

    let circuit = GameCircuit {
        id: saved_template_id,
        name: saved.name,
        nodes,
        wires: saved.wires,
    };
    loaded.insert(circuit_template_id, circuit.clone());
    loading.remove(&circuit_template_id);
    Ok(circuit)
}
