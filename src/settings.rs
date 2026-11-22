//! The player's display settings: the UI scale and the theme.
//!
//! On native builds they are a small JSON file next to the demo progress,
//! `.settings.json` in the workspaces folder of the player's data folder
//! ([`settings_dir`]; see the README's Saving section), written atomically
//! (to `.settings.json.tmp`, then renamed) whenever they change:
//!
//! ```json
//! { "format": "gatequest-settings", "version": 1,
//!   "ui_scale": "auto", "theme": "night" }
//! ```
//!
//! `ui_scale` is `"auto"` or a percentage (`90`, `100`, `115`, `130`).
//! The web build keeps the settings in memory for the session.
//!
//! The load / save API is pure ([`Settings::from_json`],
//! [`Settings::to_json`], [`Settings::load_in`]); [`SettingsPlugin`]
//! loads them at start-up, applies the theme, and saves after every
//! change of the scale or the theme.

use std::io;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::theme::{Theme, ThemeMode};

/// The settings file, inside [`settings_dir`].
pub const SETTINGS_FILE: &str = ".settings.json";
pub const SETTINGS_FORMAT: &str = "gatequest-settings";
pub const SETTINGS_VERSION: u32 = 1;

/// The folder the settings file lives in: next to the demo progress, in
/// the player's workspaces folder
/// ([`WorkspaceStore::player`](crate::editor::workspace::WorkspaceStore::player):
/// `workspaces` in the per-user data folder, or `GATEQUEST_DATA_DIR`),
/// else `./workspaces`.
pub fn settings_dir() -> PathBuf {
    crate::editor::workspace::WorkspaceStore::player().map_or_else(
        || PathBuf::from(crate::WORKSPACE_DIR),
        |store| store.dir().to_path_buf(),
    )
}

/// The UI scale presets, in percent.
pub const UI_SCALE_PRESETS: [u16; 4] = [90, 100, 115, 130];

/// How large the HUD and menus are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiScale {
    /// Picked from the window's size (see [`auto_ui_scale`]).
    #[default]
    Auto,
    /// A fixed percentage (one of [`UI_SCALE_PRESETS`]).
    Percent(u16),
}

impl UiScale {
    /// Every choice, in the order the settings list them.
    pub fn choices() -> impl Iterator<Item = UiScale> {
        std::iter::once(UiScale::Auto).chain(UI_SCALE_PRESETS.map(UiScale::Percent))
    }

    /// The label of a choice (`Auto`, `115%`).
    pub fn label(self) -> String {
        match self {
            UiScale::Auto => "Auto".into(),
            UiScale::Percent(p) => format!("{p}%"),
        }
    }

    /// The percentage asked for in a window of `window` logical pixels
    /// (before [`effective_ui_scale`] limits it).
    pub fn requested_percent(self, window: Vec2) -> u16 {
        match self {
            UiScale::Auto => auto_ui_percent(window),
            UiScale::Percent(p) => p,
        }
    }

    fn to_value(self) -> serde_json::Value {
        match self {
            UiScale::Auto => "auto".into(),
            UiScale::Percent(p) => p.into(),
        }
    }

    fn from_value(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(s) if s == "auto" => Some(UiScale::Auto),
            serde_json::Value::Number(n) => {
                let p = u16::try_from(n.as_u64()?).ok()?;
                UI_SCALE_PRESETS.contains(&p).then_some(UiScale::Percent(p))
            }
            _ => None,
        }
    }
}

/// The smallest HUD canvas, in points, the layout is made for
/// ([`crate::ui_style::HudLayout`]): a scale never shrinks the window's
/// points below this.
pub const MIN_UI_POINTS: Vec2 = Vec2::new(1280.0, 720.0);
/// The size the full (not compact) HUD layout starts at.
pub const FULL_UI_POINTS: Vec2 = Vec2::new(1600.0, 900.0);

