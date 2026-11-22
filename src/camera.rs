//! The canvas camera: the maths of fitting, zooming and panning an
//! orthographic 2D camera (pure functions, tested without rendering), and
//! the wheel-zoom, middle-drag and arrow-key pan systems.
//!
//! Screen positions are logical pixels with y down (egui and window
//! cursor coordinates); world positions have y up. A view is described by
//! its centre in world units and its *world units per pixel* (`wpp`); the
//! zoom percentage shown in the HUD is `100 / wpp`.

use bevy::{
    camera::ScalingMode,
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
    prelude::*,
    window::PrimaryWindow,
};

use crate::sim::visuals::CanvasZoom;
use crate::{MainCamera, UiPointerState};

/// Fit the open circuit into the free canvas area again (the HUD's zoom
/// percentage button and the `F` key send this).
#[derive(Message, Debug, Clone, Copy, Default)]
pub struct FitView;

/// Zoom limits, as world units per pixel (10% to 1000%).
pub const MIN_WPP: f32 = 0.1;
pub const MAX_WPP: f32 = 10.0;
/// A fit never zooms in further than this (150%), so a single gate does
/// not fill the screen.
pub const FIT_MIN_WPP: f32 = 1.0 / 1.5;
/// A fit never zooms out further than this (65%), where a tile's
/// simplified caption is still readable
/// ([`crate::sim::art::CaptionMode`]). A circuit too large for the free
/// area at 65% shows its inputs' side; the minimap has the rest.
pub const FIT_MAX_WPP: f32 = 1.0 / FIT_MIN_ZOOM;
pub const FIT_MIN_ZOOM: f32 = 0.65;
/// Room around the circuit when fitting (a factor on its size).
pub const FIT_PADDING: f32 = 1.15;

/// World units per logical pixel of `ortho` in a window `window_height`
/// pixels tall.
pub fn world_per_pixel(ortho: &OrthographicProjection, window_height: f32) -> f32 {
    ortho.scale * units_per_scale(ortho, window_height)
}

/// The `scale` that gives `wpp` world units per pixel.
pub fn scale_for_wpp(ortho: &OrthographicProjection, window_height: f32, wpp: f32) -> f32 {
    wpp / units_per_scale(ortho, window_height)
}

fn units_per_scale(ortho: &OrthographicProjection, window_height: f32) -> f32 {
    match ortho.scaling_mode {
        ScalingMode::FixedVertical { viewport_height } => viewport_height / window_height.max(1.0),
        _ => 1.0,
    }
}

/// The view (centre, world units per pixel) that shows `content` (a
/// world rectangle) inside `free` (a screen rectangle, y down) of a
/// `screen`-sized window, with `padding` room around it. See
/// [`fit_view_around`].
pub fn fit_view(content: Rect, screen: Vec2, free: Rect, padding: f32) -> (Vec2, f32) {
    fit_view_around(content, None, screen, free, padding)
}

/// The fit of `content` into `free`, zoomed no further out than
/// [`FIT_MIN_ZOOM`] (nor in than 150%). When the content does not fit at
/// that zoom, the view shows a readable part of it: its left edge
/// (circuits flow left to right from their inputs) and, vertically, the
/// part around `focus` (the inputs' bounds) or else its top, kept within
/// the content.
pub fn fit_view_around(
    content: Rect,
    focus: Option<Rect>,
    screen: Vec2,
    free: Rect,
    padding: f32,
) -> (Vec2, f32) {
    let size = content.size().max(Vec2::ONE);
    let free_size = free.size().max(Vec2::ONE);
    let wpp = ((size.x / free_size.x).max(size.y / free_size.y) * padding)
        .clamp(FIT_MIN_WPP, FIT_MAX_WPP);
    // The world point shown at the free area's centre: the content's
    // centre on each axis that fits.
    // The padding gives way first: content that fits without it is
    // still centred.
    let pad = size * (padding - 1.0) * 0.5;
    let view = free_size * wpp;
    let mut target = content.center();
    if size.x > view.x {
        target.x = content.min.x - pad.x + view.x * 0.5;
    }
    if size.y > view.y {
        let top = content.max.y + pad.y - view.y * 0.5;
        let bottom = content.min.y - pad.y + view.y * 0.5;
        target.y = focus.map_or(top, |f| f.center().y.clamp(bottom, top));
    }
    // The camera centre sits at the screen centre; shift it so the
    // target lands on the free area's centre.
    let offset = free.center() - screen * 0.5;
    let centre = target - Vec2::new(offset.x, -offset.y) * wpp;
    (centre, wpp)
}

