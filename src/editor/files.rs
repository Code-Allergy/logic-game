//! Replacing the open circuit: opening a workspace, a demo, an imported
//! netlist, a new empty workspace or an editable copy of a demo, and the
//! unsaved-changes guard in front of all of them.
//!
//! The Library drawer ([`super::ui`]) and the demo map
//! ([`super::demo_map`]) both go through [`request_open`]: when the open
//! workspace is editable and has unsaved changes, the request waits in
//! [`EditorState::pending_open`] and the workshop asks the player to Save,
//! Discard or Cancel first.

use std::sync::Arc;

use bevy::prelude::*;

use crate::sim::{
    ActiveCircuit, CircuitDocument, Demo, LoadWorkspaceEvent, WorkspaceDocument, WorkspaceRevision,
    demos,
};

use super::EditorState;
use super::workspace::{WEB_SAVE_UNAVAILABLE, WorkspaceStore};

/// Something that replaces the open circuit.
#[derive(Debug, Clone)]
pub enum OpenRequest {
    /// Open this workspace (a saved one, a demo, an imported netlist).
    Workspace {
        name: String,
        workspace: Arc<WorkspaceDocument>,
        read_only: bool,
        /// Not saved anywhere yet (see [`LoadWorkspaceEvent::unsaved`]).
        unsaved: bool,
        /// The status line to show once it is open.
        status: Option<String>,
    },
    /// An editable copy of the built-in demo or gallery item with this id
    /// (see [`copy_demo`]).
    CopyDemo(String),
    /// A new, empty workspace.
    New,
}

impl OpenRequest {
    /// What the confirmation names: "open “Counter”".
    pub fn describe(&self) -> String {
        match self {
            OpenRequest::Workspace { name, .. } => format!("open \"{name}\""),
            OpenRequest::CopyDemo(id) => {
                let title = demos::catalog()
                    .ok()
                    .and_then(|c| c.get(id))
                    .map_or(id.as_str(), |d| d.title.as_str());
                format!("copy \"{title}\"")
            }
            OpenRequest::New => "start a new workspace".to_string(),
        }
    }
}

/// An [`OpenRequest`] waiting for the player's answer.
#[derive(Debug, Clone)]
pub struct PendingOpen {
    pub request: OpenRequest,
    /// "Save" was chosen: the request goes ahead once the save is done.
    pub awaiting_save: bool,
    /// Why the last save failed.
    pub error: Option<String>,
}

/// Whether replacing the open circuit would lose work.
pub fn would_lose_work(active: &ActiveCircuit, revision: &WorkspaceRevision) -> bool {
    !active.read_only && revision.is_dirty()
}

/// Open `request`, or, when that would lose unsaved changes, park it in
/// [`EditorState::pending_open`] for the player to confirm.
pub fn request_open(
    editor: &mut EditorState,
    load: &mut MessageWriter<LoadWorkspaceEvent>,
    active: &ActiveCircuit,
    revision: &WorkspaceRevision,
    request: OpenRequest,
) {
    if would_lose_work(active, revision) {
        editor.pending_open = Some(PendingOpen {
            request,
            awaiting_save: false,
            error: None,
        });
    } else {
        open_now(Some(editor), load, request);
    }
}

