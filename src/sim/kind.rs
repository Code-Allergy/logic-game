//! `GateKind`: the editor's single node-kind enum.
//!
//! It is used by the ECS authoring components, by the save format and by the
//! palette. Every kind except [`GateKind::SubCircuit`] maps to exactly one
//! `logi_sim::netlist::NodeKind` (see [`GateKind::to_netlist`]); a subcircuit
//! becomes a `NodeKind::Instance` of a definition built from the
//! [`CircuitLibrary`](super::CircuitLibrary).

use bevy::prelude::*;
use logi_sim::netlist::{NodeKind, PinWidths};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Margin between a screen's picture and its tile edge, world units.
pub const SCREEN_BEZEL: f32 = 12.0;

/// Largest width the editor exposes for a word.
pub const MAX_WIDTH: u8 = logi_sim::netlist::MAX_WIDTH;

/// Most read ports of a [`GateKind::Ram2`].
const RAM2_READS: usize = logi_sim::netlist::MAX_READ_PORTS as usize;

/// Pin names of a [`GateKind::Ram2`] with the most read ports; fewer ports
/// use a prefix.
const RAM2_INPUTS: [&str; 3 + RAM2_READS] = [
    "waddr", "we", "wdata", "raddr0", "raddr1", "raddr2", "raddr3", "raddr4", "raddr5", "raddr6",
    "raddr7",
];
const RAM2_OUTPUTS: [&str; RAM2_READS] = [
    "rdata0", "rdata1", "rdata2", "rdata3", "rdata4", "rdata5", "rdata6", "rdata7",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Reflect)]
#[serde(tag = "type")]
pub enum GateKind {
    /// Circuit input port, driven from the editor.
    Input {
        w: u8,
    },
    /// Circuit output port.
    Output {
        w: u8,
    },
    /// A sink shown as a number. Behaves like an output in the root circuit
    /// and is omitted from subcircuit definitions (it is visual only).
    Display {
        w: u8,
    },
    Const {
        w: u8,
        value: u64,
    },
    Not {
        w: u8,
    },
    And {
        w: u8,
    },
    Or {
        w: u8,
    },
    Xor {
        w: u8,
    },
    Nand {
        w: u8,
    },
    Nor {
        w: u8,
    },
    Xnor {
        w: u8,
    },
    /// `sel ? b : a`.
    Mux {
        w: u8,
    },
    /// `en ? d : 0` (replaces the old tri-state buffer).
    Switch {
        w: u8,
    },
    Add {
        w: u8,
    },
    Eq {
        w: u8,
    },
    LtU {
        w: u8,
    },
    ReduceOr {
        w: u8,
    },
    ReduceAnd {
        w: u8,
    },
    ReduceXor {
        w: u8,
    },
    /// Bit slice `a[lo .. lo + len]`.
    Split {
        w: u8,
        lo: u8,
        len: u8,
    },
    /// Concatenation, input 0 in the lowest bits.
    Merge {
        widths: Vec<u8>,
    },
    Extend {
        from: u8,
        to: u8,
        signed: bool,
    },
    Clock {
        period: u32,
        duty: u32,
        phase: u32,
    },
    Rom {
        addr_bits: u8,
        w: u8,
        contents: Vec<u64>,
    },
    Delay {
        w: u8,
    },
    Register {
        w: u8,
    },
    DLatch {
        w: u8,
    },
    SrLatch,
    Ram {
        addr_bits: u8,
        w: u8,
    },
    /// A dual-port memory (`logi_sim`'s `Ram2`): one write port and
    /// `reads` read ports with their own addresses. `mask` makes the write
    /// enable a per-bit mask as wide as the word; `init` holds initial
    /// contents (missing words are 0). Both may be left out when saved.
    #[serde(alias = "DualPortRam")]
    Ram2 {
        addr_bits: u8,
        w: u8,
        reads: u8,
        #[serde(default)]
        mask: bool,
        #[serde(default)]
        init: Vec<u64>,
    },
    /// A graphical screen: pixels drawn into its own video memory as
    /// `mode` says, shown on the tile (see [`ScreenMode`]). `width` and
    /// `height` are in pixels, or in characters for [`ScreenMode::Text`];
    /// each is a power of two (see [`ScreenMode::sides`]). `scale` is the
    /// size of one pixel on the canvas, in world units.
    Screen {
        mode: ScreenMode,
        width: u16,
        height: u16,
        #[serde(default)]
        palette: ScreenPalette,
        #[serde(default = "default_screen_scale")]
        scale: u8,
    },
    /// An instance of a circuit definition in the [`super::CircuitLibrary`].
    SubCircuit {
        definition: Uuid,
    },
}

