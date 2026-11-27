//! Open a demo or gallery item in the full app, run it for a few ticks and
//! save a screenshot of the window (a development tool for checking the
//! look, e.g. under a virtual framebuffer).
//!
//! ```sh
//! cargo run --example screenshot -- counter out.png            # day
//! cargo run --example screenshot -- counter out.png --night
//! cargo run --example screenshot -- traffic_light out.png --ticks 40 --zoom 0.8
//! cargo run --example screenshot -- counter out.png --size 1280x720 --select Count
//! cargo run --example screenshot -- counter out.png --library   # Library drawer open
//! cargo run --example screenshot -- - map.png --map --opened gates,universal,register
//! cargo run --example screenshot -- cpu out.png --definition alu8   # a definition
//! cargo run --example screenshot -- path/to/file.json out.png       # any version 6 file
//! cargo run --example screenshot -- empty out.png               # a new empty workspace
//! cargo run --example screenshot -- loop out.png                # a build (loop) error
//! ```
//!
//! The first argument is a demo or gallery id, or the path of a version 6
//! file. `--definition NAME` opens the definition of that name (a circuit
//! of the file) instead of the root circuit. `--fit` frames the circuit in
//! the canvas (`--zoom` scales that framing); without `--fit`, `--zoom`
//! is the world units per pixel (0.25 is 400%) and `--at X,Y` (or `--at
//! LABEL`) centres the view on a world point (or a part).
//!
//! `--size WxH` sets the window size (default 1600x1000); `--select LABEL`
//! selects the first part with that label, so the inspector shows it;
//! `--set LABEL=VALUE` drives an input before the ticks run; `--tab NAME`
//! opens a parts-bar tab (`logic`, `memory`, `chips`, ...); `--editable`
//! opens the demo as an editable workspace instead of read-only. The ids
//! `empty` and `loop` are a new empty workspace and a small editable
//! circuit with a combinational loop. `--drag LABEL:DX,DY` left-drags
//! that part by (DX, DY) world units through the real pointer path (the
//! window cursor and the left button) and drops it before the shot.
//! `--wire FROM.PIN:TO.PIN` drags a wire from a pin of the part labelled
//! FROM to a pin of TO the same way (a pin by name, such as `q`, or by
//! number; the side is the one that names it, outputs first); with
//! `--hold` the button stays down, so the shot shows the drag in
//! progress. `--hover LABEL.PIN` points at a pin (its tooltip).
//!
//! `--confirm` shows the unsaved-changes question (as when opening
//! another workspace over unsaved edits).
//!
//! Set `GATEQUEST_DATA_DIR` to a scratch folder so that nothing touches the
//! player's saved workspaces.
//!
//! `--multi LABEL,LABEL,...` selects those parts together (the selection
//! card shows; `kind:NAME` stands for every part of that kind, e.g.
//! `kind:And`); with `--make-chip NAME` they are then made into a chip of
//! that name (the workspace must be editable), and `--open-chip` opens it.
//!
//! `--ui-scale auto|90|100|115|130` sets the UI scale setting (default
//! `auto`, as for a new player; the settings stay in memory).
//!
//! `--map` shows the demo map instead of a demo (the id is ignored), with
//! the demos listed by `--opened ID,ID,...` (the last one last) as the
//! progress, kept in memory only; `--level N` picks a stop;
//! `--completed ID,ID,...` marks challenges done.
//!
//! `--challenge ID` opens challenge ID as the editor would: the first
//! argument `-` is its starter, a file path a circuit to open in its place
//! (a solution, right or wrong). `--check` then presses Check:
//!
//! ```sh
//! cargo run --example screenshot -- - out.png --challenge half_adder
//! cargo run --example screenshot -- wrong.json out.png --challenge half_adder --check
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;
use bevy_egui::EguiPlugin;

