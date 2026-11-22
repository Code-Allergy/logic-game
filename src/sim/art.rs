//! The Workshop art kit for the canvas: theme-driven materials, cached
//! meshes (tiles, pins, glyphs, pills), the fonts, the pin layout of a
//! tile and the dot-grid bench.
//!
//! Every material is shared by role ([`Paint`]), so switching the theme
//! rewrites about twenty material assets instead of touching every
//! entity. Meshes are cached by [`MeshKey`]; a circuit uses only a handful
//! of distinct tile sizes, so they are built once.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::GateKind;
use super::shapes::{Shape, arc, quad_bezier};
use crate::fonts;
use crate::theme::{Family, Palette, Theme};

// ─────────────────────────────────────────────────────────────────────────────
// Dimensions (world units; 20 is one grid step)
// ─────────────────────────────────────────────────────────────────────────────

/// Ink outline around every tile.
pub const OUTLINE: f32 = 2.5;
/// Solid bottom edge drawn below a tile (instead of a soft shadow).
pub const EDGE: f32 = 4.0;
/// Pitch of the bench's dot grid.
pub const GRID: f32 = 20.0;
/// Most pins on a side are this far apart.
pub const PIN_PITCH: f32 = 20.0;
pub const PIN_RADIUS: f32 = 4.0;
pub const PIN_RING: f32 = 2.0;
/// Stroke width of the symbols drawn inside tiles.
pub const GLYPH_STROKE: f32 = 2.5;
/// Wire core and casing widths, 1-bit and bus.
pub const BIT_CORE: f32 = 4.5;
pub const BIT_CASING: f32 = 9.5;
pub const BUS_CORE: f32 = 8.0;
pub const BUS_CASING: f32 = 14.0;
/// Height of a bus value pill.
pub const PILL_HEIGHT: f32 = 22.0;
/// Width of one value digit in a pill.
pub const PILL_DIGIT: f32 = 8.0;

/// Corner radius of a tile of `size`: about 12 for a 60-unit tile.
pub fn tile_radius(size: Vec2) -> f32 {
    (size.x.min(size.y) * 0.2).clamp(6.0, 14.0)
}

/// Casing width around a core of width `core`.
pub fn casing_for(core: f32) -> f32 {
    if core >= BUS_CORE {
        BUS_CASING
    } else {
        core + (BIT_CASING - BIT_CORE)
    }
}

/// Size of the value pill for `digits` hex digits.
pub fn pill_size(digits: usize) -> Vec2 {
    Vec2::new(digits.max(1) as f32 * PILL_DIGIT + 16.0, PILL_HEIGHT)
}

// ─────────────────────────────────────────────────────────────────────────────
// Families and pins
// ─────────────────────────────────────────────────────────────────────────────

/// The tile family of a kind.
pub fn family(kind: &GateKind) -> Family {
    use GateKind as G;
    match kind {
        G::Input { .. }
        | G::Output { .. }
        | G::Display { .. }
        | G::Screen { .. }
        | G::Clock { .. } => Family::Io,
        G::Delay { .. }
        | G::Register { .. }
        | G::DLatch { .. }
        | G::SrLatch
        | G::Ram { .. }
        | G::Ram2 { .. }
        | G::Rom { .. } => Family::State,
        _ => Family::Logic,
    }
}

/// Where the pins of a tile sit, relative to its centre: inputs on the
/// left edge, outputs on the right, pin 0 at the top.
#[derive(Component, Debug, Clone, PartialEq, Default)]
pub struct PinLayout {
    pub inputs: Vec<Vec2>,
    pub outputs: Vec<Vec2>,
    /// Pin dot radius (smaller when a side is crowded).
    pub radius: f32,
}