/// The automatic scale, in percent, for a window of `window` logical
/// pixels (the window's physical size divided by its DPI scale factor, so
/// a HiDPI screen is already accounted for): 100%, or on a large window
/// the largest preset that still leaves the full-size HUD layout its room
/// (1600 x 900 points): 115% at 1920 x 1080, 130% at 2560 x 1440.
pub fn auto_ui_percent(window: Vec2) -> u16 {
    let room = (window / FULL_UI_POINTS).min_element();
    UI_SCALE_PRESETS
        .into_iter()
        .filter(|&p| p >= 100 && f32::from(p) <= room * 100.0 + 0.01)
        .max()
        .unwrap_or(100)
}

/// The largest scale, in percent, a `window` (logical pixels) takes
/// without the HUD's canvas in points falling below [`MIN_UI_POINTS`]
/// (at least 90%, the smallest preset).
pub fn max_ui_percent(window: Vec2) -> u16 {
    let room = (window / MIN_UI_POINTS).min_element();
    ((room * 100.0 + 0.01).floor() as u16).max(90)
}

/// The scale factor actually applied: the requested one, limited so the
/// HUD cards keep their room ([`max_ui_percent`]).
pub fn effective_ui_scale(scale: UiScale, window: Vec2) -> f32 {
    f32::from(scale.requested_percent(window).min(max_ui_percent(window))) / 100.0
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    format: String,
    version: u32,
    #[serde(default)]
    ui_scale: serde_json::Value,
    #[serde(default)]
    theme: Option<String>,
}

/// The display settings and where they are kept.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub ui_scale: UiScale,
    pub theme: ThemeMode,
    /// The file the settings are saved to after every change; `None` keeps
    /// them in memory (the web build, tests, tools).
    file: Option<PathBuf>,
}

impl Settings {
    /// Default settings kept in memory.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The player's settings: on native from [`SETTINGS_FILE`] in
    /// [`settings_dir`], in memory on the web.
    pub fn for_player() -> Self {
        if cfg!(target_arch = "wasm32") {
            Self::in_memory()
        } else {
            Self::load_in(&settings_dir())
        }
    }

    /// The settings in `dir`, saved back there on change. A missing file
    /// gives the defaults; an unreadable one is logged and gives the
    /// defaults (it is replaced on the next change).
    pub fn load_in(dir: &Path) -> Self {
        let path = dir.join(SETTINGS_FILE);
        let mut settings = match std::fs::read_to_string(&path) {
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
        settings.file = Some(path);
        settings
    }

    /// Parse the file's text. Unknown or missing values fall back to the
    /// defaults.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: SettingsFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if file.format != SETTINGS_FORMAT || file.version != SETTINGS_VERSION {
            return Err(format!(
                "expected format \"{SETTINGS_FORMAT}\" version {SETTINGS_VERSION}"
            ));
        }
        Ok(Self {
            ui_scale: UiScale::from_value(&file.ui_scale).unwrap_or_default(),
            theme: match file.theme.as_deref() {
                Some("night") => ThemeMode::Night,
                _ => ThemeMode::Day,
            },
            file: None,
        })
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        let theme = match self.theme {
            ThemeMode::Day => "day",
            ThemeMode::Night => "night",
        };
        let file = SettingsFile {
            format: SETTINGS_FORMAT.into(),
            version: SETTINGS_VERSION,
            ui_scale: self.ui_scale.to_value(),
            theme: Some(theme.into()),
        };
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    /// Where the settings are saved, if anywhere.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Write the settings to their file (atomically); nothing for
    /// in-memory settings.
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
}

/// Keep the theme in the settings, and save the settings when they change.
pub fn persist_settings_system(theme: Res<Theme>, mut settings: ResMut<Settings>) {
    if theme.is_changed() && settings.theme != theme.mode {
        settings.theme = theme.mode;
    }
    if settings.is_changed()
        && !settings.is_added()
        && let Err(e) = settings.save()
    {
        warn!("Cannot save the settings: {e}");
    }
}