use gate_quest::editor::challenge::ActiveChallenge;
use gate_quest::editor::demo_map::{DemoMapState, progress::DemoProgress};
use gate_quest::editor::files::{OpenRequest, PendingOpen};
use gate_quest::editor::{EditorPlugin, EditorState, PartTab};
use gate_quest::settings::{Settings, SettingsPlugin, UiScale};
use gate_quest::sim::art::PinLayout;
use gate_quest::sim::systems::canvas_cursor_system;
use gate_quest::sim::{
    ActiveCircuit, CircuitDocument, CircuitLibrary, DocNode, DocWire, GateIdentity, GateKind,
    GateLabel, GateType, LoadWorkspaceEvent, MakeChip, MultiSelection, OpenDefinitionView,
    OpenInstanceView, SelectedGate, SetInputValue, SimBridge, SimulationPlugin, WorkspaceDocument,
    WorkspaceRevision, demos, source,
};
use gate_quest::temp::GateComponentSelectionEvent;
use gate_quest::theme::{Theme, ThemeMode, ThemePlugin};
use gate_quest::ui_style::WorkshopUiPlugin;
use gate_quest::{
    GameState, MainCamera, UiPointerState, configure_camera_scaling, initial_startup_system,
    update_ui_pointer_state,
};

#[derive(Resource)]
struct Shot {
    id: String,
    out: PathBuf,
    night: bool,
    ticks: u64,
    zoom: Option<f32>,
    at: Option<String>,
    select: Option<String>,
    library: bool,
    set: Vec<(String, u64)>,
    tab: Option<PartTab>,
    map: bool,
    level: Option<u32>,
    definition: Option<String>,
    fit: bool,
    editable: bool,
    multi: Vec<String>,
    make_chip: Option<String>,
    open_chip: bool,
    challenge: Option<String>,
    check: bool,
    drag: Option<(String, Vec2)>,
    wire: Option<(PinSpec, PinSpec)>,
    hold: bool,
    hover: Option<PinSpec>,
    confirm: bool,
    frame: u32,
}

/// A pin named on the command line: `LABEL.PIN`.
#[derive(Clone, Debug)]
struct PinSpec {
    label: String,
    pin: String,
}

fn pin_spec(spec: &str) -> PinSpec {
    let (label, pin) = spec
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("a pin is LABEL.PIN, got {spec}"));
    PinSpec {
        label: label.into(),
        pin: pin.into(),
    }
}

type Circuits = Vec<(String, Arc<CircuitDocument>)>;

