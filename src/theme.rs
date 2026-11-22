//! The "Studio Workshop" look: colour tokens for day and night, and the
//! [`Theme`] resource every colour in the app is read from.
//!
//! The canvas (`crate::sim::art`, `crate::sim::visuals`) and the egui
//! panels (`crate::ui_style`) follow `Theme` when it changes; nothing else
//! hard-codes a colour. Toggle with [`Theme::toggle`] (the `T` key and the
//! Day/Night card do this).

use bevy::prelude::*;

/// Day or night.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Reflect)]
pub enum ThemeMode {
    #[default]
    Day,
    Night,
}

/// What a part is, for its tile colour: logic is white, things that
/// remember are violet, inputs and outputs are sky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Logic,
    State,
    Io,
}

/// The colour tokens of one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Canvas ground.
    pub bench: Color,
    /// The dot grid on the bench (every 20 units).
    pub bench_dot: Color,
    /// Logic tiles, panels, tags.
    pub tile: Color,
    /// Outlines, bottom edges and wire casings.
    pub ink: Color,
    /// Names and values.
    pub text: Color,
    pub text_secondary: Color,
    /// Captions and zero values.
    pub text_faint: Color,
    /// A 1-bit signal that is on (marigold).
    pub on: Color,
    /// Marigold for text on light surfaces (warnings, "on" values).
    pub on_text: Color,
    /// Text on a marigold button.
    pub on_ink: Color,
    /// Off, idle, disabled (slate).
    pub off: Color,
    /// A pin whose signal is off.
    pub pin_off: Color,
    /// Buses (teal).
    pub bus: Color,
    /// A bus whose value is zero.
    pub bus_zero: Color,
    /// Bus values in pills.
    pub bus_text: Color,
    /// Memory and state (violet).
    pub state: Color,
    pub state_tint: Color,
    pub state_text: Color,
    /// Inputs and outputs (sky).
    pub io: Color,
    pub io_tint: Color,
    /// Sky for text (notes about inputs, outputs and views).
    pub io_text: Color,
    /// Faults (tomato).
    pub fault: Color,
    pub fault_text: Color,
    /// The dark screen set into readouts.
    pub screen: Color,
    /// Buttons and inputs inside panels.
    pub raised: Color,
    pub hover: Color,
    /// Rules between groups in panels.
    pub divider: Color,
    /// Inset wells inside cards.
    pub well: Color,
    /// The halo behind a selected part.
    pub halo: Color,
}

/// `#RRGGBB` as an sRGB colour.
pub const fn hex(rgb: u32) -> Color {
    Color::Srgba(Srgba {
        red: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        green: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        blue: (rgb & 0xFF) as f32 / 255.0,
        alpha: 1.0,
    })
}

impl Palette {
    pub const DAY: Palette = Palette {
        bench: hex(0xD9E1EA),
        bench_dot: hex(0xC3CDD9),
        tile: hex(0xFFFFFF),
        ink: hex(0x263046),
        text: hex(0x263046),
        text_secondary: hex(0x5A6782),
        text_faint: hex(0x8792A8),
        on: hex(0xFFB627),
        on_text: hex(0xC98A00),
        on_ink: hex(0x263046),
        off: hex(0xA3AEC0),
        pin_off: hex(0xFFFFFF),
        bus: hex(0x1FA595),
        bus_zero: hex(0xB9DFD9),
        bus_text: hex(0x1A7F73),
        state: hex(0x7A5CFA),
        state_tint: hex(0xEEEAFF),
        state_text: hex(0x4B33C9),
        io: hex(0x3A96F0),
        io_tint: hex(0xE3F1FF),
        io_text: hex(0x2A7CD0),
        fault: hex(0xE5484D),
        fault_text: hex(0xC9353A),
        screen: hex(0x263046),
        raised: hex(0xFFFFFF),
        hover: hex(0xF3F6FA),
        divider: hex(0xE1E6EE),
        well: hex(0xE4EAF1),
        halo: hex(0xFFFFFF),
    };

