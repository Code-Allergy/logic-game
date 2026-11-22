//! Which demos the player has opened and which challenges they have
//! completed, for the demo map.
//!
//! On native builds the record is a small JSON file next to the saved
//! workspaces, `<workspaces>/.demo-progress.json` (the leading dot keeps it
//! out of the workspace list; the folder is
//! [`crate::editor::workspace::WorkspaceStore::player`]'s), written
//! atomically like a workspace manifest (to `.demo-progress.json.tmp`,
//! then renamed, the previous file kept as `.demo-progress.json.bak`). A
//! file that cannot be read is kept as `.demo-progress.json.corrupt`
//! before the record starts over:
//!
//! ```json
//! { "format": "gatequest-progress", "version": 2,
//!   "opened": ["counter", "gates"], "last": "counter",
//!   "completed": ["half_adder"] }
//! ```
//!
//! `opened` holds demo and gallery ids (sorted); `last` is the one opened
//! most recently; `completed` holds the ids of the challenges whose check
//! passed (sorted, added in version 2; version 1 files still load, with
//! none completed). Ids that are no longer in the catalog are kept but not
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

/// The progress file, inside the workspaces folder.
pub const PROGRESS_FILE: &str = ".demo-progress.json";
pub const PROGRESS_FORMAT: &str = "gatequest-progress";
pub const PROGRESS_VERSION: u32 = 2;
/// Older versions this build still reads.
const READABLE_VERSIONS: [u32; 2] = [1, 2];

#[derive(Debug, Serialize, Deserialize)]
struct ProgressFile {
    format: String,
    version: u32,
    #[serde(default)]
    opened: Vec<String>,
    #[serde(default)]
    last: Option<String>,
    #[serde(default)]
    completed: Vec<String>,
}

/// The opened demos and where they are kept.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct DemoProgress {
    opened: BTreeSet<String>,
    last: Option<String>,
    /// Challenges whose check passed.
    completed: BTreeSet<String>,
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

    /// The player's record: on native from [`PROGRESS_FILE`] in the
    /// player's workspaces folder (read from the old `./workspaces` when
    /// only that one has it), in memory on the web.
    pub fn for_player() -> Self {
        match crate::editor::workspace::WorkspaceStore::player() {
            Some(store) => match store.legacy() {
                Some(old) if !store.dir().join(PROGRESS_FILE).exists() => {
                    Self::load_from(&old.join(PROGRESS_FILE), store.dir())
                }
                _ => Self::load_in(store.dir()),
            },
            None => Self::in_memory(),
        }
    }

    /// The record in `dir`, saved back there on change. A missing file is
    /// an empty record; an unreadable one is logged, kept as
    /// `<file>.corrupt` and starts empty (the file is replaced on the next
    /// change).
    pub fn load_in(dir: &Path) -> Self {
        Self::load_from(&dir.join(PROGRESS_FILE), dir)
    }

    /// The record read from `source`, saved to [`PROGRESS_FILE`] in `dir`.
    fn load_from(source: &Path, dir: &Path) -> Self {
        let path = source;
        let mut progress = match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).unwrap_or_else(|e| {
                warn!("Ignoring {}: {e}", path.display());
                if source.parent() == Some(dir) {
                    let mut corrupt = path.as_os_str().to_os_string();
                    corrupt.push(".corrupt");
                    if let Err(e) = std::fs::copy(path, &corrupt) {
                        warn!("Cannot back up {}: {e}", path.display());
                    }
                }
                Self::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!("Cannot read {}: {e}", path.display());
                Self::default()
            }
        };
        progress.file = Some(dir.join(PROGRESS_FILE));
        progress
    }

    /// Parse the file's text (the record is in memory until saved).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: ProgressFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if file.format != PROGRESS_FORMAT || !READABLE_VERSIONS.contains(&file.version) {
            return Err(format!(
                "expected format \"{PROGRESS_FORMAT}\" version {PROGRESS_VERSION}"
            ));
        }
        Ok(Self {
            opened: file.opened.into_iter().collect(),
            last: file.last,
            completed: file.completed.into_iter().collect(),
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
            completed: self.completed.iter().cloned().collect(),
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
        crate::editor::workspace::write_atomic(path, self.to_json().as_bytes())
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

    /// Whether challenge `id` has been passed.
    pub fn is_completed(&self, id: &str) -> bool {
        self.completed.contains(id)
    }

    /// Record that challenge `id` passed its check. Returns whether
    /// anything changed.
    pub fn mark_completed(&mut self, id: &str) -> bool {
        self.completed.insert(id.to_string())
    }

    /// Tick or untick challenge `id` (the box on a challenge card).
    pub fn set_completed(&mut self, id: &str, completed: bool) -> bool {
        if completed {
            self.mark_completed(id)
        } else {
            self.completed.remove(id)
        }
    }

    /// How many of `ids` are completed.
    pub fn completed_of<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> usize {
        ids.into_iter().filter(|id| self.is_completed(id)).count()
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
    fn completed_challenges_round_trip_and_version_1_still_loads() {
        let dir = temp_dir();
        let mut progress = DemoProgress::load_in(&dir);
        assert!(progress.mark_completed("half_adder"));
        assert!(!progress.mark_completed("half_adder"), "nothing new");
        assert!(progress.mark_completed("and_from_nand"));
        progress.mark_opened("gates");
        progress.save().unwrap();
        let again = DemoProgress::load_in(&dir);
        assert_eq!(again, progress);
        assert!(again.is_completed("half_adder") && !again.is_completed("mux2"));
        assert_eq!(
            again.completed_of(["half_adder", "mux2", "and_from_nand"]),
            2
        );
        let value: serde_json::Value = serde_json::from_str(&again.to_json()).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(
            value["completed"],
            serde_json::json!(["and_from_nand", "half_adder"])
        );
        let mut again = again;
        assert!(again.set_completed("half_adder", false));
        assert!(!again.is_completed("half_adder"));
        std::fs::remove_dir_all(&dir).unwrap();

        // A version 1 file (before challenges) loads with none completed.
        let old = DemoProgress::from_json(
            r#"{"format": "gatequest-progress", "version": 1, "opened": ["gates"], "last": "gates"}"#,
        )
        .unwrap();
        assert!(old.is_opened("gates") && !old.is_completed("gates"));
        assert_eq!(old.last(), Some("gates"));
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
        let mut p = DemoProgress::load_in(&dir);
        assert!(p.is_empty());
        assert!(p.file().is_some(), "still saved there on change");
        // The unreadable file is kept before the record starts over, and
        // survives the next saves.
        let corrupt = dir.join(format!("{PROGRESS_FILE}.corrupt"));
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "not json");
        p.mark_opened("gates");
        p.save().unwrap();
        p.mark_opened("counter");
        p.save().unwrap();
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "not json");
        // The save before the last one is the backup.
        let bak = std::fs::read_to_string(dir.join(format!("{PROGRESS_FILE}.bak"))).unwrap();
        assert_eq!(DemoProgress::from_json(&bak).unwrap().last(), Some("gates"));
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