impl PinLayout {
    pub fn new(size: Vec2, inputs: usize, outputs: usize) -> Self {
        let inset = tile_radius(size).max(8.0);
        let span = (size.y - 2.0 * inset).max(0.0);
        let pitch = |n: usize| {
            if n <= 1 {
                PIN_PITCH
            } else {
                (span / (n - 1) as f32).min(PIN_PITCH)
            }
        };
        let side = |n: usize, x: f32| -> Vec<Vec2> {
            let p = pitch(n);
            let top = (n.saturating_sub(1)) as f32 * p * 0.5;
            (0..n).map(|i| Vec2::new(x, top - i as f32 * p)).collect()
        };
        let tightest = pitch(inputs).min(pitch(outputs));
        Self {
            inputs: side(inputs, -size.x * 0.5),
            outputs: side(outputs, size.x * 0.5),
            radius: PIN_RADIUS.min(tightest * 0.3).max(1.0),
        }
    }

    pub fn input(&self, pin: u8) -> Vec2 {
        self.inputs
            .get(usize::from(pin))
            .copied()
            .unwrap_or_default()
    }

    pub fn output(&self, pin: u8) -> Vec2 {
        self.outputs
            .get(usize::from(pin))
            .copied()
            .unwrap_or_default()
    }

    pub fn ring(&self) -> f32 {
        PIN_RING.min(self.radius * 0.6)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Paints
// ─────────────────────────────────────────────────────────────────────────────

/// A colour role. Materials and theme-following texts name a paint; the
/// theme decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Paint {
    Ink,
    Tile,
    StateTint,
    IoTint,
    Text,
    TextSecondary,
    TextFaint,
    StateText,
    PinOff,
    On,
    Off,
    Bus,
    BusZero,
    BusText,
    State,
    Io,
    Fault,
    Screen,
    Halo,
    /// The bench, for the gap a wire cuts in the one it passes over.
    Bench,
    /// A wire the simulator did not accept.
    FaultText,
}

impl Paint {
    pub const ALL: [Paint; 21] = [
        Paint::Ink,
        Paint::Tile,
        Paint::StateTint,
        Paint::IoTint,
        Paint::Text,
        Paint::TextSecondary,
        Paint::TextFaint,
        Paint::StateText,
        Paint::PinOff,
        Paint::On,
        Paint::Off,
        Paint::Bus,
        Paint::BusZero,
        Paint::BusText,
        Paint::State,
        Paint::Io,
        Paint::Fault,
        Paint::Screen,
        Paint::Halo,
        Paint::Bench,
        Paint::FaultText,
    ];

    pub fn color(self, p: &Palette) -> Color {
        match self {
            Paint::Ink => p.ink,
            Paint::Tile => p.tile,
            Paint::StateTint => p.state_tint,
            Paint::IoTint => p.io_tint,
            Paint::Text => p.text,
            Paint::TextSecondary => p.text_secondary,
            Paint::TextFaint => p.text_faint,
            Paint::StateText => p.state_text,
            Paint::PinOff => p.pin_off,
            Paint::On => p.on,
            Paint::Off => p.off,
            Paint::Bus => p.bus,
            Paint::BusZero => p.bus_zero,
            Paint::BusText => p.bus_text,
            Paint::State => p.state,
            Paint::Io => p.io,
            Paint::Fault => p.fault,
            Paint::Screen => p.screen,
            Paint::Halo => p.halo,
            Paint::Bench => p.bench,
            Paint::FaultText => p.fault_text,
        }
    }

    pub fn family_fill(family: Family) -> Paint {
        match family {
            Family::Logic => Paint::Tile,
            Family::State => Paint::StateTint,
            Family::Io => Paint::IoTint,
        }
    }

    pub fn family_text(family: Family) -> Paint {
        match family {
            Family::State => Paint::StateText,
            Family::Logic | Family::Io => Paint::Text,
        }
    }

    /// Paint of a signal (see [`Palette::signal`]).
    pub fn signal(width: u8, value: u64) -> Paint {
        match (width > 1, value != 0) {
            (false, true) => Paint::On,
            (false, false) => Paint::Off,
            (true, true) => Paint::Bus,
            (true, false) => Paint::BusZero,
        }
    }

