//! Keyboard bindings of root inputs: an input part may name a key (its
//! `key` property, version 6 field `"key"`); while the simulation runs, the
//! input is 1 while that key is held and 0 otherwise. This is how a
//! circuit gets a game controller (the NES console's `p1_*` buttons).
//!
//! While the simulation runs, a bound key belongs to the circuit: it is
//! taken out of `ButtonInput<KeyCode>` before the editor's shortcuts read
//! it, so a bound arrow key does not pan, a bound `Space` does not pause.
//! Paused, every key is the editor's again and bound inputs keep their
//! value. A text field with the keyboard focus still gets its keys (egui
//! reads the keyboard events, not `ButtonInput`).

use std::collections::HashSet;

use bevy::input::ButtonState;
use bevy::input::keyboard::{KeyboardFocusLost, KeyboardInput};
use bevy::prelude::*;

use crate::sim::{
    GateKind, GateType, InputDrive, PersistentGateData, RootItems, SetInputValue, SimBridge,
};

/// The node property that holds an input's key.
pub const KEY_PROPERTY: &str = "key";

/// The key of a binding's name: a Bevy `KeyCode` name (`ArrowUp`, `KeyZ`,
/// `Enter`, `ShiftRight`, `Digit1`, ...), case-insensitive, or a short form
/// (`Up`, `Z`, `1`, `Shift` for the right shift, `Ctrl`, `Alt`).
pub fn key_code(name: &str) -> Option<KeyCode> {
    let n = name.trim().to_ascii_lowercase();
    let n = n.strip_prefix("key").filter(|r| r.len() == 1).unwrap_or(&n);
    let letter = |c: char| -> Option<KeyCode> {
        Some(match c {
            'a' => KeyCode::KeyA,
            'b' => KeyCode::KeyB,
            'c' => KeyCode::KeyC,
            'd' => KeyCode::KeyD,
            'e' => KeyCode::KeyE,
            'f' => KeyCode::KeyF,
            'g' => KeyCode::KeyG,
            'h' => KeyCode::KeyH,
            'i' => KeyCode::KeyI,
            'j' => KeyCode::KeyJ,
            'k' => KeyCode::KeyK,
            'l' => KeyCode::KeyL,
            'm' => KeyCode::KeyM,
            'n' => KeyCode::KeyN,
            'o' => KeyCode::KeyO,
            'p' => KeyCode::KeyP,
            'q' => KeyCode::KeyQ,
            'r' => KeyCode::KeyR,
            's' => KeyCode::KeyS,
            't' => KeyCode::KeyT,
            'u' => KeyCode::KeyU,
            'v' => KeyCode::KeyV,
            'w' => KeyCode::KeyW,
            'x' => KeyCode::KeyX,
            'y' => KeyCode::KeyY,
            'z' => KeyCode::KeyZ,
            '0' => KeyCode::Digit0,
            '1' => KeyCode::Digit1,
            '2' => KeyCode::Digit2,
            '3' => KeyCode::Digit3,
            '4' => KeyCode::Digit4,
            '5' => KeyCode::Digit5,
            '6' => KeyCode::Digit6,
            '7' => KeyCode::Digit7,
            '8' => KeyCode::Digit8,
            '9' => KeyCode::Digit9,
            _ => return None,
        })
    };
    let mut chars = n.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return letter(c);
    }
    if let Some(d) = n.strip_prefix("digit")
        && d.len() == 1
    {
        return letter(d.chars().next()?);
    }
    Some(match n {
        "arrowup" | "up" => KeyCode::ArrowUp,
        "arrowdown" | "down" => KeyCode::ArrowDown,
        "arrowleft" | "left" => KeyCode::ArrowLeft,
        "arrowright" | "right" => KeyCode::ArrowRight,
        "enter" | "return" => KeyCode::Enter,
        "space" => KeyCode::Space,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "shiftleft" => KeyCode::ShiftLeft,
        "shiftright" | "shift" => KeyCode::ShiftRight,
        "controlleft" | "ctrlleft" | "ctrl" | "control" => KeyCode::ControlLeft,
        "controlright" | "ctrlright" => KeyCode::ControlRight,
        "altleft" | "alt" => KeyCode::AltLeft,
        "altright" => KeyCode::AltRight,
        "comma" => KeyCode::Comma,
        "period" => KeyCode::Period,
        "slash" => KeyCode::Slash,
        "semicolon" => KeyCode::Semicolon,
        "quote" => KeyCode::Quote,
        "bracketleft" => KeyCode::BracketLeft,
        "bracketright" => KeyCode::BracketRight,
        "minus" => KeyCode::Minus,
        "equal" => KeyCode::Equal,
        _ => return None,
    })
}

/// Keys held now, from the keyboard events (`ButtonInput` cannot be used:
/// bound keys are reset in it every frame).
#[derive(Resource, Default, Debug)]
pub struct HeldKeys(HashSet<KeyCode>);

/// Drive bound root inputs from the keyboard and take bound keys away from
/// the editor while the simulation runs. In `PreUpdate`, after Bevy's input
/// systems.
pub fn key_binding_system(
    mut events: MessageReader<KeyboardInput>,
    mut lost: MessageReader<KeyboardFocusLost>,
    mut held: ResMut<HeldKeys>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    bridge: Option<Res<SimBridge>>,
    inputs: Query<(Entity, &GateType, &PersistentGateData, &InputDrive), RootItems>,
    mut set: MessageWriter<SetInputValue>,
) {
    for event in events.read() {
        match event.state {
            ButtonState::Pressed => held.0.insert(event.key_code),
            ButtonState::Released => held.0.remove(&event.key_code),
        };
    }
    if lost.read().count() > 0 {
        held.0.clear();
    }
    if !bridge.is_some_and(|b| b.is_running()) {
        return;
    }
    for (entity, ty, data, drive) in &inputs {
        if !matches!(ty.kind, GateKind::Input { .. }) {
            continue;
        }
        let Some(key) = data
            .custom_properties
            .get(KEY_PROPERTY)
            .and_then(|k| key_code(k))
        else {
            continue;
        };
        keys.reset(key);
        let value = u64::from(held.0.contains(&key));
        if drive.value != value {
            set.write(SetInputValue { entity, value });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_code("ArrowUp"), Some(KeyCode::ArrowUp));
        assert_eq!(key_code("up"), Some(KeyCode::ArrowUp));
        assert_eq!(key_code("KeyZ"), Some(KeyCode::KeyZ));
        assert_eq!(key_code("z"), Some(KeyCode::KeyZ));
        assert_eq!(key_code("X"), Some(KeyCode::KeyX));
        assert_eq!(key_code("Enter"), Some(KeyCode::Enter));
        assert_eq!(key_code("shift"), Some(KeyCode::ShiftRight));
        assert_eq!(key_code("Digit1"), Some(KeyCode::Digit1));
        assert_eq!(key_code("1"), Some(KeyCode::Digit1));
        assert_eq!(key_code("keyboard"), None);
        assert_eq!(key_code(""), None);
    }
}