/// The workspace and the circuits by name of a demo or gallery id, of a
/// version 6 file, or of the fixtures `empty` / `loop`. The last value says
/// whether it is a fixture (always editable).
fn load(id: &str) -> (String, Arc<WorkspaceDocument>, Circuits, bool) {
    match id {
        "empty" => {
            let ws = WorkspaceDocument::new(CircuitDocument::new("workspace"));
            return ("workspace".into(), Arc::new(ws), Vec::new(), true);
        }
        "loop" => {
            let mut root = CircuitDocument::new("loop");
            let a = DocNode::labeled(GateKind::Input { w: 1 }, Vec2::new(-150.0, 0.0), "A");
            let and = DocNode::labeled(GateKind::And { w: 1 }, Vec2::ZERO, "gate");
            let y = DocNode::labeled(GateKind::Output { w: 1 }, Vec2::new(150.0, 0.0), "Y");
            root.wires = vec![
                DocWire::new(a.id, 0, and.id, 0),
                DocWire::new(and.id, 0, and.id, 1),
                DocWire::new(and.id, 0, y.id, 0),
            ];
            root.nodes = vec![a, and, y];
            let ws = WorkspaceDocument::new(root);
            return ("loop".into(), Arc::new(ws), Vec::new(), true);
        }
        _ => {}
    }
    let catalog = demos::catalog().expect("the demos load");
    let path = Path::new(id);
    if path.is_file() {
        let text = std::fs::read_to_string(path).expect("read the file");
        let file = source::parse(&text).expect("a version 6 file");
        let stem = path
            .file_stem()
            .map_or("file".into(), |s| s.to_string_lossy().into_owned());
        let expansion = source::expand(&file, &format!("screenshot/{stem}"), &catalog.externals())
            .expect("the file expands");
        (
            stem,
            Arc::new(expansion.workspace),
            expansion.circuits,
            false,
        )
    } else {
        let demo = catalog.get(id).expect("a demo or gallery id, or a file");
        (
            demo.title.clone(),
            Arc::clone(&demo.workspace),
            demo.circuits.clone(),
            false,
        )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "counter".into());
    let out = PathBuf::from(args.next().unwrap_or_else(|| "screenshot.png".into()));
    let (mut night, mut ticks, mut zoom, mut at) = (false, 12, None, None);
    let (mut size, mut select, mut library) = ((1600, 1000), None, false);
    let mut set = Vec::new();
    let mut tab = None;
    let (mut map, mut opened, mut level) = (false, String::new(), None);
    let (mut definition, mut fit) = (None, false);
    let mut settings = Settings::in_memory();
    let (mut editable, mut confirm) = (false, false);
    let mut drag = None;
    let (mut wire, mut hold, mut hover) = (None, false, None);
    let (mut challenge, mut check, mut completed) = (None, false, String::new());
    let (mut multi, mut make_chip, mut open_chip) = (Vec::new(), None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--night" => night = true,
            "--map" => map = true,
            "--opened" => opened = args.next().unwrap_or_default(),
            "--level" => level = args.next().and_then(|l| l.parse().ok()),
            "--library" => library = true,
            "--editable" => editable = true,
            "--confirm" => confirm = true,
            "--select" => select = args.next(),
            "--tab" => {
                let name = args.next().unwrap_or_default().to_lowercase();
                tab = PartTab::ALL
                    .into_iter()
                    .find(|t| t.title().to_lowercase().starts_with(&name));
            }
            "--set" => {
                let spec = args.next().unwrap_or_default();
                let parsed = spec
                    .split_once('=')
                    .and_then(|(l, v)| Some((l.to_string(), v.parse().ok()?)));
                set.push(parsed.unwrap_or_else(|| panic!("--set wants LABEL=VALUE, got {spec}")));
            }
            "--size" => {
                let spec = args.next().unwrap_or_default();
                let parsed = spec
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                size = parsed.unwrap_or_else(|| panic!("--size wants WxH, got {spec}"));
            }
            "--ticks" => ticks = args.next().and_then(|t| t.parse().ok()).unwrap_or(ticks),
            "--zoom" => zoom = args.next().and_then(|z| z.parse().ok()),
            "--at" => at = args.next(),
            "--definition" => definition = args.next(),
            "--fit" => fit = true,
            "--ui-scale" => {
                let spec = args.next().unwrap_or_default();
                settings.ui_scale = UiScale::choices()
                    .find(|c| c.label().trim_end_matches('%').eq_ignore_ascii_case(&spec))
                    .unwrap_or_else(|| {
                        panic!("--ui-scale wants auto, 90, 100, 115 or 130, got {spec}")
                    });
            }
            "--multi" => {
                multi = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_owned)
                    .collect();
            }
            "--make-chip" => make_chip = args.next(),
            "--open-chip" => open_chip = true,
            "--challenge" => challenge = args.next(),
            "--check" => check = true,
            "--completed" => completed = args.next().unwrap_or_default(),
            "--drag" => {
                let spec = args.next().unwrap_or_default();
                let parsed = spec.rsplit_once(':').and_then(|(l, d)| {
                    let (x, y) = d.split_once(',')?;
                    Some((l.to_string(), Vec2::new(x.parse().ok()?, y.parse().ok()?)))
                });
                drag =
                    Some(parsed.unwrap_or_else(|| panic!("--drag wants LABEL:DX,DY, got {spec}")));
            }
            "--wire" => {
                let spec = args.next().unwrap_or_default();
                let (from, to) = spec
                    .split_once(':')
                    .unwrap_or_else(|| panic!("--wire wants FROM.PIN:TO.PIN, got {spec}"));
                wire = Some((pin_spec(from), pin_spec(to)));
            }
            "--hold" => hold = true,
            "--hover" => hover = Some(pin_spec(&args.next().unwrap_or_default())),
            other => panic!("unknown argument {other}"),
        }
    }
    let mut progress = DemoProgress::with_opened(opened.split(',').filter(|id| !id.is_empty()));
    for id in completed.split(',').filter(|id| !id.is_empty()) {
        progress.mark_completed(id);
    }
    App::new()
        .insert_resource(progress)
        .insert_resource(settings)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(size.0, size.1),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            EguiPlugin::default(),
            WorkshopUiPlugin,
            ThemePlugin,
            SettingsPlugin,
        ))
        .add_plugins(SimulationPlugin::new(GameState::Simulating))
        .add_plugins(EditorPlugin)
        .insert_state(if map {
            GameState::DemoMap
        } else {
            GameState::Simulating
        })
        .insert_resource(UiPointerState::default())
        .insert_resource(ClearColor(Theme::default().palette.bench))
        .add_message::<GateComponentSelectionEvent>()
        .insert_resource(Shot {
            id,
            out,
            night,
            ticks,
            zoom,
            at,
            select,
            library,
            set,
            tab,
            map,
            level,
            definition,
            fit,
            editable,
            multi,
            make_chip,
            open_chip,
            challenge,
            check,
            drag,
            wire,
            hold,
            hover,
            confirm,
            frame: 0,
        })
        .add_systems(
            Startup,
            (initial_startup_system, configure_camera_scaling).chain(),
        )
        .add_systems(bevy_egui::EguiPrimaryContextPass, update_ui_pointer_state)
        .add_systems(Update, drive)
        .add_systems(
            Update,
            (scripted_drag, scripted_wire).before(canvas_cursor_system),
        )
        .run();
}

