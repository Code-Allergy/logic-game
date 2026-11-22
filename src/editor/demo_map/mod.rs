//! The demo map: a full-screen view of the curriculum as a winding road
//! from one gate to a CPU ([`GameState::DemoMap`]).
//!
//! - The road map (left): one stop per level, marigold once one of its
//!   demos has been opened, sky with a ring and a "You are here" pin for
//!   the current one, white before that; the last stop is a larger tile.
//!   The road is laid out from the level count ([`road`]). Click a stop to
//!   pick the level. A progress card in the corner counts the opened demos
//!   and names the next stop.
//! - The level panel (right): the stop's title and description, its demos
//!   as cards with a thumbnail ([`thumbnail`]) and a visited box, and
//!   "Open" and "Copy to my workshop"; below them its build-it challenges
//!   ([`challenges`]), each with a done box and "Build it", which opens
//!   the challenge in the editor. A stop with challenges wears a
//!   "done/total" pill.
//! - The gallery shelf (bottom): category chips and a row of cards; a
//!   click opens the item.
//!
//! Open it from the file card, the Library drawer or the title menu; `Esc`
//! or "Back to workshop" returns to the editor. Opened demos are recorded
//! in [`progress::DemoProgress`] (whoever opens them). The app still starts
//! on the title menu.

pub mod challenges;
pub mod progress;
pub mod road;
pub mod thumbnail;

use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::{
    EguiContexts, EguiPrimaryContextPass,
    egui::{self, Align2, Color32, CornerRadius, FontId, Order, Pos2, Sense, StrokeKind, Ui},
};

use crate::GameState;
use crate::sim::{ActiveCircuit, Demo, DemoCatalog, LoadWorkspaceEvent, WorkspaceRevision, demos};
use crate::theme::{Palette, Theme, ThemeMode};
use crate::ui_style::{HudIcon, egui_color, heavy, paint_icon, stroke};

use super::EditorState;
use super::files::{OpenRequest, open_now, request_open};
use bevy_egui::egui::emath::GuiRounding as _;
use progress::{DemoProgress, RoadStatus, StopState, road_status};
use road::{LabelSide, MapLayout, stop_size};

/// What the map has picked: a level, one of its demos, a gallery shelf.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct DemoMapState {
    pub level: Option<u32>,
    pub demo: Option<String>,
    /// The gallery's category chip; `None` is "All".
    pub category: Option<String>,
}

/// The gallery items on the shelf for a category chip (`None`: all), in
/// catalog order.
pub fn gallery_items<'a>(gallery: &'a [Demo], category: Option<&str>) -> Vec<&'a Demo> {
    gallery
        .iter()
        .filter(|d| category.is_none_or(|c| d.category.as_deref() == Some(c)))
        .collect()
}

/// The road's state from the catalog and the record.
pub fn catalog_status(catalog: &DemoCatalog, progress: &DemoProgress) -> RoadStatus {
    let levels: Vec<u32> = catalog.levels.iter().map(|l| l.level).collect();
    let demos: Vec<(u32, &str)> = catalog
        .demos
        .iter()
        .map(|d| (d.level, d.id.as_str()))
        .collect();
    road_status(&levels, &demos, progress)
}

/// The demo a level starts on: its first unopened demo, else the one
/// opened last if it is there, else its first.
pub fn default_demo<'a>(
    catalog: &'a DemoCatalog,
    level: u32,
    progress: &DemoProgress,
) -> Option<&'a Demo> {
    let mut demos = catalog.demos.iter().filter(|d| d.level == level);
    let first = demos.clone().next();
    demos
        .clone()
        .find(|d| !progress.is_opened(&d.id))
        .or_else(|| demos.find(|d| progress.last() == Some(d.id.as_str())))
        .or(first)
}

/// The selection on arriving: the current stop, else the next one, else
/// the first.
pub fn initial_selection(catalog: &DemoCatalog, progress: &DemoProgress) -> DemoMapState {
    let status = catalog_status(catalog, progress);
    let level = status
        .current
        .or(status.next)
        .and_then(|i| catalog.levels.get(i))
        .or(catalog.levels.first())
        .map(|l| l.level);
    DemoMapState {
        level,
        demo: level
            .and_then(|l| default_demo(catalog, l, progress))
            .map(|d| d.id.clone()),
        category: None,
    }
}

pub struct DemoMapPlugin;

impl Plugin for DemoMapPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<DemoProgress>() {
            app.insert_resource(DemoProgress::for_player());
        }
        app.init_resource::<DemoMapState>()
            .add_systems(Update, record_opened_demo_system)
            .add_systems(OnEnter(GameState::DemoMap), enter_demo_map_system)
            .add_systems(EguiPrimaryContextPass, demo_map_system);
    }
}

/// Record the demo or gallery item that is open, however it was opened.
pub fn record_opened_demo_system(active: Res<ActiveCircuit>, mut progress: ResMut<DemoProgress>) {
    if !active.is_changed() {
        return;
    }
    let Some(demo) = demos::catalog().ok().and_then(|c| c.by_root(active.uuid)) else {
        return;
    };
    if progress.is_opened(&demo.id) && progress.last() == Some(demo.id.as_str()) {
        return;
    }
    progress.mark_opened(&demo.id);
    if let Err(e) = progress.save() {
        warn!("Cannot save the demo map progress: {e}");
    }
}

fn enter_demo_map_system(progress: Res<DemoProgress>, mut map: ResMut<DemoMapState>) {
    if let Ok(catalog) = demos::catalog() {
        let category = map.category.take();
        *map = initial_selection(catalog, &progress);
        map.category = category;
    }
}

