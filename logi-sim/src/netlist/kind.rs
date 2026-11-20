//! The primitive table (design doc section 3.3).

use std::sync::Arc;

use smallvec::{SmallVec, smallvec};

use super::DefId;

/// Largest value width, in bits.
pub const MAX_WIDTH: u8 = 64;

/// Largest RAM/ROM address width. 2^20 words keeps a single memory under
/// 8 MiB of `u64`s, which is plenty for game circuits.
pub const MAX_ADDR_BITS: u8 = 20;

/// Pin widths of one node: one entry per input pin and per output pin.
pub type PinWidths = SmallVec<[u8; 4]>;

/// Every primitive a [`super::Node`] can be.
///
/// Width parameters are in bits (1..=64). Bitwise ops apply across the whole
/// width. Pin order is listed on each variant and is fixed: an unconnected pin
/// never shifts its neighbours.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKind {
    // ── Definition ports ──────────────────────────────────────────────────
    /// Definition input port. Pins: `→ y`.
    Input { w: u8 },
    /// Definition output port. Pins: `a →`.
    Output { w: u8 },

    // ── Combinational ─────────────────────────────────────────────────────
    /// Pins: `→ y`. `value` is masked to `w`.
    Const { w: u8, value: u64 },
    /// Pins: `a → y`.
    Not { w: u8 },
    /// Pins: `a, b → y`.
    And { w: u8 },
    Or { w: u8 },
    Xor { w: u8 },
    Nand { w: u8 },
    Nor { w: u8 },
    Xnor { w: u8 },
    /// Pins: `sel(1), a, b → y`; `y = sel ? b : a`.
    Mux { w: u8 },
    /// Pins: `en(1), d → y`; `y = en ? d : 0`.
    Switch { w: u8 },
    /// Pins: `a, b, cin(1) → sum, cout(1)`.
    Add { w: u8 },
    /// Pins: `a, b → y(1)`.
    Eq { w: u8 },
    /// Unsigned less-than. Pins: `a, b → y(1)`.
    LtU { w: u8 },
    /// Pins: `a → y(1)`.
    ReduceOr { w: u8 },
    ReduceAnd { w: u8 },
    ReduceXor { w: u8 },
    /// Bit slice `a[lo .. lo+len]`. Pins: `a(w) → y(len)`.
    Split { w: u8, lo: u8, len: u8 },
    /// Concatenation, input 0 in the lowest bits. Pins: `a0(widths[0]), .. → y(sum)`.
    Merge { widths: SmallVec<[u8; 4]> },
    /// Zero or sign extension. Pins: `a(from) → y(to)`.
    Extend { from: u8, to: u8, signed: bool },
    /// Pins: `→ y(1)`. At tick count `t`, `y = ((t + phase) % period) < duty`.
    Clock { period: u32, duty: u32, phase: u32 },
    /// Combinational read. Pins: `addr(addr_bits) → data(w)`. Words past the
    /// end of `contents` read 0; stored words are masked to `w` on read.
    Rom { addr_bits: u8, w: u8, contents: Arc<[u64]> },
    /// A reference to another definition. Pins follow that definition's
    /// `inputs`/`outputs` port lists.
    Instance(DefId),

    // ── State ─────────────────────────────────────────────────────────────
    /// Pins: `d → q`. Next = `d`; output = state.
    Delay { w: u8 },
    /// Pins: `en(1), d → q`. Next = `en ? d : state`; output = state.
    Register { w: u8 },
    /// Pins: `en(1), d → q`. Next = `en ? d : state`; output = `en ? d : state`.
    DLatch { w: u8 },
    /// Pins: `s(1), r(1) → q(1)`. Next = `r ? 0 : s ? 1 : state`; output = state.
    SrLatch,
    /// Pins: `addr(addr_bits), we(1), wdata(w) → rdata(w)`. Read is
    /// combinational from committed memory; the write is applied at commit.
    /// `init` holds initial contents (missing words are 0).
    Ram { addr_bits: u8, w: u8, init: Arc<[u64]> },
}

/// Which pins of a node form combinational (same-tick) dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombDeps {
    /// Every output depends on every input.
    All,
    /// No output depends on any input within a tick (Delay, Register, SrLatch).
    None,
    /// Only the listed input pin indices feed the outputs (Ram: addr).
    Pins(&'static [u8]),
    /// Depends on the referenced definition's internal paths.
    Instance(DefId),
}

impl NodeKind {
    /// True for elements that hold state across ticks.
    pub fn is_state(&self) -> bool {
        matches!(
            self,
            NodeKind::Delay { .. }
                | NodeKind::Register { .. }
                | NodeKind::DLatch { .. }
                | NodeKind::SrLatch
                | NodeKind::Ram { .. }
        )
    }

    /// Combinational input→output edges for loop detection and ranking.
    pub fn comb_deps(&self) -> CombDeps {
        match self {
            NodeKind::Delay { .. } | NodeKind::Register { .. } | NodeKind::SrLatch => {
                CombDeps::None
            }
            NodeKind::Ram { .. } => CombDeps::Pins(&[0]),
            NodeKind::Instance(def) => CombDeps::Instance(*def),
            _ => CombDeps::All,
        }
    }