fn default_screen_scale() -> u8 {
    4
}

/// How a [`GateKind::Screen`] is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, Reflect)]
pub enum ScreenMode {
    /// Pins x, y, colour, draw, clear: each tick that `draw` is on paints
    /// one pixel; the screen keeps what is drawn; `clear` blanks it.
    #[default]
    Pixel,
    /// Pins addr, we, wdata: the screen shows its own video memory, written
    /// like a RAM. Pixel `(x, y)` is at address `y * width + x`.
    Framebuffer,
    /// Pins colour, hsync, vsync: every tick the beam paints one pixel and
    /// moves right; `hsync` starts the next line, `vsync` a new frame.
    Scanline,
    /// Pins addr, we, char: the memory holds one character code per cell
    /// (`y * width + x`), drawn with the built-in 8x8 font; bit 7 shows
    /// the character inverted.
    Text,
}

impl ScreenMode {
    pub const ALL: [ScreenMode; 4] = [
        ScreenMode::Pixel,
        ScreenMode::Framebuffer,
        ScreenMode::Scanline,
        ScreenMode::Text,
    ];

    /// The sides a screen in this mode may have: pixels, or characters for
    /// text. Powers of two only, so the address of `(x, y)` is `y` and `x`
    /// side by side.
    pub fn sides(self) -> &'static [u16] {
        match self {
            ScreenMode::Text => &[4, 8, 16, 32, 64],
            _ => &[8, 16, 32, 64, 128, 256],
        }
    }

    /// The allowed side nearest to `side`.
    pub fn fit(self, side: u16) -> u16 {
        let sides = self.sides();
        *sides
            .iter()
            .min_by_key(|&&s| (i32::from(s) - i32::from(side)).abs())
            .unwrap_or(&sides[0])
    }

    pub fn name(self) -> &'static str {
        match self {
            ScreenMode::Pixel => "Pixel port",
            ScreenMode::Framebuffer => "Framebuffer",
            ScreenMode::Scanline => "Scanline",
            ScreenMode::Text => "Text",
        }
    }

    /// The simulator mode it maps to.
    fn netlist(self) -> logi_sim::netlist::ScreenMode {
        use logi_sim::netlist::ScreenMode as S;
        match self {
            ScreenMode::Pixel => S::Pixel,
            ScreenMode::Framebuffer | ScreenMode::Text => S::Memory,
            ScreenMode::Scanline => S::Scan,
        }
    }
}

/// The colours of a [`GateKind::Screen`]: how many bits a pixel has and
/// what each value looks like. Value 0 is always the dark screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, Reflect)]
pub enum ScreenPalette {
    /// 1 bit: dark or lit (amber).
    #[default]
    Mono,
    /// 2 bits: black, dark grey, light grey, white.
    Gray,
    /// 4 bits, IRGB: bit 0 blue, bit 1 green, bit 2 red, bit 3 bright.
    Colors16,
    /// 8 bits: red in bits 7-5, green in 4-2, blue in 1-0.
    Rgb332,
}

impl ScreenPalette {
    pub const ALL: [ScreenPalette; 4] = [
        ScreenPalette::Mono,
        ScreenPalette::Gray,
        ScreenPalette::Colors16,
        ScreenPalette::Rgb332,
    ];