/// What a click on the map asks for.
enum Action {
    SelectLevel(u32),
    SelectDemo(String),
    Open(&'static Demo),
    Copy(&'static Demo),
    OpenChallenge(&'static crate::sim::challenges::Challenge),
    Tick(String, bool),
    Category(Option<String>),
    Theme(ThemeMode),
    Back,
}

/// Scaled drawing helpers: the Workshop card, button and chip at the map's
/// scale (the HUD's are a fixed size).
struct Kit<'a> {
    p: &'a Palette,
    s: f32,
    night: bool,
}

impl Kit<'_> {
    fn c(&self, color: Color) -> Color32 {
        egui_color(color)
    }

    fn ink(&self) -> Color32 {
        egui_color(self.p.ink)
    }

    fn line(&self, width: f32) -> f32 {
        (width * self.s).max(1.0)
    }

    fn radius(&self, r: f32) -> CornerRadius {
        CornerRadius::same((r * self.s).round().clamp(0.0, 255.0) as u8)
    }

    /// A card: face, ink outline, solid bottom edge.
    fn card(&self, painter: &egui::Painter, rect: egui::Rect, radius: f32, fill: Color) {
        let r = self.radius(radius);
        painter.rect_filled(rect.translate(egui::vec2(0.0, 4.0 * self.s)), r, self.ink());
        painter.rect(
            rect,
            r,
            self.c(fill),
            stroke(self.line(2.5), self.ink()),
            StrokeKind::Inside,
        );
    }

    /// The sky outline around a picked thing.
    fn picked(&self, painter: &egui::Painter, rect: egui::Rect, radius: f32) {
        let gap = 4.5 * self.s;
        painter.rect_stroke(
            rect.expand(gap),
            self.radius(radius + 4.5),
            stroke(self.line(3.0), self.c(self.p.io)),
            StrokeKind::Middle,
        );
    }

    fn text(
        &self,
        painter: &egui::Painter,
        pos: Pos2,
        align: Align2,
        text: impl ToString,
        font: FontId,
        color: Color,
    ) -> egui::Rect {
        painter.text(pos, align, text, font, self.c(color))
    }

    fn heavy(&self, size: f32) -> FontId {
        heavy(size * self.s)
    }

    fn body(&self, size: f32) -> FontId {
        FontId::proportional(size * self.s)
    }

    /// A chunky button filling `rect` (its bottom edge included).
    fn button(
        &self,
        ui: &Ui,
        rect: egui::Rect,
        id: &str,
        label: &str,
        primary: bool,
    ) -> egui::Response {
        let response = ui
            .interact(rect, ui.id().with(id), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let edge = 3.0 * self.s;
        let pressed = response.is_pointer_button_down_on();
        let face = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, if pressed { edge } else { 0.0 }),
            rect.size() - egui::vec2(0.0, edge),
        );
        let painter = ui.painter();
        let r = self.radius(11.0);
        if !pressed {
            painter.rect_filled(face.translate(egui::vec2(0.0, edge)), r, self.ink());
        }
        let fill = if primary {
            self.p.on
        } else if response.hovered() {
            self.p.hover
        } else {
            self.p.raised
        };
        painter.rect(
            face,
            r,
            self.c(fill),
            stroke(self.line(2.5), self.ink()),
            StrokeKind::Inside,
        );
        let colour = if primary { self.p.on_ink } else { self.p.text };
        let font = self.heavy(14.0);
        let room = face.width() - 20.0 * self.s;
        let fits = |t: &str| {
            painter
                .layout_no_wrap(t.to_string(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
                <= room
        };
        // Too long for the face: drop words from the end.
        let mut text = label.to_string();
        let mut words: Vec<&str> = label.split(' ').collect();
        while !fits(&text) && words.len() > 1 {
            words.pop();
            text = format!("{}…", words.join(" "));
        }
        self.text(
            painter,
            face.center(),
            Align2::CENTER_CENTER,
            text,
            font,
            colour,
        );
        response
    }

    /// Width of a button for `label`.
    fn button_width(&self, painter: &egui::Painter, label: &str) -> f32 {
        painter
            .layout_no_wrap(label.into(), self.heavy(14.0), Color32::PLACEHOLDER)
            .size()
            .x
            + 30.0 * self.s
    }

    /// A toggle chip, ink-filled when `on`. Returns its response.
    fn chip(&self, ui: &Ui, rect: egui::Rect, id: &str, label: &str, on: bool) -> egui::Response {
        let response = ui
            .interact(rect, ui.id().with(id), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let fill = if on {
            self.p.text
        } else if response.hovered() {
            self.p.hover
        } else {
            self.p.raised
        };
        ui.painter().rect(
            rect,
            self.radius(9.0),
            self.c(fill),
            stroke(self.line(2.0), self.ink()),
            StrokeKind::Inside,
        );
        self.text(
            ui.painter(),
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            self.heavy(13.0),
            if on { self.p.tile } else { self.p.text },
        );
        response
    }

    fn chip_width(&self, painter: &egui::Painter, label: &str) -> f32 {
        painter
            .layout_no_wrap(label.into(), self.heavy(13.0), Color32::PLACEHOLDER)
            .size()
            .x
            + 26.0 * self.s
    }

    /// The visited box: marigold when ticked.
    fn tick_box(&self, painter: &egui::Painter, rect: egui::Rect, ticked: bool, hovered: bool) {
        let fill = if ticked {
            self.p.on
        } else if hovered {
            self.p.hover
        } else {
            self.p.tile
        };
        painter.rect(
            rect,
            self.radius(6.0),
            self.c(fill),
            stroke(self.line(2.5), self.ink()),
            StrokeKind::Inside,
        );
    }
}

fn to_pos(v: Vec2) -> Pos2 {
    egui::pos2(v.x, v.y)
}

fn to_rect(r: Rect) -> egui::Rect {
    egui::Rect::from_min_max(to_pos(r.min), to_pos(r.max))
}

/// The demo map, while [`GameState::DemoMap`] is active.
#[allow(clippy::too_many_arguments)]
pub fn demo_map_system(
    mut contexts: EguiContexts,
    game_state: Res<State<GameState>>,
    mut next_state: ResMut<NextState<GameState>>,
    mut theme: ResMut<Theme>,
    mut progress: ResMut<DemoProgress>,
    mut map: ResMut<DemoMapState>,
    editor: Option<ResMut<EditorState>>,
    mut load: MessageWriter<LoadWorkspaceEvent>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    open: (Option<Res<ActiveCircuit>>, Option<Res<WorkspaceRevision>>),
) -> Result {
    if game_state.get() != &GameState::DemoMap {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let catalog = match demos::catalog() {
        Ok(catalog) => catalog,
        Err(e) => {
            egui::Area::new(egui::Id::new("demo_map"))
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Cannot load the demos: {e}"));
                    if ui.button("Back to workshop").clicked() {
                        next_state.set(GameState::Simulating);
                    }
                });
            return Ok(());
        }
    };
    let screen = ctx.content_rect().size();
    let layout = MapLayout::new(Vec2::new(screen.x, screen.y));
    let palette = theme.palette;
    let kit = Kit {
        p: &palette,
        s: layout.scale,
        night: theme.is_night(),
    };
    let status = catalog_status(catalog, &progress);
    if map.level.is_none() {
        *map = initial_selection(catalog, &progress);
    }

    let mut actions = Vec::new();
    egui::Area::new(egui::Id::new("demo_map"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(Order::Background)
        .show(ctx, |ui| {
            let full = egui::Rect::from_min_size(egui::Pos2::ZERO, screen);
            ui.set_min_size(screen);
            ui.set_clip_rect(full);
            paint_bench(ui.painter(), &kit, full);
            header(ui, &kit, &layout, &theme, &mut actions);
            road_map(
                ui,
                &kit,
                &layout,
                catalog,
                (&status, &progress),
                &map,
                &mut actions,
            );
            level_panel(ui, &kit, &layout, catalog, &progress, &map, &mut actions);
            gallery_shelf(ui, &kit, &layout, catalog, &progress, &map, &mut actions);
        });
    if keys.is_some_and(|k| k.just_pressed(KeyCode::Escape)) && !ctx.egui_wants_keyboard_input() {
        actions.push(Action::Back);
    }

    let mut editor = editor;
    for action in actions {
        match action {
            Action::SelectLevel(level) => {
                map.level = Some(level);
                map.demo = default_demo(catalog, level, &progress).map(|d| d.id.clone());
            }
            Action::SelectDemo(id) => map.demo = Some(id),
            Action::Tick(id, ticked) => {
                if progress.set_opened(&id, ticked)
                    && let Err(e) = progress.save()
                {
                    warn!("Cannot save the demo map progress: {e}");
                }
            }
            Action::Category(category) => map.category = category,
            Action::Theme(mode) => theme.set(mode),
            Action::Open(demo) => {
                open_in_editor(
                    editor.as_deref_mut(),
                    &mut load,
                    &open,
                    OpenRequest::Workspace {
                        name: demo.title.clone(),
                        workspace: Arc::clone(&demo.workspace),
                        read_only: true,
                        unsaved: false,
                        status: None,
                    },
                );
                next_state.set(GameState::Simulating);
            }
            Action::Copy(demo) => {
                // As the Library's "Copy to my workspace": fresh ids, saved
                // right away under a free name.
                if progress.mark_opened(&demo.id)
                    && let Err(e) = progress.save()
                {
                    warn!("Cannot save the demo map progress: {e}");
                }
                open_in_editor(
                    editor.as_deref_mut(),
                    &mut load,
                    &open,
                    OpenRequest::CopyDemo(demo.id.clone()),
                );
                next_state.set(GameState::Simulating);
            }
            Action::OpenChallenge(challenge) => {
                open_in_editor(
                    editor.as_deref_mut(),
                    &mut load,
                    &open,
                    super::challenge::challenge_request(challenge, false),
                );
                next_state.set(GameState::Simulating);
            }
            Action::Back => next_state.set(GameState::Simulating),
        }
    }
    Ok(())
}

/// Open `request` in the editor as the Library does: behind the
/// unsaved-changes guard, which the workshop shows.
fn open_in_editor(
    editor: Option<&mut EditorState>,
    load: &mut MessageWriter<LoadWorkspaceEvent>,
    (active, revision): &(Option<Res<ActiveCircuit>>, Option<Res<WorkspaceRevision>>),
    request: OpenRequest,
) {
    match (editor, active, revision) {
        (Some(editor), Some(active), Some(revision)) => {
            editor.library_open = false;
            request_open(editor, load, active, revision, request);
        }
        (editor, _, _) => open_now(editor, load, request),
    }
}

/// The bench: ground and dot grid.
fn paint_bench(painter: &egui::Painter, kit: &Kit, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, kit.c(kit.p.bench));
    let step = 20.0 * kit.s;
    let dot = kit.c(kit.p.bench_dot);
    let r = 1.4 * kit.s;
    let mut y = step * 0.5;
    while y < rect.max.y {
        let mut x = step * 0.5;
        while x < rect.max.x {
            painter.circle_filled(egui::pos2(x, y), r, dot);
            x += step;
        }
        y += step;
    }
}

/// "Demo map", its line, the day/night switch and "Back to workshop".
fn header(ui: &Ui, kit: &Kit, layout: &MapLayout, theme: &Theme, actions: &mut Vec<Action>) {
    let s = kit.s;
    let r = to_rect(layout.header);
    let painter = ui.painter();
    let title = kit.text(
        painter,
        egui::pos2(r.min.x, r.center().y),
        Align2::LEFT_CENTER,
        "Demo map",
        kit.heavy(30.0),
        kit.p.text,
    );
    let back_label = "Back to workshop";
    let back_w = kit.button_width(painter, back_label) + 22.0 * s;
    let h = 41.0 * s;
    let back = egui::Rect::from_min_size(
        egui::pos2(r.max.x - back_w, r.center().y - h * 0.5),
        egui::vec2(back_w, h),
    );
    let mode = egui::Rect::from_min_size(
        egui::pos2(back.min.x - 10.0 * s - h, back.min.y),
        egui::vec2(h, h),
    );
    // The line, if it fits before the buttons.
    let line = "The curriculum is a road from one gate to a working CPU. Every stop is built from the stops before it.";
    let room = mode.min.x - 24.0 * s - (title.max.x + 18.0 * s);
    let galley = painter.layout(
        line.into(),
        kit.body(15.0),
        kit.c(kit.p.text_secondary),
        room.max(1.0),
    );
    if galley.rows.len() <= 2 && room > 200.0 * s {
        painter.galley(
            egui::pos2(title.max.x + 18.0 * s, r.center().y - galley.size().y * 0.5),
            galley,
            Color32::PLACEHOLDER,
        );
    }
    let response = kit
        .button(ui, back.with_min_x(back.min.x), "map_back", "", false)
        .on_hover_text("Return to the editor (Esc)");
    let pressed = if response.is_pointer_button_down_on() {
        3.0 * s
    } else {
        0.0
    };
    let face_mid = back.center().y - 1.5 * s + pressed;
    let arrow_x = back.min.x + 16.0 * s;
    let ink = kit.c(kit.p.text);
    let line = stroke(kit.line(2.5), ink);
    let a = |x: f32, y: f32| egui::pos2(arrow_x + x * s, face_mid + y * s);
    painter.line_segment([a(0.0, 0.0), a(12.0, 0.0)], line);
    painter.line_segment([a(0.0, 0.0), a(5.0, -5.0)], line);
    painter.line_segment([a(0.0, 0.0), a(5.0, 5.0)], line);
    kit.text(
        painter,
        egui::pos2(arrow_x + 20.0 * s, face_mid),
        Align2::LEFT_CENTER,
        back_label,
        kit.heavy(14.0),
        kit.p.text,
    );
    if response.clicked() {
        actions.push(Action::Back);
    }
    let (icon, other, tip) = if theme.is_night() {
        (HudIcon::Sun, ThemeMode::Day, "Day workshop (T)")
    } else {
        (HudIcon::Moon, ThemeMode::Night, "Night workshop (T)")
    };
    let response = kit
        .button(ui, mode, "map_theme", "", false)
        .on_hover_text(tip);
    paint_icon(
        ui.painter(),
        mode.center() - egui::vec2(0.0, 1.5 * s),
        icon,
        kit.c(kit.p.text),
        kit.p,
    );
    if response.clicked() {
        actions.push(Action::Theme(other));
    }
}

/// Paint a thick polyline with round ends.
fn road_line(painter: &egui::Painter, points: &[Vec2], width: f32, colour: Color32) {
    if points.len() < 2 {
        return;
    }
    let pts: Vec<Pos2> = points.iter().map(|p| to_pos(*p)).collect();
    painter.add(egui::Shape::line(pts.clone(), stroke(width, colour)));
    for end in [pts[0], pts[pts.len() - 1]] {
        painter.circle_filled(end, width * 0.5, colour);
    }
}

/// The road, its stops, "You are here" and the progress card.
fn road_map(
    ui: &Ui,
    kit: &Kit,
    layout: &MapLayout,
    catalog: &'static DemoCatalog,
    (status, progress): (&RoadStatus, &DemoProgress),
    map: &DemoMapState,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let painter = ui.painter();
    kit.card(painter, to_rect(layout.map), 16.0, p.tile);
    let well = to_rect(layout.well());
    painter.rect_filled(well, kit.radius(10.0), kit.c(p.well));

    let levels = &catalog.levels;
    let n = levels.len();
    let road = layout.road(n);
    if road.stops.is_empty() {
        return;
    }

    // Casing, then the travelled core, then the road ahead (white with
    // dashes).
    road_line(painter, &road.points, 22.0 * s, kit.ink());
    let ahead_from = match status.travelled_to {
        Some(t) => {
            road_line(painter, road.points_to(t), 14.0 * s, kit.c(p.on));
            t
        }
        None => 0,
    };
    let ahead = road.points_from(ahead_from);
    // The road ahead: a raised core with dashes that read in both themes.
    let (core, dash) = if kit.night {
        (p.raised, p.off)
    } else {
        (p.tile, p.bench_dot)
    };
    if ahead.len() >= 2 {
        road_line(painter, ahead, 14.0 * s, kit.c(core));
        if status.travelled_to.is_some() {
            // The marigold end stays on top of the white start.
            painter.circle_filled(to_pos(ahead[0]), 7.0 * s, kit.c(p.on));
        }
        let pts: Vec<Pos2> = ahead.iter().map(|v| to_pos(*v)).collect();
        painter.extend(egui::Shape::dashed_line(
            &pts,
            stroke(3.0 * s, kit.c(dash)),
            10.0 * s,
            10.0 * s,
        ));
    }

    // Stops.
    let mut pin_at = None;
    for (i, (stop, level)) in road.stops.iter().zip(levels).enumerate() {
        let state = status.stops.get(i).copied().unwrap_or(StopState::Unopened);
        let current = state == StopState::Current;
        let last = i + 1 == n;
        let size = stop_size(i, n, current) * s;
        let edge = if current || last { 6.0 } else { 4.0 } * s;
        let face = egui::Rect::from_center_size(to_pos(stop.pos), egui::vec2(size.x, size.y));
        let radius = if last {
            16.0
        } else if current {
            13.0
        } else {
            12.0
        };
        let response = ui
            .interact(
                face.expand(6.0 * s),
                ui.id().with(("stop", i)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Stop {}: {}", level.level, level.title));
        if response.clicked() {
            actions.push(Action::SelectLevel(level.level));
        }
        if current {
            painter.circle_stroke(
                face.center() + egui::vec2(0.0, 3.0 * s),
                size.x.max(size.y) * 0.5 + 11.0 * s,
                stroke(4.0 * s, kit.c(p.io)),
            );
            pin_at = Some((face, i));
        }
        let selected = map.level == Some(level.level);
        if selected && !current {
            kit.picked(painter, face.with_max_y(face.max.y + edge), radius);
        }
        let fill = match state {
            StopState::Visited => p.on,
            StopState::Current => p.io_tint,
            StopState::Unopened if response.hovered() => p.hover,
            StopState::Unopened => p.tile,
        };
        let r = kit.radius(radius);
        painter.rect_filled(face.translate(egui::vec2(0.0, edge)), r, kit.ink());
        painter.rect(
            face,
            r,
            kit.c(fill),
            stroke(kit.line(if current || last { 3.0 } else { 2.5 }), kit.ink()),
            StrokeKind::Inside,
        );
        let (done, total) = challenges::level_count(catalog, level.level, progress);
        challenges::stop_badge(painter, kit, face, done, total);
        let (number_colour, label_colour) = match state {
            StopState::Visited => (p.on_ink, p.text),
            StopState::Current => (p.text, p.text),
            StopState::Unopened => (p.text_faint, p.text_secondary),
        };
        if last {
            if state == StopState::Unopened {
                let inner = face.shrink2(egui::vec2(14.0, 14.0) * s);
                let corners = [
                    inner.left_top(),
                    inner.right_top(),
                    inner.right_bottom(),
                    inner.left_bottom(),
                    inner.left_top(),
                ];
                painter.extend(egui::Shape::dashed_line(
                    &corners,
                    stroke(2.0 * s, kit.c(p.bench_dot)),
                    3.0 * s,
                    5.0 * s,
                ));
            }
            kit.text(
                painter,
                face.center(),
                Align2::CENTER_CENTER,
                level.short_title(),
                kit.heavy(18.0),
                number_colour,
            );
        } else {
            kit.text(
                painter,
                face.center(),
                Align2::CENTER_CENTER,
                level.level,
                kit.heavy(if current { 22.0 } else { 20.0 }),
                number_colour,
            );
        }
        if current {
            continue; // "You are here" instead of the name.
        }
        let name = if last {
            format!("{} · {}", level.level, level.title)
        } else {
            level.short_title().to_string()
        };
        let font = kit.heavy(13.0);
        let gap = 12.0 * s;
        match stop.label {
            LabelSide::Below => kit.text(
                painter,
                egui::pos2(face.center().x, face.max.y + edge + 6.0 * s),
                Align2::CENTER_TOP,
                name,
                font,
                label_colour,
            ),
            LabelSide::Right => kit.text(
                painter,
                egui::pos2(face.max.x + gap, face.center().y),
                Align2::LEFT_CENTER,
                name,
                font,
                label_colour,
            ),
            LabelSide::Left => kit.text(
                painter,
                egui::pos2(face.min.x - gap, face.center().y),
                Align2::RIGHT_CENTER,
                name,
                font,
                label_colour,
            ),
        };
    }

    // "You are here": a tomato pin above the current stop.
    if let Some((face, i)) = pin_at {
        let tip = egui::pos2(face.center().x, face.min.y - 10.0 * s);
        let r = 22.0 * s;
        let centre = tip - egui::vec2(0.0, 30.0 * s);
        let mut outline: Vec<Pos2> = (0..=20)
            .map(|k| {
                let a =
                    -std::f32::consts::PI - 0.62 + (std::f32::consts::PI + 1.24) * k as f32 / 20.0;
                centre + egui::vec2(a.cos(), a.sin()) * r
            })
            .collect();
        outline.push(tip);
        painter.add(egui::Shape::convex_polygon(
            outline,
            kit.c(p.fault),
            stroke(kit.line(2.5), kit.ink()),
        ));
        painter.circle(
            centre,
            7.0 * s,
            kit.c(p.tile),
            stroke(kit.line(2.0), kit.ink()),
        );
        let text = "You are here";
        let font = kit.heavy(14.0);
        let width = painter
            .layout_no_wrap(text.into(), font.clone(), Color32::PLACEHOLDER)
            .size()
            .x;
        let right = centre.x + r + 18.0 * s;
        let (pos, align) = if right + width < well.max.x - 8.0 * s || i == 0 {
            (egui::pos2(right, centre.y), Align2::LEFT_CENTER)
        } else {
            (
                egui::pos2(centre.x - r - 18.0 * s, centre.y),
                Align2::RIGHT_CENTER,
            )
        };
        kit.text(painter, pos, align, text, font, p.text);
    }

    progress_card(ui, kit, layout, catalog, (status, progress), actions);
}

/// "Your road so far": a bar, "N/M demos · K/L challenges", the next stop.
fn progress_card(
    ui: &Ui,
    kit: &Kit,
    layout: &MapLayout,
    catalog: &DemoCatalog,
    (status, progress): (&RoadStatus, &DemoProgress),
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let rect = to_rect(layout.progress_card());
    let next = status.next.and_then(|i| catalog.levels.get(i));
    let response = ui
        .interact(rect, ui.id().with("progress"), Sense::click())
        .on_hover_text(if next.is_some() {
            "Go to the next stop"
        } else {
            "Every stop visited"
        });
    if response.clicked()
        && let Some(level) = next
    {
        actions.push(Action::SelectLevel(level.level));
    }
    let painter = ui.painter();
    kit.card(painter, rect, 14.0, p.tile);
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y) * s;
    kit.text(
        painter,
        at(16.0, 22.0),
        Align2::LEFT_CENTER,
        "Your road so far",
        kit.heavy(13.0),
        p.text,
    );
    let bar = egui::Rect::from_min_size(at(16.0, 40.0), egui::vec2(178.0, 14.0) * s);
    let r = kit.radius(7.0);
    let line = stroke(kit.line(2.0), kit.ink());
    painter.rect(bar, r, kit.c(p.well), line, StrokeKind::Inside);
    if status.opened > 0 && status.total > 0 {
        let frac = status.opened as f32 / status.total as f32;
        let filled = bar.with_max_x(bar.min.x + (bar.width() * frac).max(bar.height()));
        painter.rect(filled, r, kit.c(p.on), line, StrokeKind::Inside);
    }
    kit.text(
        painter,
        at(16.0, 72.0),
        Align2::LEFT_CENTER,
        if catalog.challenges.is_empty() {
            format!("{} of {} demos opened", status.opened, status.total)
        } else {
            let done = progress.completed_of(catalog.challenges.iter().map(|c| c.id.as_str()));
            format!(
                "{}/{} demos · {done}/{} challenges",
                status.opened,
                status.total,
                catalog.challenges.len()
            )
        },
        FontId::monospace(11.0 * s),
        p.text,
    );
    match next {
        Some(level) => {
            kit.text(
                painter,
                at(16.0, 100.0),
                Align2::LEFT_CENTER,
                "Next stop:",
                kit.body(12.0),
                p.text_secondary,
            );
            let name = format!("{} · {}", level.level, level.short_title());
            kit.text(
                painter,
                at(16.0, 122.0),
                Align2::LEFT_CENTER,
                name,
                kit.heavy(14.0),
                p.text,
            );
        }
        None => {
            kit.text(
                painter,
                at(16.0, 110.0),
                Align2::LEFT_CENTER,
                "Every stop visited!",
                kit.heavy(14.0),
                p.text,
            );
        }
    }
}

/// The picked level: "Stop N", title, description, its demos, the buttons.
fn level_panel(
    ui: &mut Ui,
    kit: &Kit,
    layout: &MapLayout,
    catalog: &'static DemoCatalog,
    progress: &DemoProgress,
    map: &DemoMapState,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let rect = to_rect(layout.panel);
    let Some(level) = map
        .level
        .and_then(|l| catalog.levels.iter().find(|x| x.level == l))
    else {
        return;
    };
    let painter = ui.painter().clone();
    let mut y = rect.min.y;
    let place = |painter: &egui::Painter,
                 y: &mut f32,
                 text: String,
                 font: FontId,
                 colour: Color,
                 gap: f32| {
        let galley = painter.layout(text, font, kit.c(colour), rect.width());
        let h = galley.size().y;
        painter.galley(egui::pos2(rect.min.x, *y), galley, Color32::PLACEHOLDER);
        *y += h + gap * s;
    };
    let stop = egui::RichText::new(format!("STOP {}", level.level))
        .font(kit.heavy(12.0))
        .extra_letter_spacing(1.2 * s)
        .color(kit.c(p.text_secondary));
    let galley = egui::WidgetText::from(stop).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        rect.width(),
        egui::FontSelection::Default,
    );
    let h = galley.size().y;
    painter.galley(egui::pos2(rect.min.x, y), galley, Color32::PLACEHOLDER);
    y += h + 2.0 * s;
    place(
        &painter,
        &mut y,
        level.title.clone(),
        kit.heavy(26.0),
        p.text,
        4.0,
    );
    if !level.description.is_empty() {
        place(
            &painter,
            &mut y,
            level.description.clone(),
            kit.body(14.0),
            p.text_secondary,
            0.0,
        );
    }
    y += 14.0 * s;

    // Buttons and the note at the bottom; the cards scroll between.
    let note = "Demos open read-only. Copy one to change it.";
    let note_galley = painter.layout(
        note.into(),
        kit.body(13.0),
        kit.c(p.text_secondary),
        rect.width(),
    );
    let note_h = note_galley.size().y;
    let button_h = 41.0 * s;
    let buttons_y = rect.max.y - note_h - 10.0 * s - button_h;

    let demos: Vec<&'static Demo> = catalog
        .demos
        .iter()
        .filter(|d| d.level == level.level)
        .collect();
    let selected = demos
        .iter()
        .copied()
        .find(|d| map.demo.as_deref() == Some(d.id.as_str()))
        .or(demos.first().copied());

    let cards = egui::Rect::from_min_max(
        egui::pos2(rect.min.x - 8.0 * s, y - 6.0 * s),
        egui::pos2(rect.max.x + 8.0 * s, buttons_y - 12.0 * s),
    );
    let cards = cards.round_to_pixels(ui.pixels_per_point());
    let mut cards_bottom = cards.min.y;
    if cards.height() > 20.0 * s {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cards)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(cards);
        let output = egui::ScrollArea::vertical()
            .id_salt("map_demo_cards")
            .max_height(cards.height())
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                ui.add_space((6.0 * s).round());
                ui.spacing_mut().item_spacing.y = (16.0 * s).round();
                for demo in &demos {
                    demo_card(
                        ui,
                        kit,
                        demo,
                        progress.is_opened(&demo.id),
                        selected.is_some_and(|d| d.id == demo.id),
                        actions,
                    );
                }
                let level_challenges = challenges::of_level(catalog, level.level);
                if !level_challenges.is_empty() {
                    let (done, total) = challenges::level_count(catalog, level.level, progress);
                    challenges::heading(ui, kit, done, total);
                    for challenge in level_challenges {
                        challenges::card(
                            ui,
                            kit,
                            challenge,
                            progress.is_completed(&challenge.id),
                            actions,
                        );
                    }
                }
                ui.add_space((4.0 * s).round());
            });
        cards_bottom = (cards.min.y + output.content_size.y).min(cards.max.y);
    }
    // The buttons follow the cards, as low as the bottom of the panel.
    let buttons_y = buttons_y.min(cards_bottom + 8.0 * s);

