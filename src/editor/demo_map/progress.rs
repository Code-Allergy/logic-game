//! Which demos the player has opened, for the demo map.
//!
//! On native builds the record is a small JSON file next to the saved
//! workspaces, `workspaces/.demo-progress.json` (the leading dot keeps it
//! out of the workspace list), written atomically like a workspace
//! manifest (to `.demo-progress.json.tmp`, then renamed):
//!
//! ```json
//! { "format": "gatequest-progress", "version": 1,
//!   "opened": ["counter", "gates"], "last": "counter" }
//! ```
//!
//! `opened` holds demo and gallery ids (sorted); `last` is the one opened
//! most recently. Ids that are no longer in the catalog are kept but not
//! counted. The web build has no file system and the app has no browser
//! storage helper, so there the record lives in memory for the session.
//!
//! [`road_status`] turns the record into what the map draws: which stops
//! were visited, where "You are here" is, and the next stop.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// The progress file, inside [`crate::WORKSPACE_DIR`].
pub const PROGRESS_FILE: &str = ".demo-progress.json";
pub const PROGRESS_FORMAT: &str = "gatequest-progress";
pub const PROGRESS_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct ProgressFile {
    format: String,
    version: u32,
    #[serde(default)]
    opened: Vec<String>,
    #[serde(default)]
    last: Option<String>,
}

/// The opened demos and where they are kept.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct DemoProgress {
    opened: BTreeSet<String>,
    last: Option<String>,
    /// The file the record is saved to after every change; `None` keeps it
    /// in memory (the web build, tests, tools).
    file: Option<PathBuf>,
}

impl DemoProgress {
    /// An empty record kept in memory.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// An in-memory record with these demos opened, the last one last.
    pub fn with_opened<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut progress = Self::default();
        for id in ids {
            progress.mark_opened(id);
        }
        progress
    }

    /// The player's record: on native from [`PROGRESS_FILE`] in
    /// [`crate::WORKSPACE_DIR`], in memory on the web.
    pub fn for_player() -> Self {
        if cfg!(target_arch = "wasm32") {
            Self::in_memory()
        } else {
            Self::load_in(Path::new(crate::WORKSPACE_DIR))
        }
    }

    /// The record in `dir`, saved back there on change. A missing file is
    /// an empty record; an unreadable one is logged and starts empty (it is
    /// replaced on the next change).
    pub fn load_in(dir: &Path) -> Self {
        let path = dir.join(PROGRESS_FILE);
        let mut progress = match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text).unwrap_or_else(|e| {
                warn!("Ignoring {}: {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!("Cannot read {}: {e}", path.display());
                Self::default()
            }
        };
        progress.file = Some(path);
        progress
    }

    /// Parse the file's text (the record is in memory until saved).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: ProgressFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if file.format != PROGRESS_FORMAT || file.version != PROGRESS_VERSION {
            return Err(format!(
                "expected format \"{PROGRESS_FORMAT}\" version {PROGRESS_VERSION}"
            ));
        }
        Ok(Self {
            opened: file.opened.into_iter().collect(),
            last: file.last,
            file: None,
        })
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        let file = ProgressFile {
            format: PROGRESS_FORMAT.into(),
            version: PROGRESS_VERSION,
            opened: self.opened.iter().cloned().collect(),
            last: self.last.clone(),
        };
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    /// Where the record is saved, if anywhere.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Write the record to its file (atomically); nothing for an in-memory
    /// record.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(tmp, path)
    }

    pub fn is_opened(&self, id: &str) -> bool {
        self.opened.contains(id)
    }

    /// The demo or gallery item opened most recently.
    pub fn last(&self) -> Option<&str> {
        self.last.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        self.opened.is_empty()
    }

    /// Record that `id` was opened. Returns whether anything changed.
    pub fn mark_opened(&mut self, id: &str) -> bool {
        let added = self.opened.insert(id.to_string());
        let moved = self.last.as_deref() != Some(id);
        self.last = Some(id.to_string());
        added || moved
    }

    /// Tick or untick `id` (the checkbox on a demo card).
    pub fn set_opened(&mut self, id: &str, opened: bool) -> bool {
        if opened {
            return self.mark_opened(id);
        }
        let removed = self.opened.remove(id);
        if removed && self.last.as_deref() == Some(id) {
            self.last = None;
        }
        removed
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The road's state
// ─────────────────────────────────────────────────────────────────────────────

/// How a stop is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopState {
    /// A demo of the level has been opened (marigold).
    Visited,
    /// The level of the demo opened last ("You are here", sky).
    Current,
    /// Nothing opened yet (white).
    Unopened,
}

