use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{
    EguiContexts,
    egui::{self, Align2, CornerRadius, Order, Sense, StrokeKind},
};
use uuid::Uuid;

use crate::theme::{Theme, ThemeMode};
use crate::ui_style::{
    HudIcon, HudLayout, ICON_BUTTON_WIDTH, TOP_CARD_HEIGHT, UNDO_BUTTONS_WIDTH, caption, card,
    chunky_button, chunky_button_width, chunky_icon_button, day_night_button, egui_color, heavy,
    stroke,
};
use crate::{
    GameState,
    sim::{
        ActiveCircuit, CircuitLibrary, CircuitView, CloseView, CreateGate, CreateWire, Demo,
        GateIdentity, GateKind, GateLabel, GateType, InstanceViewItem, OpenDefinitionView,
        RegisterDefinitions, WorkspaceDocument, WorkspaceRevision, demos,
        rtl::{ImportOptions, import_yosys_json},
    },
};

use super::files::{OpenRequest, PendingOpen, open_now, request_open};
use super::hud::hud_layout;
use super::workspace::{
    WEB_SAVE_UNAVAILABLE, load_saved_workspace_names, name_key, read_workspace,
};
use super::{EditorState, LoadWorkspaceEvent, SaveWorkspaceEvent};
use crate::sim::HistoryCommand;

/// The web build has no file system (yet): no saving, loading or placing
/// saved workspaces there.
const CAN_SAVE: bool = cfg!(not(target_arch = "wasm32"));

fn gate_name(label: &GateLabel, ty: &GateType) -> String {
    if label.value.trim().is_empty() {
        ty.kind.display_name().to_string()
    } else {
        format!("{} ({})", label.value, ty.kind.display_name())
    }
}

type ChangedRootGate = (
    Changed<GateType>,
    Without<InstanceViewItem>,
    Allow<Disabled>,
);

/// The open view and the messages that change it, plus the root circuit's
/// gates (also while parked behind a subcircuit view).
#[derive(SystemParam)]
pub struct EditorViewParams<'w, 's> {
    view: Res<'w, CircuitView>,
    close: MessageWriter<'w, CloseView>,
    open_definition: MessageWriter<'w, OpenDefinitionView>,
    root_gates: Query<'w, 's, &'static GateType, (Without<InstanceViewItem>, Allow<Disabled>)>,
    changed_root_gates: Query<'w, 's, (), ChangedRootGate>,
    removed_gates: RemovedComponents<'w, 's, GateType>,
    /// The definition table, rebuilt only when the root's gates or the
    /// library change.
    table: Local<'s, Option<Vec<(Uuid, String, usize)>>>,
}

impl EditorViewParams<'_, '_> {
    fn definition_table(&mut self, library: &Res<CircuitLibrary>) -> Vec<(Uuid, String, usize)> {
        let removed = self.removed_gates.read().count() > 0;
        if self.table.is_none()
            || removed
            || library.is_changed()
            || !self.changed_root_gates.is_empty()
        {
            *self.table = Some(definition_table(
                self.root_gates.iter().map(|t| &t.kind),
                library,
            ));
        }
        self.table.clone().unwrap_or_default()
    }
}

/// Every definition the root circuit uses, with how many instances of it
/// the running simulation has (counting nested ones), sorted by name.
fn definition_table<'a>(
    root_kinds: impl Iterator<Item = &'a GateKind>,
    library: &CircuitLibrary,
) -> Vec<(Uuid, String, usize)> {
    fn visit(def: Uuid, library: &CircuitLibrary, counts: &mut HashMap<Uuid, usize>, depth: usize) {
        if depth > 64 {
            return;
        }
        *counts.entry(def).or_default() += 1;
        if let Some(doc) = library.get(def) {
            for child in doc.instanced_definitions() {
                visit(child, library, counts, depth + 1);
            }
        }
    }
    let mut counts = HashMap::new();
    for kind in root_kinds {
        if let GateKind::SubCircuit { definition } = kind {
            visit(*definition, library, &mut counts, 0);
        }
    }
    let mut rows: Vec<(Uuid, String, usize)> = counts
        .into_iter()
        .map(|(id, n)| {
            (
                id,
                library
                    .get(id)
                    .map_or_else(|| "(missing)".into(), |d| d.name.clone()),
                n,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    rows
}

/// The messages the file card and Library drawer send.
#[derive(SystemParam)]
pub struct EditorWriters<'w> {
    create_gate: MessageWriter<'w, CreateGate>,
    create_wire: MessageWriter<'w, CreateWire>,
    register_definitions: MessageWriter<'w, RegisterDefinitions>,
    load: MessageWriter<'w, LoadWorkspaceEvent>,
    save: MessageWriter<'w, SaveWorkspaceEvent>,
}

/// The save state and the keyboard (Ctrl+S / Cmd+S).
#[derive(SystemParam)]
pub struct FileParams<'w> {
    revision: Res<'w, WorkspaceRevision>,
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
}

/// Ctrl+S (Cmd+S on macOS; either in the browser) was pressed.
pub fn save_shortcut(keys: &ButtonInput<KeyCode>) -> bool {
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let cmd = keys.any_pressed([KeyCode::SuperLeft, KeyCode::SuperRight]);
    let modifier = if cfg!(target_os = "macos") {
        cmd
    } else if cfg!(target_arch = "wasm32") {
        ctrl || cmd
    } else {
        ctrl
    };
    modifier && keys.just_pressed(KeyCode::KeyS)
}

/// The player's answer to the unsaved-changes question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Save,
    Discard,
    Cancel,
}