    if let Some(demo) = selected {
        let copy_label = "Copy to my workshop";
        let copy_w = kit.button_width(&painter, copy_label);
        let gap = 10.0 * s;
        let open = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, buttons_y),
            egui::vec2(rect.width() - copy_w - gap, button_h),
        );
        let copy = egui::Rect::from_min_size(
            egui::pos2(open.max.x + gap, buttons_y),
            egui::vec2(copy_w, button_h),
        );
        if kit
            .button(ui, open, "map_open", &format!("Open {}", demo.title), true)
            .on_hover_text(&demo.description)
            .clicked()
        {
            actions.push(Action::Open(demo));
        }
        if kit
            .button(ui, copy, "map_copy", copy_label, false)
            .on_hover_text("An editable copy, saved with your workspaces")
            .clicked()
        {
            actions.push(Action::Copy(demo));
        }
    }
    painter.galley(
        egui::pos2(rect.min.x, buttons_y + button_h + 10.0 * s),
        note_galley,
        Color32::PLACEHOLDER,
    );
}

/// A demo card: thumbnail, title, description, visited box.
fn demo_card(
    ui: &mut Ui,
    kit: &Kit,
    demo: &'static Demo,
    opened: bool,
    selected: bool,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let outline = 8.0 * s;
    let width = ui.available_width() - 2.0 * outline;
    let pad = 10.0 * s;
    let thumb = egui::vec2(120.0, 78.0) * s;
    let tick = 18.0 * s;
    let text_w = width - 2.0 * pad - thumb.x - 12.0 * s - tick - 12.0 * s;
    let painter = ui.painter().clone();
    let title = painter.layout(demo.title.clone(), kit.heavy(16.0), kit.c(p.text), text_w);
    // Long descriptions stop after three lines; the tooltip has it all.
    let desc = painter.layout_job(egui::text::LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: text_w,
            max_rows: 3,
            break_anywhere: false,
            overflow_character: Some('…'),
        },
        ..egui::text::LayoutJob::simple(
            demo.description.clone(),
            kit.body(13.0),
            kit.c(p.text_secondary),
            text_w,
        )
    });
    let desc_h = desc.size().y;
    let height = thumb.y.max(title.size().y + 4.0 * s + desc_h) + 2.0 * pad;
    let (outer, _) = ui.allocate_exact_size(
        egui::vec2(width + 2.0 * outline, height + 4.0 * s).round(),
        Sense::hover(),
    );
    let rect = egui::Rect::from_min_size(
        outer.min + egui::vec2(outline, 0.0),
        egui::vec2(width, height),
    );
    let response = ui
        .interact(rect, ui.id().with(("demo", &demo.id)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&demo.description);
    if response.clicked() {
        actions.push(Action::SelectDemo(demo.id.clone()));
    }
    if response.double_clicked() {
        actions.push(Action::Open(demo));
    }
    if selected {
        kit.picked(&painter, rect, 14.0);
    }
    kit.card(
        &painter,
        rect,
        14.0,
        if response.hovered() { p.hover } else { p.tile },
    );
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad, rect.center().y - thumb.y * 0.5),
        thumb,
    );
    thumbnail::paint_thumbnail(
        &painter,
        p,
        thumb_rect,
        &demo.workspace.root,
        (9.0 * s) as u8,
    );
    let text_x = thumb_rect.max.x + 12.0 * s;
    let block_h = title.size().y + 4.0 * s + desc_h;
    let top = rect.center().y - block_h * 0.5;
    let title_h = title.size().y;
    painter.galley(egui::pos2(text_x, top), title, Color32::PLACEHOLDER);
    painter.galley(
        egui::pos2(text_x, top + title_h + 4.0 * s),
        desc,
        Color32::PLACEHOLDER,
    );
    let tick_rect = egui::Rect::from_center_size(
        egui::pos2(rect.max.x - pad - tick * 0.5, rect.center().y),
        egui::vec2(tick, tick),
    );
    let tick_response = ui
        .interact(
            tick_rect.expand(4.0 * s),
            ui.id().with(("tick", &demo.id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if opened {
            "Opened. Click to clear."
        } else {
            "Not opened yet. Click to mark it opened."
        });
    kit.tick_box(&painter, tick_rect, opened, tick_response.hovered());
    if tick_response.clicked() {
        actions.push(Action::Tick(demo.id.clone(), !opened));
    }
}

/// "Gallery", its line, the category chips and a row of cards.
fn gallery_shelf(
    ui: &mut Ui,
    kit: &Kit,
    layout: &MapLayout,
    catalog: &'static DemoCatalog,
    progress: &DemoProgress,
    map: &DemoMapState,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let rect = to_rect(layout.gallery);
    let painter = ui.painter().clone();
    let row_y = rect.min.y + 15.0 * s;
    let title = kit.text(
        &painter,
        egui::pos2(rect.min.x, row_y),
        Align2::LEFT_CENTER,
        "Gallery",
        kit.heavy(18.0),
        p.text,
    );

    // Chips, right-aligned: All, then each category.
    let mut chips: Vec<(Option<String>, String)> = vec![(None, "All".into())];
    chips.extend(
        catalog
            .categories
            .iter()
            .map(|c| (Some(c.id.clone()), c.title.clone())),
    );
    let chip_h = 30.0 * s;
    let gap = 6.0 * s;
    let total: f32 = chips
        .iter()
        .map(|(_, t)| kit.chip_width(&painter, t) + gap)
        .sum::<f32>()
        - gap;
    let mut x = rect.max.x - total;
    for (id, label) in &chips {
        let w = kit.chip_width(&painter, label);
        let chip =
            egui::Rect::from_min_size(egui::pos2(x, row_y - chip_h * 0.5), egui::vec2(w, chip_h));
        let on = map.category == *id;
        if kit
            .chip(ui, chip, &format!("chip_{label}"), label, on)
            .clicked()
            && !on
        {
            actions.push(Action::Category(id.clone()));
        }
        x += w + gap;
    }
    let line = "Bigger builds to take apart, outside the road.";
    let room = rect.max.x - total - 24.0 * s - (title.max.x + 12.0 * s);
    if painter
        .layout_no_wrap(line.into(), kit.body(13.0), Color32::PLACEHOLDER)
        .size()
        .x
        <= room
    {
        kit.text(
            &painter,
            egui::pos2(title.max.x + 12.0 * s, row_y),
            Align2::LEFT_CENTER,
            line,
            kit.body(13.0),
            p.text_secondary,
        );
    }

    // Cards: six across; the row scrolls sideways when there are more.
    let items = gallery_items(&catalog.gallery, map.category.as_deref());
    let card_gap = 14.0 * s;
    let card_w = (rect.width() - 5.0 * card_gap) / 6.0;
    let card_h = MapLayout::GALLERY_CARD_H * s;
    let row = egui::Rect::from_min_max(
        egui::pos2(rect.min.x, rect.max.y - card_h - 4.0 * s),
        egui::pos2(rect.max.x, rect.max.y + 6.0 * s),
    );
    let row = row.round_to_pixels(ui.pixels_per_point());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Min)),
    );
    child.set_clip_rect(row.expand2(egui::vec2(4.0 * s, 0.0)));
    egui::ScrollArea::horizontal()
        .id_salt(("map_gallery", map.category.clone()))
        .auto_shrink([false, true])
        .show(&mut child, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = card_gap.round();
                for item in items {
                    gallery_card(
                        ui,
                        kit,
                        item,
                        card_w,
                        card_h,
                        progress.is_opened(&item.id),
                        actions,
                    );
                }
            });
        });
}

