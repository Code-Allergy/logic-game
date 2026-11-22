//! The Challenges section of the road's level panel, and the marks on
//! the road for completed challenges.

use bevy_egui::egui::{self, Align2, Color32, Sense, StrokeKind, Ui};

use crate::sim::DemoCatalog;
use crate::sim::challenges::Challenge;
use crate::ui_style::stroke;

use super::progress::DemoProgress;
use super::{Action, Kit, thumbnail};

/// The challenges of `level`, in manifest order.
pub fn of_level(catalog: &DemoCatalog, level: u32) -> Vec<&Challenge> {
    catalog
        .challenges
        .iter()
        .filter(|c| c.level == level)
        .collect()
}

/// `(done, total)` challenges of `level`.
pub fn level_count(catalog: &DemoCatalog, level: u32, progress: &DemoProgress) -> (usize, usize) {
    let all = of_level(catalog, level);
    (
        progress.completed_of(all.iter().map(|c| c.id.as_str())),
        all.len(),
    )
}

/// "CHALLENGES" and "n of m done", above the cards.
pub(super) fn heading(ui: &mut Ui, kit: &Kit, done: usize, total: usize) {
    let s = kit.s;
    let p = kit.p;
    let outline = 8.0 * s;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), (26.0 * s).round()),
        Sense::hover(),
    );
    let rect = rect.shrink2(egui::vec2(outline, 0.0));
    let label = egui::RichText::new("CHALLENGES")
        .font(kit.heavy(12.0))
        .extra_letter_spacing(1.2 * s)
        .color(kit.c(p.text_secondary));
    let galley = egui::WidgetText::from(label).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        rect.width(),
        egui::FontSelection::Default,
    );
    let y = rect.max.y - galley.size().y;
    ui.painter()
        .galley(egui::pos2(rect.min.x, y), galley, Color32::PLACEHOLDER);
    kit.text(
        ui.painter(),
        egui::pos2(rect.max.x, rect.max.y),
        Align2::RIGHT_BOTTOM,
        format!("{done} of {total} done · build it yourself, then Check"),
        kit.body(12.0),
        p.text_secondary,
    );
}

