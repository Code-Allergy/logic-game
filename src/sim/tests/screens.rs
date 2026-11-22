//! The screen part: its kinds in both save formats, an editor-made screen
//! simulating, the memory read back through the probe frame, the texture
//! drawn from it only when it changed, and state kept across edits.

use super::*;
use crate::sim::source;
use crate::sim::visuals::ScreenPicture;
use crate::sim::{GateProbes, ScreenMode, ScreenPalette, SimulationVisualsPlugin};

fn screen(mode: ScreenMode, width: u16, height: u16, palette: ScreenPalette) -> GateKind {
    GateKind::Screen {
        mode,
        width,
        height,
        palette,
        scale: 3,
    }
}

fn memory(app: &mut App, label: &str) -> Option<Vec<u64>> {
    let e = gate(app, label);
    let probe = app.world().get::<GateProbes>(e)?.memory?;
    app.world()
        .resource::<SimBridge>()
        .with_frame(|f| f.mems.get(probe as usize).cloned().flatten())
        .map(|m| m.to_vec())
}

/// Every mode and palette saves and loads as version 5, and reads and
/// prints as version 6 (palette and scale may be left out).
#[test]
fn screens_round_trip_through_both_formats() {
    let mut app = app();
    let mut kinds = Vec::new();
    for (i, mode) in ScreenMode::ALL.into_iter().enumerate() {
        for (j, palette) in ScreenPalette::ALL.into_iter().enumerate() {
            let side = mode.sides()[(i + j) % mode.sides().len()];
            let kind = screen(mode, side, mode.sides()[0], palette);
            assert_eq!(kind.check(), Ok(()), "{kind:?}");
            create_gate(&mut app, kind.clone(), &format!("s{i}{j}"));
            kinds.push(kind);
        }
    }
    let saved = read_back(&mut app);
    let dir = temp_dir();
    save_workspace_in(&dir, "screens", &saved).unwrap();
    let loaded = read_workspace_in(&dir, "screens").unwrap();
    assert_eq!(loaded, saved);
    let root = &loaded.root;
    for kind in &kinds {
        assert!(root.nodes.iter().any(|n| n.kind == *kind), "{kind:?}");
    }
    std::fs::remove_dir_all(dir).ok();

    // Version 6.
    let text = r#"{
  "format": "gatequest-workspace",
  "version": 6,
  "root": "main",
  "circuits": [
    {
      "name": "main",
      "nodes": [
        {"name": "t", "type": "Screen", "mode": "Text", "width": 16, "height": 4, "at": [0, 0]},
        {
          "name": "p",
          "type": "Screen",
          "mode": "Scanline",
          "width": 64,
          "height": 8,
          "palette": "Rgb332",
          "scale": 2,
          "at": [0, 300]
        }
      ],
      "wires": []
    }
  ]
}
"#;
    let file = source::parse(text).unwrap();
    assert_eq!(source::to_text(&file), text, "canonical");
    let expanded = source::expand(&file, "workspaces/screens", &Default::default()).unwrap();
    let kinds: Vec<GateKind> = expanded
        .workspace
        .root
        .nodes
        .iter()
        .map(|n| n.kind.clone())
        .collect();
    assert!(kinds.contains(&GateKind::Screen {
        mode: ScreenMode::Text,
        width: 16,
        height: 4,
        palette: ScreenPalette::Mono,
        scale: 4,
    }));
    assert!(kinds.contains(&GateKind::Screen {
        mode: ScreenMode::Scanline,
        width: 64,
        height: 8,
        palette: ScreenPalette::Rgb332,
        scale: 2,
    }));
    // Bad sides, scales and parameters are refused.
    let refuse = |params: &str| {
        let t = text.replace(r#""mode": "Text", "width": 16"#, params);
        let file = source::parse(&t);
        let err = file
            .and_then(|f| source::expand(&f, "workspaces/bad", &Default::default()).map(|_| ()))
            .unwrap_err();
        err.to_string()
    };
    assert!(refuse(r#""mode": "Text", "width": 12"#).contains("width and height"));
    assert!(refuse(r#""mode": "Text", "width": 128"#).contains("4, 8, 16, 32, 64"));
    assert!(refuse(r#""mode": "Pixel", "width": 4"#).contains("8, 16, 32, 64, 128, 256"));
    assert!(refuse(r#""mode": "Text", "scale": 0, "width": 16"#).contains("scale"));
    assert!(refuse(r#""mode": "Text", "w": 8, "width": 16"#).contains("no parameter \"w\""));
    assert!(refuse(r#""mode": "Lamp", "width": 16"#).contains("Screen"));
}

/// Pins follow the mode; the data width the palette (8 for text).
#[test]
fn screen_pins_follow_mode_and_palette() {
    let pins = |k: &GateKind| k.primitive_pins().unwrap();
    let k = screen(ScreenMode::Pixel, 32, 16, ScreenPalette::Colors16);
    assert_eq!(pins(&k).0.as_slice(), &[5, 4, 4, 1, 1]);
    assert!(pins(&k).1.is_empty());
    assert_eq!(k.input_pin_names(), ["x", "y", "colour", "draw", "clear"]);
    let k = screen(ScreenMode::Framebuffer, 64, 32, ScreenPalette::Rgb332);
    assert_eq!(pins(&k).0.as_slice(), &[11, 1, 8]);
    let k = screen(ScreenMode::Scanline, 8, 8, ScreenPalette::Gray);
    assert_eq!(pins(&k).0.as_slice(), &[2, 1, 1]);
    let k = screen(ScreenMode::Text, 32, 4, ScreenPalette::Mono);
    assert_eq!(pins(&k).0.as_slice(), &[7, 1, 8]);
    assert_eq!(k.input_pin_names(), ["addr", "we", "char"]);
    // The tile is the picture (pixels x scale) plus a bezel, on the grid.
    assert_eq!(
        k.screen_face(),
        Some(Vec2::new(32.0 * 8.0 * 3.0, 4.0 * 8.0 * 3.0))
    );
    let size = k.size();
    assert!(size.x >= 768.0 + 2.0 * crate::sim::SCREEN_BEZEL && size.x % 20.0 == 0.0);
    assert!(size.y >= 96.0 && size.y % 20.0 == 0.0);
    // Switching mode keeps the sides allowed.
    assert_eq!(ScreenMode::Text.fit(256), 64);
    assert_eq!(ScreenMode::Pixel.fit(4), 8);
}

/// A screen built in the editor draws: x, y, colour and draw from inputs.
/// Its memory reaches the app through the probe frame, and survives an
/// unrelated edit (state carry-over), like a RAM.
#[test]
fn editor_made_pixel_screen_draws_and_keeps_its_picture() {
    let mut app = app();
    let s = create_gate(
        &mut app,
        screen(ScreenMode::Pixel, 8, 8, ScreenPalette::Colors16),
        "Screen",
    );
    for (pin, (label, w)) in [("X", 3), ("Y", 3), ("C", 4), ("Draw", 1)]
        .iter()
        .enumerate()
    {
        let input = create_gate(&mut app, GateKind::Input { w: *w }, label);
        create_wire(&mut app, input, 0, s, pin as u8);
    }
    assert!(wires(&mut app).iter().all(|(_, _, st)| st.connected));
    assert_eq!(memory(&mut app, "Screen").map(|m| m.len()), Some(64));
    set_input(&mut app, "X", 5);
    set_input(&mut app, "Y", 2);
    set_input(&mut app, "C", 9);
    set_input(&mut app, "Draw", 1);
    run(&mut app, 3);
    set_input(&mut app, "Draw", 0);
    run(&mut app, 2);
    let m = memory(&mut app, "Screen").unwrap();
    assert_eq!(m[2 * 8 + 5], 9);
    assert_eq!(m.iter().filter(|&&v| v != 0).count(), 1);
    // An unrelated edit recompiles; the picture stays.
    create_gate(&mut app, GateKind::Not { w: 1 }, "unrelated");
    run(&mut app, 2);
    assert_eq!(memory(&mut app, "Screen").unwrap()[2 * 8 + 5], 9);
}

/// The texture of a screen is drawn from its memory, and redrawn only
/// when the memory changed.
#[test]
fn screen_texture_follows_the_memory() {
    let mut app = app();
    app.add_plugins(SimulationVisualsPlugin);
    let s = create_gate(
        &mut app,
        screen(ScreenMode::Framebuffer, 8, 8, ScreenPalette::Mono),
        "Screen",
    );
    let addr = create_gate(&mut app, GateKind::Input { w: 6 }, "Addr");
    let we = create_gate(&mut app, GateKind::Input { w: 1 }, "We");
    let data = create_gate(&mut app, GateKind::Input { w: 1 }, "Data");
    create_wire(&mut app, addr, 0, s, 0);
    create_wire(&mut app, we, 0, s, 1);
    create_wire(&mut app, data, 0, s, 2);
    set_input(&mut app, "Addr", 8 + 3);
    set_input(&mut app, "Data", 1);
    set_input(&mut app, "We", 1);
    run(&mut app, 3);
    set_input(&mut app, "We", 0);
    run(&mut app, 2);
    let picture = |app: &mut App| {
        let mut q = app.world_mut().query::<&ScreenPicture>();
        q.single(app.world()).unwrap().clone()
    };
    let shown = picture(&mut app);
    let handle = shown.image.clone().expect("a texture");
    let texels = |app: &App| {
        app.world()
            .resource::<Assets<Image>>()
            .get(&handle)
            .unwrap()
            .data
            .clone()
            .unwrap()
    };
    let t = texels(&app);
    assert_eq!(t.len(), 8 * 8 * 4);
    let lit = |t: &[u8], x: usize, y: usize| t[(y * 8 + x) * 4 + 3] != 0;
    assert!(lit(&t, 3, 1), "pixel (3, 1) is lit, row 1 from the top");
    assert!(!lit(&t, 0, 0) && !lit(&t, 3, 0));
    // Nothing changes while idle: the same copy stays shown.
    run(&mut app, 5);
    let again = picture(&mut app);
    assert!(Arc::ptr_eq(
        shown.shown.as_ref().unwrap(),
        again.shown.as_ref().unwrap()
    ));
}
