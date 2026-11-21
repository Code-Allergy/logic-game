//! The parts bar's catalogue: which kinds sit under which family tab,
//! which number key picks which slot, and the names and one-line
//! descriptions the HUD shows for a kind. Pure data, tested without
//! rendering.

use bevy::prelude::KeyCode;

use crate::sim::GateKind;

/// A family tab of the parts bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PartTab {
    #[default]
    Logic,
    Memory,
    InOut,
    Routing,
    Arithmetic,
    /// Saved workspaces and demos, placed as subcircuit cells.
    Chips,
}

impl PartTab {
    pub const ALL: [PartTab; 6] = [
        PartTab::Logic,
        PartTab::Memory,
        PartTab::InOut,
        PartTab::Routing,
        PartTab::Arithmetic,
        PartTab::Chips,
    ];

    pub fn title(self) -> &'static str {
        match self {
            PartTab::Logic => "Logic",
            PartTab::Memory => "Memory",
            PartTab::InOut => "In & out",
            PartTab::Routing => "Routing",
            PartTab::Arithmetic => "Arithmetic",
            PartTab::Chips => "Chips",
        }
    }

    /// The tab a group of [`GateKind::palette`] belongs to. `None` for a
    /// group this bar does not know (it then shows under Routing, see
    /// [`tab_kinds`]).
    pub fn of_palette_group(group: &str) -> Option<PartTab> {
        Some(match group {
            "Gates" => PartTab::Logic,
            "State" => PartTab::Memory,
            "Ports" => PartTab::InOut,
            "Routing" => PartTab::Routing,
            "Arithmetic" => PartTab::Arithmetic,
            _ => return None,
        })
    }
}

/// The primitive kinds under `tab`, width-parametric ones at width `w`, in
/// slot order. Built from [`GateKind::palette`], so every palette kind
/// has exactly one slot; groups the bar does not know land in Routing.
/// The Chips tab has no primitives.
pub fn tab_kinds(tab: PartTab, w: u8) -> Vec<GateKind> {
    if tab == PartTab::Chips {
        return Vec::new();
    }
    GateKind::palette(w)
        .into_iter()
        .filter(|(group, _)| PartTab::of_palette_group(group).unwrap_or(PartTab::Routing) == tab)
        .flat_map(|(_, kinds)| kinds)
        .collect()
}

/// Number of slots with a number key.
pub const KEYED_SLOTS: usize = 9;

/// The slot a key picks: `1`–`9` (main row or keypad) are slots 0–8.
pub fn slot_for_key(key: KeyCode) -> Option<usize> {
    use KeyCode as K;
    let slot = match key {
        K::Digit1 | K::Numpad1 => 0,
        K::Digit2 | K::Numpad2 => 1,
        K::Digit3 | K::Numpad3 => 2,
        K::Digit4 | K::Numpad4 => 3,
        K::Digit5 | K::Numpad5 => 4,
        K::Digit6 | K::Numpad6 => 5,
        K::Digit7 | K::Numpad7 => 6,
        K::Digit8 | K::Numpad8 => 7,
        K::Digit9 | K::Numpad9 => 8,
        _ => return None,
    };
    Some(slot)
}

/// The keys [`slot_for_key`] maps, for polling.
pub const SLOT_KEYS: [KeyCode; 18] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::Numpad1,
    KeyCode::Numpad2,
    KeyCode::Numpad3,
    KeyCode::Numpad4,
    KeyCode::Numpad5,
    KeyCode::Numpad6,
    KeyCode::Numpad7,
    KeyCode::Numpad8,
    KeyCode::Numpad9,
];

/// The key label of a slot (`"1"`–`"9"`), if it has one.
pub fn slot_key_label(slot: usize) -> Option<String> {
    (slot < KEYED_SLOTS).then(|| (slot + 1).to_string())
}

/// A kind's name in sentence case, as the inspector titles it.
pub fn kind_title(kind: &GateKind) -> &'static str {
    match kind.display_name() {
        "INPUT" => "Input",
        "OUTPUT" => "Output",
        "DISPLAY" => "Readout",
        "CONST" => "Constant",
        "NOT" => "NOT gate",
        "AND" => "AND gate",
        "OR" => "OR gate",
        "XOR" => "XOR gate",
        "NAND" => "NAND gate",
        "NOR" => "NOR gate",
        "XNOR" => "XNOR gate",
        "MUX" => "Multiplexer",
        "SWITCH" => "Switch",
        "ADD" => "Adder",
        "EQ" => "Equal",
        "LT" => "Less than",
        "R-OR" => "Any bit set",
        "R-AND" => "All bits set",
        "R-XOR" => "Parity",
        "SPLIT" => "Split",
        "MERGE" => "Merge",
        "EXTEND" => "Extend",
        "CLOCK" => "Clock",
        "ROM" => "ROM",
        "DELAY" => "Delay",
        "REGISTER" => "Register",
        "D-LATCH" => "D latch",
        "SR-LATCH" => "SR latch",
        "RAM" => "RAM",
        "SCREEN" => "Screen",
        "SUBCIRCUIT" => "Chip",
        other => other,
    }
}