/// World position of screen position `pos`.
pub fn screen_to_world(pos: Vec2, screen: Vec2, centre: Vec2, wpp: f32) -> Vec2 {
    centre + Vec2::new(pos.x - screen.x * 0.5, screen.y * 0.5 - pos.y) * wpp
}

/// Screen position of world position `world`.
pub fn world_to_screen(world: Vec2, screen: Vec2, centre: Vec2, wpp: f32) -> Vec2 {
    let d = (world - centre) / wpp;
    Vec2::new(screen.x * 0.5 + d.x, screen.y * 0.5 - d.y)
}

/// The world rectangle the window shows.
pub fn visible_world(screen: Vec2, centre: Vec2, wpp: f32) -> Rect {
    Rect::from_center_size(centre, screen * wpp)
}

/// The new camera centre after zooming from `wpp` to `new_wpp` so that
/// the world point `anchor` stays under the same pixel.
pub fn zoom_about(centre: Vec2, wpp: f32, new_wpp: f32, anchor: Vec2) -> Vec2 {
    anchor - (anchor - centre) * (new_wpp / wpp)
}

/// Zoom factor of one wheel notch (one "line"): 10% per notch.
pub const ZOOM_STEP: f32 = 1.1;
/// Pixels of a pixel-unit scroll (web wheels, trackpads) that count as
/// one notch. Browsers send about 100 per notch; a trackpad sends a few
/// per event, so it zooms gradually.
pub const PIXELS_PER_LINE: f32 = 100.0;
/// At most this many notches per frame, so a burst of events (a fast
/// flick, a wheel reporting several lines) cannot jump to a zoom limit.
pub const MAX_LINES_PER_FRAME: f32 = 3.0;

/// The factor on world units per pixel for one frame of wheel events
/// (`unit`, `y`): below 1 zooms in (wheel up), above 1 zooms out. Line
/// and pixel units are normalised to notches first, so one notch is one
/// [`ZOOM_STEP`] whatever the device reports.
pub fn wheel_zoom_factor(events: impl IntoIterator<Item = (MouseScrollUnit, f32)>) -> f32 {
    let lines: f32 = events
        .into_iter()
        .map(|(unit, y)| match unit {
            MouseScrollUnit::Line => y,
            MouseScrollUnit::Pixel => y / PIXELS_PER_LINE,
        })
        .sum();
    ZOOM_STEP.powf(-lines.clamp(-MAX_LINES_PER_FRAME, MAX_LINES_PER_FRAME))
}

/// Mouse wheel zoom about the cursor (not while the pointer is over a
/// HUD card, so scrolling a card does not zoom the canvas).
pub fn zoom_camera(
    mut scroll_evr: MessageReader<MouseWheel>,
    ui_pointer: Option<Res<UiPointerState>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
) -> Result {
    let factor = wheel_zoom_factor(scroll_evr.read().map(|ev| (ev.unit, ev.y)));
    if factor == 1.0 || ui_pointer.is_some_and(|p| p.over_egui) {
        return Ok(());
    }
    let window = windows.single()?;
    let (mut projection, mut transform) = cameras.single_mut()?;
    let Projection::Orthographic(ortho) = projection.as_mut() else {
        return Ok(());
    };
    let screen = window.size();
    let wpp = world_per_pixel(ortho, screen.y);
    let new_wpp = (wpp * factor).clamp(MIN_WPP, MAX_WPP);
    let centre = transform.translation.truncate();
    if let Some(cursor) = window.cursor_position() {
        let anchor = screen_to_world(cursor, screen, centre, wpp);
        let c = zoom_about(centre, wpp, new_wpp, anchor);
        transform.translation.x = c.x;
        transform.translation.y = c.y;
    }
    ortho.scale = scale_for_wpp(ortho, screen.y, new_wpp);
    Ok(())
}