/// Loads the player's settings (unless a [`Settings`] resource is already
/// there, e.g. in-memory settings for a tool), applies the saved theme,
/// and saves the settings on change.
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<Settings>() {
            app.insert_resource(Settings::for_player());
        }
        let mode = app.world().resource::<Settings>().theme;
        app.insert_resource(Theme::of(mode))
            .add_systems(Update, persist_settings_system);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_json() {
        for ui_scale in UiScale::choices() {
            for theme in [ThemeMode::Day, ThemeMode::Night] {
                let s = Settings {
                    ui_scale,
                    theme,
                    file: None,
                };
                assert_eq!(Settings::from_json(&s.to_json()).unwrap(), s);
            }
        }
        let text = Settings::default().to_json();
        assert!(text.contains("\"ui_scale\": \"auto\""), "{text}");
    }

    #[test]
    fn unknown_values_fall_back_to_the_defaults() {
        let text = r#"{"format":"gatequest-settings","version":1,"ui_scale":72,"theme":"dusk"}"#;
        assert_eq!(Settings::from_json(text).unwrap(), Settings::default());
        let text = r#"{"format":"gatequest-settings","version":1}"#;
        assert_eq!(Settings::from_json(text).unwrap(), Settings::default());
        assert!(Settings::from_json(r#"{"format":"other","version":1}"#).is_err());
        assert!(Settings::from_json("not json").is_err());
    }

    #[test]
    fn settings_save_and_load_from_a_folder() {
        let dir = std::env::temp_dir().join(format!("gq-settings-{}", uuid::Uuid::new_v4()));
        let mut s = Settings::load_in(&dir);
        assert_eq!((s.ui_scale, s.theme), (UiScale::Auto, ThemeMode::Day));
        s.ui_scale = UiScale::Percent(115);
        s.theme = ThemeMode::Night;
        s.save().unwrap();
        let back = Settings::load_in(&dir);
        assert_eq!((back.ui_scale, back.theme), (s.ui_scale, s.theme));
        assert_eq!(back.file(), Some(dir.join(SETTINGS_FILE).as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
        // In memory: nothing is written.
        assert!(Settings::in_memory().save().is_ok());
    }

    #[test]
    fn auto_scale_follows_the_window_size() {
        assert_eq!(auto_ui_percent(Vec2::new(1280.0, 720.0)), 100);
        assert_eq!(auto_ui_percent(Vec2::new(1600.0, 1000.0)), 100);
        assert_eq!(auto_ui_percent(Vec2::new(1920.0, 1080.0)), 115);
        assert_eq!(auto_ui_percent(Vec2::new(2560.0, 1440.0)), 130);
        // A HiDPI laptop: 2880 x 1800 physical at 2x is 1440 x 900 logical.
        assert_eq!(auto_ui_percent(Vec2::new(1440.0, 900.0)), 100);
    }

    #[test]
    fn the_effective_scale_keeps_the_hud_its_room() {
        let scale = |s, x, y| effective_ui_scale(s, Vec2::new(x, y));
        assert_eq!(scale(UiScale::Percent(130), 1280.0, 720.0), 1.0);
        assert_eq!(scale(UiScale::Percent(90), 1280.0, 720.0), 0.9);
        assert_eq!(scale(UiScale::Percent(130), 1920.0, 1080.0), 1.3);
        assert_eq!(scale(UiScale::Percent(115), 1920.0, 1080.0), 1.15);
        assert_eq!(scale(UiScale::Percent(115), 1366.0, 768.0), 1.06);
        assert_eq!(scale(UiScale::Auto, 2560.0, 1440.0), 1.3);
        for s in UiScale::choices() {
            for (x, y) in [(1280.0, 720.0), (1366.0, 768.0), (2560.0, 1440.0)] {
                let window = Vec2::new(x, y);
                let points = window / effective_ui_scale(s, window);
                assert!(
                    points.x >= MIN_UI_POINTS.x - 0.5 && points.y >= MIN_UI_POINTS.y - 0.5,
                    "{s:?} at {x}x{y}: {points}"
                );
            }
        }
    }
}