    /// Paint of a pin (see [`Palette::pin`]).
    pub fn pin(width: u8, value: u64) -> Paint {
        if value == 0 {
            Paint::PinOff
        } else {
            Paint::signal(width, value)
        }
    }
}

/// A text whose colour follows the theme.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone(pub Paint);

// ─────────────────────────────────────────────────────────────────────────────
// Glyphs
// ─────────────────────────────────────────────────────────────────────────────

/// A symbol drawn inside a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    And,
    Or,
    Xor,
    Not,
    Nand,
    Nor,
    Xnor,
    Mux,
    Clock,
    /// The clock notch on the enable pin of a register or latch.
    Notch,
}

impl Glyph {
    pub fn of(kind: &GateKind) -> Option<Glyph> {
        use GateKind as G;
        Some(match kind {
            G::And { .. } => Glyph::And,
            G::Or { .. } => Glyph::Or,
            G::Xor { .. } => Glyph::Xor,
            G::Not { .. } => Glyph::Not,
            G::Nand { .. } => Glyph::Nand,
            G::Nor { .. } => Glyph::Nor,
            G::Xnor { .. } => Glyph::Xnor,
            G::Mux { .. } => Glyph::Mux,
            G::Clock { .. } => Glyph::Clock,
            _ => return None,
        })
    }

    /// The symbol, about 24 units across, centred on the origin.
    pub fn shape(self) -> Shape {
        let w = GLYPH_STROKE;
        let v = Vec2::new;
        // The textbook bodies, as closed outlines.
        let and_body = |dx: f32| {
            let mut pts = vec![v(-11.0 + dx, -11.0), v(dx, -11.0)];
            pts.extend(arc(
                v(dx, 0.0),
                11.0,
                -std::f32::consts::FRAC_PI_2,
                std::f32::consts::FRAC_PI_2,
                8,
            ));
            pts.push(v(-11.0 + dx, 11.0));
            pts
        };
        let or_body = |dx: f32| {
            let mut pts = quad_bezier(v(-11.0 + dx, 11.0), v(1.5 + dx, 11.0), v(11.5 + dx, 0.0), 6);
            pts.extend(
                quad_bezier(
                    v(11.5 + dx, 0.0),
                    v(1.5 + dx, -11.0),
                    v(-11.0 + dx, -11.0),
                    6,
                )
                .into_iter()
                .skip(1),
            );
            pts.extend(
                quad_bezier(
                    v(-11.0 + dx, -11.0),
                    v(-5.5 + dx, 0.0),
                    v(-11.0 + dx, 11.0),
                    6,
                )
                .into_iter()
                .skip(1),
            );
            pts.pop();
            pts
        };
        let bubble = |x: f32| {
            Shape::stroke(
                &arc(v(x, 0.0), 3.0, 0.0, std::f32::consts::TAU, 12),
                w,
                true,
            )
        };
        let xor_back = |dx: f32| {
            Shape::stroke(
                &quad_bezier(
                    v(-15.5 + dx, 11.0),
                    v(-10.0 + dx, 0.0),
                    v(-15.5 + dx, -11.0),
                    6,
                ),
                w,
                false,
            )
        };
        let mut s = Shape::default();
        match self {
            Glyph::And => {
                s.append(&Shape::stroke(&and_body(0.0), w, true));
            }
            Glyph::Nand => {
                s.append(&Shape::stroke(&and_body(-3.0), w, true));
                s.append(&bubble(11.5));
            }
            Glyph::Or => {
                s.append(&Shape::stroke(&or_body(0.0), w, true));
            }
            Glyph::Nor => {
                s.append(&Shape::stroke(&or_body(-3.0), w, true));
                s.append(&bubble(11.5));
            }
            Glyph::Xor => {
                s.append(&Shape::stroke(&or_body(2.0), w, true));
                s.append(&xor_back(2.0));
            }
            Glyph::Xnor => {
                s.append(&Shape::stroke(&or_body(0.0), w, true));
                s.append(&xor_back(0.0));
                s.append(&bubble(14.5));
            }
            Glyph::Not => {
                s.append(&Shape::stroke(
                    &[v(-9.0, 10.0), v(6.0, 0.0), v(-9.0, -10.0)],
                    w,
                    true,
                ));
                s.append(&bubble(9.5));
            }
            Glyph::Mux => {
                s.append(&Shape::stroke(
                    &[v(-7.0, 14.0), v(7.0, 8.0), v(7.0, -8.0), v(-7.0, -14.0)],
                    w,
                    true,
                ));
            }
            Glyph::Clock => {
                s.append(&Shape::stroke(
                    &[
                        v(-14.0, -7.0),
                        v(-7.0, -7.0),
                        v(-7.0, 7.0),
                        v(1.0, 7.0),
                        v(1.0, -7.0),
                        v(8.0, -7.0),
                        v(8.0, 7.0),
                        v(14.0, 7.0),
                    ],
                    w,
                    false,
                ));
            }
            Glyph::Notch => {
                s.append(&Shape::stroke(
                    &[v(0.0, 6.0), v(7.0, 0.0), v(0.0, -6.0)],
                    2.0,
                    false,
                ));
            }
        }
        s
    }
}

