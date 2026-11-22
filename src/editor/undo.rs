//! Undo and redo in the editor: `Ctrl+Z`, `Ctrl+Shift+Z` and `Ctrl+Y`
//! (`Cmd` on macOS) and the Undo and Redo buttons of the file card. The
//! history itself is [`crate::sim::history`].

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use crate::GameState;
use crate::sim::{ActiveCircuit, HistoryCommand, UndoHistory, WorkspaceRevision};
use crate::theme::Palette;
use crate::ui_style::{HudIcon, HudLayout, card, chunky_icon_button};

/// The modifier of the shortcuts, as the tooltips name it.
const MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// The history command for the keys pressed this frame: `Z` with `Ctrl`
/// or `Cmd` undoes, with `Shift` too redoes, `Y` with `Ctrl` or `Cmd`
/// redoes.
pub fn shortcut(keys: &ButtonInput<KeyCode>) -> Option<HistoryCommand> {
    let modifier = keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    if !modifier {
        return None;
    }
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if keys.just_pressed(KeyCode::KeyZ) {
        Some(if shift {
            HistoryCommand::Redo
        } else {
            HistoryCommand::Undo
        })
    } else if keys.just_pressed(KeyCode::KeyY) {
        Some(HistoryCommand::Redo)
    } else {
        None
    }
}

/// The undo shortcuts, ignored while a text field has the keyboard (it
/// has its own undo). A text field with the keyboard also holds the
/// history, so typing a label is one step.
pub fn undo_keys_system(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    game_state: Res<State<GameState>>,
    mut history: ResMut<UndoHistory>,
    mut commands: MessageWriter<HistoryCommand>,
) -> Result {
    let typing = contexts.ctx_mut()?.egui_wants_keyboard_input();
    if history.hold != typing {
        history.hold = typing;
    }
    if typing || game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    if let Some(command) = shortcut(&keys) {
        commands.write(command);
    }
    Ok(())
}

/// Width of the two buttons in a row with `spacing` between items.
pub fn buttons_width(spacing: f32) -> f32 {
    2.0 * (40.0 + spacing)
}

/// Whether the buttons fit in the file card (its text keeps at least 90
/// px), given the room the file card has for text and buttons; else
/// they get a card of their own below it.
pub fn fits_in_file_card(room: f32) -> bool {
    room - buttons_width(8.0) >= 90.0
}

/// Height of the buttons' own card: a 40 px button with its 3 px edge,
/// 8 px padding and the 2.5 px outline on each side.
pub const UNDO_CARD_HEIGHT: f32 = 40.0 + 3.0 + 2.0 * 8.0 + 2.0 * 2.5;

/// What the Undo and Redo buttons need.
#[derive(SystemParam)]
pub struct UndoControls<'w> {
    history: Res<'w, UndoHistory>,
    revision: Res<'w, WorkspaceRevision>,
    active: Res<'w, ActiveCircuit>,
    commands: MessageWriter<'w, HistoryCommand>,
}

impl UndoControls<'_> {
    /// Width the buttons take in a row with `spacing` between items (none
    /// for a read-only demo, which has no history).
    pub fn width(&self, spacing: f32) -> f32 {
        if self.active.read_only {
            0.0
        } else {
            buttons_width(spacing)
        }
    }

    /// The Undo and Redo buttons, greyed out when there is nothing to undo
    /// or redo.
    pub fn buttons(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        if self.active.read_only {
            return;
        }
        let buttons = [
            (
                HudIcon::Undo,
                HistoryCommand::Undo,
                self.history.can_undo(&self.revision),
                format!("Undo ({MODIFIER}+Z)"),
            ),
            (
                HudIcon::Redo,
                HistoryCommand::Redo,
                self.history.can_redo(),
                format!("Redo ({MODIFIER}+Shift+Z or {MODIFIER}+Y)"),
            ),
        ];
        for (icon, command, enabled, hint) in buttons {
            let response = ui
                .add_enabled_ui(enabled, |ui| chunky_icon_button(ui, palette, icon, false))
                .inner;
            if response
                .on_hover_text(&hint)
                .on_disabled_hover_text(hint)
                .clicked()
            {
                self.commands.write(command);
            }
        }
    }
}

impl UndoControls<'_> {
    /// The buttons in a card of their own, for a window too narrow to fit
    /// them in the file card: below it, or beside the Library drawer when
    /// it is open.
    pub fn card(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        layout: &HudLayout,
        library_open: bool,
    ) {
        if self.active.read_only {
            return;
        }
        let x = if library_open {
            layout.margin + layout.library_width + layout.gap
        } else {
            layout.margin
        };
        egui::Area::new(egui::Id::new("hud_undo"))
            .fixed_pos(egui::pos2(x, layout.top_band()))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                card(palette)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            self.buttons(ui, palette);
                        });
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(held: &[KeyCode], pressed: KeyCode) -> ButtonInput<KeyCode> {
        let mut keys = ButtonInput::default();
        for k in held {
            keys.press(*k);
        }
        keys.clear();
        keys.press(pressed);
        keys
    }

    #[test]
    fn shortcuts() {
        use HistoryCommand::{Redo, Undo};
        use KeyCode::*;
        assert_eq!(shortcut(&keys(&[ControlLeft], KeyZ)), Some(Undo));
        assert_eq!(shortcut(&keys(&[SuperLeft], KeyZ)), Some(Undo));
        assert_eq!(
            shortcut(&keys(&[ControlRight, ShiftLeft], KeyZ)),
            Some(Redo)
        );
        assert_eq!(shortcut(&keys(&[SuperRight, ShiftRight], KeyZ)), Some(Redo));
        assert_eq!(shortcut(&keys(&[ControlLeft], KeyY)), Some(Redo));
        assert_eq!(shortcut(&keys(&[], KeyZ)), None, "a bare Z is not undo");
        assert_eq!(shortcut(&keys(&[ShiftLeft], KeyZ)), None);
        assert_eq!(shortcut(&keys(&[ControlLeft], KeyX)), None);
    }
}