    /// Bits per pixel.
    pub fn bits(self) -> u8 {
        match self {
            ScreenPalette::Mono => 1,
            ScreenPalette::Gray => 2,
            ScreenPalette::Colors16 => 4,
            ScreenPalette::Rgb332 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ScreenPalette::Mono => "Mono",
            ScreenPalette::Gray => "4 greys",
            ScreenPalette::Colors16 => "16 colours",
            ScreenPalette::Rgb332 => "256 colours",
        }
    }
}

/// Largest pixel scale of a screen on the canvas.
pub const MAX_SCREEN_SCALE: u8 = 16;

/// Pixels per character side in [`ScreenMode::Text`].
pub const TEXT_CELL: u16 = 8;

impl GateKind {
    /// The simulator primitive this kind maps to (`None` for subcircuits).
    pub fn to_netlist(&self) -> Option<NodeKind> {
        use GateKind as G;
        Some(match self {
            G::Input { w } => NodeKind::Input { w: *w },
            G::Output { w } | G::Display { w } => NodeKind::Output { w: *w },
            G::Const { w, value } => NodeKind::Const {
                w: *w,
                value: *value & logi_sim::netlist::mask(*w),
            },
            G::Not { w } => NodeKind::Not { w: *w },
            G::And { w } => NodeKind::And { w: *w },
            G::Or { w } => NodeKind::Or { w: *w },
            G::Xor { w } => NodeKind::Xor { w: *w },
            G::Nand { w } => NodeKind::Nand { w: *w },
            G::Nor { w } => NodeKind::Nor { w: *w },
            G::Xnor { w } => NodeKind::Xnor { w: *w },
            G::Mux { w } => NodeKind::Mux { w: *w },
            G::Switch { w } => NodeKind::Switch { w: *w },
            G::Add { w } => NodeKind::Add { w: *w },
            G::Eq { w } => NodeKind::Eq { w: *w },
            G::LtU { w } => NodeKind::LtU { w: *w },
            G::ReduceOr { w } => NodeKind::ReduceOr { w: *w },
            G::ReduceAnd { w } => NodeKind::ReduceAnd { w: *w },
            G::ReduceXor { w } => NodeKind::ReduceXor { w: *w },
            G::Split { w, lo, len } => NodeKind::Split {
                w: *w,
                lo: *lo,
                len: *len,
            },
            G::Merge { widths } => NodeKind::Merge {
                widths: widths.iter().copied().collect(),
            },
            G::Extend { from, to, signed } => NodeKind::Extend {
                from: *from,
                to: *to,
                signed: *signed,
            },
            G::Clock {
                period,
                duty,
                phase,
            } => NodeKind::Clock {
                period: *period,
                duty: *duty,
                phase: *phase,
            },
            G::Rom {
                addr_bits,
                w,
                contents,
            } => NodeKind::Rom {
                addr_bits: *addr_bits,
                w: *w,
                contents: contents.as_slice().into(),
            },
            G::Delay { w } => NodeKind::Delay { w: *w },
            G::Register { w } => NodeKind::Register { w: *w },
            G::DLatch { w } => NodeKind::DLatch { w: *w },
            G::SrLatch => NodeKind::SrLatch,
            G::Ram { addr_bits, w } => NodeKind::Ram {
                addr_bits: *addr_bits,
                w: *w,
                init: Vec::new().into(),
            },
            G::Ram2 {
                addr_bits,
                w,
                reads,
                mask,
                init,
            } => NodeKind::Ram2 {
                addr_bits: *addr_bits,
                w: *w,
                reads: *reads,
                mask: *mask,
                init: init.as_slice().into(),
            },
            G::Screen {
                mode,
                width,
                height,
                palette,
                ..
            } => {
                // A side that is not a power of two gives 0 address bits,
                // which `check_params` refuses.
                let bits = |side: u16| {
                    if side.is_power_of_two() {
                        side.trailing_zeros() as u8
                    } else {
                        0
                    }
                };
                NodeKind::Screen {
                    mode: mode.netlist(),
                    x_bits: bits(*width),
                    y_bits: bits(*height),
                    w: if *mode == ScreenMode::Text {
                        8
                    } else {
                        palette.bits()
                    },
                }
            }
            G::SubCircuit { .. } => return None,
        })
    }