    /// The same workshop with the lights down: surfaces swap, the signal
    /// colours barely move (on is still marigold).
    pub const NIGHT: Palette = Palette {
        bench: hex(0x1A2030),
        bench_dot: hex(0x283247),
        tile: hex(0x2B3449),
        ink: hex(0x0B0F1A),
        text: hex(0xE8ECF4),
        text_secondary: hex(0x9AA6BE),
        text_faint: hex(0x6B7690),
        on: hex(0xFFB627),
        on_text: hex(0xFFB627),
        on_ink: hex(0x1A2030),
        off: hex(0x4A556C),
        pin_off: hex(0x36405A),
        bus: hex(0x2CC2B0),
        bus_zero: hex(0x24504C),
        bus_text: hex(0x2CC2B0),
        state: hex(0x9B85FF),
        state_tint: hex(0x332C55),
        state_text: hex(0xB9A9FF),
        io: hex(0x5AAEFF),
        io_tint: hex(0x1F3550),
        io_text: hex(0x5AAEFF),
        fault: hex(0xFF6B6B),
        fault_text: hex(0xFF8A8A),
        screen: hex(0x0B0F1A),
        raised: hex(0x36405A),
        hover: hex(0x404B66),
        divider: hex(0x3C4762),
        well: hex(0x222A3C),
        halo: hex(0xE8ECF4),
    };

    /// Tile fill of a family.
    pub fn family_fill(&self, family: Family) -> Color {
        match family {
            Family::Logic => self.tile,
            Family::State => self.state_tint,
            Family::Io => self.io_tint,
        }
    }

    /// Name text on a tile of a family.
    pub fn family_text(&self, family: Family) -> Color {
        match family {
            Family::State => self.state_text,
            Family::Logic | Family::Io => self.text,
        }
    }

    /// Colour of a signal of `width` bits carrying `value` (wire cores and
    /// lit pins): marigold or slate for one bit, teal or pale teal for a
    /// bus.
    pub fn signal(&self, width: u8, value: u64) -> Color {
        match (width > 1, value != 0) {
            (false, true) => self.on,
            (false, false) => self.off,
            (true, true) => self.bus,
            (true, false) => self.bus_zero,
        }
    }

    /// Fill of a pin: white when off, the signal colour when on.
    pub fn pin(&self, width: u8, value: u64) -> Color {
        if value == 0 {
            self.pin_off
        } else {
            self.signal(width, value)
        }
    }
}

/// The active theme. Change it with [`Theme::set`] or [`Theme::toggle`];
/// the canvas and egui restyle when it changes.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub mode: ThemeMode,
    pub palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self::day()
    }
}

impl Theme {
    pub fn day() -> Self {
        Self {
            mode: ThemeMode::Day,
            palette: Palette::DAY,
        }
    }

    pub fn night() -> Self {
        Self {
            mode: ThemeMode::Night,
            palette: Palette::NIGHT,
        }
    }

    pub fn of(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Day => Self::day(),
            ThemeMode::Night => Self::night(),
        }
    }

    pub fn is_night(&self) -> bool {
        self.mode == ThemeMode::Night
    }

    pub fn set(&mut self, mode: ThemeMode) {
        *self = Self::of(mode);
    }

    pub fn toggle(&mut self) {
        self.set(match self.mode {
            ThemeMode::Day => ThemeMode::Night,
            ThemeMode::Night => ThemeMode::Day,
        });
    }
}

/// Keep the window's clear colour on the bench colour.
pub fn sync_clear_colour_system(theme: Res<Theme>, clear: Option<ResMut<ClearColor>>) {
    if !theme.is_changed() {
        return;
    }
    if let Some(mut clear) = clear
        && clear.0 != theme.palette.bench
    {
        clear.0 = theme.palette.bench;
    }
}

/// `T` toggles day and night (unless a text field has the keyboard).
pub fn toggle_theme_key_system(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    egui_input: Option<Res<bevy_egui::input::EguiWantsInput>>,
    mut theme: ResMut<Theme>,
) {
    let Some(keys) = keys else { return };
    if egui_input.is_some_and(|i| i.wants_any_keyboard_input()) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyT) {
        theme.toggle();
    }
}

