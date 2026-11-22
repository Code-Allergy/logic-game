//! Chips in the editor:
//!
//! - the selection card: with two or more parts selected
//!   ([`MultiSelection`]) it takes the inspector's place, with Make chip
//!   (`Ctrl+G`) and Delete (`Del`, all of them, one undo step);
//! - the Make chip dialog, which asks for the new chip's name and sends
//!   [`MakeChip`];
//! - the chip update prompt: placing a chip whose saved version is newer
//!   than (or diverged from) the copy in this workspace asks whether to
//!   refresh every instance ([`RefreshDefinitions`]) or keep this one.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::GameState;
use crate::sim::{
    ActiveCircuit, CircuitLibrary, DeleteGate, GateIdentity, GateLabel, GateType, Incoming,
    MakeChip, MultiSelection, PendingChipUpdates, RefreshDefinitions, SelectedGate,
    chip::stays_outside,
};
use crate::theme::Theme;
use crate::ui_style::{HudLayout, caption, card, chunky_button, egui_color, heavy};

/// The modifier of the shortcuts, as the tooltips name it.
const MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// The Make chip dialog: open while it asks for a name.
#[derive(Resource, Debug, Default, Clone, PartialEq)]
pub struct MakeChipDialog {
    /// The parts that become the chip (by id), or `None` when closed.
    pub gates: Option<Vec<Uuid>>,
    pub name: String,
    /// Give the name field the keyboard on the next frame.
    focus: bool,
}

impl MakeChipDialog {
    pub fn is_open(&self) -> bool {
        self.gates.is_some()
    }

    /// Open the dialog for `gates`, with a fresh default name.
    pub fn open(&mut self, gates: Vec<Uuid>, library: &CircuitLibrary) {
        self.gates = Some(gates);
        self.name = default_chip_name(library);
        self.focus = true;
    }

    pub fn close(&mut self) {
        self.gates = None;
    }
}

/// `Chip 1`, `Chip 2`, ...: the first name no definition has.
pub fn default_chip_name(library: &CircuitLibrary) -> String {
    (1..)
        .map(|i| format!("Chip {i}"))
        .find(|n| library.definitions.values().all(|d| &d.name != n))
        .expect("a free name")
}

/// Whether `Ctrl+G` (`Cmd+G`) was pressed this frame.
pub fn make_chip_shortcut(keys: &ButtonInput<KeyCode>) -> bool {
    keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]) && keys.just_pressed(KeyCode::KeyG)
}

/// What the selection card reads and writes.
#[derive(SystemParam)]
pub struct SelectionCard<'w, 's> {
    multi: Res<'w, MultiSelection>,
    selected: ResMut<'w, SelectedGate>,
    active: Res<'w, ActiveCircuit>,
    library: Res<'w, CircuitLibrary>,
    gates: Query<'w, 's, (&'static GateIdentity, &'static GateLabel, &'static GateType)>,
    dialog: ResMut<'w, MakeChipDialog>,
    delete: MessageWriter<'w, DeleteGate>,
    make: MessageWriter<'w, MakeChip>,
}

/// The selection card, the Make chip dialog and their keys (`Ctrl+G`,
/// `Del`). See the module docs.
pub fn selection_card_system(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    theme: Res<Theme>,
    game_state: Res<State<GameState>>,
    mut card_state: SelectionCard,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let s = &mut card_state;
    let selection = s.multi.all(s.selected.0);
    let parts: Vec<(Uuid, String, bool)> = selection
        .iter()
        .filter_map(|e| s.gates.get(*e).ok())
        .map(|(id, label, ty)| {
            let name = if label.value.is_empty() {
                ty.kind.display_name().to_string()
            } else {
                label.value.clone()
            };
            (id.uuid, name, stays_outside(&ty.kind))
        })
        .collect();
    let editable = !s.active.read_only;
    let typing = ctx.egui_wants_keyboard_input();
    let palette = theme.palette;

    // Keys.
    if !typing && !s.dialog.is_open() {
        if make_chip_shortcut(&keys) && !parts.is_empty() && editable {
            let ids = parts.iter().map(|p| p.0).collect();
            s.dialog.open(ids, &s.library);
        }
        if s.multi.len() >= 2
            && (keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace))
        {
            delete_all(s, &selection);
        }
    }