/// What a kind does, in a line.
pub fn kind_blurb(kind: &GateKind) -> &'static str {
    match kind.display_name() {
        "INPUT" => "A value you set by hand",
        "OUTPUT" => "A port of this circuit",
        "DISPLAY" => "Shows the number on its input",
        "CONST" => "Always the same value",
        "NOT" => "On when its input is off",
        "AND" => "On when both inputs are on",
        "OR" => "On when either input is on",
        "XOR" => "On when the inputs differ",
        "NAND" => "Off only when both inputs are on",
        "NOR" => "On only when both inputs are off",
        "XNOR" => "On when the inputs match",
        "MUX" => "Picks a or b by sel",
        "SWITCH" => "Passes d through while en is on",
        "ADD" => "Adds two numbers and a carry",
        "EQ" => "On when a equals b",
        "LT" => "On when a is less than b",
        "R-OR" => "On when any bit is on",
        "R-AND" => "On when every bit is on",
        "R-XOR" => "On when an odd number of bits are on",
        "SPLIT" => "Takes some bits out of a bus",
        "MERGE" => "Joins wires into one bus",
        "EXTEND" => "Widens a number",
        "CLOCK" => "Ticks on and off by itself",
        "ROM" => "A table of numbers, read by address",
        "DELAY" => "Its input, one tick later",
        "REGISTER" => "Remembers a number between ticks",
        "D-LATCH" => "Follows d while en is on, then holds",
        "SR-LATCH" => "Set turns it on, reset turns it off",
        "RAM" => "Memory you can write and read",
        "SCREEN" => "Shows the pixels a circuit draws",
        "SUBCIRCUIT" => "A circuit used as a part",
        _ => "",
    }
}

/// The short text on a part slot without a glyph.
pub fn slot_text(kind: &GateKind) -> &'static str {
    use GateKind as G;
    match kind {
        G::Input { .. } => "IN",
        G::Output { .. } => "OUT",
        G::Display { .. } => "SHOW",
        G::Screen { .. } => "SCRN",
        G::Const { .. } => "K",
        G::Clock { .. } => "CLK",
        G::Register { .. } => "REG",
        G::DLatch { .. } => "D-L",
        G::SrLatch => "SR",
        G::Delay { .. } => "DLY",
        G::ReduceOr { .. } => "ANY",
        G::ReduceAnd { .. } => "ALL",
        G::ReduceXor { .. } => "PAR",
        G::Switch { .. } => "SW",
        G::Extend { .. } => "EXT",
        other => other.display_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_kind_has_exactly_one_slot() {
        for w in [1, 8, 32] {
            let palette: Vec<GateKind> = GateKind::palette(w)
                .into_iter()
                .flat_map(|(_, k)| k)
                .collect();
            let slotted: Vec<GateKind> = PartTab::ALL
                .iter()
                .flat_map(|tab| tab_kinds(*tab, w))
                .collect();
            assert_eq!(slotted.len(), palette.len());
            for kind in &palette {
                assert_eq!(slotted.iter().filter(|k| *k == kind).count(), 1, "{kind:?}");
            }
        }
        // Every palette group has a tab of its own.
        for (group, _) in GateKind::palette(1) {
            assert!(PartTab::of_palette_group(group).is_some(), "{group}");
        }
        assert!(tab_kinds(PartTab::Chips, 8).is_empty());
    }

    #[test]
    fn tabs_hold_their_families() {
        let logic = tab_kinds(PartTab::Logic, 1);
        assert!(logic.contains(&GateKind::And { w: 1 }));
        assert!(logic.contains(&GateKind::Not { w: 1 }));
        let memory = tab_kinds(PartTab::Memory, 8);
        assert!(memory.contains(&GateKind::Register { w: 8 }));
        let io = tab_kinds(PartTab::InOut, 4);
        assert!(io.contains(&GateKind::Input { w: 4 }));
        assert!(tab_kinds(PartTab::Arithmetic, 8).contains(&GateKind::Add { w: 8 }));
        assert!(tab_kinds(PartTab::Routing, 8).contains(&GateKind::Mux { w: 8 }));
        // Each primitive tab fits the numbered slots.
        for tab in PartTab::ALL {
            assert!(tab_kinds(tab, 8).len() <= KEYED_SLOTS, "{tab:?}");
        }
    }

    #[test]
    fn number_keys_pick_slots_one_to_nine() {
        assert_eq!(slot_for_key(KeyCode::Digit1), Some(0));
        assert_eq!(slot_for_key(KeyCode::Numpad1), Some(0));
        assert_eq!(slot_for_key(KeyCode::Digit9), Some(8));
        assert_eq!(slot_for_key(KeyCode::Numpad5), Some(4));
        assert_eq!(slot_for_key(KeyCode::Digit0), None);
        assert_eq!(slot_for_key(KeyCode::KeyT), None);
        for key in SLOT_KEYS {
            let slot = slot_for_key(key).expect("a slot key");
            assert_eq!(slot_key_label(slot), Some((slot + 1).to_string()));
        }
        assert_eq!(slot_key_label(9), None);
    }

    #[test]
    fn every_kind_has_a_title_and_blurb() {
        for (_, kinds) in GateKind::palette(8) {
            for kind in kinds {
                assert!(!kind_title(&kind).is_empty());
                assert!(!kind_blurb(&kind).is_empty());
                assert!(slot_text(&kind).len() <= 6, "{kind:?}");
            }
        }
    }
}