/// The theme resource, the clear colour and the `T` key. The egui style
/// is applied by [`crate::ui_style::WorkshopUiPlugin`].
pub struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Theme>().add_systems(
            Update,
            (toggle_theme_key_system, sync_clear_colour_system).chain(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb8(c: Color) -> [u8; 3] {
        let s = c.to_srgba();
        [s.red, s.green, s.blue].map(|v| (v * 255.0).round() as u8)
    }

    #[test]
    fn day_tokens_match_the_design() {
        let p = Palette::DAY;
        assert_eq!(rgb8(p.bench), [0xD9, 0xE1, 0xEA]);
        assert_eq!(rgb8(p.bench_dot), [0xC3, 0xCD, 0xD9]);
        assert_eq!(rgb8(p.tile), [0xFF, 0xFF, 0xFF]);
        assert_eq!(rgb8(p.ink), [0x26, 0x30, 0x46]);
        assert_eq!(rgb8(p.text_secondary), [0x5A, 0x67, 0x82]);
        assert_eq!(rgb8(p.on), [0xFF, 0xB6, 0x27]);
        assert_eq!(rgb8(p.off), [0xA3, 0xAE, 0xC0]);
        assert_eq!(rgb8(p.bus), [0x1F, 0xA5, 0x95]);
        assert_eq!(rgb8(p.bus_zero), [0xB9, 0xDF, 0xD9]);
        assert_eq!(rgb8(p.state), [0x7A, 0x5C, 0xFA]);
        assert_eq!(rgb8(p.state_tint), [0xEE, 0xEA, 0xFF]);
        assert_eq!(rgb8(p.io), [0x3A, 0x96, 0xF0]);
        assert_eq!(rgb8(p.io_tint), [0xE3, 0xF1, 0xFF]);
        assert_eq!(rgb8(p.fault), [0xE5, 0x48, 0x4D]);
    }

    #[test]
    fn night_tokens_match_the_design() {
        let p = Palette::NIGHT;
        assert_eq!(rgb8(p.bench), [0x1A, 0x20, 0x30]);
        assert_eq!(rgb8(p.bench_dot), [0x28, 0x32, 0x47]);
        assert_eq!(rgb8(p.tile), [0x2B, 0x34, 0x49]);
        assert_eq!(rgb8(p.ink), [0x0B, 0x0F, 0x1A]);
        assert_eq!(rgb8(p.text), [0xE8, 0xEC, 0xF4]);
        assert_eq!(rgb8(p.off), [0x4A, 0x55, 0x6C]);
        assert_eq!(rgb8(p.bus), [0x2C, 0xC2, 0xB0]);
        assert_eq!(rgb8(p.state), [0x9B, 0x85, 0xFF]);
        assert_eq!(rgb8(p.state_tint), [0x33, 0x2C, 0x55]);
        assert_eq!(rgb8(p.io), [0x5A, 0xAE, 0xFF]);
        assert_eq!(rgb8(p.io_tint), [0x1F, 0x35, 0x50]);
        assert_eq!(rgb8(p.fault), [0xFF, 0x6B, 0x6B]);
        // Marigold is unchanged at night.
        assert_eq!(p.on, Palette::DAY.on);
    }

    #[test]
    fn signals_and_pins() {
        let p = Palette::DAY;
        assert_eq!(p.signal(1, 1), p.on);
        assert_eq!(p.signal(1, 0), p.off);
        assert_eq!(p.signal(8, 0x2A), p.bus);
        assert_eq!(p.signal(8, 0), p.bus_zero);
        assert_eq!(p.pin(1, 0), p.pin_off);
        assert_eq!(p.pin(8, 0), p.pin_off);
        assert_eq!(p.pin(1, 1), p.on);
        assert_eq!(p.pin(8, 3), p.bus);
        assert_eq!(p.family_fill(Family::Logic), p.tile);
        assert_eq!(p.family_fill(Family::State), p.state_tint);
        assert_eq!(p.family_fill(Family::Io), p.io_tint);
    }

    #[test]
    fn toggle_switches_mode_and_palette() {
        let mut theme = Theme::default();
        assert_eq!(theme.mode, ThemeMode::Day);
        assert_eq!(theme.palette, Palette::DAY);
        theme.toggle();
        assert!(theme.is_night());
        assert_eq!(theme.palette, Palette::NIGHT);
        theme.toggle();
        assert_eq!(theme, Theme::day());
    }
}