    if s.multi.len() >= 2 {
        let mut make = false;
        let mut delete = false;
        let mut clear = false;
        let layout = HudLayout::new(Vec2::new(
            ctx.content_rect().width(),
            ctx.content_rect().height(),
        ));
        let column = layout.inspector_rect();
        let frame = card(&palette).inner_margin(egui::Margin::same(16));
        let inner_w = column.width() - frame.total_margin().sum().x;
        egui::Area::new(egui::Id::new("hud_selection"))
            .anchor(egui::Align2::RIGHT_TOP, [-layout.margin, layout.top_band()])
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(inner_w);
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} parts selected", parts.len()))
                                .font(heavy(18.0)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if ui.small_button("×").on_hover_text("Clear the selection (Esc)").clicked() {
                                clear = true;
                            }
                        });
                    });
                    ui.label(
                        egui::RichText::new(
                            "Shift+click a part to add or remove it; Shift+drag a box to add parts.",
                        )
                        .size(12.0)
                        .color(egui_color(palette.text_secondary)),
                    );
                    ui.label(caption(&palette, "Parts"));
                    let shown = 8;
                    let mut names: Vec<&str> = parts.iter().map(|p| p.1.as_str()).take(shown).collect();
                    let more = parts.len().saturating_sub(shown);
                    let more_text = format!("… and {more} more");
                    if more > 0 {
                        names.push(&more_text);
                    }
                    ui.label(egui::RichText::new(names.join(", ")).monospace().size(12.0));
                    let ports = parts.iter().filter(|p| p.2).count();
                    if ports > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "{ports} input/output/display part(s) stay outside a chip; \
                                 their wires become its ports."
                            ))
                            .size(12.0)
                            .color(egui_color(palette.text_secondary)),
                        );
                    }
                    ui.horizontal(|ui| {
                        if editable {
                            let can_make = parts.iter().any(|p| !p.2);
                            let response = ui
                                .add_enabled_ui(can_make, |ui| {
                                    chunky_button(ui, &palette, "Make chip…", true)
                                })
                                .inner;
                            if response
                                .on_hover_text(format!(
                                    "Turn the selection into a new chip; wires crossing it \
                                     become its ports ({MODIFIER}+G)"
                                ))
                                .clicked()
                            {
                                make = true;
                            }
                            if chunky_button(ui, &palette, "Delete", false)
                                .on_hover_text("Delete every selected part (Del)")
                                .clicked()
                            {
                                delete = true;
                            }
                        }
                    });
                });
            });
        if make && !s.dialog.is_open() {
            let ids = parts.iter().map(|p| p.0).collect();
            s.dialog.open(ids, &s.library);
        }
        if delete {
            delete_all(s, &selection);
        }
        if clear {
            s.selected.0 = None;
        }
    }

    make_chip_dialog(ctx, &palette, s, &keys);
    Ok(())
}

fn delete_all(s: &mut SelectionCard, selection: &[Entity]) {
    // All in one frame: one undo step. A read-only demo refuses them
    // (with a toast) and keeps the selection.
    for &entity in selection {
        s.delete.write(DeleteGate { entity });
    }
    if !s.active.read_only {
        s.selected.0 = None;
    }
}

