//! The Workshop look for egui: bundled fonts (Nunito, JetBrains Mono),
//! a [`egui::Style`] built from the [`Theme`], chunky buttons with a solid
//! bottom edge, floating HUD cards, and the Day/Night card.

use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response, Sense,
    Shadow, Stroke, StrokeKind, TextStyle, Ui, Vec2 as EVec2, WidgetText,
};

use crate::fonts;
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

    let prop = |size| FontId::new(size, FontFamily::Proportional);
    style.text_styles = [
        (TextStyle::Heading, heavy(20.0)),
        (TextStyle::Body, prop(14.0)),
        (TextStyle::Button, heavy(14.0)),
        (TextStyle::Small, prop(12.0)),
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
    let galley = label.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Button,
    );
    let body = EVec2::new((galley.size().x + 28.0).max(40.0), 34.0);
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
        painter.galley(face.center() - galley.size() * 0.5, galley, colour);
    }
    response
}

/// A small uppercase caption (`TICK`, `SPEED`).
pub fn caption(palette: &Palette, text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .text_style(TextStyle::Button)
        .size(11.0)
        .color(egui_color(palette.text_secondary))
}

/// Install the fonts once, and the style whenever the theme changes. egui
/// takes new fonts at the start of the next pass, so the style (whose text
/// styles name the heavy family) is applied from the frame after.
pub fn apply_workshop_ui_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    mut fonts_set: Local<bool>,
    mut applied: Local<Option<ThemeMode>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if !*fonts_set {
        ctx.set_fonts(workshop_fonts());
        *fonts_set = true;
        return Ok(());
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

/// The Day/Night card in the bottom-right corner.
pub fn theme_toggle_card_system(
    mut contexts: EguiContexts,
    mut theme: ResMut<Theme>,
    state: Option<Res<State<crate::GameState>>>,
) -> Result {
    if state.is_some_and(|s| s.get() != &crate::GameState::Simulating) {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    let palette = theme.palette;
    let mut wanted = None;
    egui::Area::new(egui::Id::new("theme_toggle"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
        .show(ctx, |ui| {
            card(&palette).inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if chunky_button(ui, &palette, "☀ Day", !theme.is_night()).clicked() {
                        wanted = Some(ThemeMode::Day);
                    }
                    if chunky_button(ui, &palette, "🌙 Night", theme.is_night()).clicked() {
                        wanted = Some(ThemeMode::Night);
                    }
                })
                .response
                .on_hover_text("Toggle with T");
            });
        });
    if let Some(mode) = wanted
        && mode != theme.mode
    {
        theme.set(mode);
    }
    Ok(())
}

/// The egui half of the theme: fonts, style and the Day/Night card.
/// Expects bevy_egui's plugin and [`crate::theme::ThemePlugin`].
pub struct WorkshopUiPlugin;

impl Plugin for WorkshopUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            EguiPrimaryContextPass,
            (apply_workshop_ui_system, theme_toggle_card_system).chain(),
        );
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
}
