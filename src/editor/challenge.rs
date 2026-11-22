//! The open challenge in the editor: which challenge the workspace is,
//! the challenge card (brief, parts rule, **Check** and its result) and
//! opening a challenge from the road or the Library.
//!
//! A challenge opens as an editable workspace named after it
//! ([`Challenge::workspace_name`]): the saved one if the player has saved
//! (or quit) before, else a fresh copy of the starter. The editor knows a
//! workspace is a challenge by that name, so a challenge saved and loaded
//! again from the Library is still one. **Check** runs
//! [`Challenge::check`] on the circuit as it is (a separate simulation,
//! never the running one) and a pass is recorded in
//! [`DemoProgress`].

use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::{
    EguiContexts,
    egui::{self, Color32, CornerRadius, Order, Sense, StrokeKind},
};
use uuid::Uuid;

use crate::GameState;
use crate::sim::challenges::{Challenge, CheckReport, Failure, Signal};
use crate::sim::{
    ActiveCircuit, CircuitReader, LoadWorkspaceEvent, WorkspaceDocument, WorkspaceRevision, demos,
};
use crate::theme::{Palette, Theme};
use crate::ui_style::{caption, card, chunky_button, egui_color, heavy, stroke};

use super::EditorState;
use super::demo_map::progress::DemoProgress;
use super::hud::hud_layout;

/// The challenge the open workspace is, and its last check.
#[derive(Resource, Debug, Default)]
pub struct ActiveChallenge {
    /// The challenge's id; `None` when the workspace is not a challenge.
    pub id: Option<String>,
    /// The workspace the id belongs to (its root circuit).
    root: Option<Uuid>,
    /// The last check, and the workspace revision it checked.
    pub report: Option<(CheckReport, u64)>,
    /// The card is folded to its title and the Check button.
    pub folded: bool,
    /// "Start over" was clicked once: the next click confirms.
    confirm_restart: bool,
}

impl ActiveChallenge {
    /// The open challenge.
    pub fn challenge(&self) -> Option<&'static Challenge> {
        let id = self.id.as_deref()?;
        demos::catalog().ok()?.challenge(id)
    }
}

/// The challenge an open workspace is: an editable workspace named after
/// one.
pub fn challenge_of(name: &str, read_only: bool) -> Option<&'static Challenge> {
    if read_only {
        return None;
    }
    demos::catalog().ok()?.challenge_for_workspace(name)
}

/// Follow the open workspace: tag it with its challenge, and forget the
/// last check when another workspace opens.
pub fn track_challenge_system(active: Res<ActiveCircuit>, mut challenge: ResMut<ActiveChallenge>) {
    if !active.is_changed() {
        return;
    }
    let id = challenge_of(&active.name, active.read_only).map(|c| c.id.clone());
    if challenge.id != id || challenge.root != Some(active.uuid) {
        challenge.id = id;
        challenge.root = Some(active.uuid);
        challenge.report = None;
        challenge.confirm_restart = false;
    }
}

/// What opening a challenge loads: the player's saved workspace for it,
/// if there is one, else a fresh editable copy of the starter.
pub fn challenge_workspace(challenge: &Challenge, fresh: bool) -> (Arc<WorkspaceDocument>, bool) {
    let name = challenge.workspace_name();
    if !fresh
        && cfg!(not(target_arch = "wasm32"))
        && super::workspace::load_saved_workspace_names().is_ok_and(|names| names.contains(&name))
        && let Ok(saved) = super::workspace::read_workspace(&name)
    {
        return (Arc::new(saved), true);
    }
    (
        Arc::new(demos::editable_copy(&challenge.starter, &name)),
        false,
    )
}

/// Open `challenge` in the editor (from the road or the Library); with
/// `fresh`, the starter even when there is saved work.
pub fn open_challenge(
    challenge: &Challenge,
    fresh: bool,
    editor: Option<&mut EditorState>,
    load: &mut MessageWriter<LoadWorkspaceEvent>,
) {
    let name = challenge.workspace_name();
    let (workspace, resumed) = challenge_workspace(challenge, fresh);
    if let Some(editor) = editor {
        editor.current_workspace = Some(name.clone());
        editor.workspace_file_name = name.clone();
        editor.selected_gate_type = None;
        editor.library_open = false;
        editor.clear_connection();
        editor.status = Some(if resumed {
            format!("Back to \"{}\": your saved work.", challenge.title)
        } else {
            format!(
                "Challenge: {}. Build it, then press Check.",
                challenge.title
            )
        });
    }
    load.write(LoadWorkspaceEvent {
        name,
        workspace,
        read_only: false,
    });
}