/// The unsaved-changes question, over everything (Esc or a click outside
/// cancels).
fn confirm_card(
    ctx: &egui::Context,
    palette: &crate::theme::Palette,
    name: &str,
    pending: &PendingOpen,
) -> Option<Answer> {
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("hud_unsaved_changes"))
        .frame(card(palette).inner_margin(egui::Margin::same(18)))
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(egui::RichText::new("Unsaved changes").font(heavy(18.0)));
            ui.add_space(6.0);
            ui.label(format!(
                "\"{name}\" has changes that are not saved. Save them before you {}?",
                pending.request.describe()
            ));
            if !CAN_SAVE {
                ui.add_space(4.0);
                ui.colored_label(
                    egui_color(palette.text_secondary),
                    format!("{WEB_SAVE_UNAVAILABLE} Discarding loses the changes."),
                );
            }
            if let Some(error) = &pending.error {
                ui.add_space(4.0);
                ui.colored_label(egui_color(palette.fault_text), error);
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if CAN_SAVE
                    && ui
                        .add_enabled_ui(!pending.awaiting_save, |ui| {
                            chunky_button(ui, palette, "Save", true)
                        })
                        .inner
                        .clicked()
                {
                    answer = Some(Answer::Save);
                }
                if chunky_button(ui, palette, "Discard", false)
                    .on_hover_text("Throw the changes away")
                    .clicked()
                {
                    answer = Some(Answer::Discard);
                }
                if chunky_button(ui, palette, "Cancel", false)
                    .on_hover_text("Keep working on this workspace (Esc)")
                    .clicked()
                {
                    answer = Some(Answer::Cancel);
                }
            });
        });
    if answer.is_none() && modal.should_close() {
        answer = Some(Answer::Cancel);
    }
    answer
}

/// Where the open circuit comes from, for the file card's breadcrumb.
fn origin(read_only: bool, demo: Option<&Demo>) -> String {
    match demo {
        Some(demo) if demo.level > 0 => format!("Demos › Level {}", demo.level),
        Some(_) => "Gallery".to_string(),
        None if read_only => "Read-only".to_string(),
        None => "My workshop".to_string(),
    }
}

/// The file card's second line: origin, the open view's path, and
/// whether the work is saved (or read-only).
pub fn file_subtitle(origin: &str, breadcrumb: &str, read_only: bool, dirty: bool) -> String {
    let (path, state) = file_subtitle_parts(origin, breadcrumb, read_only, dirty);
    format!("{path} · {state}")
}

/// [`file_subtitle`] in its two parts: the path (which the card may
/// shorten) and the state (which it always shows whole).
pub fn file_subtitle_parts(
    origin: &str,
    breadcrumb: &str,
    read_only: bool,
    dirty: bool,
) -> (String, &'static str) {
    let state = match (read_only, dirty) {
        (true, _) => "read-only",
        (false, true) => "unsaved",
        (false, false) => "saved",
    };
    (
        format!("{origin} › {}", breadcrumb.replace(" / ", " › ")),
        state,
    )
}

/// What the file card shows.
pub struct FileCardView<'a> {
    pub title: &'a str,
    /// Origin and the open view's path ([`file_subtitle_parts`]).
    pub path: &'a str,
    /// `saved`, `unsaved` or `read-only`.
    pub state: &'a str,
    /// The subtitle's tooltip.
    pub note: &'a str,
    /// Subcircuit views open: one shows a back button, two or more also
    /// a back-to-root button.
    pub depth: usize,
    pub library_open: bool,
    /// The Undo and Redo buttons, in the card: whether there is something
    /// to undo and to redo. `None` for a read-only demo, or when they
    /// have a card of their own ([`HudLayout::undo_in_file_card`]).
    pub undo: Option<(bool, bool)>,
}

/// What was clicked on the file card this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileCardClicks {
    pub back: bool,
    pub root: bool,
    pub map: bool,
    pub library: bool,
    pub theme: Option<ThemeMode>,
    pub history: Option<HistoryCommand>,
}

/// Card padding and outline, left and right, of the file card.
const FILE_CARD_PAD: (i8, i8) = (10, 12);
/// The badge, and the space after it and after the text.
const FILE_BADGE: f32 = 38.0;
const FILE_TEXT_GAP: f32 = 12.0;
const FILE_BUTTON_GAP: f32 = 8.0;
/// The most the title and subtitle take.
const FILE_TEXT_MAX: f32 = 300.0;
/// The least room for the path before the save state; with less, the
/// card shows the state alone.
const FILE_PATH_MIN: f32 = 40.0;
/// The Library button keeps its word while the text keeps this much.
const FILE_TEXT_WITH_LIBRARY_WORD: f32 = 200.0;

