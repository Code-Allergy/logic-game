//! The Workshop look for egui: bundled fonts (Nunito, JetBrains Mono),
//! a [`egui::Style`] built from the [`Theme`], chunky buttons and chips
//! with a solid bottom edge, floating HUD cards, and the [`HudLayout`]
//! that places the cards over the canvas.

use std::sync::Arc;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response, Sense,
    Shadow, Stroke, StrokeKind, TextStyle, Ui, Vec2 as EVec2, WidgetText,
};

use crate::fonts;
use crate::settings::{Settings, effective_ui_scale};
use crate::theme::{Palette, Theme, ThemeMode};

/// The heavy face (Nunito ExtraBold), for titles, names and buttons.
pub const HEAVY: &str = "heavy";

/// A Bevy colour as an egui colour.
pub fn egui_color(color: Color) -> Color32 {
    let c = color.to_srgba();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(byte(c.red), byte(c.green), byte(c.blue), byte(c.alpha))
}

/// Nunito ExtraBold at `size`. Only valid once [`workshop_fonts`] are
/// installed; widgets use [`TextStyle::Button`] and [`TextStyle::Heading`],
/// which name this family once the Workshop style is applied.
pub fn heavy(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(HEAVY.into()))
}

/// Whether the Workshop fonts are installed in `ctx` yet. egui takes new
/// fonts at the start of the next pass, so for the first frame or two
/// [`heavy`] text would panic; the HUD systems wait for this.
pub fn workshop_fonts_ready(ctx: &egui::Context) -> bool {
    ctx.fonts(|f| {
        f.definitions()
            .families
            .contains_key(&FontFamily::Name(HEAVY.into()))
    })
}

/// egui's default fonts with Nunito SemiBold first for proportional text,
/// JetBrains Mono Bold first for monospace, and the heavy family. The
/// defaults stay as fallbacks for symbols and emoji.
pub fn workshop_fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    for (name, bytes) in [
        ("Nunito-SemiBold", fonts::NUNITO_SEMIBOLD),
        ("Nunito-ExtraBold", fonts::NUNITO_EXTRABOLD),
        ("JetBrainsMono-Bold", fonts::JETBRAINS_MONO_BOLD),
    ] {
        defs.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    let fallbacks = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    defs.families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Nunito-SemiBold".into());
    defs.families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono-Bold".into());
    let mut heavy = vec!["Nunito-ExtraBold".to_string()];
    heavy.extend(fallbacks);
    defs.families.insert(FontFamily::Name(HEAVY.into()), heavy);
    defs
}

/// An egui stroke of width `width`.
pub fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

fn shadow(ink: Color32, drop: i8) -> Shadow {
    Shadow {
        offset: [0, drop],
        blur: 0,
        spread: 0,
        color: ink,
    }
}