    /// Check the kind's parameters. Subcircuits are checked when the bridge
    /// builds their definition.
    pub fn check(&self) -> Result<(), String> {
        if let GateKind::Screen {
            mode,
            width,
            height,
            scale,
            ..
        } = self
        {
            let sides = mode.sides();
            let list = || {
                sides
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if !sides.contains(width) || !sides.contains(height) {
                return Err(format!(
                    "a {} screen's width and height must each be one of {}",
                    mode.name().to_lowercase(),
                    list()
                ));
            }
            if !(1..=MAX_SCREEN_SCALE).contains(scale) {
                return Err(format!("screen scale must be 1..={MAX_SCREEN_SCALE}"));
            }
        }
        match self.to_netlist() {
            Some(kind) => kind.check_params(),
            None => Ok(()),
        }
    }

    /// Pin widths `(inputs, outputs)` of a primitive kind. Subcircuit pins
    /// come from their definition (see [`super::CircuitLibrary::pins`]).
    pub fn primitive_pins(&self) -> Option<(PinWidths, PinWidths)> {
        self.to_netlist().and_then(|k| k.pins())
    }

    /// Display name shown in the editor.
    pub fn display_name(&self) -> &'static str {
        use GateKind as G;
        match self {
            G::Input { .. } => "INPUT",
            G::Output { .. } => "OUTPUT",
            G::Display { .. } => "DISPLAY",
            G::Const { .. } => "CONST",
            G::Not { .. } => "NOT",
            G::And { .. } => "AND",
            G::Or { .. } => "OR",
            G::Xor { .. } => "XOR",
            G::Nand { .. } => "NAND",
            G::Nor { .. } => "NOR",
            G::Xnor { .. } => "XNOR",
            G::Mux { .. } => "MUX",
            G::Switch { .. } => "SWITCH",
            G::Add { .. } => "ADD",
            G::Eq { .. } => "EQ",
            G::LtU { .. } => "LT",
            G::ReduceOr { .. } => "R-OR",
            G::ReduceAnd { .. } => "R-AND",
            G::ReduceXor { .. } => "R-XOR",
            G::Split { .. } => "SPLIT",
            G::Merge { .. } => "MERGE",
            G::Extend { .. } => "EXTEND",
            G::Clock { .. } => "CLOCK",
            G::Rom { .. } => "ROM",
            G::Delay { .. } => "DELAY",
            G::Register { .. } => "REGISTER",
            G::DLatch { .. } => "D-LATCH",
            G::SrLatch => "SR-LATCH",
            G::Ram { .. } => "RAM",
            G::Ram2 { .. } => "DUAL-PORT RAM",
            G::Screen { .. } => "SCREEN",
            G::SubCircuit { .. } => "SUBCIRCUIT",
        }
    }

