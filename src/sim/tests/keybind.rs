//! Keyboard bindings of root inputs (`sim::keybind`).

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};

use super::*;
use crate::sim::keybind::{HeldKeys, KEY_PROPERTY, key_binding_system};
use crate::sim::source;

const TEXT: &str = r#"{
  "format": "gatequest-workspace",
  "version": 6,
  "root": "main",
  "circuits": [
    {
      "name": "main",
      "nodes": [
        {"name": "jump", "type": "Input", "label": "jump", "key": "KeyZ", "at": [0, 0]},
        {"name": "lamp", "type": "Output", "label": "lamp", "at": [200, 0]}
      ],
      "wires": [["jump", "lamp"]]
    }
  ]
}
"#;

fn keyboard_app() -> App {
    let mut app = app();
    app.add_plugins(bevy::input::InputPlugin)
        .init_resource::<HeldKeys>()
        .add_systems(
            PreUpdate,
            key_binding_system.after(bevy::input::InputSystems),
        );
    let file = source::parse(TEXT).unwrap();
    assert_eq!(
        source::to_text(&file),
        TEXT,
        "the key round-trips in canonical form"
    );
    let expanded = source::expand(&file, "workspaces/keys", &Default::default()).unwrap();
    let jump = expanded
        .workspace
        .root
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("jump"));
    assert_eq!(
        jump.unwrap()
            .properties
            .get(KEY_PROPERTY)
            .map(String::as_str),
        Some("KeyZ"),
        "the key is the node's property"
    );
    load(&mut app, expanded.workspace);
    app
}

fn key(app: &mut App, code: KeyCode, state: ButtonState) {
    let window = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(KeyboardInput {
        key_code: code,
        logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window,
    });
}

fn set_running(app: &mut App, running: bool) {
    app.world_mut()
        .resource_mut::<SimBridge>()
        .set_running(running);
}

#[test]
fn a_held_key_drives_its_input_while_running() {
    let mut app = keyboard_app();
    set_running(&mut app, true);
    key(&mut app, KeyCode::KeyZ, ButtonState::Pressed);
    settle(&mut app);
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "lamp"), 1, "held: the circuit sees 1");
    key(&mut app, KeyCode::KeyZ, ButtonState::Released);
    settle(&mut app);
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "lamp"), 0, "released: 0");
}

#[test]
fn a_bound_key_is_hidden_from_shortcuts_only_while_running() {
    let mut app = keyboard_app();
    set_running(&mut app, true);
    key(&mut app, KeyCode::KeyZ, ButtonState::Pressed);
    key(&mut app, KeyCode::KeyX, ButtonState::Pressed);
    app.update();
    let keys = app.world().resource::<ButtonInput<KeyCode>>();
    assert!(
        !keys.pressed(KeyCode::KeyZ),
        "bound and running: the circuit's"
    );
    assert!(keys.pressed(KeyCode::KeyX), "not bound: the editor's");

    set_running(&mut app, false);
    key(&mut app, KeyCode::KeyZ, ButtonState::Released);
    app.update();
    key(&mut app, KeyCode::KeyZ, ButtonState::Pressed);
    app.update();
    let keys = app.world().resource::<ButtonInput<KeyCode>>();
    assert!(
        keys.just_pressed(KeyCode::KeyZ),
        "paused: the editor's again"
    );
}

#[test]
fn key_on_a_non_input_is_refused() {
    let text = TEXT.replace(
        r#"{"name": "lamp", "type": "Output", "label": "lamp", "at": [200, 0]}"#,
        r#"{"name": "lamp", "type": "Output", "label": "lamp", "key": "KeyX", "at": [200, 0]}"#,
    );
    let e = source::parse(&text).unwrap_err().to_string();
    assert!(e.contains("not one"), "{e}");
    let bad = TEXT.replace("KeyZ", "Hyper");
    let e = source::parse(&bad).unwrap_err().to_string();
    assert!(e.contains("is not a key"), "{e}");
}