fn make_chip_dialog(
    ctx: &egui::Context,
    palette: &crate::theme::Palette,
    s: &mut SelectionCard,
    keys: &ButtonInput<KeyCode>,
) {
    let Some(gates) = s.dialog.gates.clone() else {
        return;
    };
    let mut confirm = false;
    let mut cancel = keys.just_pressed(KeyCode::Escape);
    egui::Area::new(egui::Id::new("hud_make_chip"))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            card(palette)
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.label(egui::RichText::new("Make chip").font(heavy(18.0)));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} part(s) become one chip. Wires that cross the selection become \
                             its ports, named after what drives them.",
                            gates.len()
                        ))
                        .size(12.0)
                        .color(egui_color(palette.text_secondary)),
                    );
                    ui.label(caption(palette, "Name"));
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut s.dialog.name).desired_width(f32::INFINITY),
                    );
                    if s.dialog.focus {
                        response.request_focus();
                        s.dialog.focus = false;
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirm = true;
                    }
                    ui.horizontal(|ui| {
                        let named = !s.dialog.name.trim().is_empty();
                        if ui
                            .add_enabled_ui(named, |ui| {
                                chunky_button(ui, palette, "Make chip", true)
                            })
                            .inner
                            .clicked()
                        {
                            confirm = true;
                        }
                        if chunky_button(ui, palette, "Cancel", false).clicked() {
                            cancel = true;
                        }
                    });
                });
        });
    if confirm && !s.dialog.name.trim().is_empty() {
        s.make.write(MakeChip {
            gates,
            name: s.dialog.name.trim().to_owned(),
        });
        s.dialog.close();
    } else if cancel {
        s.dialog.close();
    }
}

/// The question a chip update asks.
pub fn update_question(change: &crate::sim::ChipChange) -> String {
    match change.how {
        Incoming::Diverged => format!(
            "\"{}\" was changed both here and in its saved workspace (version {}).",
            change.name, change.offered
        ),
        _ => format!(
            "A newer \"{}\" was saved (version {}; this workspace has version {}).",
            change.name, change.offered, change.have
        ),
    }
}

/// The chip update prompt (see the module docs).
pub fn chip_update_prompt_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    game_state: Res<State<GameState>>,
    mut pending: ResMut<PendingChipUpdates>,
    mut refresh: MessageWriter<RefreshDefinitions>,
    gates: Query<&GateType>,
    library: Res<CircuitLibrary>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let Some(update) = pending.updates.last().cloned() else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let instances = |id: Uuid| {
        let kind = crate::sim::GateKind::SubCircuit { definition: id };
        gates.iter().filter(|t| t.kind == kind).count()
            + library
                .definitions
                .values()
                .flat_map(|d| d.instanced_definitions())
                .filter(|d| *d == id)
                .count()
    };
    let mut answer: Option<bool> = None;
    egui::Area::new(egui::Id::new("hud_chip_update"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 120.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            card(&palette)
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(420.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.label(egui::RichText::new("Chip updated").font(heavy(18.0)));
                    for change in &update.changed {
                        ui.label(update_question(change));
                        ui.label(
                            egui::RichText::new(format!(
                                "{} instance(s) here use this workspace's version.",
                                instances(change.id)
                            ))
                            .size(12.0)
                            .color(egui_color(palette.text_secondary)),
                        );
                    }
                    ui.horizontal(|ui| {
                        if chunky_button(ui, &palette, "Refresh instances", true)
                            .on_hover_text(
                                "Use the saved version everywhere (wires stay on ports of the \
                                 same name; undo with Ctrl+Z)",
                            )
                            .clicked()
                        {
                            answer = Some(true);
                        }
                        if chunky_button(ui, &palette, "Keep this version", false).clicked() {
                            answer = Some(false);
                        }
                    });
                });
        });
    if let Some(refresh_it) = answer {
        pending.updates.pop();
        if refresh_it {
            refresh.write(RefreshDefinitions {
                definitions: update.definitions,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::CircuitDocument;
    use std::sync::Arc;

    #[test]
    fn default_chip_names_are_free() {
        let mut library = CircuitLibrary::default();
        assert_eq!(default_chip_name(&library), "Chip 1");
        let d = CircuitDocument::new("Chip 1");
        library.definitions.insert(d.id, Arc::new(d));
        assert_eq!(default_chip_name(&library), "Chip 2");
    }

    #[test]
    fn ctrl_g_makes_a_chip() {
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::ControlLeft);
        keys.clear();
        keys.press(KeyCode::KeyG);
        assert!(make_chip_shortcut(&keys));
        let mut bare = ButtonInput::default();
        bare.press(KeyCode::KeyG);
        assert!(!make_chip_shortcut(&bare));
    }
}