/// The road as the progress record sees it. Indices are stop indices (the
/// order of the levels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoadStatus {
    pub stops: Vec<StopState>,
    /// "You are here".
    pub current: Option<usize>,
    /// The next stop to visit: the first stop after the current one that
    /// still has a demo to open, else the first such stop from the start;
    /// `None` once every demo has been opened.
    pub next: Option<usize>,
    /// The road is marigold up to here: the current stop or the furthest
    /// visited one, whichever is further.
    pub travelled_to: Option<usize>,
    /// Curriculum demos opened, of `total`.
    pub opened: usize,
    pub total: usize,
}

/// The road's state for the `levels` (in order) and the curriculum
/// `demos` as `(level, id)`.
///
/// The current stop is the level of the demo opened last; when that was a
/// gallery item (or something no longer listed), the furthest visited
/// stop.
pub fn road_status(levels: &[u32], demos: &[(u32, &str)], progress: &DemoProgress) -> RoadStatus {
    let stop_of = |level: u32| levels.iter().position(|&l| l == level);
    let mut visited = vec![false; levels.len()];
    let mut unopened = vec![false; levels.len()];
    let mut opened = 0;
    for &(level, id) in demos {
        let Some(stop) = stop_of(level) else { continue };
        if progress.is_opened(id) {
            visited[stop] = true;
            opened += 1;
        } else {
            unopened[stop] = true;
        }
    }
    let furthest = visited.iter().rposition(|&v| v);
    let current = progress
        .last()
        .and_then(|last| demos.iter().find(|(_, id)| *id == last))
        .and_then(|&(level, _)| stop_of(level))
        .or(furthest);
    let start = current.map_or(0, |c| c + 1);
    let next = (start..levels.len())
        .find(|&i| unopened[i])
        .or_else(|| (0..levels.len()).find(|&i| unopened[i]));
    let stops = (0..levels.len())
        .map(|i| {
            if Some(i) == current {
                StopState::Current
            } else if visited[i] {
                StopState::Visited
            } else {
                StopState::Unopened
            }
        })
        .collect();
    RoadStatus {
        stops,
        current,
        next,
        travelled_to: current.max(furthest),
        opened,
        total: demos.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("gatequest-progress-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn progress_round_trips_through_its_file() {
        let dir = temp_dir();
        let mut progress = DemoProgress::load_in(&dir);
        assert!(progress.is_empty(), "no file yet");
        assert_eq!(progress.file(), Some(dir.join(PROGRESS_FILE).as_path()));
        assert!(progress.mark_opened("gates"));
        assert!(progress.mark_opened("counter"));
        assert!(!progress.mark_opened("counter"), "nothing new");
        assert!(progress.mark_opened("gates"), "last moves");
        progress.save().unwrap();
        assert!(!dir.join(format!("{PROGRESS_FILE}.tmp")).exists());

        let again = DemoProgress::load_in(&dir);
        assert_eq!(again, progress);
        assert!(again.is_opened("counter") && again.is_opened("gates"));
        assert_eq!(again.last(), Some("gates"));

        // The text is the documented format, ids sorted.
        let value: serde_json::Value = serde_json::from_str(&progress.to_json()).unwrap();
        assert_eq!(value["format"], PROGRESS_FORMAT);
        assert_eq!(value["version"], PROGRESS_VERSION);
        assert_eq!(value["opened"], serde_json::json!(["counter", "gates"]));

        // Unticking forgets it (and `last` when it was last).
        let mut again = again;
        assert!(again.set_opened("gates", false));
        assert_eq!(again.last(), None);
        assert!(!again.is_opened("gates"));

        // The file does not show up as a workspace.
        assert!(
            crate::editor::workspace::list_workspaces_in(&dir)
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_progress_files_start_empty() {
        assert!(DemoProgress::from_json("{}").is_err());
        assert!(
            DemoProgress::from_json(r#"{"format": "gatequest-progress", "version": 9}"#).is_err()
        );
        let p =
            DemoProgress::from_json(r#"{"format": "gatequest-progress", "version": 1}"#).unwrap();
        assert!(p.is_empty() && p.last().is_none());

        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROGRESS_FILE), "not json").unwrap();
        let p = DemoProgress::load_in(&dir);
        assert!(p.is_empty());
        assert!(p.file().is_some(), "still saved there on change");
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(DemoProgress::in_memory().file(), None);
        assert!(DemoProgress::in_memory().save().is_ok());
    }

    const LEVELS: [u32; 4] = [1, 2, 3, 4];
    const DEMOS: [(u32, &str); 6] = [
        (1, "a1"),
        (1, "a2"),
        (2, "b"),
        (3, "c1"),
        (3, "c2"),
        (4, "d"),
    ];

    fn status(opened: &[&str]) -> RoadStatus {
        road_status(
            &LEVELS,
            &DEMOS,
            &DemoProgress::with_opened(opened.iter().copied()),
        )
    }

    #[test]
    fn next_stop_starts_at_the_first_level() {
        let s = status(&[]);
        assert_eq!(s.current, None);
        assert_eq!(s.next, Some(0));
        assert_eq!(s.travelled_to, None);
        assert_eq!((s.opened, s.total), (0, 6));
        assert!(s.stops.iter().all(|&st| st == StopState::Unopened));
    }

    #[test]
    fn next_stop_is_the_first_unfinished_stop_after_the_current_one() {
        // Level 3 is current; level 4 is next even though level 3 has a
        // demo left (as in the mockup: here, then onwards).
        let s = status(&["a1", "a2", "b", "c1"]);
        assert_eq!(s.current, Some(2));
        assert_eq!(s.next, Some(3));
        assert_eq!(s.travelled_to, Some(2));
        assert_eq!(
            s.stops,
            [
                StopState::Visited,
                StopState::Visited,
                StopState::Current,
                StopState::Unopened
            ]
        );
        assert_eq!(s.opened, 4);

        // At the end of the road with gaps behind: back to the first gap.
        let s = status(&["a1", "d"]);
        assert_eq!(s.current, Some(3));
        assert_eq!(s.next, Some(0), "a2 is left at stop 1");
        assert_eq!(s.travelled_to, Some(3));

        // Going back down the road: the road stays travelled to the
        // furthest stop, "You are here" moves back.
        let s = status(&["c1", "b"]);
        assert_eq!(s.current, Some(1));
        assert_eq!(s.next, Some(2), "c2 is left at stop 3");
        assert_eq!(s.travelled_to, Some(2));

        // Everything opened: no next stop.
        let s = status(&["a1", "a2", "b", "c1", "c2", "d"]);
        assert_eq!(s.next, None);
        assert_eq!((s.opened, s.total), (6, 6));
    }

    #[test]
    fn a_gallery_item_last_keeps_you_at_the_furthest_stop() {
        let mut p = DemoProgress::with_opened(["a1", "b"]);
        p.mark_opened("some_gallery_item");
        let s = road_status(&LEVELS, &DEMOS, &p);
        assert_eq!(s.current, Some(1));
        assert_eq!(s.opened, 2, "gallery items are not counted");
    }
}