/// The challenge card (left, below the file card) while a challenge is
/// open and the Library is closed.
#[allow(clippy::too_many_arguments)]
pub fn challenge_card_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    game_state: Res<State<GameState>>,
    mut challenge: ResMut<ActiveChallenge>,
    mut editor: ResMut<EditorState>,
    mut progress: ResMut<DemoProgress>,
    revision: Res<WorkspaceRevision>,
    reader: CircuitReader,
    mut load: MessageWriter<LoadWorkspaceEvent>,
) -> Result {
    if game_state.get() != &GameState::Simulating || editor.library_open {
        return Ok(());
    }
    let Some(current) = challenge.challenge() else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let room = layout.library_rect();
    let completed = progress.is_completed(&current.id);
    let (mut check, mut restart, mut fold) = (false, false, false);
    egui::Area::new(egui::Id::new("hud_challenge"))
        .fixed_pos(egui::pos2(room.min.x, room.min.y))
        .order(Order::Middle)
        .show(ctx, |ui| {
            card(&palette).show(ui, |ui| {
                let width = room.width() - 24.0;
                ui.set_width(width);
                ui.set_max_height(room.height() - 20.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    ui.label(caption(
                        &palette,
                        &format!("Challenge · stop {}", current.level),
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (label, tip) = if challenge.folded {
                            ("+", "Show the brief")
                        } else {
                            ("–", "Fold the card")
                        };
                        if ui.small_button(label).on_hover_text(tip).clicked() {
                            fold = true;
                        }
                        if completed {
                            done_badge(ui, &palette);
                        }
                    });
                });
                ui.add(
                    egui::Label::new(egui::RichText::new(&current.title).font(heavy(17.0))).wrap(),
                );
                if !challenge.folded {
                    ui.label(
                        egui::RichText::new(&current.brief)
                            .size(13.0)
                            .color(egui_color(palette.text_secondary)),
                    );
                    if let Some(rule) = current.parts_rule() {
                        rule_line(ui, &palette, &rule);
                    }
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if chunky_button(ui, &palette, "Check", true)
                        .on_hover_text("Test your circuit against the challenge")
                        .clicked()
                    {
                        check = true;
                    }
                    if !challenge.folded {
                        let label = if challenge.confirm_restart {
                            "Sure? Start over"
                        } else {
                            "Start over"
                        };
                        if chunky_button(ui, &palette, label, false)
                            .on_hover_text("Throw your circuit away and start from the ports")
                            .clicked()
                        {
                            restart = true;
                        }
                    }
                });
                if let Some((report, at)) = &challenge.report {
                    result(ui, &palette, current, report, *at != revision.current);
                }
            });
        });

    if fold {
        challenge.folded = !challenge.folded;
    }
    if restart {
        if challenge.confirm_restart {
            challenge.confirm_restart = false;
            open_challenge(current, true, Some(&mut editor), &mut load);
        } else {
            challenge.confirm_restart = true;
        }
    } else if check {
        challenge.confirm_restart = false;
        let report = current.check(&reader.workspace());
        if report.passed()
            && progress.mark_completed(&current.id)
            && let Err(e) = progress.save()
        {
            warn!("Cannot save the demo map progress: {e}");
        }
        challenge.report = Some((report, revision.current));
    }
    Ok(())
}

/// "Done": a marigold tick chip.
fn done_badge(ui: &mut egui::Ui, palette: &Palette) {
    let text = "Done";
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), heavy(11.0), egui_color(palette.on_ink));
    let (rect, _) = ui.allocate_exact_size(galley.size() + egui::vec2(14.0, 6.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        egui_color(palette.on),
        stroke(2.0, egui_color(palette.ink)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        rect.center() - galley.size() * 0.5,
        galley,
        egui_color(palette.on_ink),
    );
}

/// The parts rule, in a well.
fn rule_line(ui: &mut egui::Ui, palette: &Palette, rule: &str) {
    egui::Frame::new()
        .fill(egui_color(palette.well))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(format!("Parts: {rule}."))
                    .size(12.0)
                    .color(egui_color(palette.text)),
            );
        });
}

