//! The app side of the MCP server: the open workspace of a running app
//! (the game, or the same app with no window) served to a
//! [`Session`](super::session::Session) on another thread
//! (`docs/mcp-server.md`, "How an edit reaches the editor").
//!
//! [`link`] makes the two ends: a [`ChannelLink`] for the session and an
//! [`McpInbox`] for the app. [`McpAppPlugin`] adds one exclusive system,
//! in `First`, that answers the requests: so a request runs between
//! frames and never interleaves with the editor's own systems. While a
//! gesture is in progress (a mouse button is down, or a text field holds
//! an edit, [`UndoHistory::hold`]) requests wait.
//!
//! An edit arrives as a whole workspace document and is written back by
//! difference, as undo restores a snapshot
//! ([`HistoryWorld::restore`]): parts that did not change keep their
//! entities and their simulator state. The revision moves on, so the
//! history records the edit as one undo step in the same frame.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::session::{AppLink, AppWorkspace};
use crate::editor::files::{OpenRequest, open_now};
use crate::sim::history::{HistoryWorld, Snapshot};
use crate::sim::source::parse_int_text;
use crate::sim::{
    ActiveCircuit, AuthoringFeedback, CircuitReader, CircuitView, GateKind, GateLabel, GateType,
    GateValues, InputDrive, LoadWorkspaceEvent, RootItems, SaveWorkspaceEvent, SelectedGate,
    SetInputValue, SimBridge, SimDiagnostics, SimulationConfig, UndoHistory, WorkspaceDocument,
    WorkspaceRevision, WorkspaceTests,
};
use crate::{GameState, sim::CircuitLibrary};

type Reply<T> = Sender<Result<T, String>>;

/// A request from the session to the app.
pub enum AppRequest {
    Read(Reply<AppWorkspace>),
    Commit {
        workspace: Box<WorkspaceDocument>,
        stamp: u64,
        reply: Reply<()>,
    },
    Open {
        name: String,
        workspace: Box<WorkspaceDocument>,
        unsaved: bool,
        reply: Reply<()>,
    },
    Save {
        name: Option<String>,
        reply: Reply<String>,
    },
    Transport {
        command: Map<String, Value>,
        reply: Reply<String>,
    },
    State(Reply<String>),
}

/// How long a call waits for the app (it answers between frames; a long
/// gesture, or a window that is not being drawn, holds it up).
pub const APP_TIMEOUT: Duration = Duration::from_secs(30);

/// The session's end: each call sends a request and waits for the app.
pub struct ChannelLink {
    requests: Sender<AppRequest>,
    pub timeout: Duration,
}

impl ChannelLink {
    fn call<T>(&self, request: impl FnOnce(Reply<T>) -> AppRequest) -> Result<T, String> {
        let (reply, answer) = channel();
        self.requests
            .send(request(reply))
            .map_err(|_| "the game is not running any more".to_string())?;
        match answer.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(_) => Err(format!(
                "the game did not answer in {} s (a drag or a text edit in progress holds \
                 requests back; so does a window that is not being drawn)",
                self.timeout.as_secs()
            )),
        }
    }
}

impl AppLink for ChannelLink {
    fn read(&mut self) -> Result<AppWorkspace, String> {
        self.call(AppRequest::Read)
    }

    fn commit(&mut self, workspace: WorkspaceDocument, stamp: u64) -> Result<(), String> {
        self.call(|reply| AppRequest::Commit {
            workspace: Box::new(workspace),
            stamp,
            reply,
        })
    }

    fn open(
        &mut self,
        name: String,
        workspace: WorkspaceDocument,
        unsaved: bool,
    ) -> Result<(), String> {
        self.call(|reply| AppRequest::Open {
            name,
            workspace: Box::new(workspace),
            unsaved,
            reply,
        })
    }

    fn save(&mut self, name: Option<String>) -> Result<String, String> {
        self.call(|reply| AppRequest::Save { name, reply })
    }

    fn transport(&mut self, command: Map<String, Value>) -> Result<String, String> {
        self.call(|reply| AppRequest::Transport { command, reply })
    }