/// The egui style of a theme: white (or night) cards with a 2.5 ink
/// border, radius 14 and a solid 4-unit bottom shadow; ink-outlined
/// widgets; Nunito and JetBrains Mono.
pub fn workshop_style(theme: &Theme) -> egui::Style {
    let p = &theme.palette;
    let c = egui_color;
    let ink = c(p.ink);
    let text = c(p.text);
    let mut style = egui::Style::default();
    let mut v = if theme.is_night() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(text);
    v.weak_text_color = Some(c(p.text_secondary));
    v.panel_fill = c(p.tile);
    v.window_fill = c(p.tile);
    v.window_stroke = stroke(2.5, ink);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow = shadow(ink, 4);
    v.popup_shadow = shadow(ink, 3);
    v.menu_corner_radius = CornerRadius::same(10);
    v.faint_bg_color = c(p.well);
    v.extreme_bg_color = c(p.raised);
    v.text_edit_bg_color = Some(c(p.raised));
    v.code_bg_color = c(p.well);
    v.hyperlink_color = c(p.io_text);
    v.warn_fg_color = c(p.on_text);
    v.error_fg_color = c(p.fault_text);
    v.selection.bg_fill = c(p.on);
    v.selection.stroke = stroke(2.0, c(p.on_ink));
    v.slider_trailing_fill = true;

    let widget = |fill: Color, outline: Stroke, expansion: f32| egui::style::WidgetVisuals {
        bg_fill: c(fill),
        weak_bg_fill: c(fill),
        bg_stroke: outline,
        corner_radius: CornerRadius::same(10),
        fg_stroke: stroke(1.5, text),
        expansion,
    };
    v.widgets.noninteractive = widget(p.tile, stroke(2.0, c(p.divider)), 0.0);
    v.widgets.inactive = widget(p.raised, stroke(2.0, ink), 0.0);
    v.widgets.hovered = widget(p.hover, stroke(2.0, ink), 1.0);
    v.widgets.active = widget(p.hover, stroke(2.5, ink), 0.0);
    v.widgets.open = widget(p.hover, stroke(2.0, ink), 0.0);
    style.visuals = v;

    style.spacing.item_spacing = EVec2::new(8.0, 7.0);
    style.spacing.button_padding = EVec2::new(12.0, 5.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.interact_size.y = 26.0;
    // The fade is painted in the parent's colour, not the card's.
    style.spacing.scroll.fade.strength = 0.0;

    let prop = |size| FontId::new(size, FontFamily::Proportional);
    style.text_styles = [
        (TextStyle::Heading, heavy(20.0)),
        (TextStyle::Body, prop(14.0)),
        (TextStyle::Button, heavy(14.0)),
        (TextStyle::Small, prop(13.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
    style
}

/// A floating HUD card frame.
pub fn card(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(egui_color(palette.tile))
        .stroke(stroke(2.5, egui_color(palette.ink)))
        .corner_radius(CornerRadius::same(16))
        .shadow(shadow(egui_color(palette.ink), 4))
        .inner_margin(Margin::symmetric(12, 10))
}

/// Height of a chunky button's bottom edge.
const BUTTON_EDGE: f32 = 3.0;

/// A chunky button: ink outline, a solid bottom edge that the button
/// loses while pressed, marigold when `primary`.
pub fn chunky_button(
    ui: &mut Ui,
    palette: &Palette,
    label: impl Into<WidgetText>,
    primary: bool,
) -> Response {
    let galley = button_galley(ui, label);
    let width = chunky_width(&galley);
    chunky_button_with(ui, palette, width, primary, |painter, face, colour| {
        painter.galley(face.center() - galley.size() * 0.5, galley, colour);
    })
}

fn button_galley(ui: &Ui, label: impl Into<WidgetText>) -> std::sync::Arc<egui::Galley> {
    label.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Button,
    )
}

fn chunky_width(galley: &egui::Galley) -> f32 {
    (galley.size().x + 28.0).max(40.0)
}

/// How wide [`chunky_button`] makes a button labelled `label`.
pub fn chunky_button_width(ui: &Ui, label: impl Into<WidgetText>) -> f32 {
    chunky_width(&button_galley(ui, label))
}

/// Width of a [`chunky_icon_button`].
pub const ICON_BUTTON_WIDTH: f32 = 40.0;

/// Icons the HUD paints itself (the bundled fonts lack these glyphs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudIcon {
    Pause,
    Sun,
    Moon,
    /// A folded map (the demo map).
    Map,
}

/// A square chunky button with a painted icon.
pub fn chunky_icon_button(
    ui: &mut Ui,
    palette: &Palette,
    icon: HudIcon,
    primary: bool,
) -> Response {
    let fill = palette;
    chunky_button_with(
        ui,
        palette,
        ICON_BUTTON_WIDTH,
        primary,
        |painter, face, colour| {
            paint_icon(painter, face.center(), icon, colour, fill);
        },
    )
}

/// Paint `icon` centred at `c` in `colour`.
pub fn paint_icon(
    painter: &egui::Painter,
    c: egui::Pos2,
    icon: HudIcon,
    colour: Color32,
    palette: &Palette,
) {
    match icon {
        HudIcon::Pause => {
            for dx in [-4.0, 4.0] {
                painter.rect_filled(
                    egui::Rect::from_center_size(c + EVec2::new(dx, 0.0), EVec2::new(4.5, 14.0)),
                    CornerRadius::same(1),
                    colour,
                );
            }
        }
        HudIcon::Sun => {
            painter.circle(c, 4.5, egui_color(palette.on), stroke(2.0, colour));
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let d = EVec2::new(a.cos(), a.sin());
                painter.line_segment([c + d * 7.5, c + d * 10.0], stroke(2.0, colour));
            }
        }
        HudIcon::Moon => {
            painter.circle_filled(c, 8.0, colour);
            painter.circle_filled(c + EVec2::new(4.0, -3.0), 7.0, egui_color(palette.raised));
        }
        HudIcon::Map => {
            let p = |x: f32, y: f32| c + EVec2::new(x, y);
            let outline = vec![
                p(-9.0, -6.0),
                p(-3.0, -9.0),
                p(3.0, -6.0),
                p(9.0, -9.0),
                p(9.0, 6.0),
                p(3.0, 9.0),
                p(-3.0, 6.0),
                p(-9.0, 9.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                vec![p(3.0, -6.0), p(9.0, -9.0), p(9.0, 6.0), p(3.0, 9.0)],
                egui_color(palette.on),
                Stroke::NONE,
            ));
            painter.add(egui::Shape::closed_line(outline, stroke(2.0, colour)));
            painter.line_segment([p(-3.0, -9.0), p(-3.0, 6.0)], stroke(2.0, colour));
            painter.line_segment([p(3.0, -6.0), p(3.0, 9.0)], stroke(2.0, colour));
        }
    }
}

/// A chunky button `width` wide whose face content `paint` draws (given
/// the face rectangle and the text colour).
pub fn chunky_button_with(
    ui: &mut Ui,
    palette: &Palette,
    width: f32,
    primary: bool,
    paint: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> Response {
    let body = EVec2::new(width, 34.0);
    let (rect, response) =
        ui.allocate_exact_size(body + EVec2::new(0.0, BUTTON_EDGE), Sense::click());
    if ui.is_rect_visible(rect) {
        let ink = egui_color(palette.ink);
        let pressed = response.is_pointer_button_down_on();
        let top = rect.min + EVec2::new(0.0, if pressed { BUTTON_EDGE } else { 0.0 });
        let face = egui::Rect::from_min_size(top, body);
        let painter = ui.painter();
        let radius = CornerRadius::same(10);
        if !pressed {
            painter.rect_filled(face.translate(EVec2::new(0.0, BUTTON_EDGE)), radius, ink);
        }
        let fill = if primary {
            palette.on
        } else if response.hovered() {
            palette.hover
        } else {
            palette.raised
        };
        painter.rect(
            face,
            radius,
            egui_color(fill),
            stroke(2.5, ink),
            StrokeKind::Inside,
        );
        let colour = egui_color(if primary {
            palette.on_ink
        } else {
            palette.text
        });
        paint(painter, face, colour);
    }
    response
}

/// A small uppercase caption (`TICK`, `SPEED`).
pub fn caption(palette: &Palette, text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .text_style(TextStyle::Button)
        .size(12.0)
        .color(egui_color(palette.text_secondary))
}

/// The UI scale in effect for a window of `window` logical pixels: the
/// player's [`Settings`] limited to what the HUD layout has room for
/// ([`effective_ui_scale`]), 100% without settings (tools, tests).
pub fn ui_scale_for(settings: Option<&Settings>, window: Vec2) -> f32 {
    settings.map_or(1.0, |s| effective_ui_scale(s.ui_scale, window))
}

/// Install the fonts once, the style whenever the theme changes, and the
/// UI scale (egui's zoom factor, so every card, text and the
/// [`HudLayout`], which is laid out in egui points, scale together). egui
/// takes new fonts at the start of the next pass, so the style (whose
/// text styles name the heavy family) is applied from the frame after.
pub fn apply_workshop_ui_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    settings: Option<Res<Settings>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut fonts_set: Local<bool>,
    mut applied: Local<Option<ThemeMode>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if !*fonts_set {
        ctx.set_fonts(workshop_fonts());
        // The scale is a setting: keep Ctrl +/- from changing it.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        *fonts_set = true;
        return Ok(());
    }
    if let Ok(window) = windows.single() {
        let scale = ui_scale_for(settings.as_deref(), window.size());
        if (ctx.zoom_factor() - scale).abs() > 1e-4 {
            ctx.set_zoom_factor(scale);
        }
    }
    if *applied != Some(theme.mode) || theme.is_changed() {
        let style = workshop_style(&theme);
        ctx.set_theme(if theme.is_night() {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        ctx.all_styles_mut(|s| *s = style.clone());
        *applied = Some(theme.mode);
    }
    Ok(())
}

/// A compact Day/Night switch: one chunky button showing the other
/// mode's icon. Returns the mode to switch to when clicked.
pub fn day_night_button(ui: &mut Ui, theme: &Theme) -> Option<ThemeMode> {
    let (icon, other, tip) = if theme.is_night() {
        (HudIcon::Sun, ThemeMode::Day, "Day workshop (T)")
    } else {
        (HudIcon::Moon, ThemeMode::Night, "Night workshop (T)")
    };
    chunky_icon_button(ui, &theme.palette, icon, false)
        .on_hover_text(tip)
        .clicked()
        .then_some(other)
}

/// A small toggle chip (speed, width, tabs): ink-filled when `on`.
pub fn chip(ui: &mut Ui, palette: &Palette, text: impl Into<String>, on: bool) -> Response {
    chip_sized(ui, palette, text, on, None)
}

/// A chip at least `min_width` wide (equal-width chip rows).
pub fn chip_sized(
    ui: &mut Ui,
    palette: &Palette,
    text: impl Into<String>,
    on: bool,
    min_width: Option<f32>,
) -> Response {
    let text: String = text.into();
    let galley = ui.painter().layout_no_wrap(
        text,
        heavy(13.0),
        egui_color(if on { palette.tile } else { palette.text }),
    );
    let width = (galley.size().x + 22.0).max(min_width.unwrap_or(0.0));
    let (rect, response) = ui.allocate_exact_size(EVec2::new(width, CHIP_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if on {
            palette.text
        } else if response.hovered() {
            palette.hover
        } else {
            palette.raised
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(9),
            egui_color(fill),
            stroke(2.0, egui_color(palette.ink)),
            StrokeKind::Inside,
        );
        ui.painter().galley(
            rect.center() - galley.size() * 0.5,
            galley,
            Color32::PLACEHOLDER,
        );
    }
    response
}

/// Height of a [`chip`].
pub const CHIP_HEIGHT: f32 = 28.0;

/// A thin vertical rule between groups in a card row.
pub fn rule(ui: &mut Ui, palette: &Palette, height: f32) {
    let (rect, _) = ui.allocate_exact_size(EVec2::new(2.0, height), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(1), egui_color(palette.divider));
}

/// A live value as the HUD shows it: `on`/`off` for one bit, otherwise
/// hex and decimal (`0x2A · 42`).
pub fn format_live_value(width: u8, value: u64) -> String {
    if width <= 1 {
        if value != 0 { "on" } else { "off" }.to_string()
    } else {
        let digits = usize::from(width).div_ceil(4);
        format!("0x{value:0digits$X} · {value}")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HUD layout
// ─────────────────────────────────────────────────────────────────────────────

/// Height of the top row of cards (file, transport, build status).
pub const TOP_CARD_HEIGHT: f32 = 60.0;
/// The zoom row beside the minimap: `−`, the percentage, `+`.
pub const ZOOM_ROW: Vec2 = Vec2::new(176.0, 43.0);
/// Slots in the parts bar: nine numbered parts and the wire tool.
pub const PART_SLOTS: usize = 10;
/// Space between slots, and the extra space before the wire tool.
pub const SLOT_GAP: f32 = 8.0;
pub const TOOL_GAP: f32 = 14.0;

/// The transport card's width (top row) in the full and compact layouts.
/// Its contents are fixed except the tick, which
/// [`crate::sim::systems::tick_label`] keeps to a fixed number of
/// characters.
pub const TRANSPORT_WIDTH: f32 = 730.0;
pub const TRANSPORT_WIDTH_COMPACT: f32 = 628.0;
/// The most the build status card (top right) takes.
pub const STATUS_WIDTH: f32 = 200.0;
/// The least room the file card (top left) gets: the badge, a readable
/// title and its buttons, with both back buttons of a nested view.
pub const FILE_MIN_WIDTH: f32 = 396.0;
/// The exact-rate popover under the transport card.
pub const TRANSPORT_MORE_SIZE: Vec2 = Vec2::new(280.0, 170.0);

/// Where the floating HUD cards sit for a window size (logical pixels,
/// y down): the canvas fills the window and the cards float over it,
/// anchored to its edges. Below 1600 x 900 the layout is compact
/// (smaller margins, slots, minimap and signals card).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudLayout {
    pub screen: Vec2,
    pub compact: bool,
    /// Distance of the cards from the window edges.
    pub margin: f32,
    /// Space between neighbouring cards.
    pub gap: f32,
    pub inspector_width: f32,
    pub library_width: f32,
    pub minimap: Vec2,
    /// The zoom row sits beside the minimap when there is room, else
    /// above it.
    pub zoom_beside_minimap: bool,
    /// Side of a part slot.
    pub slot: f32,
    pub parts: Vec2,
    pub signals: Vec2,
    /// The transport card (fixed size).
    pub transport: Vec2,
}

impl HudLayout {
    pub fn new(screen: Vec2) -> Self {
        let compact = screen.x < 1600.0 || screen.y < 900.0;
        let (margin, gap, inspector_width, library_width, minimap, slot, signals_h) = if compact {
            (
                16.0,
                12.0,
                296.0,
                330.0,
                Vec2::new(200.0, 128.0),
                52.0,
                176.0,
            )
        } else {
            (
                24.0,
                16.0,
                320.0,
                360.0,
                Vec2::new(250.0, 168.0),
                64.0,
                250.0,
            )
        };
        let slots = PART_SLOTS as f32;
        // Card padding 14 a side; tab row 28, a 10 gap, the slot with its
        // 3-unit edge, padding 10 above and 14 below.
        let parts = Vec2::new(
            slots * slot + (slots - 1.0) * SLOT_GAP + TOOL_GAP + 28.0,
            slot + 3.0 + CHIP_HEIGHT + 10.0 + 24.0 + 6.0,
        );
        let room = (screen.x - margin) - (screen.x + parts.x) * 0.5 - gap;
        let signals = Vec2::new(room.clamp(240.0, 560.0), signals_h);
        let zoom_beside_minimap =
            margin + minimap.x + gap + ZOOM_ROW.x + gap <= (screen.x - parts.x) * 0.5;
        Self {
            screen,
            compact,
            margin,
            gap,
            inspector_width,
            library_width,
            minimap,
            zoom_beside_minimap,
            slot,
            parts,
            signals,
            transport: Vec2::new(
                if compact {
                    TRANSPORT_WIDTH_COMPACT
                } else {
                    TRANSPORT_WIDTH
                },
                TOP_CARD_HEIGHT,
            ),
        }
    }

    /// The transport card: centred, unless the file card or the build
    /// status would not get their room beside it.
    pub fn transport_rect(&self) -> Rect {
        let w = self.transport.x;
        let lo = self.margin + FILE_MIN_WIDTH + self.gap;
        let hi = self.screen.x - self.margin - STATUS_WIDTH - self.gap - w;
        let x = ((self.screen.x - w) * 0.5).min(hi).max(lo);
        Rect::new(x, self.margin, x + w, self.margin + self.transport.y)
    }

    /// The room for the file card, top left: up to the transport card.
    /// The card is as wide as its contents, at most this.
    pub fn file_rect(&self) -> Rect {
        Rect::new(
            self.margin,
            self.margin,
            self.transport_rect().min.x - self.gap,
            self.margin + TOP_CARD_HEIGHT,
        )
    }

    /// The room for the build status card, top right: from the transport
    /// card to the edge.
    pub fn status_rect(&self) -> Rect {
        Rect::new(
            self.transport_rect().max.x + self.gap,
            self.margin,
            self.screen.x - self.margin,
            self.margin + TOP_CARD_HEIGHT,
        )
    }

    /// The build error details: beside the inspector column (which may
    /// be open below the status card), clear of the library drawer, down
    /// to the bottom row at most.
    pub fn build_details_rect(&self) -> Rect {
        let right = self.inspector_rect().min.x - self.gap;
        let left = (right - self.inspector_width).max(self.margin + self.library_width + self.gap);
        Rect::new(left, self.top_band(), right, self.bottom_band() - self.gap)
    }

    /// The exact-rate popover: under the transport card, between the
    /// library drawer and the build error details.
    pub fn transport_more_rect(&self) -> Rect {
        let w = TRANSPORT_MORE_SIZE.x;
        let lo = self.margin + self.library_width + self.gap;
        let hi = self.build_details_rect().min.x - self.gap - w;
        let x = self.transport_rect().min.x.min(hi).max(lo);
        let min = Vec2::new(x, self.top_band());
        Rect::from_corners(min, min + TRANSPORT_MORE_SIZE)
    }

    /// Below the top row of cards.
    pub fn top_band(&self) -> f32 {
        self.margin + TOP_CARD_HEIGHT + self.gap
    }

    pub fn minimap_rect(&self) -> Rect {
        let min = Vec2::new(self.margin, self.screen.y - self.margin - self.minimap.y);
        Rect::from_corners(min, min + self.minimap)
    }

    pub fn zoom_rect(&self) -> Rect {
        let map = self.minimap_rect();
        let min = if self.zoom_beside_minimap {
            Vec2::new(
                map.max.x + self.gap,
                self.screen.y - self.margin - ZOOM_ROW.y,
            )
        } else {
            Vec2::new(self.margin, map.min.y - self.gap * 0.5 - ZOOM_ROW.y)
        };
        Rect::from_corners(min, min + ZOOM_ROW)
    }

    pub fn parts_rect(&self) -> Rect {
        let min = Vec2::new(
            (self.screen.x - self.parts.x) * 0.5,
            self.screen.y - self.margin - self.parts.y,
        );
        Rect::from_corners(min, min + self.parts)
    }

    pub fn signals_rect(&self) -> Rect {
        let min = self.screen - Vec2::splat(self.margin) - self.signals;
        Rect::from_corners(min, min + self.signals)
    }

    /// The inspector column: right edge, from below the top row down to
    /// just above the signals card (the card is as tall as its content,
    /// up to this).
    pub fn inspector_rect(&self) -> Rect {
        let x = self.screen.x - self.margin - self.inspector_width;
        Rect::new(
            x,
            self.top_band(),
            x + self.inspector_width,
            self.signals_rect().min.y - self.gap,
        )
    }

    /// The library drawer: left edge, below the file card, down to just
    /// above the minimap stack.
    pub fn library_rect(&self) -> Rect {
        let bottom = self.minimap_rect().min.y.min(self.zoom_rect().min.y) - self.gap;
        Rect::new(
            self.margin,
            self.top_band(),
            self.margin + self.library_width,
            bottom,
        )
    }

    /// Top of the bottom row of cards.
    pub fn bottom_band(&self) -> f32 {
        [
            self.minimap_rect().min.y,
            self.zoom_rect().min.y,
            self.parts_rect().min.y,
            self.signals_rect().min.y,
        ]
        .into_iter()
        .fold(f32::INFINITY, f32::min)
    }

    /// The layout of a window `window` logical pixels large at UI scale
    /// `scale`: the cards are laid out in egui points, `window / scale`.
    pub fn scaled(window: Vec2, scale: f32) -> Self {
        Self::new(window / scale.max(0.1))
    }

    /// [`Self::free_area`] in the window's logical pixels (the canvas
    /// camera's units) at UI scale `scale`.
    pub fn free_area_in_window(&self, scale: f32) -> Rect {
        let free = self.free_area();
        Rect::from_corners(free.min * scale, free.max * scale)
    }

    /// The canvas area no card covers: between the top and bottom rows,
    /// left of the inspector column. The camera fits circuits into it.
    pub fn free_area(&self) -> Rect {
        Rect::new(
            self.margin,
            self.top_band(),
            self.inspector_rect().min.x - self.gap,
            self.bottom_band() - self.gap,
        )
    }
}

/// A bevy rectangle as an egui one (both logical pixels, y down).
pub fn egui_rect(r: Rect) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(r.min.x, r.min.y), egui::pos2(r.max.x, r.max.y))
}

/// The egui half of the theme: fonts and style.
/// Expects bevy_egui's plugin and [`crate::theme::ThemePlugin`].
pub struct WorkshopUiPlugin;

impl Plugin for WorkshopUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(EguiPrimaryContextPass, apply_workshop_ui_system);
    }
}

/// Laying out HUD cards in a headless egui context, to measure them.
#[cfg(test)]
pub(crate) mod test_ui {
    use super::*;

    /// A context with the Workshop fonts and `theme`'s style.
    pub fn ctx(theme: &Theme) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(workshop_fonts());
        for _ in 0..2 {
            let _ = ctx.run_ui(screen(), |_| {});
        }
        assert!(workshop_fonts_ready(&ctx));
        ctx.all_styles_mut(|s| *s = workshop_style(theme));
        ctx
    }

    fn screen() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(4000.0, 2000.0),
            )),
            ..Default::default()
        }
    }

    /// The rectangle a card drawn by `card` takes in an area at the
    /// origin (after egui's sizing pass).
    pub fn measure(ctx: &egui::Context, mut card: impl FnMut(&mut Ui)) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        for _ in 0..3 {
            let _ = ctx.run_ui(screen(), |ui| {
                rect = egui::Area::new(egui::Id::new("measured"))
                    .fixed_pos(egui::Pos2::ZERO)
                    .show(ui.ctx(), |ui| card(ui))
                    .response
                    .rect;
            });
        }
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_put_nunito_and_jetbrains_first() {
        let defs = workshop_fonts();
        assert_eq!(
            defs.families[&FontFamily::Proportional][0],
            "Nunito-SemiBold"
        );
        assert_eq!(
            defs.families[&FontFamily::Monospace][0],
            "JetBrainsMono-Bold"
        );
        let heavy = &defs.families[&FontFamily::Name(HEAVY.into())];
        assert_eq!(heavy[0], "Nunito-ExtraBold");
        assert!(heavy.len() > 1, "symbol fallbacks kept");
        assert!(defs.font_data.contains_key("Nunito-ExtraBold"));
    }

    #[test]
    fn style_uses_the_theme_tokens() {
        for theme in [Theme::day(), Theme::night()] {
            let style = workshop_style(&theme);
            let v = &style.visuals;
            assert_eq!(v.window_fill, egui_color(theme.palette.tile));
            assert_eq!(v.window_stroke, stroke(2.5, egui_color(theme.palette.ink)));
            assert_eq!(v.window_shadow.offset, [0, 4]);
            assert_eq!(v.window_shadow.blur, 0);
            assert_eq!(v.window_corner_radius, CornerRadius::same(14));
            assert_eq!(v.dark_mode, theme.is_night());
            assert_eq!(
                v.widgets.inactive.bg_stroke.color,
                egui_color(theme.palette.ink)
            );
            assert_eq!(style.text_styles[&TextStyle::Heading], heavy(20.0));
        }
        assert_eq!(
            egui_color(crate::theme::hex(0xFFB627)),
            Color32::from_rgb(0xFF, 0xB6, 0x27)
        );
    }

    #[test]
    fn live_values_read_like_the_inspector() {
        assert_eq!(format_live_value(1, 0), "off");
        assert_eq!(format_live_value(1, 1), "on");
        assert_eq!(format_live_value(8, 42), "0x2A · 42");
        assert_eq!(format_live_value(12, 5), "0x005 · 5");
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        !a.intersect(b).is_empty()
    }

    /// The named sizes (common laptop and desktop windows) and a sweep
    /// between 1280 x 720 and 2560 x 1440.
    fn hud_test_sizes() -> Vec<(f32, f32)> {
        let mut sizes = vec![
            (1280.0, 720.0),
            (1366.0, 768.0),
            (1440.0, 900.0),
            (1536.0, 864.0),
            (1600.0, 900.0),
            (1600.0, 1000.0),
            (1920.0, 1080.0),
            (2560.0, 1440.0),
        ];
        for w in (1280..=2560).step_by(16) {
            for h in [720, 768, 800, 864, 900, 1000, 1080, 1200, 1440] {
                sizes.push((w as f32, h as f32));
            }
        }
        sizes
    }

    #[test]
    fn hud_cards_fit_and_do_not_overlap_from_720p_to_1440p() {
        for (w, h) in hud_test_sizes() {
            assert_hud_fits(HudLayout::new(Vec2::new(w, h)), "");
        }
        // The full-size layout keeps the mockup's sizes.
        let l = HudLayout::new(Vec2::new(1920.0, 1080.0));
        assert_eq!((l.margin, l.inspector_width, l.slot), (24.0, 320.0, 64.0));
        assert!(l.zoom_beside_minimap);
        assert!(l.signals.x > 500.0);
    }

    /// At every UI scale (and Auto) the cards, laid out in egui points,
    /// still fit and do not overlap, and the free canvas area maps back
    /// into the window.
    #[test]
    fn hud_cards_fit_at_every_ui_scale() {
        use crate::settings::{UiScale, effective_ui_scale};
        for (w, h) in hud_test_sizes() {
            let window = Vec2::new(w, h);
            for choice in UiScale::choices() {
                let scale = effective_ui_scale(choice, window);
                let l = HudLayout::scaled(window, scale);
                assert_hud_fits(l, &format!(" ({} = {scale})", choice.label()));
                let free = l.free_area_in_window(scale);
                let whole = Rect::from_corners(Vec2::ZERO, window);
                assert!(
                    whole.contains(free.min) && whole.contains(free.max),
                    "free area at {w}x{h}, {scale}"
                );
            }
        }
        // 130% really is larger where there is room for it.
        let l = HudLayout::scaled(Vec2::new(1920.0, 1080.0), 1.3);
        assert!(l.screen.x < 1480.0 && l.compact);
    }

    /// The cards of `l` are inside its screen, apart, and leave the free
    /// area its share. `what` names the case in failures.
    fn assert_hud_fits(l: HudLayout, what: &str) {
        let screen = l.screen;
        let (w, h) = (screen.x, screen.y);
        let window = Rect::from_corners(Vec2::ZERO, screen);
        let top = [
            ("file", l.file_rect()),
            ("transport", l.transport_rect()),
            ("status", l.status_rect()),
        ];
        let bottom = [
            ("minimap", l.minimap_rect()),
            ("zoom", l.zoom_rect()),
            ("parts", l.parts_rect()),
            ("signals", l.signals_rect()),
        ];
        let side = [
            ("inspector", l.inspector_rect()),
            ("library", l.library_rect()),
            ("build details", l.build_details_rect()),
            ("exact rate", l.transport_more_rect()),
        ];
        let all: Vec<_> = top.iter().chain(&bottom).chain(&side).collect();
        for (name, r) in &all {
            assert!(
                window.contains(r.min) && window.contains(r.max),
                "{name} at {w}x{h}{what}"
            );
        }
        for (i, (a, ra)) in all.iter().enumerate() {
            for (b, rb) in &all[i + 1..] {
                assert!(!overlaps(*ra, *rb), "{a} overlaps {b} at {w}x{h}{what}");
            }
        }
        assert!(
            l.file_rect().width() >= FILE_MIN_WIDTH,
            "file card room at {w}x{h}{what}"
        );
        assert!(
            l.status_rect().width() >= STATUS_WIDTH,
            "status room at {w}x{h}{what}"
        );
        let details = l.build_details_rect();
        assert!(
            details.width() >= 250.0 && details.height() >= 200.0,
            "build details at {w}x{h}{what}: {details:?}"
        );
        let free = l.free_area();
        assert!(
            free.width() > 0.45 * w && free.height() > 0.5 * h,
            "free area at {w}x{h}{what}"
        );
        assert!(free.min.y >= l.top_band() && free.max.y < l.bottom_band());
        for (name, r) in bottom.iter().chain([&("inspector", l.inspector_rect())]) {
            assert!(
                !overlaps(free, *r),
                "free area overlaps {name} at {w}x{h}{what}"
            );
        }
        let inspector = l.inspector_rect();
        assert!(inspector.height() > 300.0, "inspector at {w}x{h}{what}");
        let library = l.library_rect();
        assert!(library.height() > 250.0, "library at {w}x{h}{what}");
        assert_eq!(l.compact, w < 1600.0 || h < 900.0);
    }
}