/// What `--multi`, `--make-chip` and `--open-chip` use.
#[derive(bevy::ecs::system::SystemParam)]
struct ChipShot<'w, 's> {
    group: ResMut<'w, MultiSelection>,
    ids: Query<'w, 's, (Entity, &'static GateIdentity, &'static GateType)>,
    make: MessageWriter<'w, MakeChip>,
    open_instance: MessageWriter<'w, OpenInstanceView>,
}

/// The map, the open circuit and challenge, the progress, the chip
/// parameters (grouped: a
/// system takes at most 16 parameters).
type MoreState<'w, 's> = (
    ResMut<'w, DemoMapState>,
    ResMut<'w, ActiveCircuit>,
    ResMut<'w, ActiveChallenge>,
    ResMut<'w, DemoProgress>,
    Res<'w, WorkspaceRevision>,
    Local<'s, Option<Arc<WorkspaceDocument>>>,
    ChipShot<'w, 's>,
);

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    mut shot: ResMut<Shot>,
    mut selected: ResMut<SelectedGate>,
    mut editor: ResMut<EditorState>,
    labels: Query<(Entity, &GateLabel)>,
    mut inputs: MessageWriter<SetInputValue>,
    mut theme: ResMut<Theme>,
    bridge: Res<SimBridge>,
    mut load_workspace: MessageWriter<LoadWorkspaceEvent>,
    mut open: MessageWriter<OpenDefinitionView>,
    mut circuits: Local<Circuits>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    gates: Query<&Transform, (With<GateIdentity>, Without<MainCamera>)>,
    windows: Query<&Window>,
    mut exit: MessageWriter<AppExit>,
    // Grouped: a system takes at most 16 parameters.
    (mut map, mut active, mut open_challenge, mut progress, revision, mut loaded, mut chips): MoreState,
) {
    let ChipShot {
        group,
        ids,
        make,
        open_instance,
    } = &mut chips;
    shot.frame += 1;
    if shot.map {
        match shot.frame {
            5 if shot.night => theme.set(ThemeMode::Night),
            30 => {
                if let Some(level) = shot.level {
                    map.level = Some(level);
                    map.demo = None;
                }
            }
            70 => {
                commands
                    .spawn(Screenshot::primary_window())
                    .observe(save_to_disk(shot.out.clone()));
            }
            100 => {
                exit.write(AppExit::Success);
            }
            _ => {}
        }
        return;
    }
    match shot.frame {
        5 if shot.challenge.is_some() => {
            let catalog = demos::catalog().expect("the demos load");
            let id = shot.challenge.as_deref().unwrap_or_default();
            let challenge = catalog.challenge(id).expect("a challenge id");
            let name = challenge.workspace_name();
            let workspace = if shot.id == "-" {
                Arc::new(demos::editable_copy(&challenge.starter, &name))
            } else {
                let (_, workspace, all, _) = load(&shot.id);
                *circuits = all;
                let mut ws = (*workspace).clone();
                ws.root.name = name.clone();
                Arc::new(ws)
            };
            *loaded = Some(Arc::clone(&workspace));
            load_workspace.write(LoadWorkspaceEvent {
                name,
                workspace,
                read_only: false,
                unsaved: false,
            });
            if shot.night {
                theme.set(ThemeMode::Night);
            }
        }
        5 => {
            let (name, workspace, all, fixture) = load(&shot.id);
            *circuits = all;
            load_workspace.write(LoadWorkspaceEvent {
                name,
                workspace,
                read_only: !(fixture || shot.editable),
                unsaved: false,
            });
            if shot.night {
                theme.set(ThemeMode::Night);
            }
        }
        15 => {
            for (label, value) in &shot.set {
                for (entity, l) in &labels {
                    if &l.value == label {
                        inputs.write(SetInputValue {
                            entity,
                            value: *value,
                        });
                    }
                }
            }
        }
        20 => bridge.step(shot.ticks),
        30 => {
            if let Some(label) = &shot.select {
                let found = labels.iter().find(|(_, l)| &l.value == label);
                selected.0 = Some(found.expect("a part with that label").0);
            }
            editor.library_open = shot.library;
            if shot.confirm {
                editor.pending_open = Some(PendingOpen {
                    request: OpenRequest::New,
                    awaiting_save: false,
                    error: None,
                });
            }
            if let Some(tab) = shot.tab {
                editor.part_tab = tab;
            }
            if let Some(name) = &shot.definition {
                let (_, doc) = circuits
                    .iter()
                    .find(|(n, _)| n == name)
                    .unwrap_or_else(|| panic!("no circuit {name}"));
                open.write(OpenDefinitionView { definition: doc.id });
            }
            let mut primary = None;
            for token in &shot.multi {
                let found: Vec<Entity> = match token.strip_prefix("kind:") {
                    Some(kind) => ids
                        .iter()
                        .filter(|(_, _, t)| t.kind.display_name() == kind)
                        .map(|(e, ..)| e)
                        .collect(),
                    None => labels
                        .iter()
                        .filter(|(_, l)| &l.value == token)
                        .map(|(e, _)| e)
                        .take(1)
                        .collect(),
                };
                assert!(!found.is_empty(), "no part {token}");
                for entity in found {
                    primary = group.toggle(entity, primary);
                }
            }
            if primary.is_some() {
                selected.0 = primary;
            }
        }
        34 => {
            if let Some(name) = &shot.make_chip {
                make.write(MakeChip {
                    gates: group
                        .entities()
                        .iter()
                        .filter_map(|e| ids.get(*e).ok())
                        .map(|(_, id, _)| id.uuid)
                        .collect(),
                    name: name.clone(),
                });
            }
        }
        37 => {
            if shot.open_chip
                && let Some(entity) = selected.0
            {
                open_instance.write(OpenInstanceView { entity });
            }
        }
        40 => {
            let Ok((mut projection, mut camera)) = cameras.single_mut() else {
                return;
            };
            let Projection::Orthographic(ortho) = projection.as_mut() else {
                return;
            };
            if shot.fit
                && let Ok(window) = windows.single()
                && !gates.is_empty()
            {
                // The HUD cards float over the canvas: leave a margin for them.
                let (lo, hi) = gates.iter().fold(
                    (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
                    |(lo, hi), t| {
                        (
                            lo.min(t.translation.truncate()),
                            hi.max(t.translation.truncate()),
                        )
                    },
                );
                let (lo, hi) = (lo - Vec2::splat(60.0), hi + Vec2::splat(60.0));
                let panel = 0.0;
                let room = Vec2::new(window.width() - panel - 200.0, window.height() - 260.0);
                let scale = ((hi - lo) / room).max_element() * shot.zoom.unwrap_or(1.0);
                ortho.scale = scale;
                let centre = (lo + hi) * 0.5;
                camera.translation.x = centre.x - panel * 0.5 * scale;
                camera.translation.y = centre.y + 40.0 * scale;
            } else if let Some(zoom) = shot.zoom {
                ortho.scale = zoom;
            }
            if let Some(spec) = &shot.at {
                let point = spec
                    .split_once(',')
                    .and_then(|(x, y)| Some(Vec2::new(x.parse().ok()?, y.parse().ok()?)));
                let at = point.unwrap_or_else(|| {
                    let (entity, _) = labels
                        .iter()
                        .find(|(_, l)| &l.value == spec)
                        .unwrap_or_else(|| panic!("--at wants X,Y or a label, got {spec}"));
                    gates.get(entity).expect("a part").translation.truncate()
                });
                camera.translation.x = at.x;
                camera.translation.y = at.y;
            }
        }
        50 if shot.check => {
            // As the card's Check button: the circuit that was loaded.
            if let (Some(challenge), Some(ws)) = (open_challenge.challenge(), loaded.as_ref()) {
                let report = challenge.check(ws);
                if report.passed() {
                    progress.mark_completed(&challenge.id);
                }
                println!("{}: {}", challenge.id, report.summary());
                open_challenge.report = Some((report, revision.current));
            }
        }
        70 => {
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(shot.out.clone()));
        }
        100 => {
            // An editable workspace is saved on exit: a screenshot must not
            // write the player's workspaces.
            active.read_only = true;
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

/// `--drag`: point at the part (frame 44), press the left button (45),
/// move in steps (46-55) and release (56), before the shot at frame 70.
fn scripted_drag(
    shot: Res<Shot>,
    mut windows: Query<&mut Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    parts: Query<(&GateLabel, &Transform)>,
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut from: Local<Option<Vec2>>,
) {
    let Some((label, delta)) = &shot.drag else {
        return;
    };
    let (Ok(mut window), Ok((camera, camera_transform))) = (windows.single_mut(), cameras.single())
    else {
        return;
    };
    let to_screen = |world: Vec2| {
        camera
            .world_to_viewport(camera_transform, world.extend(0.0))
            .ok()
    };
    match shot.frame {
        44 => {
            let (_, t) = parts
                .iter()
                .find(|(l, _)| &l.value == label)
                .expect("a part with the --drag label");
            // A little off centre, as a hand would grab it.
            *from = Some(t.translation.truncate() + Vec2::new(4.0, -3.0));
            window.set_cursor_position(from.and_then(to_screen));
        }
        45 => buttons.press(MouseButton::Left),
        46..=55 => {
            let step = (shot.frame - 45) as f32 / 10.0;
            let at = from.map(|f| f + *delta * step);
            window.set_cursor_position(at.and_then(to_screen));
        }
        56 => buttons.release(MouseButton::Left),
        _ => {}
    }
}

/// World position of a pin named on the command line.
fn find_pin(
    spec: &PinSpec,
    parts: &Query<(&GateLabel, &Transform, &GateType, &PinLayout)>,
    library: &CircuitLibrary,
) -> Vec2 {
    let (_, t, ty, layout) = parts
        .iter()
        .find(|(l, ..)| l.value == spec.label)
        .unwrap_or_else(|| panic!("no part labelled {}", spec.label));
    let (ins, outs) = library.pin_names(&ty.kind);
    let named = |names: &[String]| {
        names
            .iter()
            .position(|n| n == &spec.pin)
            .or_else(|| spec.pin.parse().ok().filter(|&i: &usize| i < names.len()))
    };
    let offset = match (named(&outs), named(&ins)) {
        (Some(i), _) => layout.output(i as u8),
        (None, Some(i)) => layout.input(i as u8),
        _ => panic!(
            "{} has no pin {} ({ins:?} / {outs:?})",
            spec.label, spec.pin
        ),
    };
    t.translation.truncate() + offset
}

/// `--wire` and `--hover`: point at the first pin (frame 44), press the
/// left button (45), move to the second pin in steps (46-55) and release
/// (56) unless `--hold`, before the shot at frame 70. `--hover` only
/// points.
fn scripted_wire(
    shot: Res<Shot>,
    mut windows: Query<&mut Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    parts: Query<(&GateLabel, &Transform, &GateType, &PinLayout)>,
    library: Res<CircuitLibrary>,
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut ends: Local<Option<(Vec2, Vec2)>>,
) {
    if shot.wire.is_none() && shot.hover.is_none() {
        return;
    }
    let (Ok(mut window), Ok((camera, camera_transform))) = (windows.single_mut(), cameras.single())
    else {
        return;
    };
    let to_screen = |world: Vec2| {
        camera
            .world_to_viewport(camera_transform, world.extend(0.0))
            .ok()
    };
    if let Some(spec) = &shot.hover {
        if shot.frame == 44 {
            let at = find_pin(spec, &parts, &library) + Vec2::new(1.0, -1.0);
            window.set_cursor_position(to_screen(at));
        }
        return;
    }
    let Some((from, to)) = &shot.wire else {
        return;
    };
    match shot.frame {
        // Pointed at again as the button goes down, in case the view
        // settled in between.
        44 | 45 => {
            let a = find_pin(from, &parts, &library);
            let b = find_pin(to, &parts, &library);
            *ends = Some((a, b));
            window.set_cursor_position(to_screen(a));
            if shot.frame == 45 {
                buttons.press(MouseButton::Left);
            }
        }
        46..=55 => {
            let step = (shot.frame - 45) as f32 / 10.0;
            let at = ends.map(|(a, b)| a.lerp(b, step) + Vec2::new(1.0, -1.0) * step);
            window.set_cursor_position(at.and_then(to_screen));
        }
        56 if !shot.hold => buttons.release(MouseButton::Left),
        _ => {}
    }
}