/// Short name written inside a tile without a symbol.
pub fn tile_caption(kind: &GateKind) -> Option<String> {
    use GateKind as G;
    let w = kind.width().unwrap_or(1);
    let sized = |name: &str| {
        if w > 1 {
            format!("{name}{w}")
        } else {
            name.to_string()
        }
    };
    Some(match kind {
        G::Switch { .. } => sized("SW"),
        G::Add { .. } => sized("ADD"),
        G::Eq { .. } => "=".into(),
        G::LtU { .. } => "<".into(),
        G::ReduceOr { .. } => "|OR".into(),
        G::ReduceAnd { .. } => "&AND".into(),
        G::ReduceXor { .. } => "^XOR".into(),
        G::Split { lo, len: 1, .. } => format!("[{lo}]"),
        G::Split { lo, len, .. } => format!("[{lo}:{}]", lo + len - 1),
        G::Merge { widths } => format!("M{}", widths.len()),
        G::Extend { from, to, .. } => format!("{from}>{to}"),
        G::Delay { .. } => "DLY".into(),
        G::Register { .. } => sized("REG"),
        G::DLatch { .. } => "D-L".into(),
        G::SrLatch => "SR".into(),
        G::Ram { .. } => "RAM".into(),
        G::Ram2 { .. } => "RAM2".into(),
        G::Rom { .. } => "ROM".into(),
        G::Const { w, value } if *w > 4 => format!("{value:X}"),
        G::Const { value, .. } => format!("{value}"),
        _ => return None,
    })
}

/// `name` cut to `max` characters with an ellipsis.
pub fn shorten(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Mesh cache
// ─────────────────────────────────────────────────────────────────────────────

/// Half-unit quantisation for mesh keys.
fn q(x: f32) -> u32 {
    (x.max(0.0) * 2.0).round() as u32
}

/// A cached mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshKey {
    /// Outline plus bottom edge of a tile (quantised size).
    TileInk {
        w: u32,
        h: u32,
    },
    /// The tinted face of a tile.
    TileFill {
        w: u32,
        h: u32,
    },
    /// The ink rings of a tile's pins.
    PinRings {
        w: u32,
        h: u32,
        ins: u16,
        outs: u16,
    },
    /// A rounded rectangle (quantised size and radius).
    RoundedRect {
        w: u32,
        h: u32,
        r: u32,
    },
    /// A disc of quantised radius.
    Dot {
        r: u32,
    },
    Glyph(Glyph),
}

impl MeshKey {
    pub fn tile_ink(size: Vec2) -> Self {
        MeshKey::TileInk {
            w: q(size.x),
            h: q(size.y),
        }
    }
    pub fn tile_fill(size: Vec2) -> Self {
        MeshKey::TileFill {
            w: q(size.x),
            h: q(size.y),
        }
    }
    pub fn pin_rings(size: Vec2, ins: usize, outs: usize) -> Self {
        MeshKey::PinRings {
            w: q(size.x),
            h: q(size.y),
            ins: ins as u16,
            outs: outs as u16,
        }
    }
    pub fn rounded_rect(size: Vec2, radius: f32) -> Self {
        MeshKey::RoundedRect {
            w: q(size.x),
            h: q(size.y),
            r: q(radius),
        }
    }
    pub fn dot(radius: f32) -> Self {
        MeshKey::Dot { r: q(radius) }
    }