    /// Short names for the input pins of a primitive.
    pub fn input_pin_names(&self) -> &'static [&'static str] {
        use GateKind as G;
        match self {
            G::Output { .. } | G::Display { .. } | G::Not { .. } | G::Delay { .. } => &["in"],
            G::ReduceOr { .. } | G::ReduceAnd { .. } | G::ReduceXor { .. } => &["a"],
            G::Split { .. } | G::Extend { .. } => &["a"],
            G::And { .. }
            | G::Or { .. }
            | G::Xor { .. }
            | G::Nand { .. }
            | G::Nor { .. }
            | G::Xnor { .. }
            | G::Eq { .. }
            | G::LtU { .. } => &["a", "b"],
            G::Mux { .. } => &["sel", "a", "b"],
            G::Switch { .. } => &["en", "d"],
            G::Add { .. } => &["a", "b", "cin"],
            G::Register { .. } | G::DLatch { .. } => &["en", "d"],
            G::SrLatch => &["s", "r"],
            G::Rom { .. } => &["addr"],
            G::Ram { .. } => &["addr", "we", "wdata"],
            G::Ram2 { reads, .. } => &RAM2_INPUTS[..3 + usize::from(*reads).min(RAM2_READS)],
            G::Screen { mode, .. } => match mode {
                ScreenMode::Pixel => &["x", "y", "colour", "draw", "clear"],
                ScreenMode::Framebuffer => &["addr", "we", "wdata"],
                ScreenMode::Scanline => &["colour", "hsync", "vsync"],
                ScreenMode::Text => &["addr", "we", "char"],
            },
            _ => &[],
        }
    }

    /// Short names for the output pins of a primitive.
    pub fn output_pin_names(&self) -> &'static [&'static str] {
        use GateKind as G;
        match self {
            G::Add { .. } => &["sum", "cout"],
            G::Output { .. } | G::Display { .. } | G::Screen { .. } | G::SubCircuit { .. } => &[],
            G::Delay { .. } | G::Register { .. } | G::DLatch { .. } | G::SrLatch => &["q"],
            G::Rom { .. } | G::Ram { .. } => &["data"],
            G::Ram2 { reads, .. } => &RAM2_OUTPUTS[..usize::from(*reads).min(RAM2_READS)],
            _ => &["y"],
        }
    }

    /// The main width parameter, for kinds that have one.
    pub fn width(&self) -> Option<u8> {
        use GateKind as G;
        match self {
            G::Input { w }
            | G::Output { w }
            | G::Display { w }
            | G::Const { w, .. }
            | G::Not { w }
            | G::And { w }
            | G::Or { w }
            | G::Xor { w }
            | G::Nand { w }
            | G::Nor { w }
            | G::Xnor { w }
            | G::Mux { w }
            | G::Switch { w }
            | G::Add { w }
            | G::Eq { w }
            | G::LtU { w }
            | G::ReduceOr { w }
            | G::ReduceAnd { w }
            | G::ReduceXor { w }
            | G::Split { w, .. }
            | G::Rom { w, .. }
            | G::Delay { w }
            | G::Register { w }
            | G::DLatch { w }
            | G::Ram { w, .. }
            | G::Ram2 { w, .. } => Some(*w),
            G::Extend { to, .. } => Some(*to),
            G::Merge { .. }
            | G::Clock { .. }
            | G::SrLatch
            | G::Screen { .. }
            | G::SubCircuit { .. } => None,
        }
    }

    /// This kind with its main width parameter set to `w` (clamped to
    /// 1..=64). Kinds without one are returned unchanged.
    pub fn with_width(&self, w: u8) -> Self {
        use GateKind as G;
        let w = w.clamp(1, MAX_WIDTH);
        let mut k = self.clone();
        match &mut k {
            G::Input { w: x }
            | G::Output { w: x }
            | G::Display { w: x }
            | G::Not { w: x }
            | G::And { w: x }
            | G::Or { w: x }
            | G::Xor { w: x }
            | G::Nand { w: x }
            | G::Nor { w: x }
            | G::Xnor { w: x }
            | G::Mux { w: x }
            | G::Switch { w: x }
            | G::Add { w: x }
            | G::Eq { w: x }
            | G::LtU { w: x }
            | G::ReduceOr { w: x }
            | G::ReduceAnd { w: x }
            | G::ReduceXor { w: x }
            | G::Rom { w: x, .. }
            | G::Delay { w: x }
            | G::Register { w: x }
            | G::DLatch { w: x }
            | G::Ram { w: x, .. }
            | G::Ram2 { w: x, .. } => *x = w,
            G::Const { w: x, value } => {
                *x = w;
                *value &= logi_sim::netlist::mask(w);
            }
            G::Split { w: x, lo, len } => {
                *x = w;
                *lo = (*lo).min(w - 1);
                *len = (*len).clamp(1, w - *lo);
            }
            G::Extend { from, to, .. } => {
                *to = w;
                *from = (*from).min(w);
            }
            G::Merge { .. }
            | G::Clock { .. }
            | G::SrLatch
            | G::Screen { .. }
            | G::SubCircuit { .. } => {}
        }
        k
    }

    /// The pixels of a screen, `(width, height)`: its resolution, or the
    /// characters times [`TEXT_CELL`] for text.
    pub fn screen_pixels(&self) -> Option<(u32, u32)> {
        match self {
            GateKind::Screen {
                mode,
                width,
                height,
                ..
            } => {
                let cell = if *mode == ScreenMode::Text {
                    u32::from(TEXT_CELL)
                } else {
                    1
                };
                Some((u32::from(*width) * cell, u32::from(*height) * cell))
            }
            _ => None,
        }
    }

    /// Size of a screen's picture on the canvas: pixels times scale.
    pub fn screen_face(&self) -> Option<Vec2> {
        let GateKind::Screen { scale, .. } = self else {
            return None;
        };
        let (w, h) = self.screen_pixels()?;
        Some(Vec2::new(w as f32, h as f32) * f32::from((*scale).max(1)))
    }

    pub fn is_input(&self) -> bool {
        matches!(self, GateKind::Input { .. })
    }

    /// Ports that need a label in a saved workspace.
    pub fn is_port(&self) -> bool {
        matches!(
            self,
            GateKind::Input { .. } | GateKind::Output { .. } | GateKind::Display { .. }
        )
    }

    /// Kinds offered by the editor palette, with width-parametric kinds at
    /// width `w`.
    pub fn palette(w: u8) -> Vec<(&'static str, Vec<GateKind>)> {
        use GateKind as G;
        let w = w.clamp(1, MAX_WIDTH);
        vec![
            (
                "Ports",
                vec![
                    G::Input { w },
                    G::Output { w },
                    G::Display { w },
                    G::Screen {
                        mode: ScreenMode::Pixel,
                        width: 32,
                        height: 32,
                        palette: ScreenPalette::Mono,
                        scale: 4,
                    },
                    G::Const { w, value: 0 },
                    G::Clock {
                        period: 10,
                        duty: 5,
                        phase: 0,
                    },
                ],
            ),
            (
                "Gates",
                vec![
                    G::Not { w },
                    G::And { w },
                    G::Or { w },
                    G::Xor { w },
                    G::Nand { w },
                    G::Nor { w },
                    G::Xnor { w },
                ],
            ),
            (
                "Routing",
                vec![
                    G::Mux { w },
                    G::Switch { w },
                    G::Split { w, lo: 0, len: 1 },
                    G::Merge {
                        widths: vec![1; usize::from(w.max(2))],
                    },
                    G::Extend {
                        from: 1,
                        to: w,
                        signed: false,
                    },
                ],
            ),
            (
                "Arithmetic",
                vec![
                    G::Add { w },
                    G::Eq { w },
                    G::LtU { w },
                    G::ReduceOr { w },
                    G::ReduceAnd { w },
                    G::ReduceXor { w },
                ],
            ),
            (
                "State",
                vec![
                    G::Delay { w },
                    G::Register { w },
                    G::DLatch { w },
                    G::SrLatch,
                    G::Ram { addr_bits: 4, w },
                    G::Ram2 {
                        addr_bits: 4,
                        w,
                        reads: 2,
                        mask: false,
                        init: Vec::new(),
                    },
                    G::Rom {
                        addr_bits: 4,
                        w,
                        contents: Vec::new(),
                    },
                ],
            ),
        ]
    }

    /// Tile size for this kind.
    pub fn size(&self) -> Vec2 {
        match self {
            GateKind::Screen { .. } => {
                let pins = self.input_pin_names().len() as f32;
                let face = self.screen_face().unwrap_or(Vec2::splat(64.0));
                let up = |v: f32| (v / 20.0).ceil() * 20.0;
                Vec2::new(
                    up(face.x + 2.0 * SCREEN_BEZEL),
                    up((face.y + 2.0 * SCREEN_BEZEL).max(pins * 20.0 + 20.0)),
                )
            }
            GateKind::Display { w } => {
                let hex_digits = usize::from(*w).div_ceil(4);
                Vec2::new((hex_digits as f32 * 18.0 + 40.0).max(120.0), 70.0)
            }
            // Room for the write port and every read port.
            GateKind::Ram2 { reads, .. } => {
                Vec2::new(70.0, 50.0 + 20.0 * f32::from((*reads).clamp(1, 8) - 1))
            }
            _ => Vec2::new(50.0, 50.0),
        }
    }
}