    fn state(&mut self) -> Result<String, String> {
        self.call(AppRequest::State)
    }
}

/// A save the editor was asked for, waiting for its outcome.
struct PendingSave {
    reply: Reply<String>,
    /// The name asked for, when it is a Save As.
    requested: Option<String>,
    frames: u8,
}

/// A `step` of the transport, waiting for the simulation to get there.
struct PendingStep {
    reply: Reply<String>,
    /// The tick the step ends at.
    target: u64,
    frames: u16,
}

/// The app's end: the requests that came in and are waiting.
#[derive(Resource)]
pub struct McpInbox {
    requests: Mutex<Receiver<AppRequest>>,
    waiting: VecDeque<AppRequest>,
    saving: Option<PendingSave>,
    stepping: Option<PendingStep>,
    /// Chips that came through this link (an edit, an opened workspace).
    /// The editor's library also keeps the chips of workspaces opened
    /// earlier; of the chips the root does not reach, only these are the
    /// session's.
    chips: HashSet<Uuid>,
    /// Frames to let pass before the next request (after a load).
    settle: u8,
}

/// The two ends of a link between a session and an app.
pub fn link() -> (ChannelLink, McpInbox) {
    let (requests, inbox) = channel();
    (
        ChannelLink {
            requests,
            timeout: APP_TIMEOUT,
        },
        McpInbox {
            requests: Mutex::new(inbox),
            waiting: VecDeque::new(),
            saving: None,
            stepping: None,
            chips: HashSet::new(),
            settle: 0,
        },
    )
}

/// Serves the requests of an [`McpInbox`] in the app it is added to.
pub struct McpAppPlugin {
    inbox: Mutex<Option<McpInbox>>,
}

impl McpAppPlugin {
    pub fn new(inbox: McpInbox) -> Self {
        Self {
            inbox: Mutex::new(Some(inbox)),
        }
    }
}

impl Plugin for McpAppPlugin {
    fn build(&self, app: &mut App) {
        if let Some(inbox) = self.inbox.lock().ok().and_then(|mut i| i.take()) {
            app.insert_resource(inbox);
        }
        app.add_systems(First, serve_requests);
    }
}

/// Changes whenever the open circuit is edited or replaced.
fn stamp(world: &World) -> u64 {
    let revision = world
        .get_resource::<WorkspaceRevision>()
        .map_or(0, |r| r.current);
    let circuit = world
        .get_resource::<ActiveCircuit>()
        .map_or(0, |a| a.uuid.as_u128() as u64);
    revision.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ circuit
}

fn gesture(world: &World) -> bool {
    world
        .get_resource::<ButtonInput<MouseButton>>()
        .is_some_and(|m| m.get_pressed().next().is_some())
        || world.get_resource::<UndoHistory>().is_some_and(|h| h.hold)
}

/// Answer the waiting requests (see the module docs).
pub fn serve_requests(world: &mut World) {
    let Some(mut inbox) = world.remove_resource::<McpInbox>() else {
        return;
    };
    if let Ok(requests) = inbox.requests.lock() {
        inbox.waiting.extend(requests.try_iter());
    }
    finish_save(world, &mut inbox);
    finish_step(world, &mut inbox);
    if inbox.settle > 0 {
        inbox.settle -= 1;
    } else if !gesture(world) {
        while inbox.saving.is_none() && inbox.stepping.is_none() && inbox.settle == 0 {
            let Some(request) = inbox.waiting.pop_front() else {
                break;
            };
            serve(world, &mut inbox, request);
        }
    }
    world.insert_resource(inbox);
}