/// The file card's contents (top left), at most `layout.file_rect()`
/// wide: the badge, the title and subtitle (shortened to fit, the save
/// state always whole), and the back, map, library and day/night
/// buttons.
pub fn file_card(
    ui: &mut egui::Ui,
    theme: &Theme,
    view: &FileCardView,
    layout: &HudLayout,
) -> FileCardClicks {
    let palette = &theme.palette;
    let mut clicks = FileCardClicks::default();
    let frame = card(palette).inner_margin(egui::Margin {
        left: FILE_CARD_PAD.0,
        right: FILE_CARD_PAD.1,
        top: 0,
        bottom: 0,
    });
    let outline = frame.total_margin().sum().x;
    let room = layout.file_rect().width() - outline;
    frame.show(ui, |ui| {
        ui.set_height(TOP_CARD_HEIGHT - frame.total_margin().sum().y);
        ui.set_max_width(room);
        let back = match view.depth {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        let back_w: f32 = ["←", "⇤"][..back]
            .iter()
            .map(|label| chunky_button_width(ui, *label))
            .sum();
        let buttons = back + 3; // back, map, library, day/night
        let undo_w = if view.undo.is_some() {
            UNDO_BUTTONS_WIDTH
        } else {
            0.0
        };
        let fixed = FILE_BADGE
            + 2.0 * FILE_TEXT_GAP
            + undo_w
            + back_w
            + 2.0 * ICON_BUTTON_WIDTH
            + (buttons - 1) as f32 * FILE_BUTTON_GAP;
        let word = chunky_button_width(ui, "☰ Library");
        let icon = chunky_button_width(ui, "☰");
        let library_label = if !layout.compact && room - fixed - word >= FILE_TEXT_WITH_LIBRARY_WORD
        {
            "☰ Library"
        } else {
            "☰"
        };
        let library_w = if library_label == "☰" { icon } else { word };
        let text_width = (room - fixed - library_w).min(FILE_TEXT_MAX);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = FILE_TEXT_GAP;
            let (badge, _) =
                ui.allocate_exact_size(egui::vec2(FILE_BADGE, FILE_BADGE), Sense::hover());
            ui.painter().rect(
                badge,
                CornerRadius::same(10),
                egui_color(palette.on),
                stroke(2.5, egui_color(palette.ink)),
                StrokeKind::Inside,
            );
            ui.painter().text(
                badge.center(),
                Align2::CENTER_CENTER,
                "GQ",
                heavy(15.0),
                egui_color(palette.on_ink),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.set_max_width(text_width);
                ui.add_space(9.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(view.title).font(heavy(16.0))).truncate(),
                );
                let secondary = egui_color(palette.text_secondary);
                let state = format!(" · {}", view.state);
                let state_w = ui
                    .painter()
                    .layout_no_wrap(state.clone(), egui::FontId::proportional(12.0), secondary)
                    .size()
                    .x;
                let path_w = text_width - state_w;
                // One text line tall, not a widget row.
                ui.spacing_mut().interact_size.y = 0.0;
                let crumb = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        // Too little room for a useful path: the state alone
                        // (the tooltip has the path).
                        if path_w < FILE_PATH_MIN {
                            ui.label(egui::RichText::new(view.state).size(12.0).color(secondary));
                            return;
                        }
                        ui.scope(|ui| {
                            ui.set_max_width(path_w);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(view.path).size(12.0).color(secondary),
                                )
                                .truncate(),
                            );
                        });
                        ui.label(egui::RichText::new(state).size(12.0).color(secondary));
                    })
                    .response;
                crumb.on_hover_text(view.note);
            });
            ui.spacing_mut().item_spacing.x = FILE_BUTTON_GAP;
            if let Some(can) = view.undo {
                clicks.history = super::undo::undo_buttons(ui, palette, can);
            }
            if back >= 1 {
                clicks.back = chunky_button(ui, palette, "←", false)
                    .on_hover_text("Back to the parent circuit")
                    .clicked();
            }
            if back >= 2 {
                clicks.root = chunky_button(ui, palette, "⇤", false)
                    .on_hover_text("Back to the root circuit")
                    .clicked();
            }
            clicks.map = chunky_icon_button(ui, palette, HudIcon::Map, false)
                .on_hover_text("Demo map: the road from one gate to a CPU")
                .clicked();
            clicks.library = chunky_button(ui, palette, library_label, view.library_open)
                .on_hover_text("Workspaces, demos, gallery, import and save")
                .clicked();
            clicks.theme = day_night_button(ui, theme);
        });
    });
    clicks
}