fn gallery_card(
    ui: &mut Ui,
    kit: &Kit,
    item: &'static Demo,
    width: f32,
    height: f32,
    opened: bool,
    actions: &mut Vec<Action>,
) {
    let s = kit.s;
    let p = kit.p;
    let (outer, response) =
        ui.allocate_exact_size(egui::vec2(width, height + 4.0 * s).round(), Sense::click());
    let rect = outer.with_max_y(outer.max.y - 4.0 * s);
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("{}\n\n{}", item.title, item.description));
    if response.clicked() {
        actions.push(Action::Open(item));
    }
    let painter = ui.painter();
    kit.card(
        painter,
        rect,
        14.0,
        if response.hovered() { p.hover } else { p.tile },
    );
    let pad = 8.0 * s;
    let thumb = egui::Rect::from_min_size(
        rect.min + egui::vec2(pad, pad),
        egui::vec2(rect.width() - 2.0 * pad, 56.0 * s),
    );
    thumbnail::paint_thumbnail(painter, p, thumb, &item.workspace.root, (8.0 * s) as u8);
    let tick = 12.0 * s;
    let title_rect = egui::Rect::from_min_max(
        egui::pos2(thumb.min.x, thumb.max.y + 6.0 * s),
        egui::pos2(
            thumb.max.x - if opened { tick + 6.0 * s } else { 0.0 },
            rect.max.y - pad,
        ),
    );
    let galley = painter.layout_job(egui::text::LayoutJob {
        wrap: egui::text::TextWrapping::truncate_at_width(title_rect.width()),
        ..egui::text::LayoutJob::simple_singleline(
            item.title.clone(),
            kit.heavy(14.0),
            kit.c(p.text),
        )
    });
    painter.galley(
        egui::pos2(
            title_rect.min.x,
            title_rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        Color32::PLACEHOLDER,
    );
    if opened {
        let r = egui::Rect::from_center_size(
            egui::pos2(thumb.max.x - tick * 0.5, title_rect.center().y),
            egui::vec2(tick, tick),
        );
        kit.tick_box(painter, r, true, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_filter_keeps_catalog_order_per_category() {
        let catalog = demos::catalog().unwrap();
        let all = gallery_items(&catalog.gallery, None);
        assert_eq!(all.len(), catalog.gallery.len());
        let mut total = 0;
        for category in &catalog.categories {
            let items = gallery_items(&catalog.gallery, Some(&category.id));
            assert!(!items.is_empty(), "{}", category.id);
            assert!(
                items
                    .iter()
                    .all(|d| d.category.as_deref() == Some(category.id.as_str()))
            );
            // In catalog order.
            let positions: Vec<usize> = items
                .iter()
                .map(|d| catalog.gallery.iter().position(|g| g.id == d.id).unwrap())
                .collect();
            assert!(positions.windows(2).all(|w| w[0] < w[1]));
            total += items.len();
        }
        assert_eq!(total, all.len(), "every item is on one shelf");
        assert!(gallery_items(&catalog.gallery, Some("no such shelf")).is_empty());
    }

    #[test]
    fn the_map_opens_on_the_current_stop_with_its_next_demo() {
        let catalog = demos::catalog().unwrap();
        // Nothing opened: stop 1, its first demo.
        let fresh = initial_selection(catalog, &DemoProgress::in_memory());
        assert_eq!(fresh.level, Some(1));
        assert_eq!(fresh.demo.as_deref(), Some("gates"));

        // As in the mockup: levels 1 to 6 and the register opened. Stop 7
        // is current with the counter to open; stop 8 is next.
        let mut ids: Vec<&str> = catalog
            .demos
            .iter()
            .filter(|d| d.level <= 6)
            .map(|d| d.id.as_str())
            .collect();
        ids.push("register");
        let progress = DemoProgress::with_opened(ids);
        let picked = initial_selection(catalog, &progress);
        assert_eq!(picked.level, Some(7));
        assert_eq!(picked.demo.as_deref(), Some("counter"));
        let status = catalog_status(catalog, &progress);
        assert_eq!(status.next.map(|i| catalog.levels[i].level), Some(8));
        assert_eq!((status.opened, status.total), (12, 16));

        // A finished level picks the demo opened last.
        let progress = DemoProgress::with_opened(["counter", "register"]);
        assert_eq!(
            default_demo(catalog, 7, &progress).map(|d| d.id.as_str()),
            Some("register")
        );
    }
}
