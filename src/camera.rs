//! The canvas camera: the maths of fitting, zooming and panning an
//! orthographic 2D camera (pure functions, tested without rendering), and
//! the wheel-zoom and middle-drag pan systems.
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
/// `screen`-sized window, with `padding` room around it.
pub fn fit_view(content: Rect, screen: Vec2, free: Rect, padding: f32) -> (Vec2, f32) {
    let size = content.size().max(Vec2::ONE);
    let free_size = free.size().max(Vec2::ONE);
    let wpp =
        ((size.x / free_size.x).max(size.y / free_size.y) * padding).clamp(FIT_MIN_WPP, MAX_WPP);
    // The camera centre sits at the screen centre; shift it so the
    // content's centre lands on the free area's centre.
    let offset = free.center() - screen * 0.5;
    let centre = content.center() - Vec2::new(offset.x, -offset.y) * wpp;
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
        let huge = Rect::from_center_size(Vec2::ZERO, Vec2::splat(1.0e6));
        assert_eq!(fit_view(huge, screen, free, 1.0).1, MAX_WPP);
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
}