    pub fn shape(self) -> Shape {
        let un = |v: u32| v as f32 * 0.5;
        match self {
            MeshKey::TileInk { w, h } => {
                let size = Vec2::new(un(w), un(h));
                let outer = size + Vec2::splat(OUTLINE);
                let r = tile_radius(size) + OUTLINE * 0.5;
                let mut s = Shape::rounded_rect(outer, r);
                s.append(&Shape::rounded_rect(outer, r).translated(Vec2::new(0.0, -EDGE)));
                s
            }
            MeshKey::TileFill { w, h } => {
                let size = Vec2::new(un(w), un(h));
                Shape::rounded_rect(
                    size - Vec2::splat(OUTLINE),
                    tile_radius(size) - OUTLINE * 0.5,
                )
            }
            MeshKey::PinRings { w, h, ins, outs } => {
                let size = Vec2::new(un(w), un(h));
                let layout = PinLayout::new(size, usize::from(ins), usize::from(outs));
                let mut s = Shape::default();
                let r = layout.radius + layout.ring();
                for p in layout.inputs.iter().chain(&layout.outputs) {
                    s.append(&Shape::dot(*p, r));
                }
                s
            }
            MeshKey::RoundedRect { w, h, r } => Shape::rounded_rect(Vec2::new(un(w), un(h)), un(r)),
            MeshKey::Dot { r } => Shape::circle(un(r)),
            MeshKey::Glyph(g) => g.shape(),
        }
    }
}

/// Fonts for world-space text (Nunito for names, JetBrains Mono for
/// values). Default handles where there is no font asset store
/// (headless tests).
#[derive(Debug, Clone, Default)]
pub struct CanvasFonts {
    pub name: Handle<Font>,
    pub mono: Handle<Font>,
}

impl CanvasFonts {
    pub fn name(&self, size: f32) -> TextFont {
        TextFont {
            font: self.name.clone().into(),
            ..TextFont::from_font_size(size)
        }
    }

    pub fn mono(&self, size: f32) -> TextFont {
        TextFont {
            font: self.mono.clone().into(),
            ..TextFont::from_font_size(size)
        }
    }
}

/// Materials by paint, meshes by key, fonts.
#[derive(Resource)]
pub struct CanvasArt {
    materials: HashMap<Paint, Handle<ColorMaterial>>,
    meshes: HashMap<MeshKey, Handle<Mesh>>,
    pub fonts: CanvasFonts,
}

impl FromWorld for CanvasArt {
    fn from_world(world: &mut World) -> Self {
        let palette = world
            .get_resource::<Theme>()
            .map_or(Palette::DAY, |t| t.palette);
        let mut materials = HashMap::new();
        if let Some(mut assets) = world.get_resource_mut::<Assets<ColorMaterial>>() {
            for paint in Paint::ALL {
                materials.insert(
                    paint,
                    assets.add(ColorMaterial::from_color(paint.color(&palette))),
                );
            }
        }
        let fonts = match world.get_resource_mut::<Assets<Font>>() {
            Some(mut assets) => CanvasFonts {
                name: assets.add(Font::from_bytes(fonts::NUNITO_EXTRABOLD.to_vec())),
                mono: assets.add(Font::from_bytes(fonts::JETBRAINS_MONO_BOLD.to_vec())),
            },
            None => CanvasFonts::default(),
        };
        Self {
            materials,
            meshes: HashMap::new(),
            fonts,
        }
    }
}

impl CanvasArt {
    pub fn material(&self, paint: Paint) -> Handle<ColorMaterial> {
        self.materials.get(&paint).cloned().unwrap_or_default()
    }