    /// Input and output pin widths, or `None` for [`NodeKind::Instance`]
    /// (whose pins come from the referenced definition).
    ///
    /// The result is only meaningful when [`Self::check_params`] passes.
    pub fn pins(&self) -> Option<(PinWidths, PinWidths)> {
        use NodeKind::*;
        let p = |i: &[u8], o: &[u8]| Some((PinWidths::from_slice(i), PinWidths::from_slice(o)));
        match self {
            Input { w } => p(&[], &[*w]),
            Output { w } => p(&[*w], &[]),
            Const { w, .. } => p(&[], &[*w]),
            Not { w } => p(&[*w], &[*w]),
            And { w } | Or { w } | Xor { w } | Nand { w } | Nor { w } | Xnor { w } => {
                p(&[*w, *w], &[*w])
            }
            Mux { w } => p(&[1, *w, *w], &[*w]),
            Switch { w } => p(&[1, *w], &[*w]),
            Add { w } => p(&[*w, *w, 1], &[*w, 1]),
            Eq { w } | LtU { w } => p(&[*w, *w], &[1]),
            ReduceOr { w } | ReduceAnd { w } | ReduceXor { w } => p(&[*w], &[1]),
            Split { w, len, .. } => p(&[*w], &[*len]),
            Merge { widths } => {
                let total = widths.iter().map(|&w| w as u32).sum::<u32>().min(255) as u8;
                Some((widths.iter().copied().collect(), smallvec![total]))
            }
            Extend { from, to, .. } => p(&[*from], &[*to]),
            Clock { .. } => p(&[], &[1]),
            Rom { addr_bits, w, .. } => p(&[*addr_bits], &[*w]),
            Instance(_) => None,
            Delay { w } => p(&[*w], &[*w]),
            Register { w } | DLatch { w } => p(&[1, *w], &[*w]),
            SrLatch => p(&[1, 1], &[1]),
            Ram { addr_bits, w, .. } => p(&[*addr_bits, 1, *w], &[*w]),
        }
    }

    /// Check the kind's own parameters (widths in range, slices in bounds...).
    /// Returns a human-readable reason on failure.
    pub fn check_params(&self) -> Result<(), String> {
        use NodeKind::*;
        let w_ok = |w: u8| (1..=MAX_WIDTH).contains(&w);
        let need = |ok: bool, msg: &str| if ok { Ok(()) } else { Err(msg.to_string()) };
        match self {
            Input { w }
            | Output { w }
            | Const { w, .. }
            | Not { w }
            | And { w }
            | Or { w }
            | Xor { w }
            | Nand { w }
            | Nor { w }
            | Xnor { w }
            | Mux { w }
            | Switch { w }
            | Add { w }
            | Eq { w }
            | LtU { w }
            | ReduceOr { w }
            | ReduceAnd { w }
            | ReduceXor { w }
            | Delay { w }
            | Register { w }
            | DLatch { w } => need(w_ok(*w), "width must be 1..=64"),
            Split { w, lo, len } => {
                need(w_ok(*w), "width must be 1..=64")?;
                need(*len >= 1, "split length must be at least 1")?;
                need(
                    (*lo as u32 + *len as u32) <= *w as u32,
                    "split range exceeds input width",
                )
            }
            Merge { widths } => {
                need(!widths.is_empty(), "merge needs at least one input")?;
                need(widths.iter().all(|&w| w_ok(w)), "merge input width must be 1..=64")?;
                need(
                    widths.iter().map(|&w| w as u32).sum::<u32>() <= MAX_WIDTH as u32,
                    "merged width exceeds 64",
                )
            }
            Extend { from, to, .. } => {
                need(w_ok(*from) && w_ok(*to), "width must be 1..=64")?;
                need(from <= to, "extend cannot narrow")
            }
            Clock { period, duty, .. } => {
                need(*period >= 1, "clock period must be at least 1")?;
                need(duty <= period, "clock duty exceeds period")
            }
            Rom { addr_bits, w, contents } => {
                need(w_ok(*w), "width must be 1..=64")?;
                need(
                    (1..=MAX_ADDR_BITS).contains(addr_bits),
                    "address width must be 1..=20",
                )?;
                need(
                    contents.len() <= 1usize << addr_bits,
                    "ROM contents exceed address space",
                )
            }
            Ram { addr_bits, w, init } => {
                need(w_ok(*w), "width must be 1..=64")?;
                need(
                    (1..=MAX_ADDR_BITS).contains(addr_bits),
                    "address width must be 1..=20",
                )?;
                need(
                    init.len() <= 1usize << addr_bits,
                    "RAM contents exceed address space",
                )
            }
            SrLatch | Instance(_) => Ok(()),
        }
    }
}

/// Bit mask with the low `w` bits set (`w` in 0..=64).
#[inline]
pub fn mask(w: u8) -> u64 {
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}