/// The last check: passed, or the first failure with its case laid out
/// as inputs, expected and got.
fn result(
    ui: &mut egui::Ui,
    palette: &Palette,
    challenge: &Challenge,
    report: &CheckReport,
    edited: bool,
) {
    let passed = report.passed();
    let (fill, edge) = if passed {
        (palette.on, palette.ink)
    } else {
        (palette.tile, palette.fault)
    };
    egui::Frame::new()
        .fill(egui_color(fill))
        .stroke(stroke(2.5, egui_color(edge)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            let headline_colour = if passed {
                palette.on_ink
            } else {
                palette.fault_text
            };
            let headline = match &report.failure {
                None => report.summary(),
                Some(Failure::Case(c)) => match c.tick {
                    Some(t) => format!("Step {} went wrong (tick {t})", c.case),
                    None => format!("Case {} went wrong", c.case),
                },
                Some(Failure::Part { .. }) => "A part that is not allowed".into(),
                Some(Failure::Port(_)) => "A port is missing or changed".into(),
                Some(Failure::Build(_)) => "The circuit does not build".into(),
            };
            ui.label(
                egui::RichText::new(headline)
                    .font(heavy(14.0))
                    .color(egui_color(headline_colour)),
            );
            match &report.failure {
                None => {
                    ui.label(
                        egui::RichText::new(format!(
                            "\"{}\" is done; it is ticked on the demo map.",
                            challenge.title
                        ))
                        .size(12.0)
                        .color(egui_color(palette.on_ink)),
                    );
                }
                Some(Failure::Case(c)) => {
                    let wrong: Vec<&str> = c
                        .expected
                        .iter()
                        .zip(&c.got)
                        .filter(|(e, g)| e.value != g.value)
                        .map(|(e, _)| e.name.as_str())
                        .collect();
                    egui::Grid::new("challenge_case")
                        .num_columns(2)
                        .spacing([10.0, 3.0])
                        .show(ui, |ui| {
                            signal_row(ui, palette, "Inputs", &c.inputs, &[]);
                            signal_row(ui, palette, "Expected", &c.expected, &[]);
                            signal_row(ui, palette, "Got", &c.got, &wrong);
                        });
                }
                Some(other) => {
                    let text = other.to_string();
                    ui.label(
                        egui::RichText::new(text)
                            .size(12.0)
                            .color(egui_color(palette.text)),
                    );
                }
            }
            if edited {
                ui.label(
                    egui::RichText::new("You have changed the circuit since: check again.")
                        .size(11.0)
                        .italics()
                        .color(egui_color(if passed {
                            palette.on_ink
                        } else {
                            palette.text_secondary
                        })),
                );
            }
        });
}

/// One row of the failing case: a caption and the values, the wrong ones
/// in the fault colour.
fn signal_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    signals: &[Signal],
    wrong: &[&str],
) {
    ui.label(caption(palette, label));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for s in signals {
            let bad = wrong.contains(&s.name.as_str());
            let colour: Color32 = egui_color(if bad {
                palette.fault_text
            } else {
                palette.text
            });
            let text = egui::RichText::new(s.to_string())
                .monospace()
                .size(12.5)
                .color(colour);
            ui.label(if bad { text.strong().underline() } else { text });
        }
    });
    ui.end_row();
}

/// The Library's list of challenges, by stop. Returns the one clicked.
pub fn library_list(
    ui: &mut egui::Ui,
    palette: &Palette,
    progress: &DemoProgress,
    open: Option<&str>,
) -> Option<&'static Challenge> {
    let catalog = demos::catalog().ok()?;
    let mut picked = None;
    let mut level = None;
    for c in &catalog.challenges {
        if level != Some(c.level) {
            level = Some(c.level);
            ui.label(caption(
                palette,
                &format!("Level {}: {}", c.level, catalog.level_title(c.level)),
            ));
        }
        let done = if progress.is_completed(&c.id) {
            " (done)"
        } else {
            ""
        };
        if ui
            .selectable_label(open == Some(c.id.as_str()), format!("{}{done}", c.title))
            .on_hover_text(&c.brief)
            .clicked()
        {
            picked = Some(c);
        }
    }
    picked
}

/// Where the parts bar stands on a part while a challenge is open:
/// `None` when it is allowed, else why not.
pub fn refusal(challenge: Option<&Challenge>, allowed: bool) -> Option<String> {
    let challenge = challenge?;
    (!allowed).then(|| {
        format!(
            "Not for this challenge. {}.",
            challenge.parts_rule().unwrap_or_default()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_is_a_challenge_by_its_name_when_editable() {
        let catalog = demos::catalog().unwrap();
        let c = catalog.challenge("half_adder").unwrap();
        assert_eq!(c.workspace_name(), "Build a half adder (challenge)");
        assert_eq!(
            challenge_of(&c.workspace_name(), false).map(|c| c.id.as_str()),
            Some("half_adder")
        );
        assert!(challenge_of(&c.workspace_name(), true).is_none());
        assert!(challenge_of("Half adder", false).is_none());
    }

    #[test]
    fn a_fresh_challenge_is_a_copy_of_its_starter_under_new_ids() {
        let catalog = demos::catalog().unwrap();
        let c = catalog.challenge("mux2").unwrap();
        let (ws, resumed) = challenge_workspace(c, true);
        assert!(!resumed);
        assert_ne!(ws.root.id, c.starter.root.id);
        assert_eq!(ws.root.name, c.workspace_name());
        assert_eq!(ws.root.nodes.len(), c.starter.root.nodes.len());
        assert!(!c.check(&ws).passed());
    }
}