fn serve(world: &mut World, inbox: &mut McpInbox, request: AppRequest) {
    let failed = |e: bevy::ecs::system::RunSystemError| format!("the game could not do it: {e}");
    match request {
        AppRequest::Read(reply) => {
            let _ = reply.send(read(world, &inbox.chips));
        }
        AppRequest::Commit {
            workspace,
            stamp: at,
            reply,
        } => {
            let result = if stamp(world) != at {
                Err(
                    "the circuit was changed in the editor while this edit was being made; \
                     nothing was changed, try again"
                        .to_string(),
                )
            } else {
                inbox.chips.extend(workspace.definitions.keys().copied());
                let mut workspace = Some(*workspace);
                world
                    .run_system_once(
                        move |active: Res<ActiveCircuit>,
                              mut history: HistoryWorld,
                              mut revision: ResMut<WorkspaceRevision>|
                              -> Result<(), String> {
                            let Some(ws) = workspace.take() else {
                                return Ok(());
                            };
                            if active.read_only {
                                return Err(crate::sim::READ_ONLY_MESSAGE.to_string());
                            }
                            // The tests go in with the circuit, so that an
                            // edit of only them is a step of the history
                            // too and the workspace stays unsaved.
                            let tests = ws.kept_tests();
                            let target =
                                Snapshot::of(ws.root, ws.definitions, tests, revision.current);
                            history.restore(&target);
                            revision.mark_dirty();
                            Ok(())
                        },
                    )
                    .map_err(failed)
                    .and_then(|r| r)
            };
            let _ = reply.send(result);
        }
        AppRequest::Open {
            name,
            workspace,
            unsaved,
            reply,
        } => {
            inbox.chips = workspace.definitions.keys().copied().collect();
            let mut request = Some(OpenRequest::Workspace {
                name,
                workspace: Arc::new(*workspace),
                read_only: false,
                unsaved,
                status: Some("Opened through the MCP server.".to_string()),
            });
            let result = world
                .run_system_once(
                    move |mut load: MessageWriter<LoadWorkspaceEvent>,
                          mut editor: Option<ResMut<crate::editor::EditorState>>,
                          state: Option<Res<State<GameState>>>,
                          next: Option<ResMut<NextState<GameState>>>| {
                        if let Some(request) = request.take() {
                            open_now(editor.as_deref_mut(), &mut load, request);
                        }
                        // The editor, if the game was on another screen.
                        if let (Some(state), Some(mut next)) = (state, next)
                            && *state.get() != GameState::Simulating
                        {
                            next.set(GameState::Simulating);
                        }
                    },
                )
                .map_err(failed);
            // The load is applied in this frame's update; the next
            // request sees it.
            inbox.settle = 2;
            let _ = reply.send(result);
        }
        AppRequest::Save { name, reply } => {
            let requested = name.clone();
            let sent = world
                .run_system_once(
                    move |active: Res<ActiveCircuit>,
                          mut feedback: ResMut<AuthoringFeedback>,
                          mut save: MessageWriter<SaveWorkspaceEvent>| {
                        feedback.message = None;
                        save.write(SaveWorkspaceEvent {
                            name: name.clone().unwrap_or_else(|| active.name.clone()),
                        });
                    },
                )
                .map_err(failed);
            // The editor's save system answers the message in this
            // frame's update. An app without the editor (no window) has
            // no such system: run it here.
            let sent = sent.and_then(|()| {
                if world.contains_resource::<State<GameState>>() {
                    Ok(())
                } else {
                    world
                        .run_system_once(crate::sim::systems::workspace_io_system)
                        .map_err(failed)
                }
            });
            match sent {
                Ok(()) => {
                    inbox.saving = Some(PendingSave {
                        reply,
                        requested,
                        frames: 3,
                    });
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            }
        }
        AppRequest::Transport { command, reply } => {
            let stepping = command.get("action").and_then(Value::as_str) == Some("step");
            let before = world.get_resource::<SimBridge>().map(SimBridge::tick_count);
            match (transport(world, &command), before) {
                // A step is done by the simulation's own thread: answer
                // when it has got there.
                (Ok(_), Some(before)) if stepping => {
                    let ticks = command.get("ticks").and_then(Value::as_u64).unwrap_or(1);
                    inbox.stepping = Some(PendingStep {
                        reply,
                        target: before + ticks,
                        frames: 600,
                    });
                }
                (result, _) => {
                    let _ = reply.send(result);
                }
            }
        }
        AppRequest::State(reply) => {
            let _ = reply.send(Ok(state(world)));
        }
    }
}

/// Answer a step once the simulation has run it.
fn finish_step(world: &mut World, inbox: &mut McpInbox) {
    let Some(pending) = &mut inbox.stepping else {
        return;
    };
    let (Some(bridge), Some(config)) = (
        world.get_resource::<SimBridge>(),
        world.get_resource::<SimulationConfig>(),
    ) else {
        return;
    };
    let tick = bridge.tick_count();
    if tick < pending.target && pending.frames > 0 {
        pending.frames -= 1;
        return;
    }
    let text = format!(
        "{}, tick {tick}, rate {}{}\n",
        if bridge.is_running() {
            "running"
        } else {
            "paused"
        },
        rate_text(config),
        if tick < pending.target {
            " (the step is still running)"
        } else {
            ""
        }
    );
    let pending = inbox.stepping.take().expect("checked above");
    let _ = pending.reply.send(Ok(text));
}

/// Answer a save once the editor has done it (or refused).
fn finish_save(world: &mut World, inbox: &mut McpInbox) {
    let Some(pending) = &mut inbox.saving else {
        return;
    };
    let message = world
        .get_resource::<AuthoringFeedback>()
        .and_then(|f| f.message.clone());
    if message.is_none() && pending.frames > 0 {
        pending.frames -= 1;
        return;
    }
    let pending = inbox.saving.take().expect("checked above");
    let dirty = world
        .get_resource::<WorkspaceRevision>()
        .is_some_and(WorkspaceRevision::is_dirty);
    let name = world
        .get_resource::<ActiveCircuit>()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let renamed = pending
        .requested
        .as_ref()
        .is_none_or(|r| r.trim().eq_ignore_ascii_case(name.trim()));
    let result = match message {
        Some(message) if !dirty && renamed => Ok(message),
        Some(message) => Err(message),
        None if !dirty && renamed => Ok(format!("saved \"{name}\"")),
        None => Err("the editor did not save (is this the web build?)".to_string()),
    };
    let _ = pending.reply.send(result);
}

fn read(world: &mut World, chips: &HashSet<Uuid>) -> Result<AppWorkspace, String> {
    let stamp = stamp(world);
    let chips = chips.clone();
    world
        .run_system_once(
            move |reader: CircuitReader,
                  library: Res<CircuitLibrary>,
                  revision: Res<WorkspaceRevision>,
                  tests: Option<Res<WorkspaceTests>>| {
                // The root, the chips it reaches, and the session's chips
                // that are not placed yet.
                let root = reader.root_document();
                let mut definitions = library.reachable_from(&root);
                for id in &chips {
                    if let Some(def) = library.get(*id) {
                        definitions.insert(*id, Arc::clone(def));
                        definitions.extend(library.reachable_from(def));
                    }
                }
                definitions.remove(&root.id);
                let mut workspace = WorkspaceDocument {
                    root,
                    definitions,
                    tests: tests.map(|t| t.0.clone()).unwrap_or_default(),
                };
                workspace.tests = workspace.kept_tests();
                AppWorkspace {
                    name: reader.active.name.clone(),
                    workspace,
                    unsaved: !reader.active.read_only && revision.is_dirty(),
                    read_only: reader.active.read_only,
                    stamp,
                }
            },
        )
        .map_err(|e| format!("the game could not read its circuit: {e}"))
}

fn rate_text(config: &SimulationConfig) -> String {
    if config.unlimited_tick_rate {
        "max".to_string()
    } else {
        format!("{} Hz", config.tick_rate)
    }
}

/// The `transport` tool on the simulation the editor shows.
fn transport(world: &mut World, command: &Map<String, Value>) -> Result<String, String> {
    let number = |key: &str| -> Result<Option<u64>, String> {
        match command.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => n
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("\"{key}\" must be a whole number")),
            Some(Value::String(s)) => parse_int_text(s)
                .map(Some)
                .ok_or_else(|| format!("\"{key}\": \"{s}\" is not a number")),
            Some(_) => Err(format!("\"{key}\" must be a number")),
        }
    };
    if let Some(k) = command
        .keys()
        .find(|k| !matches!(k.as_str(), "action" | "ticks" | "hz" | "inputs"))
    {
        return Err(format!(
            "unknown field \"{k}\" (transport has action, ticks, hz, inputs)"
        ));
    }
    let action = command
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .to_string();
    let ticks = number("ticks")?.unwrap_or(1);
    let hz = match command.get("hz") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "max" => Some(None),
        Some(Value::Number(n)) if n.as_f64().is_some_and(|v| v > 0.0) => Some(n.as_f64()),
        Some(_) => return Err("\"hz\" is a rate above 0, or \"max\"".into()),
    };
    let inputs: Vec<(String, u64)> = match command.get("inputs") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| match v {
                Value::Number(n) => n
                    .as_u64()
                    .map(|v| (k.clone(), v))
                    .ok_or_else(|| format!("input \"{k}\": {n} is not a value")),
                Value::String(s) => parse_int_text(s)
                    .map(|v| (k.clone(), v))
                    .ok_or_else(|| format!("input \"{k}\": \"{s}\" is not a value")),
                other => Err(format!("input \"{k}\": {other} is not a value")),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("\"inputs\" takes {label: value, ...}".into()),
    };
    world
        .run_system_once(
            move |mut bridge: ResMut<SimBridge>,
                  mut config: ResMut<SimulationConfig>,
                  gates: Query<(Entity, &GateLabel, &GateType), RootItems>,
                  mut set: MessageWriter<SetInputValue>|
                  -> Result<String, String> {
                // Inputs first, so a step in the same call sees them.
                for (label, value) in &inputs {
                    let found: Vec<(Entity, u8)> = gates
                        .iter()
                        .filter_map(|(e, l, t)| match t.kind {
                            GateKind::Input { w } if l.value == *label => Some((e, w)),
                            _ => None,
                        })
                        .collect();
                    let [(entity, w)] = found.as_slice() else {
                        return Err(format!(
                            "{} inputs of the root circuit are labelled \"{label}\"",
                            found.len()
                        ));
                    };
                    if *w < 64 && *value >> *w != 0 {
                        return Err(format!("{value} does not fit input \"{label}\" ({w} bits)"));
                    }
                    set.write(SetInputValue {
                        entity: *entity,
                        value: *value,
                    });
                    // The simulation gets it now; the message keeps the
                    // part's own record in step.
                    bridge.set_input(*entity, *value);
                }
                if let Some(rate) = hz {
                    match rate {
                        Some(hz) => {
                            config.unlimited_tick_rate = false;
                            config.tick_rate = hz;
                        }
                        None => config.unlimited_tick_rate = true,
                    }
                }
                match action.as_str() {
                    "run" => bridge.set_running(true),
                    "pause" => bridge.set_running(false),
                    "step" => {
                        bridge.set_running(false);
                        bridge.step(ticks);
                    }
                    "reset" => bridge.reset(),
                    "status" | "rate" | "set" => {}
                    other => {
                        return Err(format!(
                            "unknown action \"{other}\" (actions: run, pause, step, reset, \
                             rate, set, status)"
                        ));
                    }
                }
                Ok(format!(
                    "{}, tick {}, rate {}\n",
                    if bridge.is_running() {
                        "running"
                    } else {
                        "paused"
                    },
                    bridge.tick_count(),
                    rate_text(&config)
                ))
            },
        )
        .map_err(|e| format!("the game could not do it: {e}"))
        .and_then(|r| r)
}