    /// The paint a material handle stands for.
    pub fn paint_of(&self, handle: &Handle<ColorMaterial>) -> Option<Paint> {
        self.materials
            .iter()
            .find(|(_, h)| *h == handle)
            .map(|(p, _)| *p)
    }

    pub fn mesh(&mut self, meshes: &mut Assets<Mesh>, key: MeshKey) -> Handle<Mesh> {
        self.meshes
            .entry(key)
            .or_insert_with(|| meshes.add(key.shape().into_mesh()))
            .clone()
    }

    /// A `Mesh2d` piece: mesh, material and a local transform.
    pub fn piece(
        &mut self,
        meshes: &mut Assets<Mesh>,
        key: MeshKey,
        paint: Paint,
        at: Vec3,
    ) -> (Mesh2d, MeshMaterial2d<ColorMaterial>, Transform) {
        (
            Mesh2d(self.mesh(meshes, key)),
            MeshMaterial2d(self.material(paint)),
            Transform::from_translation(at),
        )
    }

    /// Recolour every material for `palette`.
    pub fn apply_palette(&self, palette: &Palette, assets: &mut Assets<ColorMaterial>) {
        for (paint, handle) in &self.materials {
            let color = paint.color(palette);
            if let Some(mut material) = assets.get_mut(handle)
                && material.color != color
            {
                material.color = color;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bench
// ─────────────────────────────────────────────────────────────────────────────

/// Pixels per grid cell of the dot texture.
const DOT_TEXELS: u32 = 32;
/// Dot radius in grid units (1.4 of 20 in the design).
const DOT_RADIUS: f32 = 1.5;
/// Side of the bench quad.
pub const BENCH_SIZE: f32 = 400_000.0;

/// RGBA8 sRGB texels of one grid cell: bench colour with an
/// anti-aliased dot in the middle.
pub fn bench_texels(palette: &Palette) -> Vec<u8> {
    let bench = palette.bench.to_srgba();
    let dot = palette.bench_dot.to_srgba();
    let n = DOT_TEXELS;
    let texel = GRID / n as f32;
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let p = (Vec2::new(x as f32, y as f32) + 0.5) * texel - Vec2::splat(GRID * 0.5);
            let coverage = (DOT_RADIUS - p.length() + texel * 0.5).clamp(0.0, texel) / texel;
            let mix = |a: f32, b: f32| ((a + (b - a) * coverage) * 255.0).round() as u8;
            out.extend([
                mix(bench.red, dot.red),
                mix(bench.green, dot.green),
                mix(bench.blue, dot.blue),
                255,
            ]);
        }
    }
    out
}

/// The dot texture, repeating.
pub fn bench_image(palette: &Palette) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: DOT_TEXELS,
            height: DOT_TEXELS,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bench_texels(palette),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

/// The material of the bench: the dot texture repeated once per grid
/// step across a [`BENCH_SIZE`] quad.
pub fn bench_material(image: Handle<Image>) -> ColorMaterial {
    ColorMaterial {
        color: Color::WHITE,
        texture: Some(image),
        uv_transform: Affine2::from_scale(Vec2::splat(BENCH_SIZE / GRID)),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_sit_on_the_tile_edges_top_first() {
        let size = Vec2::new(50.0, 50.0);
        let l = PinLayout::new(size, 2, 1);
        assert_eq!(
            l.inputs,
            vec![Vec2::new(-25.0, 10.0), Vec2::new(-25.0, -10.0)]
        );
        assert_eq!(l.outputs, vec![Vec2::new(25.0, 0.0)]);
        assert_eq!(l.radius, PIN_RADIUS);
        let three = PinLayout::new(size, 3, 2);
        assert_eq!(three.input(0).y, -three.input(2).y);
        assert!(three.input(0).y > three.input(1).y);
        assert!(three.input(0).y <= size.y * 0.5);
        // A crowded side packs its pins inside the tile and shrinks them.
        let crowded = PinLayout::new(size, 12, 1);
        assert!(crowded.inputs.iter().all(|p| p.y.abs() <= size.y * 0.5));
        assert!(crowded.radius < PIN_RADIUS);
        assert_eq!(
            crowded.input(40),
            Vec2::ZERO,
            "missing pins fall back to the centre"
        );
    }

    #[test]
    fn tile_meshes_have_outline_and_bottom_edge() {
        let size = Vec2::new(50.0, 50.0);
        let ink = MeshKey::tile_ink(size).shape();
        let (lo, hi) = ink.bounds().unwrap();
        assert!((hi.y - (size.y * 0.5 + OUTLINE * 0.5)).abs() < 1e-3);
        assert!(
            (lo.y - (-size.y * 0.5 - OUTLINE * 0.5 - EDGE)).abs() < 1e-3,
            "edge below"
        );
        assert!((hi.x - (size.x * 0.5 + OUTLINE * 0.5)).abs() < 1e-3);
        let fill = MeshKey::tile_fill(size).shape();
        let (flo, fhi) = fill.bounds().unwrap();
        assert!(
            flo.cmpgt(lo).all() && fhi.cmplt(hi).all(),
            "the face sits inside the ink"
        );
        let rings = MeshKey::pin_rings(size, 2, 1).shape();
        assert_eq!(rings.bounds().unwrap().0.x, -25.0 - PIN_RADIUS - PIN_RING);
    }

    #[test]
    fn every_glyph_fits_a_small_tile() {
        for g in [
            Glyph::And,
            Glyph::Or,
            Glyph::Xor,
            Glyph::Not,
            Glyph::Nand,
            Glyph::Nor,
            Glyph::Xnor,
            Glyph::Mux,
            Glyph::Clock,
            Glyph::Notch,
        ] {
            let (lo, hi) = g.shape().bounds().unwrap();
            assert!(
                lo.cmpge(Vec2::splat(-20.0)).all() && hi.cmple(Vec2::splat(20.0)).all(),
                "{g:?}"
            );
            assert!(g.shape().triangle_count() > 0);
        }
    }

    #[test]
    fn families_and_captions() {
        assert_eq!(family(&GateKind::And { w: 1 }), Family::Logic);
        assert_eq!(family(&GateKind::Register { w: 8 }), Family::State);
        assert_eq!(family(&GateKind::Input { w: 1 }), Family::Io);
        assert_eq!(Glyph::of(&GateKind::Nand { w: 1 }), Some(Glyph::Nand));
        assert_eq!(
            tile_caption(&GateKind::Add { w: 8 }).as_deref(),
            Some("ADD8")
        );
        assert_eq!(
            tile_caption(&GateKind::Const { w: 8, value: 0x2A }).as_deref(),
            Some("2A")
        );
        assert_eq!(shorten("Register file", 6), "Regis…");
        assert_eq!(shorten("ALU", 6), "ALU");
    }

    #[test]
    fn paints_follow_the_palette() {
        for paint in Paint::ALL {
            assert_eq!(
                paint.color(&Palette::DAY) == paint.color(&Palette::NIGHT),
                paint == Paint::On,
                "{paint:?}"
            );
        }
        assert_eq!(
            Paint::signal(8, 0).color(&Palette::DAY),
            Palette::DAY.signal(8, 0)
        );
        assert_eq!(
            Paint::pin(1, 1).color(&Palette::NIGHT),
            Palette::NIGHT.pin(1, 1)
        );
    }

    #[test]
    fn bench_texture_has_a_dot_in_the_middle() {
        let texels = bench_texels(&Palette::DAY);
        let n = DOT_TEXELS as usize;
        assert_eq!(texels.len(), n * n * 4);
        let at = |x: usize, y: usize| &texels[(y * n + x) * 4..(y * n + x) * 4 + 3];
        assert_eq!(at(0, 0), [0xD9, 0xE1, 0xEA], "bench at the corner");
        assert_eq!(at(n / 2, n / 2), [0xC3, 0xCD, 0xD9], "dot at the centre");
        let casings = (casing_for(BIT_CORE), casing_for(BUS_CORE));
        assert_eq!(casings, (BIT_CASING, BUS_CASING));
        assert!(pill_size(2).x < pill_size(4).x);
    }
}