/// Keep [`CanvasZoom`] (the canvas's screen pixels per world unit) in
/// step with the camera, for the zoom-dependent canvas text.
pub fn track_canvas_zoom_system(
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<&Projection, With<MainCamera>>,
    zoom: Option<ResMut<CanvasZoom>>,
) {
    let (Ok(window), Ok(Projection::Orthographic(ortho)), Some(mut zoom)) =
        (windows.single(), cameras.single(), zoom)
    else {
        return;
    };
    let now = 1.0 / world_per_pixel(ortho, window.height()).max(1e-6);
    if (zoom.0 - now).abs() > 1e-4 {
        zoom.0 = now;
    }
}

/// Drag with the middle mouse button to pan the canvas.
pub fn pan_camera(
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    ui_pointer: Option<Res<UiPointerState>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&Projection, &mut Transform), With<MainCamera>>,
    mut panning: Local<bool>,
) -> Result {
    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    if buttons.just_pressed(MouseButton::Middle) {
        *panning = !ui_pointer.is_some_and(|p| p.over_egui);
    }
    if !buttons.pressed(MouseButton::Middle) {
        *panning = false;
    }
    if !*panning || delta == Vec2::ZERO {
        return Ok(());
    }
    let window = windows.single()?;
    let (projection, mut transform) = cameras.single_mut()?;
    let Projection::Orthographic(ortho) = projection else {
        return Ok(());
    };
    let wpp = world_per_pixel(ortho, window.height());
    transform.translation.x -= delta.x * wpp;
    transform.translation.y += delta.y * wpp;
    Ok(())
}

/// Arrow-key pan speed, in logical pixels per second (the same on screen
/// at any zoom).
pub const KEY_PAN_SPEED: f32 = 900.0;

/// The pan direction of the held arrow keys, x right and y up (world
/// directions); opposite keys cancel. Diagonals are not faster.
pub fn arrow_pan_direction(keys: &ButtonInput<KeyCode>) -> Vec2 {
    let axis = |neg: KeyCode, pos: KeyCode| {
        f32::from(u8::from(keys.pressed(pos))) - f32::from(u8::from(keys.pressed(neg)))
    };
    Vec2::new(
        axis(KeyCode::ArrowLeft, KeyCode::ArrowRight),
        axis(KeyCode::ArrowDown, KeyCode::ArrowUp),
    )
    .normalize_or_zero()
}

