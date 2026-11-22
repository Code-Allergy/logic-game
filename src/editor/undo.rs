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

/// The Undo and Redo buttons, greyed out when there is nothing to undo
/// (`can.0`) or redo (`can.1`); the command clicked, if any.
pub fn undo_buttons(
    ui: &mut egui::Ui,
    palette: &Palette,
    can: (bool, bool),
) -> Option<HistoryCommand> {
    let buttons = [
        (
            HudIcon::Undo,
            HistoryCommand::Undo,
            can.0,
            format!("Undo ({MODIFIER}+Z)"),
        ),
        (
            HudIcon::Redo,
            HistoryCommand::Redo,
            can.1,
            format!("Redo ({MODIFIER}+Shift+Z or {MODIFIER}+Y)"),
        ),
    ];
    let mut clicked = None;
    for (icon, command, enabled, hint) in buttons {
        let response = ui
            .add_enabled_ui(enabled, |ui| chunky_icon_button(ui, palette, icon, false))
            .inner;
        if response
            .on_hover_text(&hint)
            .on_disabled_hover_text(hint)
            .clicked()
        {
            clicked = Some(command);
        }
    }
    clicked
}

/// The buttons in a card of their own ([`crate::ui_style::UNDO_CARD`]).
pub fn undo_card(
    ui: &mut egui::Ui,
    palette: &Palette,
    can: (bool, bool),
) -> Option<HistoryCommand> {
    card(palette)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                undo_buttons(ui, palette, can)
            })
            .inner
        })
        .inner
}

/// What the Undo and Redo buttons need.
#[derive(SystemParam)]
pub struct UndoControls<'w> {
    history: Res<'w, UndoHistory>,
    revision: Res<'w, WorkspaceRevision>,
    active: Res<'w, ActiveCircuit>,
    commands: MessageWriter<'w, HistoryCommand>,
}

impl UndoControls<'_> {
    /// Whether there is something to undo and to redo; `None` for a
    /// read-only demo, which has no history (and no buttons).
    pub fn state(&self) -> Option<(bool, bool)> {
        (!self.active.read_only).then(|| {
            (
                self.history.can_undo(&self.revision),
                self.history.can_redo(),
            )
        })
    }

    /// Whether the buttons are shown in a card of their own: the file
    /// card has no room for them ([`HudLayout::undo_in_file_card`]).
    pub fn in_own_card(&self, layout: &HudLayout) -> bool {
        !self.active.read_only && !layout.undo_in_file_card()
    }

    /// Undo or redo, as a button asked.
    pub fn send(&mut self, command: Option<HistoryCommand>) {
        if let Some(command) = command {
            self.commands.write(command);
        }
    }

    /// The buttons in a card of their own, below the file card
    /// ([`HudLayout::undo_rect`]), when [`Self::in_own_card`].
    pub fn card(&mut self, ctx: &egui::Context, palette: &Palette, layout: &HudLayout) {
        if !self.in_own_card(layout) {
            return;
        }
        let Some(can) = self.state() else {
            return;
        };
        let rect = layout.undo_rect();
        let command = egui::Area::new(egui::Id::new("hud_undo"))
            .fixed_pos(egui::pos2(rect.min.x, rect.min.y))
            .order(egui::Order::Middle)
            .show(ctx, |ui| undo_card(ui, palette, can))
            .inner;
        self.send(command);
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

    /// The card is as large as the layout reserves for it.
    #[test]
    fn the_undo_card_fits_its_rect() {
        let theme = crate::theme::Theme::day();
        let ctx = crate::ui_style::test_ui::ctx(&theme);
        let rect = crate::ui_style::test_ui::measure(&ctx, |ui| {
            undo_card(ui, &theme.palette, (true, false));
        });
        let size = crate::ui_style::UNDO_CARD;
        assert!(
            rect.width() <= size.x + 0.5 && rect.height() <= size.y + 0.5,
            "{rect:?} in {size:?}"
        );
        assert!(rect.width() >= size.x - 2.0 && rect.height() >= size.y - 2.0);
    }
}