/// The file card (top left), the Library drawer it opens, the wire
/// builder (in wire mode) and placement of the armed part by click.
#[allow(clippy::too_many_arguments)]
pub fn editor_ui_system(
    mut contexts: EguiContexts,
    mut editor_state: ResMut<EditorState>,
    mut writers: EditorWriters,
    mut views: EditorViewParams,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
    (game_state, active): (Res<State<GameState>>, Res<ActiveCircuit>),
    files: FileParams,
    library: Res<CircuitLibrary>,
    gates: Query<(&GateIdentity, &GateType, &GateLabel)>,
    mut theme: ResMut<Theme>,
    mut next_state: ResMut<NextState<GameState>>,
    mut seen_active: Local<Option<(Uuid, String)>>,
    mut undo: super::undo::UndoControls,
    (progress, active_challenge): (
        Res<super::demo_map::progress::DemoProgress>,
        Res<super::challenge::ActiveChallenge>,
    ),
) -> Result<(), BevyError> {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let palette = theme.palette;
    let info = egui_color(palette.io_text);
    let warn = egui_color(palette.on_text);
    let error = egui_color(palette.fault_text);

    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let Ok(window) = q_window.single() else {
        return Ok(());
    };
    let Ok((camera, camera_transform)) = q_camera.single() else {
        return Ok(());
    };
    let layout = hud_layout(ctx);

    // Drop stale wire-builder picks (deleted gates).
    if editor_state
        .connection_source
        .is_some_and(|e| !gates.contains(e))
    {
        editor_state.clear_connection();
    }
    if editor_state
        .connection_sink
        .is_some_and(|e| !gates.contains(e))
    {
        editor_state.connection_sink = None;
    }
    // Follow the open circuit, whoever loaded it (the editor, the default
    // demo, a tool): the file card and the save name name what is open.
    let identity = (active.uuid, active.name.clone());
    if seen_active.as_ref() != Some(&identity) {
        editor_state.current_workspace = Some(active.name.clone());
        editor_state.workspace_file_name = active.name.clone();
        *seen_active = Some(identity);
    }
    if editor_state.saved_workspaces.is_none() {
        editor_state.saved_workspaces = Some(load_saved_workspace_names().unwrap_or_default());
    }

    // ── Unsaved changes ────────────────────────────────────────────────
    // A request parked by `request_open` waits here for the player.
    if let Some(mut pending) = editor_state.pending_open.take() {
        if pending.awaiting_save {
            match editor_state.last_save.clone() {
                Some(Ok(_)) if !files.revision.is_dirty() => {
                    open_now(Some(&mut editor_state), &mut writers.load, pending.request);
                    return Ok(());
                }
                Some(Err(err)) => {
                    pending.awaiting_save = false;
                    pending.error = Some(err);
                }
                _ => {}
            }
        }
        match confirm_card(ctx, &palette, &active.name, &pending) {
            Some(Answer::Save) => {
                pending.awaiting_save = true;
                pending.error = None;
                editor_state.last_save = None;
                writers.save.write(SaveWorkspaceEvent {
                    name: active.name.clone(),
                });
                editor_state.pending_open = Some(pending);
            }
            Some(Answer::Discard) => {
                open_now(Some(&mut editor_state), &mut writers.load, pending.request);
                return Ok(());
            }
            Some(Answer::Cancel) => {}
            None => editor_state.pending_open = Some(pending),
        }
    }

    // ── Ctrl+S / Cmd+S ─────────────────────────────────────────────────
    if editor_state.pending_open.is_none()
        && files.keys.as_deref().is_some_and(save_shortcut)
        && !ctx.egui_wants_keyboard_input()
    {
        if CAN_SAVE {
            writers.save.write(SaveWorkspaceEvent {
                name: active.name.clone(),
            });
        } else {
            editor_state.status = Some(WEB_SAVE_UNAVAILABLE.to_string());
        }
    }

    let catalog = demos::catalog();
    let open_demo = catalog.ok().and_then(|c| c.by_root(active.uuid));
    let mut place_catalog: Option<&Demo> = None;
    let mut open_catalog: Option<&Demo> = None;
    let mut copy_demo = false;
    let mut new_workspace = false;
    let mut import_rtl = false;
    let mut load_saved: Option<String> = None;
    let mut place_saved: Option<String> = None;
    let mut close_requested: Option<bool> = None;
    let mut open_definition: Option<Uuid> = None;
    let mut open_map = false;
    let mut open_challenge = None;
    let table = views.definition_table(&library);
    let breadcrumb = views.view.breadcrumb(&active.name);
    let view_note = views.view.frames().last().map(|frame| {
        let instances = table
            .iter()
            .find(|r| r.0 == frame.definition)
            .map_or(0, |r| r.2);
        if views.view.is_live() {
            format!(
                "Instance of \"{}\": values are live from the running simulation. Inputs \
                 are driven by the parent circuit. Edits change the shared definition and \
                 every instance of it ({instances} in this workspace).",
                frame.name
            )
        } else {
            format!(
                "Definition \"{}\" (not instanced here, so no live values). Edits change \
                 the shared definition.",
                frame.name
            )
        }
    });

    // ── File card ──────────────────────────────────────────────────────
    let title = editor_state
        .current_workspace
        .clone()
        .unwrap_or_else(|| active.name.clone());
    let from = origin(active.read_only, open_demo);
    let (read_only, dirty) = (active.read_only, files.revision.is_dirty());
    let (path, state) = file_subtitle_parts(&from, &breadcrumb, read_only, dirty);
    let subtitle = file_subtitle(&from, &breadcrumb, read_only, dirty);
    let card_view = FileCardView {
        title: &title,
        path: &path,
        state,
        note: view_note.as_deref().unwrap_or(&subtitle),
        depth: views.view.frames().len(),
        library_open: editor_state.library_open,
        undo: if undo.in_own_card(&layout) {
            None
        } else {
            undo.state()
        },
    };
    let room = layout.file_rect();
    let mut clicks = FileCardClicks::default();
    egui::Area::new(egui::Id::new("hud_file"))
        .fixed_pos(egui::pos2(room.min.x, room.min.y))
        .order(Order::Middle)
        .show(ctx, |ui| {
            clicks = file_card(ui, &theme, &card_view, &layout);
        });
    if clicks.back {
        close_requested = Some(false);
    }
    if clicks.root {
        close_requested = Some(true);
    }
    open_map |= clicks.map;
    if clicks.library {
        editor_state.library_open = !editor_state.library_open;
    }
    if let Some(mode) = clicks.theme {
        theme.set(mode);
    }
    undo.send(clicks.history);
    undo.card(ctx, &palette, &layout);

    // ── Library drawer ─────────────────────────────────────────────────
    if editor_state.library_open {
        // Below the Undo and Redo card, when they have one.
        let rect = layout.left_column_rect(undo.in_own_card(&layout));
        let frame = card(&palette);
        let inner = egui::vec2(rect.width(), rect.height()) - frame.total_margin().sum();
        egui::Area::new(egui::Id::new("hud_library"))
            .fixed_pos(egui::pos2(rect.min.x, rect.min.y))
            .order(Order::Foreground)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(inner.x);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Library").font(heavy(18.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                                editor_state.library_open = false;
                            }
                        });
                    });
                    egui::ScrollArea::vertical()
                        .max_height(inner.y - 36.0)
                        .min_scrolled_height(inner.y - 36.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
            ui.label(caption(&palette, "This workspace"));
            if active.read_only {
                if let Some(demo) = open_demo {
                    match (&demo.category, &catalog) {
                        (Some(category), Ok(catalog)) => ui.label(format!(
                            "Gallery, {}: {}",
                            catalog.category_title(category),
                            demo.title
                        )),
                        _ => ui.label(format!("Demo, level {}: {}", demo.level, demo.title)),
                    };
                    ui.label(egui::RichText::new(&demo.description).size(12.0));
                }
                ui.colored_label(
                    info,
                    "Read-only demo: toggle inputs and open subcircuits freely; copy it to edit.",
                );
                if chunky_button(ui, &palette, "Copy to my workspace", true).clicked() {
                    copy_demo = true;
                }
            }
            if let Some(note) = &view_note {
                ui.label(format!("View: {breadcrumb}"));
                ui.colored_label(info, note);
                ui.horizontal(|ui| {
                    if ui.button("← Back to parent circuit").clicked() {
                        close_requested = Some(false);
                    }
                    if views.view.frames().len() > 1 && ui.button("⇤ Back to root").clicked() {
                        close_requested = Some(true);
                    }
                });
            }
            if CAN_SAVE {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor_state.workspace_file_name)
                            .desired_width(f32::INFINITY),
                    );
                });
            }
            ui.horizontal(|ui| {
                if CAN_SAVE {
                    let typed = editor_state.workspace_file_name.trim().to_string();
                    let save_as = name_key(&typed) != name_key(&active.name);
                    let (label, hint) = if save_as {
                        ("Save as new", "Save a copy under this name; it becomes the open workspace")
                    } else {
                        ("Save", "Save this workspace (Ctrl+S)")
                    };
                    if ui
                        .add_enabled(!active.read_only && !typed.is_empty(), egui::Button::new(label))
                        .on_hover_text(hint)
                        .clicked()
                    {
                        writers.save.write(SaveWorkspaceEvent { name: typed });
                    }
                }
                if ui.button("New empty workspace").clicked() {
                    new_workspace = true;
                }
            });
            if !CAN_SAVE {
                ui.colored_label(
                    info,
                    format!("{WEB_SAVE_UNAVAILABLE} Your edits last until you close the page."),
                );
            }
            // Import RTL… (native only: the web build has no file system).
            if cfg!(not(target_arch = "wasm32")) {
                ui.horizontal(|ui| {
                    ui.label("Import RTL…")
                        .on_hover_text("Path of a Yosys JSON netlist (write_json); see docs/rtl-import.md");
                    if ui.button("Import").clicked() && !editor_state.rtl_import_path.trim().is_empty() {
                        import_rtl = true;
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut editor_state.rtl_import_path)
                        .hint_text("path/to/netlist.json")
                        .desired_width(f32::INFINITY),
                );
            }
            if let Some(status) = &editor_state.status {
                ui.colored_label(warn, status);
            }

            ui.separator();
            match catalog {
                Ok(catalog) => {
                    egui::CollapsingHeader::new(egui::RichText::new("Demos").font(heavy(15.0)))
                        .default_open(true)
                        .show(ui, |ui| {
                    ui.label(egui::RichText::new("From gates to a CPU: each level is built from the ones before it. Demos open read-only.").size(12.0));
                    if chunky_button(ui, &palette, "Open the demo map", false)
                        .on_hover_text("The road from one gate to a CPU, with your progress")
                        .clicked()
                    {
                        open_map = true;
                    }
                    let mut level = None;
                    for demo in &catalog.demos {
                        if level != Some(demo.level) {
                            level = Some(demo.level);
                            ui.label(caption(&palette, &format!("Level {}: {}", demo.level, catalog.level_title(demo.level))));
                        }
                        let open = open_demo.is_some_and(|d| d.id == demo.id);
                        ui.horizontal(|ui| {
                            if ui.selectable_label(open, &demo.title).on_hover_text(&demo.description).clicked() {
                                open_catalog = Some(demo);
                            }
                            if ui.small_button("Place").on_hover_text("Place it as a chip").clicked() {
                                place_catalog = Some(demo);
                            }
                        });
                    }
                        });
                    egui::CollapsingHeader::new(egui::RichText::new("Gallery").font(heavy(15.0)))
                        .show(ui, |ui| {
                    ui.label(egui::RichText::new("More circuits, from simple to complex, built from the demos and from each other. They open read-only too.").size(12.0));
                    let mut category = None;
                    for item in &catalog.gallery {
                        if category != item.category.as_deref() {
                            category = item.category.as_deref();
                            ui.label(caption(&palette, catalog.category_title(category.unwrap_or_default())));
                        }
                        let open = open_demo.is_some_and(|d| d.id == item.id);
                        ui.horizontal(|ui| {
                            if ui.selectable_label(open, &item.title).on_hover_text(&item.description).clicked() {
                                open_catalog = Some(item);
                            }
                            if ui.small_button("Place").on_hover_text("Place it as a chip").clicked() {
                                place_catalog = Some(item);
                            }
                        });
                    }
                        });
                }
                Err(e) => {
                    ui.colored_label(error, format!("Cannot load the demos: {e}"));
                }
            }

            egui::CollapsingHeader::new(egui::RichText::new("Challenges").font(heavy(15.0)))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Build it yourself: each opens the ports ready to wire, and Check tests your circuit.").size(12.0));
                    open_challenge = super::challenge::library_list(
                        ui,
                        &palette,
                        &progress,
                        active_challenge.id.as_deref(),
                    );
                });

            egui::CollapsingHeader::new(egui::RichText::new("My workspaces").font(heavy(15.0)))
                .default_open(true)
                .show(ui, |ui| {
            if ui.small_button("Refresh").clicked() {
                editor_state.saved_workspaces = None;
            }
            match editor_state.saved_workspaces.as_deref() {
                _ if !CAN_SAVE => {
                    ui.label(WEB_SAVE_UNAVAILABLE);
                }
                Some([]) | None => {
                    ui.label("No saved workspace files yet.");
                }
                Some(saved) => {
                    for name in saved {
                        ui.horizontal(|ui| {
                            if ui.button(format!("Load {name}")).clicked() {
                                load_saved = Some(name.clone());
                            }
                            if ui.small_button("Place").on_hover_text("Place it as a chip").clicked() {
                                place_saved = Some(name.clone());
                            }
                        });
                    }
                }
            }
                });

            egui::CollapsingHeader::new(egui::RichText::new("Definitions").font(heavy(15.0)))
                .show(ui, |ui| {
            if table.is_empty() {
                ui.label("No subcircuits in this workspace.");
            }
            for (id, name, count) in &table {
                ui.horizontal(|ui| {
                    let short: String = id.to_string().chars().take(8).collect();
                    ui.label(format!("{name} ({short}…): {count} instance(s)"))
                        .on_hover_text(format!("Definition id {id}"));
                    if ui.small_button("Open").clicked() {
                        open_definition = Some(*id);
                    }
                });
            }
                });

            egui::CollapsingHeader::new(egui::RichText::new("Keys and mouse").font(heavy(15.0)))
                .show(ui, |ui| {
            for tip in [
                "1–9: pick a part from the open tab; click or drag it onto the canvas",
                "Left-click: select a part and flip a 1-bit input (inspector: edit, \
                 delete, set wider inputs); empty canvas: deselect",
                "Shift+click: add a part to the selection or take it out · \
                 Shift+drag on empty canvas: select the parts in a box",
                "Left- or right-drag: move a part (right-drag: the selected group) · \
                 Middle-drag or arrow keys: pan · Wheel: zoom",
                "Space: run or pause",
                "Delete: delete the selected part(s)",
                "Ctrl+G: make a chip of the selected parts",
                "W: wire tool (click source, then sink, choose pins)",
                "F: fit the circuit · R: reset to tick 0",
                "Esc: drop the armed part or wire tool, else deselect",
                "T: day or night workshop",
            ] {
                ui.label(egui::RichText::new(tip).size(12.0));
            }
                });
                        });
                });
            });
    }

    if open_map {
        editor_state.library_open = false;
        next_state.set(GameState::DemoMap);
        return Ok(());
    }

    if let Some(challenge) = open_challenge {
        editor_state.library_open = false;
        request_open(
            &mut editor_state,
            &mut writers.load,
            &active,
            &files.revision,
            super::challenge::challenge_request(challenge, false),
        );
        return Ok(());
    }

    if let Some(to_root) = close_requested {
        views.close.write(CloseView { to_root });
        return Ok(());
    }
    if let Some(definition) = open_definition {
        views
            .open_definition
            .write(OpenDefinitionView { definition });
        return Ok(());
    }

    // Place a workspace as a cell: its root becomes a definition, keyed by
    // the root's id (definitions already in the library are kept).
    let mut place = |editor_state: &mut EditorState, name: String, ws: &WorkspaceDocument| {
        let (registration, kind) = RegisterDefinitions::chip(ws);
        writers.register_definitions.write(registration);
        editor_state.arm(kind, Some(name));
    };
    if let Some(demo) = place_catalog {
        place(&mut editor_state, demo.title.clone(), &demo.workspace);
    }
    if let Some(name) = place_saved {
        match read_workspace(&name) {
            Ok(ws) => {
                editor_state.status = None;
                place(&mut editor_state, name, &ws);
            }
            Err(err) => {
                warn!("Cannot place workspace {name}: {err}");
                editor_state.status = Some(err.to_string());
            }
        }
    }

    // Everything below replaces the open circuit: through the
    // unsaved-changes guard.
    let mut request: Option<OpenRequest> = None;
    if let Some(demo) = open_catalog {
        request = Some(OpenRequest::Workspace {
            name: demo.title.clone(),
            workspace: Arc::clone(&demo.workspace),
            read_only: true,
            unsaved: false,
            status: None,
        });
    }

    // An imported netlist opens as an unsaved, editable workspace.
    if import_rtl {
        let path = editor_state.rtl_import_path.trim().to_string();
        let result = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                import_yosys_json(&text, &ImportOptions::default()).map_err(|e| e.to_string())
            });
        match result {
            Ok(import) => {
                request = Some(OpenRequest::Workspace {
                    name: import.name,
                    workspace: Arc::new(import.workspace),
                    read_only: false,
                    unsaved: true,
                    status: Some(if import.warnings.is_empty() {
                        format!("Imported {path}. Save the workspace to keep it.")
                    } else {
                        format!("Imported {path}: {}", import.warnings.join("; "))
                    }),
                });
            }
            Err(err) => {
                warn!("Cannot import {path}: {err}");
                editor_state.status = Some(format!("Cannot import {path}: {err}"));
            }
        }
    }

    if new_workspace {
        request = Some(OpenRequest::New);
    }

    // An editable copy under fresh ids, saved right away (under a free
    // name) so it is listed with the saved workspaces.
    if copy_demo && let Some(demo) = open_demo {
        request = Some(OpenRequest::CopyDemo(demo.id.clone()));
    }

    if let Some(name) = load_saved {
        // Reads the disk once, on the click.
        match read_workspace(&name) {
            Ok(ws) => {
                request = Some(OpenRequest::Workspace {
                    name,
                    workspace: Arc::new(ws),
                    read_only: false,
                    unsaved: false,
                    status: None,
                });
            }
            Err(err) => {
                warn!("Cannot load workspace {name}: {err}");
                editor_state.status = Some(err.to_string());
            }
        }
    }
    if let Some(request) = request {
        request_open(
            &mut editor_state,
            &mut writers.load,
            &active,
            &files.revision,
            request,
        );
    }

    // ── Wire builder ───────────────────────────────────────────────────
    if editor_state.connection_mode {
        let source = editor_state
            .connection_source
            .and_then(|e| gates.get(e).ok());
        let sink = editor_state.connection_sink.and_then(|e| gates.get(e).ok());
        egui::Area::new(egui::Id::new("hud_wire_builder"))
            .anchor(Align2::CENTER_TOP, [0.0, layout.top_band()])
            .order(Order::Middle)
            .show(ctx, |ui| {
                card(&palette).show(ui, |ui| {
                    ui.set_max_width(360.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Wire tool").font(heavy(16.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("×")
                                .on_hover_text("Leave the wire tool (W)")
                                .clicked()
                            {
                                editor_state.connection_mode = false;
                                editor_state.clear_connection();
                            }
                        });
                    });
                    let mut source_width = None;
                    match source {
                        Some((_, ty, label)) => {
                            ui.label(format!("Source: {}", gate_name(label, ty)));
                            let (_, outs) = library.pins(&ty.kind).unwrap_or_default();
                            let (_, names) = library.pin_names(&ty.kind);
                            source_width = draw_pin_selector(
                                ui,
                                "Source output pin",
                                &mut editor_state.connection_source_pin,
                                &outs,
                                &names,
                            );
                        }
                        None => {
                            ui.label("Click the source part on the canvas.");
                        }
                    }
                    let mut sink_width = None;
                    match sink {
                        Some((_, ty, label)) => {
                            ui.label(format!("Sink: {}", gate_name(label, ty)));
                            let (ins, _) = library.pins(&ty.kind).unwrap_or_default();
                            let (names, _) = library.pin_names(&ty.kind);
                            sink_width = draw_pin_selector(
                                ui,
                                "Sink input pin",
                                &mut editor_state.connection_sink_pin,
                                &ins,
                                &names,
                            );
                        }
                        None if source.is_some() => {
                            ui.label("Now click the sink part.");
                        }
                        None => {}
                    }
                    let widths_match = source_width.is_some() && source_width == sink_width;
                    if source_width.is_some() && sink_width.is_some() && !widths_match {
                        ui.colored_label(
                            warn,
                            format!(
                                "Pin widths differ ({} vs {} bits); use Split/Merge/Extend.",
                                source_width.unwrap_or(0),
                                sink_width.unwrap_or(0)
                            ),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(widths_match, egui::Button::new("Create wire"))
                            .clicked()
                            && let (Some((src, ..)), Some((snk, ..))) = (source, sink)
                        {
                            writers.create_wire.write(CreateWire {
                                source_gate: src.uuid,
                                source_pin: editor_state.connection_source_pin,
                                sink_gate: snk.uuid,
                                sink_pin: editor_state.connection_sink_pin,
                            });
                            editor_state.clear_connection();
                        }
                        if ui.button("Clear").clicked() {
                            editor_state.clear_connection();
                        }
                    });
                });
            });
    }

    // Placement of the armed part on left-click.
    if let Some(kind) = editor_state.selected_gate_type.clone() {
        if let Some(pos) = ctx.input(|i| i.pointer.latest_pos())
            && ctx.layer_id_at(pos).is_none()
        {
            // A hint beside the cursor while a part is armed.
            ui_armed_hint(
                ctx,
                &palette,
                pos,
                &kind,
                editor_state.selected_gate_label.as_deref(),
            );
        }
        if button_input.just_pressed(MouseButton::Left)
            && !ctx.is_pointer_over_egui()
            && let Some(screen_pos) = window.cursor_position()
            && let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos)
        {
            let d = -ray.origin.z / ray.direction.z;
            let world_pos = (ray.origin + ray.direction * d).truncate();
            writers.create_gate.write(CreateGate {
                kind,
                label: editor_state.selected_gate_label.clone(),
                position: world_pos,
            });
        }
    }

    Ok(())
}