/// A challenge card: the starter's ports, title, brief and parts rule, a
/// done box and the button that opens it.
pub(super) fn card(
    ui: &mut Ui,
    kit: &Kit,
    challenge: &'static Challenge,
    done: bool,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let outline = 8.0 * s;
    let width = ui.available_width() - 2.0 * outline;
    let pad = 10.0 * s;
    let thumb = egui::vec2(120.0, 78.0) * s;
    let button_w = 96.0 * s;
    let text_w = width - 2.0 * pad - thumb.x - 12.0 * s - button_w - 12.0 * s;
    let painter = ui.painter().clone();
    let title = painter.layout(
        challenge.title.clone(),
        kit.heavy(16.0),
        kit.c(p.text),
        text_w,
    );
    let wrapped = |text: String, size: f32, colour, rows| {
        painter.layout_job(egui::text::LayoutJob {
            wrap: egui::text::TextWrapping {
                max_width: text_w,
                max_rows: rows,
                break_anywhere: false,
                overflow_character: Some('…'),
            },
            ..egui::text::LayoutJob::simple(text, kit.body(size), kit.c(colour), text_w)
        })
    };
    let brief = wrapped(challenge.brief.clone(), 13.0, p.text_secondary, 3);
    let rule = challenge
        .parts_rule()
        .map(|r| wrapped(format!("Parts: {r}"), 12.0, p.text_faint, 1));
    let gap = 4.0 * s;
    let block_h =
        title.size().y + gap + brief.size().y + rule.as_ref().map_or(0.0, |r| gap + r.size().y);
    let height = thumb.y.max(block_h) + 2.0 * pad;
    let (outer, _) = ui.allocate_exact_size(
        egui::vec2(width + 2.0 * outline, height + 4.0 * s).round(),
        Sense::hover(),
    );
    let rect = egui::Rect::from_min_size(
        outer.min + egui::vec2(outline, 0.0),
        egui::vec2(width, height),
    );
    let response = ui
        .interact(
            rect,
            ui.id().with(("challenge", &challenge.id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{}\n\nDouble-click to open it.", challenge.brief));
    if response.double_clicked() {
        actions.push(Action::OpenChallenge(challenge));
    }
    kit.card(
        &painter,
        rect,
        14.0,
        if response.hovered() { p.hover } else { p.tile },
    );
    // The starter: just its ports, waiting to be wired.
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad, rect.center().y - thumb.y * 0.5),
        thumb,
    );
    thumbnail::paint_thumbnail(
        &painter,
        p,
        thumb_rect,
        &challenge.starter.root,
        (9.0 * s) as u8,
    );
    let text_x = thumb_rect.max.x + 12.0 * s;
    let mut y = rect.center().y - block_h * 0.5;
    for galley in [Some(title), Some(brief), rule].into_iter().flatten() {
        let h = galley.size().y;
        painter.galley(egui::pos2(text_x, y), galley, Color32::PLACEHOLDER);
        y += h + gap;
    }

    // Right: the done box, the button below it.
    let tick = 18.0 * s;
    let right = rect.max.x - pad;
    let button = egui::Rect::from_min_size(
        egui::pos2(right - button_w, rect.center().y + 2.0 * s),
        egui::vec2(button_w, 37.0 * s),
    );
    let tick_rect = egui::Rect::from_center_size(
        egui::pos2(button.center().x, button.min.y - 6.0 * s - tick * 0.5),
        egui::vec2(tick, tick),
    );
    let tick_response = ui
        .interact(
            tick_rect.expand(4.0 * s),
            ui.id().with(("challenge_tick", &challenge.id)),
            Sense::hover(),
        )
        .on_hover_text(if done {
            "Done: your circuit passed the check."
        } else {
            "Not done yet: open it, build it and press Check."
        });
    kit.tick_box(&painter, tick_rect, done, tick_response.hovered());
    if done {
        // A tick in the box.
        let a = tick_rect.center();
        let line = stroke(kit.line(2.5), kit.c(p.on_ink));
        let pts = [
            a + egui::vec2(-4.5, 0.0) * s,
            a + egui::vec2(-1.0, 3.5) * s,
            a + egui::vec2(5.0, -4.0) * s,
        ];
        painter.line_segment([pts[0], pts[1]], line);
        painter.line_segment([pts[1], pts[2]], line);
    }
    let label = if done { "Again" } else { "Build it" };
    if kit
        .button(
            ui,
            button,
            &format!("challenge_open_{}", challenge.id),
            label,
            !done,
        )
        .on_hover_text("Open it as your own workspace, with the ports placed")
        .clicked()
    {
        actions.push(Action::OpenChallenge(challenge));
    }
}

/// The mark on a stop: "done/total" challenges in a little pill at the
/// stop's top right, marigold once all are done.
pub(super) fn stop_badge(
    painter: &egui::Painter,
    kit: &Kit,
    face: egui::Rect,
    done: usize,
    total: usize,
) {
    if total == 0 {
        return;
    }
    let s = kit.s;
    let p = kit.p;
    let all = done == total;
    let text = format!("{done}/{total}");
    let font = kit.heavy(10.0);
    let galley = painter.layout_no_wrap(text, font, kit.c(if all { p.on_ink } else { p.text }));
    let size = galley.size() + egui::vec2(10.0, 4.0) * s;
    let rect = egui::Rect::from_center_size(face.right_top() + egui::vec2(-2.0, 2.0) * s, size);
    painter.rect(
        rect,
        kit.radius(size.y * 0.5 / s),
        kit.c(if all { p.on } else { p.tile }),
        stroke(kit.line(2.0), kit.ink()),
        StrokeKind::Inside,
    );
    painter.galley(
        rect.center() - galley.size() * 0.5,
        galley,
        Color32::PLACEHOLDER,
    );
}