/// Hold the arrow keys to pan the canvas (not while a text field has the
/// keyboard). `W`/`A`/`S`/`D` are not used: `W` is the wire tool.
pub fn key_pan_camera(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    ui_pointer: Option<Res<UiPointerState>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&Projection, &mut Transform), With<MainCamera>>,
) -> Result {
    let direction = arrow_pan_direction(&keys);
    if direction == Vec2::ZERO || ui_pointer.is_some_and(|p| p.wants_keyboard) {
        return Ok(());
    }
    let window = windows.single()?;
    let (projection, mut transform) = cameras.single_mut()?;
    let Projection::Orthographic(ortho) = projection else {
        return Ok(());
    };
    let step =
        direction * KEY_PAN_SPEED * time.delta_secs() * world_per_pixel(ortho, window.height());
    transform.translation.x += step.x;
    transform.translation.y += step.y;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn fit_centres_content_in_the_free_area() {
        let screen = Vec2::new(1920.0, 1080.0);
        // Free area: everything right of a 400 px panel, below a 100 px bar.
        let free = Rect::new(400.0, 100.0, 1920.0, 1080.0);
        let content = Rect::new(-1000.0, -400.0, 1000.0, 400.0);
        let (centre, wpp) = fit_view(content, screen, free, 1.0);
        // 2000 x 800 world units into 1520 x 980 pixels: width decides.
        assert!((wpp - 2000.0 / 1520.0).abs() < 1e-4);
        // The content's centre lands on the free area's centre, and its
        // corners inside the free area.
        let at = |w| world_to_screen(w, screen, centre, wpp);
        assert!(close(at(content.center()), free.center()));
        let (a, b) = (at(content.min), at(content.max));
        for p in [a, b] {
            assert!(p.x >= free.min.x - 0.01 && p.x <= free.max.x + 0.01);
            assert!(p.y >= free.min.y - 0.01 && p.y <= free.max.y + 0.01);
        }
        // World y up is screen y down.
        assert!(a.y > b.y);
    }

    #[test]
    fn fit_respects_the_zoom_limits() {
        let screen = Vec2::new(1280.0, 720.0);
        let free = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let tiny = Rect::from_center_size(Vec2::ZERO, Vec2::splat(10.0));
        assert_eq!(fit_view(tiny, screen, free, 1.0).1, FIT_MIN_WPP);
        // A fit zooms out no further than 65% (the wheel can: MAX_WPP).
        let huge = Rect::from_center_size(Vec2::ZERO, Vec2::splat(1.0e6));
        assert_eq!(fit_view(huge, screen, free, 1.0).1, FIT_MAX_WPP);
        const { assert!(FIT_MAX_WPP < MAX_WPP) };
        // A full-window free area centres the camera on the content.
        let content = Rect::new(100.0, 50.0, 900.0, 450.0);
        let (centre, _) = fit_view(content, screen, free, FIT_PADDING);
        assert!(close(centre, content.center()));
    }

    #[test]
    fn screen_and_world_round_trip() {
        let screen = Vec2::new(1600.0, 900.0);
        let (centre, wpp) = (Vec2::new(30.0, -40.0), 1.7);
        for p in [Vec2::ZERO, Vec2::new(800.0, 450.0), Vec2::new(17.0, 880.0)] {
            let w = screen_to_world(p, screen, centre, wpp);
            assert!(close(world_to_screen(w, screen, centre, wpp), p));
        }
        assert!(close(
            screen_to_world(screen * 0.5, screen, centre, wpp),
            centre
        ));
        let visible = visible_world(screen, centre, wpp);
        assert!(close(visible.size(), screen * wpp));
        assert!(close(
            screen_to_world(Vec2::new(0.0, screen.y), screen, centre, wpp),
            visible.min
        ));
    }

    #[test]
    fn zoom_keeps_the_anchor_still() {
        let screen = Vec2::new(1000.0, 800.0);
        let (centre, wpp, new_wpp) = (Vec2::new(5.0, 5.0), 2.0, 0.5);
        let cursor = Vec2::new(120.0, 640.0);
        let anchor = screen_to_world(cursor, screen, centre, wpp);
        let moved = zoom_about(centre, wpp, new_wpp, anchor);
        assert!(close(
            screen_to_world(cursor, screen, moved, new_wpp),
            anchor
        ));
    }

    #[test]
    fn a_wheel_notch_is_one_step_in_line_or_pixel_units() {
        use MouseScrollUnit::{Line, Pixel};
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert_eq!(wheel_zoom_factor([]), 1.0);
        // One notch up zooms in one step, one down zooms out one step.
        let notch = wheel_zoom_factor([(Line, 1.0)]);
        assert!(close(notch, 1.0 / ZOOM_STEP));
        assert!(close(wheel_zoom_factor([(Line, -1.0)]), ZOOM_STEP));
        // A browser wheel notch (about 100 pixels) is the same step,
        // not a jump to the zoom limit.
        assert!(close(wheel_zoom_factor([(Pixel, PIXELS_PER_LINE)]), notch));
        // A trackpad's small pixel deltas zoom gradually, adding up to
        // the same as one notch.
        let small = wheel_zoom_factor([(Pixel, 4.0)]);
        assert!(small < 1.0 && small > 0.99, "{small}");
        let mut total = 1.0;
        for _ in 0..25 {
            total *= wheel_zoom_factor([(Pixel, 4.0)]);
        }
        assert!(close(total, notch));
        // A burst in one frame is capped.
        let burst = wheel_zoom_factor([(Pixel, 5000.0), (Line, 10.0)]);
        assert!(close(burst, ZOOM_STEP.powf(-MAX_LINES_PER_FRAME)));
        assert!(burst > MIN_WPP / MAX_WPP);
    }

    #[test]
    fn arrow_keys_pan_in_world_directions() {
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(arrow_pan_direction(&keys), Vec2::ZERO);
        keys.press(KeyCode::ArrowRight);
        assert_eq!(arrow_pan_direction(&keys), Vec2::X);
        keys.press(KeyCode::ArrowLeft);
        assert_eq!(arrow_pan_direction(&keys), Vec2::ZERO, "opposites cancel");
        keys.release(KeyCode::ArrowLeft);
        keys.press(KeyCode::ArrowUp);
        let d = arrow_pan_direction(&keys);
        assert!(d.x > 0.0 && d.y > 0.0 && (d.length() - 1.0).abs() < 1e-5);
        keys.release_all();
        keys.press(KeyCode::ArrowDown);
        assert_eq!(arrow_pan_direction(&keys), Vec2::NEG_Y);
        // The wire tool's key does not pan.
        keys.release_all();
        keys.press(KeyCode::KeyW);
        assert_eq!(arrow_pan_direction(&keys), Vec2::ZERO);
    }

    #[test]
    fn scale_and_wpp_convert_both_ways() {
        let mut ortho = OrthographicProjection::default_2d();
        ortho.scaling_mode = ScalingMode::FixedVertical {
            viewport_height: 1000.0,
        };
        ortho.scale = 1.5;
        // A 500 px tall window shows 1500 world units: 3 per pixel.
        assert!((world_per_pixel(&ortho, 500.0) - 3.0).abs() < 1e-5);
        assert!((scale_for_wpp(&ortho, 500.0, 3.0) - 1.5).abs() < 1e-5);
        ortho.scaling_mode = ScalingMode::WindowSize;
        assert!((world_per_pixel(&ortho, 500.0) - 1.5).abs() < 1e-5);
    }

    /// A circuit too large for the free area at 65% shows its left edge,
    /// and vertically the inputs (or the top without them); one that fits
    /// at 65%, if only without its padding, is still centred whole.
    #[test]
    fn a_large_circuit_fits_its_inputs_side_at_the_lowest_fit_zoom() {
        let screen = Vec2::new(1280.0, 720.0);
        let free = Rect::new(16.0, 88.0, 948.0, 580.0);
        let content = Rect::new(-2000.0, -1000.0, 2000.0, 1000.0);
        let inputs = Rect::new(-1990.0, -300.0, -1950.0, -100.0);
        let (centre, wpp) = fit_view_around(content, Some(inputs), screen, free, FIT_PADDING);
        assert_eq!(wpp, FIT_MAX_WPP);
        let at = |w| world_to_screen(w, screen, centre, wpp);
        // The left edge (with its padding) at the free area's left edge.
        let pad = content.width() * (FIT_PADDING - 1.0) * 0.5;
        assert!((at(Vec2::new(content.min.x - pad, 0.0)).x - free.min.x).abs() < 0.01);
        // The inputs are in view, vertically centred.
        let (a, b) = (at(inputs.min), at(inputs.max));
        assert!(a.x > free.min.x && b.x < free.max.x);
        assert!((at(inputs.center()).y - free.center().y).abs() < 0.01);
        // Without inputs: the top.
        let (centre, _) = fit_view_around(content, None, screen, free, FIT_PADDING);
        let top = world_to_screen(Vec2::new(0.0, content.max.y), screen, centre, wpp);
        let pad_y = content.height() * (FIT_PADDING - 1.0) * 0.5;
        assert!((top.y - pad_y / wpp - free.min.y).abs() < 0.01);
        // Inputs at the very bottom: the view stays inside the content.
        let low = Rect::new(-1990.0, -1000.0, -1950.0, -990.0);
        let (centre, _) = fit_view_around(content, Some(low), screen, free, FIT_PADDING);
        let bottom = world_to_screen(Vec2::new(0.0, content.min.y), screen, centre, wpp);
        assert!((bottom.y + pad_y / wpp - free.max.y).abs() < 0.01);
        // A circuit that fits at 65% or closer is centred as before, and
        // so is one that fits only without its padding.
        let fits = Rect::new(0.0, 0.0, 400.0, 200.0);
        let (centre, wpp) = fit_view_around(fits, Some(inputs), screen, free, FIT_PADDING);
        assert!(wpp < FIT_MAX_WPP);
        assert!(close(
            world_to_screen(fits.center(), screen, centre, wpp),
            free.center()
        ));
        let snug = Rect::from_center_size(Vec2::ZERO, free.size() * FIT_MAX_WPP * 0.95);
        let (centre, wpp) = fit_view_around(snug, Some(inputs), screen, free, FIT_PADDING);
        assert_eq!(wpp, FIT_MAX_WPP);
        assert!(close(
            world_to_screen(snug.center(), screen, centre, wpp),
            free.center()
        ));
    }
}