/// The armed part's name beside the cursor over the canvas.
fn ui_armed_hint(
    ctx: &egui::Context,
    palette: &crate::theme::Palette,
    pos: egui::Pos2,
    kind: &GateKind,
    label: Option<&str>,
) {
    let text = label.unwrap_or(super::parts::kind_title(kind));
    let painter = ctx.layer_painter(egui::LayerId::new(
        Order::Tooltip,
        egui::Id::new("armed_hint"),
    ));
    let galley = painter.layout_no_wrap(text.to_string(), heavy(12.0), egui_color(palette.on_ink));
    let rect = egui::Rect::from_min_size(
        pos + egui::vec2(14.0, 14.0),
        galley.size() + egui::vec2(14.0, 6.0),
    );
    painter.rect(
        rect,
        CornerRadius::same(8),
        egui_color(palette.on),
        stroke(2.0, egui_color(palette.ink)),
        StrokeKind::Inside,
    );
    painter.galley(
        rect.min + egui::vec2(7.0, 3.0),
        galley,
        egui_color(palette.on_ink),
    );
}

/// Pin picker; returns the selected pin's width.
fn draw_pin_selector(
    ui: &mut egui::Ui,
    label: &str,
    selected_pin: &mut u8,
    widths: &[u8],
    names: &[String],
) -> Option<u8> {
    if widths.is_empty() {
        ui.label(format!("{label}: (unavailable)"));
        *selected_pin = 0;
        return None;
    }
    let clamped = usize::from(*selected_pin).min(widths.len() - 1);
    *selected_pin = clamped as u8;
    let describe = |pin: usize| {
        let name = names.get(pin).map_or("", String::as_str);
        format!("pin {pin} {name} ({} bit)", widths[pin])
    };
    egui::ComboBox::from_label(label)
        .selected_text(describe(clamped))
        .show_ui(ui, |ui| {
            for pin in 0..widths.len() {
                ui.selectable_value(selected_pin, pin as u8, describe(pin));
            }
        });
    widths.get(usize::from(*selected_pin)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file card stays within its room, beside the transport card,
    /// with a long demo title and path, in nested views (both back
    /// buttons), with the library open or closed, at the named sizes of
    /// the layout test.
    #[test]
    fn file_card_fits_its_room() {
        let theme = Theme::day();
        let ctx = crate::ui_style::test_ui::ctx(&theme);
        let sizes = [
            (1280.0, 720.0),
            (1366.0, 768.0),
            (1440.0, 900.0),
            (1536.0, 864.0),
            (1600.0, 900.0),
            (1600.0, 1000.0),
            (1920.0, 1080.0),
            (2560.0, 1440.0),
        ];
        let long_path = file_subtitle_parts(
            "Demos › Level 12",
            "16-bit CPU of gates / ALU16 / ADD16 / FA",
            true,
            false,
        );
        for (w, h) in sizes {
            let layout = HudLayout::new(Vec2::new(w, h));
            let room = layout.file_rect();
            // The Undo and Redo buttons are in the card only when the
            // layout says there is room.
            let undo = layout.undo_in_file_card().then_some((true, false));
            for depth in 0..=3 {
                for library_open in [false, true] {
                    let view = FileCardView {
                        title: "16-bit CPU of gates",
                        path: &long_path.0,
                        state: long_path.1,
                        note: "",
                        depth,
                        library_open,
                        undo,
                    };
                    let rect = crate::ui_style::test_ui::measure(&ctx, |ui| {
                        file_card(ui, &theme, &view, &layout);
                    });
                    assert!(
                        rect.width() <= room.width() + 0.5,
                        "{} > {} at {w}x{h}, depth {depth}",
                        rect.width(),
                        room.width()
                    );
                    assert!((rect.height() - TOP_CARD_HEIGHT).abs() < 0.5);
                }
            }
        }
    }

    #[test]
    fn file_subtitle_reads_origin_path_and_state() {
        assert_eq!(
            file_subtitle("My workshop", "Counter", false, false),
            "My workshop › Counter · saved"
        );
        assert_eq!(
            file_subtitle("Demos › Level 7", "Counter / REG8", true, true),
            "Demos › Level 7 › Counter › REG8 · read-only"
        );
        assert_eq!(
            file_subtitle("My workshop", "w", false, true),
            "My workshop › w · unsaved"
        );
    }
}