/// Replace the open circuit now (no questions).
pub fn open_now(
    editor: Option<&mut EditorState>,
    load: &mut MessageWriter<LoadWorkspaceEvent>,
    request: OpenRequest,
) {
    let store = WorkspaceStore::player();
    let (name, workspace, read_only, unsaved, status) = match request {
        OpenRequest::Workspace {
            name,
            workspace,
            read_only,
            unsaved,
            status,
        } => (name, workspace, read_only, unsaved, status),
        OpenRequest::New => {
            let name = store
                .as_ref()
                .map_or_else(|| "workspace".to_string(), |s| s.unique_name("workspace"));
            let root = CircuitDocument::new(name.clone());
            (
                name,
                Arc::new(WorkspaceDocument::new(root)),
                false,
                false,
                None,
            )
        }
        OpenRequest::CopyDemo(id) => {
            let Some(demo) = demos::catalog().ok().and_then(|c| c.get(&id)) else {
                warn!("No demo {id} to copy");
                return;
            };
            let copy = copy_demo(demo, store.as_ref());
            (
                copy.name,
                Arc::new(copy.workspace),
                false,
                !copy.saved,
                Some(copy.status),
            )
        }
    };
    if let Some(editor) = editor {
        editor.current_workspace = Some(name.clone());
        editor.workspace_file_name = name.clone();
        editor.selected_gate_type = None;
        editor.clear_connection();
        editor.pending_open = None;
        editor.status = status;
        editor.saved_workspaces = None;
    }
    load.write(LoadWorkspaceEvent {
        name,
        workspace,
        read_only,
        unsaved,
    });
}

/// An editable copy of a demo (see [`copy_demo`]).
#[derive(Debug, Clone)]
pub struct DemoCopy {
    pub name: String,
    pub workspace: WorkspaceDocument,
    /// Written to the player's workspaces.
    pub saved: bool,
    /// What to tell the player.
    pub status: String,
}

/// An editable copy of `demo` under fresh ids, named `"<title> (copy)"`
/// (or `"(copy 2)"`, ... when that name is taken: an earlier copy is never
/// overwritten) and saved right away so it is listed with the saved
/// workspaces. Without a file system (`store` is `None`, the web build) it
/// is kept in memory only.
pub fn copy_demo(demo: &Demo, store: Option<&WorkspaceStore>) -> DemoCopy {
    let base = format!("{} (copy)", demo.title);
    let name = store.map_or_else(|| base.clone(), |s| s.unique_name(&base));
    let workspace = demos::editable_copy(&demo.workspace, &name);
    let (saved, status) = match store {
        Some(store) => match store.save(&name, &workspace) {
            Ok(_) => (
                true,
                format!("Copied \"{}\" to your workspace \"{name}\".", demo.title),
            ),
            Err(err) => (
                false,
                format!(
                    "Copied \"{}\" (not saved to disk: {err}). Save it to keep it.",
                    demo.title
                ),
            ),
        },
        None => (
            false,
            format!(
                "Copied \"{}\": an editable copy for this session. {WEB_SAVE_UNAVAILABLE}",
                demo.title
            ),
        ),
    };
    DemoCopy {
        name,
        workspace,
        saved,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_get_free_names_and_never_overwrite_an_earlier_copy() {
        let dir = std::env::temp_dir().join(format!("gatequest-copy-{}", uuid::Uuid::new_v4()));
        let store = WorkspaceStore::in_dir(&dir);
        let demo = demos::catalog().unwrap().get("counter").unwrap();

        let first = copy_demo(demo, Some(&store));
        assert!(first.saved, "{}", first.status);
        assert_eq!(first.name, format!("{} (copy)", demo.title));
        let manifest = dir.join(&first.name).join("manifest.json");
        let before = std::fs::read(&manifest).unwrap();

        let second = copy_demo(demo, Some(&store));
        assert_eq!(second.name, format!("{} (copy 2)", demo.title));
        assert_ne!(second.workspace.root.id, first.workspace.root.id);
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        assert_eq!(store.list().unwrap().len(), 2);

        // Without a file system: in memory, editable, not saved.
        let web = copy_demo(demo, None);
        assert!(!web.saved);
        assert!(web.status.contains(WEB_SAVE_UNAVAILABLE));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn only_unsaved_editable_work_asks_first() {
        let mut revision = WorkspaceRevision::default();
        let mut active = ActiveCircuit::default();
        assert!(!would_lose_work(&active, &revision));
        revision.mark_dirty();
        assert!(would_lose_work(&active, &revision));
        active.read_only = true;
        assert!(
            !would_lose_work(&active, &revision),
            "a demo is never saved"
        );
        active.read_only = false;
        revision.mark_saved();
        assert!(!would_lose_work(&active, &revision));
    }
}