/// What the state readout reads of each part.
type SignalParts = (
    Entity,
    Option<&'static GateLabel>,
    &'static GateType,
    Option<&'static GateValues>,
    Option<&'static InputDrive>,
);

/// What the editor shows, as text: the `app state` readout.
fn state(world: &mut World) -> String {
    let screen = world
        .get_resource::<State<GameState>>()
        .map(|s| format!("{:?}", s.get()));
    let editor = world.get_resource::<crate::editor::EditorState>().map(|e| {
        (
            e.status.clone(),
            e.library_open,
            e.connection_mode,
            e.selected_gate_type.clone(),
        )
    });
    let text = world.run_system_once(
        move |active: Res<ActiveCircuit>,
              revision: Res<WorkspaceRevision>,
              view: Res<CircuitView>,
              bridge: Res<SimBridge>,
              config: Res<SimulationConfig>,
              diagnostics: Option<Res<SimDiagnostics>>,
              feedback: Option<Res<AuthoringFeedback>>,
              history: Option<Res<UndoHistory>>,
              selected: Option<Res<SelectedGate>>,
              gates: Query<SignalParts, RootItems>| {
            let mut out = String::new();
            if let Some(screen) = &screen {
                let _ = writeln!(out, "screen: {screen}");
            }
            let _ = writeln!(
                out,
                "workspace: \"{}\"{}",
                active.name,
                if active.read_only {
                    " (read-only demo)"
                } else if revision.is_dirty() {
                    " (unsaved changes)"
                } else {
                    " (saved)"
                }
            );
            let path: Vec<&str> = view.frames().iter().map(|f| f.name.as_str()).collect();
            let _ = writeln!(
                out,
                "view: {}",
                if path.is_empty() {
                    "the root circuit".to_string()
                } else {
                    format!("inside {}", path.join(" / "))
                }
            );
            let _ = writeln!(
                out,
                "transport: {}, tick {}, rate {}",
                if bridge.is_running() {
                    "running"
                } else {
                    "paused"
                },
                bridge.tick_count(),
                rate_text(&config)
            );
            match diagnostics.as_ref().and_then(|d| d.compile_error.as_ref()) {
                None => {
                    let _ = writeln!(out, "build: ok");
                }
                Some(failure) => {
                    let _ = writeln!(out, "build: failed");
                    for m in &failure.messages {
                        let _ = writeln!(out, "  {m}");
                    }
                }
            }
            if let Some(history) = &history {
                let _ = writeln!(
                    out,
                    "undo: {} step(s), redo: {} step(s)",
                    history.undo_len(),
                    history.redo_len()
                );
            }
            let name = |label: Option<&GateLabel>, kind: &GateKind| match label {
                Some(l) if !l.value.trim().is_empty() => {
                    format!("\"{}\" ({})", l.value, kind.display_name())
                }
                _ => format!("an unlabelled {}", kind.display_name()),
            };
            if let Some(entity) = selected.as_ref().and_then(|s| s.0)
                && let Ok((_, label, ty, ..)) = gates.get(entity)
            {
                let _ = writeln!(out, "selected: {}", name(label, &ty.kind));
            }
            // The Signals card: inputs as driven, outputs as simulated.
            let mut signals: Vec<String> = Vec::new();
            for (_, label, ty, values, drive) in &gates {
                let Some(label) = label.filter(|l| !l.value.trim().is_empty()) else {
                    continue;
                };
                match ty.kind {
                    GateKind::Input { .. } => {
                        signals.push(format!(
                            "in {}={}",
                            label.value,
                            drive.map_or(0, |d| d.value)
                        ));
                    }
                    GateKind::Output { .. } | GateKind::Display { .. } => {
                        let v = values.and_then(|v| v.inputs.first().copied()).unwrap_or(0);
                        signals.push(format!("out {}={v}", label.value));
                    }
                    _ => {}
                }
            }
            signals.sort();
            if !signals.is_empty() {
                let more = signals.len().saturating_sub(40);
                signals.truncate(40);
                let _ = writeln!(
                    out,
                    "signals: {}{}",
                    signals.join(", "),
                    if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    }
                );
            }
            if let Some(message) = feedback.as_ref().and_then(|f| f.message.as_ref()) {
                let _ = writeln!(out, "message: {message}");
            }
            if let Some((status, library_open, wiring, tool)) = &editor {
                if let Some(status) = status {
                    let _ = writeln!(out, "status line: {status}");
                }
                if *library_open {
                    let _ = writeln!(out, "the Library drawer is open");
                }
                if *wiring {
                    let _ = writeln!(out, "the wire tool is on");
                }
                if let Some(kind) = tool {
                    let _ = writeln!(out, "armed part: {}", kind.display_name());
                }
            }
            out
        },
    );
    text.unwrap_or_else(|e| format!("the game could not read its state: {e}\n"))
}
