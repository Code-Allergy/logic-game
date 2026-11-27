//! A private simulation of one circuit of a workspace, with its signals
//! by name (`docs/mcp-server.md`, "Simulation").
//!
//! A [`Rig`] compiles a circuit as a root ([`crate::sim::netlist`]) and
//! runs it in a [`logi_sim::sim::Sim`] of its own, so tests and
//! experiments never touch the simulation the editor shows.
//!
//! ## Signals
//!
//! A signal is a path of labels from the circuit down: `cpu/alu/sum.y`
//! is output pin `y` of part `sum`, inside the chip placed as `alu`,
//! inside the chip placed as `cpu`. The last step is a reference as the
//! batch editor reads them (`name[index].pin:bits`,
//! [`crate::mcp::batch::names_of`]); the pin may be left out when the
//! part has one output. An `Output` or `Display` names what drives it.
//! `ram@0x10` is one word of a memory part (`Ram`, `Ram2`, `Rom`,
//! `Screen`). A path that names an array of parts reads them as one word
//! of state when it can be one ([`StateTarget`]), element 0 lowest.
//!
//! ## State targets
//!
//! [`Rig::target`] resolves a path to state that [`Rig::poke`] writes and
//! [`Rig::peek`] reads, with the same syntax as signals:
//!
//! - a `Register`, `Delay`, `DLatch` or `SrLatch` part (`cpu/a`, or
//!   `cpu/a.q`): its state. A `DLatch`'s state is what it holds while it
//!   is not enabled; while enabled its output follows its input.
//! - a chip, or one output pin of it (`cpu/a`, `cpu/a.Q`): the state
//!   elements that output is wired to, through wiring only (`Split`,
//!   `Merge`, zero `Extend`, chip ports). Every bit of the output must be
//!   one state element's bit. Other state inside the chip is analysed
//!   cell by cell (below): a copy of an output bit, such as the master
//!   latch of a master-slave flip-flop, is written with it; state the
//!   output does not depend on is left alone; anything else is refused.
//! - an `Output` or `Display` part: what drives it, traced the same way.
//! - an array of any of these (`cpu/a/bit`, `cpu/a/bit[0..8]`): one word,
//!   element 0 in the lowest bits.
//! - a memory word (`ram@0x1234`) of a `Ram`, `Ram2` or `Screen`.
//!
//! `:lo..hi` picks bits of any of these. Inputs, gates and ROM words are
//! not state and are refused, naming what to use instead.
//!
//! **Cells.** For a chip target, each state element inside the chip that
//! the output does not show belongs to the smallest chip instance around
//! it that holds an output bit's element: its cell (the flip-flop around
//! a master latch). The cell's definition is compiled alone and its next
//! state computed for every combination of its inputs and state bits (at
//! most [`MAX_CELL_BITS`]). A hidden bit `h` is a copy of a shown bit `s`
//! when `s` only ever keeps its value or takes `h`'s (or `h`'s
//! complement): `h` is written with `s`'s value. A hidden bit that no
//! written bit depends on is left alone. A hidden bit that a written bit
//! depends on otherwise (an edge detector's previous clock, say) makes
//! the target refused, since no one value says what it should hold.

use std::collections::HashMap;
use std::sync::Arc;

use logi_sim::compile::{NodePath, Op, Options, Program, StateLoc};
use logi_sim::netlist::{DefId, Design, NetId, NodeId, NodeKind};
use logi_sim::sim::{Sim, StateSnapshot};
use uuid::Uuid;

use super::batch::{Names, Place, circuit_at, names_of};
use super::{compiled, memfile};
use crate::sim::netlist::{DesignMap, design_map_of};
use crate::sim::source::{self, Dir};
use crate::sim::{CircuitDocument, CircuitLibrary, GateKind, WorkspaceDocument};

fn mask(w: u8) -> u64 {
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}

/// A memory part: where its words are.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    /// The circuit the part is in, and the part.
    pub circuit: Uuid,
    pub part: Uuid,
    pub path: NodePath,
    pub addr_bits: u8,
    pub width: u8,
    /// A `Rom`: its words are a parameter of the part, not state.
    pub rom: bool,
}

/// What a signal reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Bits `lo .. lo + len` of a net.
    Net {
        path: Vec<NodeId>,
        net: NetId,
        lo: u8,
        len: u8,
    },
    /// An input pin that nothing drives.
    Zero,
    /// One word of a memory.
    Word { memory: Memory, addr: usize },
    /// State read as one word (an array of state parts, see
    /// [`StateTarget`]).
    State(StateTarget),
}

/// A resolved signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub text: String,
    pub source: Source,
    pub width: u8,
}

/// The most input and state bits a cell may have to be analysed (see the
/// module docs).
pub const MAX_CELL_BITS: u32 = 16;

/// Bits `lo .. lo + len` of the state of the state element at `path`, as
/// bits `at .. at + len` of a target's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateBits {
    pub path: NodePath,
    pub lo: u8,
    pub len: u8,
    pub at: u8,
    /// Written as the complement of the value's bits.
    pub invert: bool,
    /// Read back as the target's value. A copy (a master latch) is only
    /// written.
    pub shown: bool,
}

/// Where a state target's value is kept.
#[derive(Debug, Clone, PartialEq)]
pub enum Stored {
    /// Bits of state elements; the shown runs cover the value's bits once.
    Bits(Vec<StateBits>),
    /// One word of a memory.
    Word { memory: Memory, addr: usize },
}

/// Resolved state that [`Rig::poke`] writes and [`Rig::peek`] reads (see
/// the module docs). It names state elements by path, so it stays good
/// across recompiles of the same design, and can be resolved once and
/// written many times.
#[derive(Debug, Clone, PartialEq)]
pub struct StateTarget {
    pub text: String,
    pub width: u8,
    pub stored: Stored,
}

impl StateTarget {
    /// `Err` when `value` has bits past the target's width.
    pub fn fits(&self, value: u64) -> Result<(), String> {
        if value & !mask(self.width) != 0 {
            return Err(format!(
                "{value} (0x{value:X}) does not fit \"{}\" ({} bits)",
                self.text, self.width
            ));
        }
        Ok(())
    }
}

/// What each hidden state bit of a cell is: a copy of a shown bit (its
/// index among the cell's shown bits, and whether it holds the
/// complement), or `None`, free (nothing written depends on it).
type Roles = Vec<((NodePath, u8), Option<(usize, bool)>)>;

/// A cell analysis's key: the cell's definition and its shown bits.
type CellKey = (DefId, Vec<(NodePath, u8)>);

/// One write of [`Rig::write_state`], checked.
enum Write {
    Slot {
        key: usize,
        lo: u8,
        len: u8,
        value: u64,
    },
    Mem {
        index: u32,
        addr: usize,
        value: u64,
    },
}

/// A [`Rig`]'s simulation at one moment ([`Rig::snapshot`]).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: StateSnapshot,
    /// The root inputs the logic had seen, and as last set.
    settled: Vec<u64>,
    staged: Vec<u64>,
}

/// Import `state` into `sim` and settle it with the inputs `settled`,
/// leaving `staged` to reach the logic at the next tick (as
/// [`Sim::set_input`] values do). `Sim::import_state` alone would settle
/// with the staged inputs, a tick early.
fn import(sim: &mut Sim, state: &StateSnapshot, settled: &[u64], staged: &[u64]) {
    for (port, value) in settled.iter().enumerate() {
        sim.set_input(port, *value);
    }
    sim.import_state(state);
    for (port, value) in staged.iter().enumerate() {
        sim.set_input(port, *value);
    }
}

/// A private simulation of one circuit.
pub struct Rig {
    /// The workspace with the circuit under test as its root.
    ws: WorkspaceDocument,
    library: CircuitLibrary,
    design: Design,
    map: DesignMap,
    sim: Sim,
    /// Nets the program keeps readable.
    probes: Vec<(Vec<NodeId>, NetId)>,
    /// Every net is readable (the fallback when probes do not suffice).
    debug: bool,
    names: HashMap<Uuid, Names>,
    /// Compiler options; the probes and `debug` the rig needs are added.
    options: Options,
    /// The state key of each state element held in a slot, and the index
    /// of each memory, in the program the simulation runs.
    slot_keys: HashMap<NodePath, usize>,
    mem_keys: HashMap<NodePath, u32>,
    /// State keys whose slot the program does not commit (see
    /// `index_keys`); writing them is refused.
    unwritable: std::collections::HashSet<usize>,
    /// Cell analyses by definition and shown bits (see the module docs).
    cells: HashMap<CellKey, Result<Roles, String>>,
    /// How to name each node of the design in messages (filled on use).
    labels: HashMap<(DefId, NodeId), String>,
}

fn compile(
    ws: &WorkspaceDocument,
    design: &Design,
    map: &DesignMap,
    options: &Options,
) -> Result<Program, String> {
    compiled::compile_design(ws, design, map, options)
        .map_err(|e| format!("the circuit does not build: {}", compiled::one_line(&e)))
}

impl Rig {
    /// Compile the circuit at `place` of `workspace` as a root.
    pub fn build(workspace: &WorkspaceDocument, place: Place) -> Result<Rig, String> {
        let mut ws = workspace.clone();
        if let Place::Definition(_) = place {
            ws.root = circuit_at(workspace, place).clone();
        }
        Self::of(ws)
    }

    /// [`Rig::build`] with compiler options.
    pub fn build_with(
        workspace: &WorkspaceDocument,
        place: Place,
        options: Options,
    ) -> Result<Rig, String> {
        let mut ws = workspace.clone();
        if let Place::Definition(_) = place {
            ws.root = circuit_at(workspace, place).clone();
        }
        Self::with_options(ws, options)
    }

    /// Compile `ws`, its root the circuit under test.
    pub fn of(ws: WorkspaceDocument) -> Result<Rig, String> {
        Self::with_options(ws, Options::default())
    }

    /// [`Rig::of`] with compiler options. Every recompile uses them too,
    /// with the probes the rig needs added.
    pub fn with_options(ws: WorkspaceDocument, options: Options) -> Result<Rig, String> {
        let (design, map) = design_map_of(&ws)?;
        let program = compile(&ws, &design, &map, &options)?;
        let sim = Sim::try_new(Arc::new(program)).map_err(|e| e.0)?;
        let mut rig = Rig {
            library: CircuitLibrary {
                definitions: ws.definitions.clone(),
            },
            ws,
            design,
            map,
            sim,
            probes: Vec::new(),
            debug: false,
            names: HashMap::new(),
            options,
            slot_keys: HashMap::new(),
            mem_keys: HashMap::new(),
            unwritable: Default::default(),
            cells: HashMap::new(),
            labels: HashMap::new(),
        };
        rig.index_keys();
        Ok(rig)
    }

    /// The compiler options the rig was made with.
    pub fn options(&self) -> &Options {
        &self.options
    }

    fn index_keys(&mut self) {
        self.slot_keys.clear();
        self.mem_keys.clear();
        self.unwritable.clear();
        let program = Arc::clone(self.sim.program());
        let committed: std::collections::HashSet<u32> = program
            .commits
            .iter()
            .filter_map(|c| match *c {
                logi_sim::compile::Commit::Copy { state, .. } => Some(state),
                _ => None,
            })
            .collect();
        for (k, key) in program.state_keys.iter().enumerate() {
            match key.loc {
                StateLoc::Slot(s) => {
                    self.slot_keys.insert(key.path.clone(), k);
                    // A key that names a copy of the state, not the state,
                    // would lose a write at the next settle.
                    if !program
                        .resolve(s)
                        .is_some_and(|(real, _, _)| committed.contains(&real))
                    {
                        self.unwritable.insert(k);
                    }
                }
                StateLoc::Mem(m) => {
                    self.mem_keys.insert(key.path.clone(), m);
                }
            }
        }
    }

    /// The circuit under test.
    pub fn circuit(&self) -> &CircuitDocument {
        &self.ws.root
    }

    pub fn sim(&self) -> &Sim {
        &self.sim
    }

    pub fn tick_count(&self) -> u64 {
        self.sim.tick_count()
    }

    /// The root input `name`, if `value` fits it.
    pub fn check_input(&self, name: &str, value: u64) -> Result<usize, String> {
        let ports = &self.sim.program().ports;
        let Some(port) = ports.input(name) else {
            let names: Vec<&str> = ports
                .inputs
                .iter()
                .filter_map(|p| p.name.as_deref())
                .collect();
            return Err(format!(
                "there is no input \"{name}\" (inputs: {})",
                names.join(", ")
            ));
        };
        let width = ports.inputs[port].width;
        if value & !mask(width) != 0 {
            return Err(format!(
                "{value} (0x{value:X}) does not fit input \"{name}\" ({width} bits)"
            ));
        }
        Ok(port)
    }

    /// Set a root input (seen by the logic at the next tick).
    pub fn set(&mut self, name: &str, value: u64) -> Result<(), String> {
        let port = self.check_input(name, value)?;
        self.sim.set_input(port, value);
        Ok(())
    }

    pub fn tick(&mut self) {
        self.sim.tick();
    }

    pub fn run(&mut self, ticks: u64) {
        self.sim.run(ticks);
    }

    /// Back to power-on: state cleared, tick 0, inputs kept.
    pub fn reset(&mut self) {
        self.sim.reset();
    }

    /// The whole simulation: state, tick, and the root inputs (those the
    /// logic has seen and those set since).
    pub fn snapshot(&self) -> Snapshot {
        let (settled, staged) = self.input_values();
        Snapshot {
            state: self.sim.export_state(),
            settled,
            staged,
        }
    }

    pub fn restore(&mut self, snapshot: &Snapshot) {
        import(
            &mut self.sim,
            &snapshot.state,
            &snapshot.settled,
            &snapshot.staged,
        );
    }

    /// The root inputs as of the last settle, and as last set.
    fn input_values(&self) -> (Vec<u64>, Vec<u64>) {
        let ports = 0..self.sim.program().ports.inputs.len();
        (
            ports.clone().map(|p| self.sim.settled_input(p)).collect(),
            ports.map(|p| self.sim.input(p)).collect(),
        )
    }

    /// Put `state` into the simulation, the root inputs keeping their
    /// timing: one set since the last tick still waits for the next.
    fn import_state(&mut self, state: &StateSnapshot) {
        let (settled, staged) = self.input_values();
        import(&mut self.sim, state, &settled, &staged);
    }

    fn names(&mut self, doc: &CircuitDocument) -> &Names {
        let library = &self.library;
        self.names
            .entry(doc.id)
            .or_insert_with(|| names_of(doc, library))
    }

    /// Resolve a signal path. The signal is not readable until
    /// [`Rig::watch`] has been called for it.
    pub fn resolve(&mut self, text: &str) -> Result<Signal, String> {
        self.resolve_inner(text)
            .map_err(|e| format!("signal \"{text}\": {e}"))
    }

    /// Follow the chip steps of a path from the root: the circuit the
    /// last step is in, and the instance path to it.
    fn descend<'w>(
        &mut self,
        ws: &'w WorkspaceDocument,
        steps: &[&str],
    ) -> Result<(&'w CircuitDocument, Vec<NodeId>), String> {
        let mut doc: &CircuitDocument = &ws.root;
        let mut path: Vec<NodeId> = Vec::new();
        for &step in steps {
            let ids = self.part(doc, step)?;
            let [id] = ids.as_slice() else {
                return Err(format!("\"{step}\" names {} parts; pick one", ids.len()));
            };
            let node = doc.node(*id).expect("named parts are in the document");
            let GateKind::SubCircuit { definition } = node.kind else {
                return Err(format!(
                    "\"{step}\" is not a chip; there is nothing inside it"
                ));
            };
            path.push(self.node(doc, *id)?);
            doc = ws
                .definitions
                .get(&definition)
                .ok_or_else(|| format!("the definition of \"{step}\" is missing"))?;
        }
        Ok((doc, path))
    }

    /// An array (or a range of one) read as one word of state.
    fn state_signal(&mut self, text: &str, why: String) -> Result<Signal, String> {
        match self.target_inner(text) {
            Ok(target) => Ok(Signal {
                text: text.to_string(),
                width: target.width,
                source: Source::State(target),
            }),
            Err(e) => Err(format!("{why}; as one word of state: {e}")),
        }
    }

    fn resolve_inner(&mut self, text: &str) -> Result<Signal, String> {
        let ws = self.ws.clone();
        let mut steps: Vec<&str> = text.split('/').map(str::trim).collect();
        let last = steps.pop().unwrap_or_default();
        let (doc, path) = self.descend(&ws, &steps)?;

        // `part@addr`: a word of a memory.
        if let Some((part, addr)) = last.split_once('@') {
            let memory = self.memory_in(doc, &path, part.trim())?;
            let addr = source::parse_int_text(addr.trim())
                .ok_or_else(|| format!("\"{addr}\" is not an address"))?
                as usize;
            if addr >= 1usize << memory.addr_bits {
                return Err(format!(
                    "address {addr} is past the memory's {} words",
                    1usize << memory.addr_bits
                ));
            }
            let width = memory.width;
            return Ok(Signal {
                text: text.to_string(),
                source: Source::Word { memory, addr },
                width,
            });
        }

        let r = source::parse_ref(last).map_err(|e| e.0)?;
        let whole = match r.index {
            Some((a, b)) if b == a + 1 => format!("{}[{a}]", r.node),
            Some(_) => return self.state_signal(text, "a range of parts is not one net".into()),
            None => r.node.to_string(),
        };
        let ids = self.part(doc, &whole)?;
        let [id] = ids.as_slice() else {
            let why = format!(
                "\"{}\" is an array of {} parts (name one element, e.g. \"{}[0]\")",
                r.node,
                ids.len(),
                r.node
            );
            return self.state_signal(text, why);
        };
        let part = doc.node(*id).expect("named parts are in the document");
        let node = self.node(doc, *id)?;
        let def = self.map.defs[&doc.id];
        let (ins, outs) = self.library.pins(&part.kind).unwrap_or_default();
        // The output pin a reference names (before the design is borrowed).
        let out_pin = if outs.is_empty() {
            None
        } else {
            let table = &self.names(doc).table;
            let pins =
                source::resolve(table, &pin_ref(&whole, r.pin), Dir::Out).map_err(|e| e.0)?;
            Some(pins[0].1)
        };
        let definition = self.design.def(def).map_err(|e| format!("{e:?}"))?;
        let (net, width) = if outs.is_empty() {
            // A sink (Output, Display, Screen): what drives its input.
            let names = self.library.pin_names(&part.kind).0;
            let pin = match r.pin {
                None if ins.len() == 1 => 0,
                None => {
                    return Err(format!(
                        "\"{whole}\" has several inputs ({}); name one",
                        names.join(", ")
                    ));
                }
                Some(p) => names
                    .iter()
                    .position(|n| n == p)
                    .or_else(|| p.parse().ok().filter(|i| *i < ins.len()))
                    .ok_or_else(|| format!("\"{whole}\" has no pin \"{p}\""))?,
            };
            let index = u8::try_from(pin).map_err(|_| {
                format!("\"{whole}\": a pin past the 256th cannot be wired, so it has no signal")
            })?;
            (definition.input_net(node, index), ins[pin])
        } else {
            let pin = out_pin.expect("found above");
            (definition.output_net(node, pin), outs[usize::from(pin)])
        };
        let (lo, len) = match r.bits {
            None => (0, width),
            Some((a, b)) if b <= u32::from(width) => (a as u8, (b - a) as u8),
            Some((a, b)) => {
                return Err(format!("bits {a}..{b} are past the pin's width ({width})"));
            }
        };
        Ok(Signal {
            text: text.to_string(),
            source: match net {
                Some(net) => Source::Net { path, net, lo, len },
                None => Source::Zero,
            },
            width: len,
        })
    }

    /// The parts `text` names in `doc`.
    fn part(&mut self, doc: &CircuitDocument, text: &str) -> Result<Vec<Uuid>, String> {
        let r = source::parse_ref(text).map_err(|e| e.0)?;
        if r.pin.is_some() || r.bits.is_some() {
            return Err(format!("\"{text}\": a part is expected here, not a pin"));
        }
        let names = self.names(doc);
        let Some(entry) = names.table.get(r.node) else {
            return Err(if names.ambiguous.contains(r.node) {
                format!(
                    "several parts of \"{}\" are labelled \"{}\"",
                    doc.name, r.node
                )
            } else {
                format!("no part \"{}\" in \"{}\"", r.node, doc.name)
            });
        };
        let elements = match r.index {
            None => &entry.elements[..],
            Some((a, b)) if entry.is_array && b as usize <= entry.elements.len() => {
                &entry.elements[a as usize..b as usize]
            }
            Some(_) => return Err(format!("\"{text}\": no such element")),
        };
        if let Some(gap) = elements.iter().find(|e| e.id.is_nil()) {
            return Err(format!("\"{}\" is not there any more", gap.name));
        }
        Ok(elements.iter().map(|e| e.id).collect())
    }

    fn node(&self, doc: &CircuitDocument, part: Uuid) -> Result<NodeId, String> {
        self.map
            .nodes
            .get(&doc.id)
            .and_then(|nodes| nodes.get(&part))
            .copied()
            .ok_or_else(|| "that part is not simulated inside a chip (a Display)".to_string())
    }

    fn memory_in(
        &mut self,
        doc: &CircuitDocument,
        path: &[NodeId],
        part: &str,
    ) -> Result<Memory, String> {
        let ids = self.part(doc, part)?;
        let [id] = ids.as_slice() else {
            return Err(format!("\"{part}\" names {} parts; pick one", ids.len()));
        };
        let node = doc.node(*id).expect("named parts are in the document");
        let (addr_bits, width, rom) = match &node.kind {
            GateKind::Rom { addr_bits, w, .. } => (*addr_bits, *w, true),
            GateKind::Ram { addr_bits, w } | GateKind::Ram2 { addr_bits, w, .. } => {
                (*addr_bits, *w, false)
            }
            GateKind::Screen { .. } => {
                let path = NodePath {
                    instances: path.to_vec(),
                    node: self.node(doc, *id)?,
                };
                let index = self
                    .sim
                    .mem_index(&path)
                    .ok_or("this screen has no memory to read")?;
                let d = &self.sim.program().mems[index as usize];
                return Ok(Memory {
                    circuit: doc.id,
                    part: *id,
                    path,
                    addr_bits: d.addr_bits,
                    width: d.width,
                    rom: false,
                });
            }
            GateKind::SubCircuit { .. } => {
                return Err(format!(
                    "\"{part}\" is a chip, not a memory; name the memory inside it \
                     (\"{part}/ram\")"
                ));
            }
            other => {
                return Err(format!(
                    "\"{part}\" is a {}, not a memory",
                    other.display_name()
                ));
            }
        };
        Ok(Memory {
            circuit: doc.id,
            part: *id,
            path: NodePath {
                instances: path.to_vec(),
                node: self.node(doc, *id)?,
            },
            addr_bits,
            width,
            rom,
        })
    }

    /// The memory part at `text` (`cpu/ram`).
    pub fn memory(&mut self, text: &str) -> Result<Memory, String> {
        let inner = |rig: &mut Rig| -> Result<Memory, String> {
            let ws = rig.ws.clone();
            let mut steps: Vec<&str> = text.split('/').map(str::trim).collect();
            let last = steps.pop().unwrap_or_default();
            let (doc, path) = rig.descend(&ws, &steps)?;
            rig.memory_in(doc, &path, last)
        };
        inner(self).map_err(|e| format!("memory \"{text}\": {e}"))
    }

    /// Make `signals` readable: their nets are kept through the
    /// compiler's passes. Recompiles when a net is new, carrying state
    /// and inputs over.
    pub fn watch(&mut self, signals: &[Signal]) -> Result<(), String> {
        let mut added = false;
        for s in signals {
            if let Source::Net { path, net, .. } = &s.source {
                let probe = (path.clone(), *net);
                if !self.probes.contains(&probe) {
                    self.probes.push(probe);
                    added = true;
                }
            }
        }
        let readable = |rig: &Rig| {
            rig.probes
                .iter()
                .all(|(path, net)| rig.sim.program().debug.slot(path, *net).is_some())
        };
        if added || !readable(self) {
            self.recompile()?;
            if !readable(self) {
                // Some net is not kept by a probe alone: keep them all.
                self.debug = true;
                self.recompile()?;
            }
        }
        Ok(())
    }

    /// Compile the design again (new probes, or a changed ROM), keeping
    /// state, tick and inputs.
    fn recompile(&mut self) -> Result<(), String> {
        let state = self.sim.export_state();
        let mut probes = self.options.probes.clone();
        probes.extend(self.probes.iter().cloned());
        let options = Options {
            debug: self.options.debug || self.debug,
            probes,
            ..self.options.clone()
        };
        let program = compile(&self.ws, &self.design, &self.map, &options)?;
        let mut sim = Sim::try_new(Arc::new(program)).map_err(|e| e.0)?;
        // The same design: the same root inputs, in the same order.
        let (settled, staged) = self.input_values();
        import(&mut sim, &state, &settled, &staged);
        self.sim = sim;
        self.index_keys();
        Ok(())
    }

    /// The value of a watched signal after the last tick.
    pub fn value(&self, signal: &Signal) -> u64 {
        match &signal.source {
            Source::Zero => 0,
            Source::Net { path, net, lo, len } => {
                (self.sim.read_net(path, *net).unwrap_or(0) >> lo) & mask(*len)
            }
            // One word, without a copy of the memory: a recorded word is
            // read every tick.
            Source::Word { memory, addr } => {
                let word = if memory.rom {
                    self.rom_contents(memory)
                        .and_then(|words| words.get(*addr).copied())
                } else {
                    self.sim
                        .mem_index(&memory.path)
                        .and_then(|i| self.sim.mem(i).get(*addr).copied())
                };
                word.unwrap_or(0) & mask(memory.width)
            }
            Source::State(target) => self.peek(target),
        }
    }

    /// The words of a memory (a ROM's may be shorter than its address
    /// space; the rest read 0).
    pub fn words(&self, memory: &Memory) -> Vec<u64> {
        if memory.rom {
            return self
                .rom_contents(memory)
                .map(<[u64]>::to_vec)
                .unwrap_or_default();
        }
        self.sim
            .mem_index(&memory.path)
            .map(|i| self.sim.mem(i).to_vec())
            .unwrap_or_default()
    }

    fn rom_contents(&self, memory: &Memory) -> Option<&[u64]> {
        let doc = if memory.circuit == self.ws.root.id {
            &self.ws.root
        } else {
            &**self.ws.definitions.get(&memory.circuit)?
        };
        match &doc.node(memory.part)?.kind {
            GateKind::Rom { contents, .. } => Some(contents),
            _ => None,
        }
    }

    /// Write words into a memory, for this simulation only. A ROM's
    /// contents are part of its chip, so every placement of that chip in
    /// this simulation gets them.
    pub fn load(&mut self, memory: &Memory, load: &memfile::Load) -> Result<(), String> {
        if memory.rom {
            let mut contents = self.rom_contents(memory).unwrap_or_default().to_vec();
            memfile::place(&mut contents, memory.addr_bits, load)?;
            let mut set = |doc: &mut CircuitDocument| {
                for n in &mut doc.nodes {
                    if n.id == memory.part
                        && let GateKind::Rom { contents: c, .. } = &mut n.kind
                    {
                        *c = std::mem::take(&mut contents);
                    }
                }
            };
            if memory.circuit == self.ws.root.id {
                set(&mut self.ws.root);
            } else if let Some(def) = self.ws.definitions.get_mut(&memory.circuit) {
                set(Arc::make_mut(def));
            }
            // The same parts in the same order give the same node ids, so
            // probes and state keys stay good.
            let (design, map) = design_map_of(&self.ws)?;
            self.design = design;
            self.map = map;
            return self.recompile();
        }
        let mut state = self.sim.export_state();
        let Some((_, _, words)) = state.mems.get_mut(&memory.path) else {
            return Err("that memory holds no state in this simulation".into());
        };
        memfile::place(words, memory.addr_bits, load)?;
        self.import_state(&state);
        Ok(())
    }
}

/// Why a trace from an output bit stopped short of a state element.
enum Stop {
    /// A node that computes the bit: its path and what it is.
    Node(NodePath, String),
    /// The bit comes from an input port of the circuit the path starts in.
    Input,
    /// An input pin nothing drives (it reads 0).
    Undriven,
    Bad(String),
}

/// Short name of a node kind for messages (`And`, `Mux`, ...).
fn kind_name(kind: &NodeKind) -> String {
    match kind {
        NodeKind::DLatch { .. } => {
            "DLatch (whose output follows its input while enabled; name the latch itself)".into()
        }
        other => {
            let text = format!("{other:?}");
            text.split([' ', '{', '('])
                .next()
                .unwrap_or_default()
                .to_string()
        }
    }
}

impl Rig {
    // ── State targets: resolving ─────────────────────────────────────────

    /// Resolve a path to state (see the module docs, "State targets").
    pub fn target(&mut self, text: &str) -> Result<StateTarget, String> {
        self.target_inner(text)
            .map_err(|e| format!("state \"{text}\": {e}"))
    }

    /// A target for word `addr` of `memory` (not a ROM).
    pub fn word_target(&mut self, memory: &Memory, addr: usize) -> Result<StateTarget, String> {
        if memory.rom {
            return Err(
                "a ROM's words are not state; change them with \"load\" (for this simulation) \
                 or an edit (for good)"
                    .into(),
            );
        }
        if addr >= 1usize << memory.addr_bits {
            return Err(format!(
                "address {addr} (0x{addr:X}) is past the memory's {} words",
                1usize << memory.addr_bits
            ));
        }
        let name = self.describe(self.design.root, &memory.path);
        Ok(StateTarget {
            text: format!("{name}@0x{addr:X}"),
            width: memory.width,
            stored: Stored::Word {
                memory: memory.clone(),
                addr,
            },
        })
    }

    fn target_inner(&mut self, text: &str) -> Result<StateTarget, String> {
        let ws = self.ws.clone();
        let mut steps: Vec<&str> = text.split('/').map(str::trim).collect();
        let last = steps.pop().unwrap_or_default();
        let (doc, path) = self.descend(&ws, &steps)?;

        if let Some((part, addr)) = last.split_once('@') {
            let memory = self.memory_in(doc, &path, part.trim())?;
            let addr = source::parse_int_text(addr.trim())
                .ok_or_else(|| format!("\"{addr}\" is not an address"))?
                as usize;
            let mut target = self.word_target(&memory, addr)?;
            target.text = text.to_string();
            return Ok(target);
        }

        let r = source::parse_ref(last).map_err(|e| e.0)?;
        let whole = match r.index {
            Some((a, b)) if b == a + 1 => format!("{}[{a}]", r.node),
            Some((a, b)) => format!("{}[{a}..{b}]", r.node),
            None => r.node.to_string(),
        };
        let ids = self.part(doc, &whole)?;
        let def = self.map.defs[&doc.id];
        let mut bits: Vec<StateBits> = Vec::new();
        let mut at: u32 = 0;
        for id in &ids {
            let name = self
                .names(doc)
                .reference
                .get(id)
                .cloned()
                .unwrap_or_else(|| whole.clone());
            let (shown, scope) = self.element_state(doc, def, &path, *id, r.pin, &name)?;
            if at + shown.len() as u32 > 64 {
                return Err("more than 64 bits; name fewer of them".into());
            }
            let followers = match scope {
                Some(scope) => self
                    .followers(&scope, &shown)
                    .map_err(|e| format!("\"{name}\" cannot be written as one value: {e}"))?,
                None => Vec::new(),
            };
            for (i, (p, b)) in shown.iter().enumerate() {
                bits.push(StateBits {
                    path: p.clone(),
                    lo: *b,
                    len: 1,
                    at: (at + i as u32) as u8,
                    invert: false,
                    shown: true,
                });
            }
            for (p, b, i, invert) in followers {
                bits.push(StateBits {
                    path: p,
                    lo: b,
                    len: 1,
                    at: (at + i as u32) as u8,
                    invert,
                    shown: false,
                });
            }
            at += shown.len() as u32;
        }
        let (lo, len) = match r.bits {
            None => (0, at),
            Some((a, b)) if b <= at => (a, b - a),
            Some((a, b)) => return Err(format!("bits {a}..{b} are past its width ({at})")),
        };
        bits.retain(|b| (lo..lo + len).contains(&u32::from(b.at)));
        for b in &mut bits {
            b.at -= lo as u8;
        }
        // Shown runs first, in value order; then copies. Neighbouring bits
        // of one element become one run.
        bits.sort_by_key(|b| (!b.shown, b.at));
        let mut runs: Vec<StateBits> = Vec::new();
        for b in bits {
            if let Some(last) = runs.last_mut()
                && last.path == b.path
                && last.shown == b.shown
                && last.invert == b.invert
                && last.lo + last.len == b.lo
                && last.at + last.len == b.at
            {
                last.len += 1;
            } else {
                runs.push(b);
            }
        }
        for b in &runs {
            let (missing, copied) = match self.slot_keys.get(&b.path) {
                None => (true, false),
                Some(k) => (false, self.unwritable.contains(k)),
            };
            if missing || copied {
                let what = self.describe(self.design.root, &b.path);
                return Err(if missing {
                    format!("\"{what}\" holds no state in this simulation")
                } else {
                    format!(
                        "the compiled program keeps only a copy of the state of \"{what}\", \
                         which a write would not change; build the rig with other compile options"
                    )
                });
            }
        }
        Ok(StateTarget {
            text: text.to_string(),
            width: len as u8,
            stored: Stored::Bits(runs),
        })
    }

    /// The state bits part `id` of `doc` (at instance `path`, definition
    /// `def`) shows, bit 0 first, and the chip whose other state must be
    /// analysed (`None` for a state part).
    #[allow(clippy::type_complexity)]
    fn element_state(
        &mut self,
        doc: &CircuitDocument,
        def: DefId,
        path: &[NodeId],
        id: Uuid,
        pin: Option<&str>,
        name: &str,
    ) -> Result<(Vec<(NodePath, u8)>, Option<Vec<NodeId>>), String> {
        let part = doc.node(id).expect("named parts are in the document");
        let node = self.node(doc, id)?;
        let here = NodePath {
            instances: path.to_vec(),
            node,
        };
        let not_state = |what: &str| format!("\"{name}\" is {what}");
        let (_, outs) = self.library.pin_names(&part.kind);
        match &part.kind {
            GateKind::Register { .. }
            | GateKind::Delay { .. }
            | GateKind::DLatch { .. }
            | GateKind::SrLatch => {
                let w = match part.kind {
                    GateKind::SrLatch => 1,
                    ref kind => kind.width().unwrap_or(1),
                };
                if let Some(p) = pin
                    && !outs.iter().any(|o| o == p)
                {
                    return Err(format!("\"{name}\" has no output \"{p}\""));
                }
                Ok(((0..w).map(|b| (here.clone(), b)).collect(), None))
            }
            GateKind::Input { .. } => Err(not_state(if path.is_empty() {
                "an input, not state; set it with \"set\""
            } else {
                "an input port of its chip, not state"
            })),
            GateKind::Rom { .. } => Err(not_state(
                "a ROM; its words are not state: change them with \"load\"",
            )),
            GateKind::Ram { .. } | GateKind::Ram2 { .. } | GateKind::Screen { .. } => {
                Err(not_state(
                    "a memory; name one word (\"ram@0x10\"), or poke several as \
                     {\"ram\": [[address, value], ...]}",
                ))
            }
            GateKind::SubCircuit { .. } => {
                if outs.is_empty() {
                    return Err(not_state("a chip with no outputs"));
                }
                let table = &self.names(doc).table;
                let pins = source::resolve(table, &pin_ref(name, pin), Dir::Out)
                    .map_err(|e| format!("{} (name one output of the chip)", e.0))?;
                let pin = pins[0].1;
                let net = self
                    .design
                    .def(def)
                    .ok()
                    .and_then(|d| d.output_net(node, pin))
                    .ok_or_else(|| not_state("a chip whose output is not simulated"))?;
                let mut inside = path.to_vec();
                inside.push(node);
                let shown = self.trace_all(path, net, name)?;
                Ok((shown, Some(inside)))
            }
            GateKind::Output { .. } | GateKind::Display { .. } => {
                let net = self
                    .design
                    .def(def)
                    .ok()
                    .and_then(|d| d.input_net(node, 0))
                    .ok_or_else(|| not_state("not driven"))?;
                let shown = self.trace_all(path, net, name)?;
                // The chip that holds every shown bit, if any.
                let mut scope: Option<Vec<NodeId>> = None;
                for (p, _) in &shown {
                    scope = Some(match scope {
                        None => p.instances.clone(),
                        Some(s) => s
                            .iter()
                            .zip(&p.instances)
                            .take_while(|(a, b)| a == b)
                            .map(|(a, _)| *a)
                            .collect(),
                    });
                }
                Ok((shown, scope.filter(|s| s.len() > path.len())))
            }
            other => Err(not_state(&format!(
                "a {}, not state; poke a register, latch or Delay, a chip whose output is \
                 one, or a memory word",
                other.display_name()
            ))),
        }
    }

    /// Every bit of `net` (in the instance at `path`) traced to the state
    /// bit it is wired to.
    fn trace_all(
        &mut self,
        path: &[NodeId],
        net: NetId,
        name: &str,
    ) -> Result<Vec<(NodePath, u8)>, String> {
        let width = self
            .defs_along(path)
            .and_then(|defs| {
                let def = self.design.def(*defs.last()?).ok()?;
                def.nets.get(net).map(|n| n.width)
            })
            .ok_or("its net is not in the design")?;
        let mut shown = Vec::with_capacity(usize::from(width));
        for bit in 0..width {
            match self.trace(path, net, bit) {
                Ok(found) => {
                    if shown.contains(&found) {
                        return Err(format!(
                            "bits of \"{name}\" repeat one state bit, so they cannot be written \
                             apart"
                        ));
                    }
                    shown.push(found);
                }
                Err(stop) => {
                    let why = match stop {
                        Stop::Node(p, kind) => {
                            let at = self.describe(self.design.root, &p);
                            format!("is computed by {kind} \"{at}\", not held by state")
                        }
                        Stop::Input => "comes from an input, not from state".into(),
                        Stop::Undriven => "is not driven (it reads 0)".into(),
                        Stop::Bad(e) => e,
                    };
                    return Err(format!(
                        "bit {bit} of \"{name}\" {why}; a chip names state only when its output \
                         is wired straight to registers, latches or Delays"
                    ));
                }
            }
        }
        Ok(shown)
    }

    /// The definitions along an instance path, the root's first.
    fn defs_along(&self, path: &[NodeId]) -> Option<Vec<DefId>> {
        let mut defs = vec![self.design.root];
        for &n in path {
            let d = self.design.def(*defs.last()?).ok()?;
            match d.nodes.get(n)?.kind {
                NodeKind::Instance(child) => defs.push(child),
                _ => return None,
            }
        }
        Some(defs)
    }

    /// The state bit that bit `bit` of `net` (in the instance at `path`)
    /// is wired to, through `Split`, `Merge`, zero extension and chip
    /// ports, never above `path`.
    fn trace(&self, path: &[NodeId], net: NetId, bit: u8) -> Result<(NodePath, u8), Stop> {
        let floor = path.len();
        let mut path = path.to_vec();
        let mut defs = self.defs_along(&path).ok_or(Stop::Bad("bad path".into()))?;
        let (mut net, mut bit) = (net, bit);
        let design = &self.design;
        let get = |d: DefId| design.def(d).map_err(|e| Stop::Bad(format!("{e}")));
        loop {
            let def = get(*defs.last().expect("the root"))?;
            let (node, pin) = def.nets.get(net).ok_or(Stop::Bad("bad net".into()))?.driver;
            let n = &def.nodes[node];
            let input = |k: usize| n.inputs.get(k).copied().flatten().ok_or(Stop::Undriven);
            match &n.kind {
                NodeKind::Delay { .. } | NodeKind::Register { .. } | NodeKind::SrLatch => {
                    return Ok((
                        NodePath {
                            instances: path,
                            node,
                        },
                        bit,
                    ));
                }
                NodeKind::Split { lo, .. } => {
                    net = input(0)?;
                    bit += lo;
                }
                NodeKind::Merge { widths } => {
                    let mut lo = 0;
                    let mut found = None;
                    for (k, &w) in widths.iter().enumerate() {
                        if bit < lo + w {
                            found = Some(k);
                            break;
                        }
                        lo += w;
                    }
                    net = input(found.ok_or(Stop::Bad("bad merge".into()))?)?;
                    bit -= lo;
                }
                NodeKind::Extend { from, .. } if bit < *from => net = input(0)?,
                NodeKind::Instance(child) => {
                    let c = get(*child)?;
                    let out = c.outputs[usize::from(pin)];
                    path.push(node);
                    defs.push(*child);
                    net = c.nodes[out].inputs[0].ok_or(Stop::Undriven)?;
                }
                NodeKind::Input { .. } if path.len() > floor => {
                    let k = def
                        .inputs
                        .iter()
                        .position(|&i| i == node)
                        .ok_or(Stop::Bad("bad port".into()))?;
                    let inst = path.pop().expect("above the floor");
                    defs.pop();
                    let parent = get(*defs.last().expect("the root"))?;
                    net = parent.nodes[inst].inputs[k].ok_or(Stop::Undriven)?;
                }
                NodeKind::Input { .. } => return Err(Stop::Input),
                NodeKind::Extend { .. } => {
                    return Err(Stop::Node(
                        NodePath {
                            instances: path,
                            node,
                        },
                        "an extension (its high bits are not state)".into(),
                    ));
                }
                other => {
                    let kind = kind_name(other);
                    return Err(Stop::Node(
                        NodePath {
                            instances: path,
                            node,
                        },
                        format!("a {kind}"),
                    ));
                }
            }
        }
    }

    /// The copies to write along with `shown`, the state bits a chip at
    /// `scope` shows: `(element, bit, index into shown, inverted)`.
    #[allow(clippy::type_complexity)]
    fn followers(
        &mut self,
        scope: &[NodeId],
        shown: &[(NodePath, u8)],
    ) -> Result<Vec<(NodePath, u8, usize, bool)>, String> {
        // Every state element inside the scope.
        let defs = self.defs_along(scope).ok_or("bad path")?;
        let mut elements: Vec<(NodePath, u8)> = Vec::new();
        let mut memory = None;
        let mut stack = vec![(scope.to_vec(), *defs.last().expect("the root"))];
        while let Some((at, def)) = stack.pop() {
            let d = self.design.def(def).map_err(|e| e.to_string())?;
            for (node, n) in &d.nodes {
                let here = || NodePath {
                    instances: at.clone(),
                    node,
                };
                match &n.kind {
                    NodeKind::Instance(child) => {
                        let mut inner = at.clone();
                        inner.push(node);
                        stack.push((inner, *child));
                    }
                    NodeKind::Delay { w } | NodeKind::Register { w } | NodeKind::DLatch { w } => {
                        elements.push((here(), *w));
                    }
                    NodeKind::SrLatch => elements.push((here(), 1)),
                    NodeKind::Ram { .. } | NodeKind::Ram2 { .. } | NodeKind::Screen { .. } => {
                        memory.get_or_insert_with(here);
                    }
                    _ => {}
                }
            }
        }
        if let Some(p) = memory {
            let chip = self.describe_instances(scope);
            let mem = self.describe(self.design.root, &p);
            return Err(format!(
                "\"{chip}\" holds a memory (\"{mem}\"), so it is not a register; poke the \
                 memory's words, or the state elements inside it by path"
            ));
        }
        let hidden: Vec<(NodePath, u8)> = elements
            .iter()
            .flat_map(|(p, w)| (0..*w).map(move |b| (p.clone(), b)))
            .filter(|bit| !shown.contains(bit))
            .collect();
        if hidden.is_empty() {
            return Ok(Vec::new());
        }
        // Each hidden bit's cell: the deepest instance around it that
        // holds a shown bit.
        let mut cells: Vec<Vec<NodeId>> = Vec::new();
        for (p, _) in &hidden {
            let cell = (scope.len()..=p.instances.len())
                .rev()
                .map(|l| &p.instances[..l])
                .find(|prefix| shown.iter().any(|(s, _)| s.instances.starts_with(prefix)))
                .expect("the scope holds a shown bit")
                .to_vec();
            if !cells.contains(&cell) {
                cells.push(cell);
            }
        }
        let mut out = Vec::new();
        for cell in cells {
            let def = self
                .defs_along(&cell)
                .and_then(|d| d.last().copied())
                .ok_or("bad path")?;
            let relative = |p: &NodePath| NodePath {
                instances: p.instances[cell.len()..].to_vec(),
                node: p.node,
            };
            // The shown bits in this cell, as indices into `shown`.
            let inside: Vec<usize> = (0..shown.len())
                .filter(|&i| shown[i].0.instances.starts_with(&cell))
                .collect();
            let key: Vec<(NodePath, u8)> = inside
                .iter()
                .map(|&i| (relative(&shown[i].0), shown[i].1))
                .collect();
            let roles = match self.cells.get(&(def, key.clone())) {
                Some(r) => r.clone(),
                None => {
                    let r = self.analyse(def, &key);
                    self.cells.insert((def, key.clone()), r.clone());
                    r
                }
            };
            let roles = roles.map_err(|e| {
                let name = self.describe_instances(&cell);
                format!("in \"{name}\", {e}; write its state elements by path instead")
            })?;
            for ((p, b), role) in roles {
                if let Some((k, invert)) = role {
                    let mut instances = cell.clone();
                    instances.extend(p.instances);
                    out.push((
                        NodePath {
                            instances,
                            node: p.node,
                        },
                        b,
                        inside[k],
                        invert,
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Analyse the cell of definition `def` whose bits `shown` (paths
    /// relative to it) are written: what each other state bit of it is.
    /// See the module docs, "Cells".
    fn analyse(&mut self, def: DefId, shown: &[(NodePath, u8)]) -> Result<Roles, String> {
        let mut design = self.design.clone();
        design.root = def;
        let program = logi_sim::compile::compile(&design, &Options::unoptimized())
            .map_err(|_| "its cell does not build on its own".to_string())?;
        if program.blocks.iter().any(|b| b.op == Op::Clock) {
            return Err("its cell holds a clock".into());
        }
        let mut sim = Sim::try_new(Arc::new(program)).map_err(|e| e.0)?;
        let p = Arc::clone(sim.program());
        let mut bits: Vec<(usize, u8)> = Vec::new();
        for (k, key) in p.state_keys.iter().enumerate() {
            if let StateLoc::Mem(_) = key.loc {
                return Err("its cell holds a memory".into());
            }
            bits.extend((0..key.width).map(|b| (k, b)));
        }
        let n_in: u32 = p.ports.inputs.iter().map(|q| u32::from(q.width)).sum();
        let total = n_in + bits.len() as u32;
        if total > MAX_CELL_BITS {
            return Err(format!(
                "its cell has {n_in} input and {} state bits, more than the {MAX_CELL_BITS} that \
                 are analysed",
                bits.len()
            ));
        }
        let index = |e: &(NodePath, u8)| {
            bits.iter()
                .position(|&(k, b)| p.state_keys[k].path == e.0 && b == e.1)
        };
        // Next state for every input and state.
        let n = 1usize << total;
        let mut next = vec![0u64; n];
        let mut values = vec![0u64; p.state_keys.len()];
        for (x, slot) in next.iter_mut().enumerate() {
            let mut shift = 0;
            for (port, q) in p.ports.inputs.iter().enumerate() {
                sim.set_input(port, (x as u64 >> shift) & mask(q.width));
                shift += u32::from(q.width);
            }
            sim.tick();
            values.fill(0);
            for (i, &(k, b)) in bits.iter().enumerate() {
                values[k] |= ((x as u64 >> (n_in + i as u32)) & 1) << b;
            }
            for (k, &v) in values.iter().enumerate() {
                sim.set_state(k, v);
            }
            sim.resettle();
            sim.tick();
            *slot = bits
                .iter()
                .enumerate()
                .map(|(i, &(k, b))| ((sim.state(k) >> b) & 1) << i)
                .fold(0, |a, v| a | v);
        }
        let cur = |x: usize, i: usize| ((x as u64) >> (n_in as usize + i)) & 1;
        let nxt = |x: usize, i: usize| (next[x] >> i) & 1;
        let mut name = |i: usize| {
            let (k, b) = bits[i];
            let key = &p.state_keys[k];
            let text = self.describe(def, &key.path);
            if key.width > 1 {
                format!("{text}:{b}")
            } else {
                text
            }
        };
        // What each bit is: shown (written), a copy of a shown bit, or
        // not known yet.
        let mut role: Vec<Option<(usize, bool)>> = vec![None; bits.len()];
        let mut is_shown = vec![false; bits.len()];
        for (j, e) in shown.iter().enumerate() {
            let i = index(e).ok_or("a shown bit is not state in its cell")?;
            role[i] = Some((j, false));
            is_shown[i] = true;
        }
        loop {
            let mut changed = false;
            for h in 0..bits.len() {
                if role[h].is_some() {
                    continue;
                }
                let mut found: Option<(usize, bool)> = None;
                for (s, r) in role.iter().enumerate() {
                    let Some((of, pol)) = *r else { continue };
                    for inv in [0, 1] {
                        let mut taken = false;
                        let only = (0..n).all(|x| {
                            let (e, c, v) = (nxt(x, s), cur(x, s), cur(x, h) ^ inv);
                            taken |= e != c && e == v;
                            e == c || e == v
                        });
                        if only && taken {
                            let this = (of, pol ^ (inv == 1));
                            match found {
                                Some(f) if f != this => {
                                    return Err(format!(
                                        "\"{}\" is a copy of two different bits",
                                        name(h)
                                    ));
                                }
                                _ => found = Some(this),
                            }
                        }
                    }
                }
                if found.is_some() {
                    role[h] = found;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // A bit left free must not change what a written bit does.
        for f in (0..bits.len()).filter(|&f| role[f].is_none()) {
            let flip = 1usize << (n_in as usize + f);
            for s in (0..bits.len()).filter(|&s| role[s].is_some()) {
                if (0..n).any(|x| nxt(x, s) != nxt(x ^ flip, s)) {
                    return Err(format!(
                        "\"{}\" holds state that \"{}\" depends on and that is not a copy of it, \
                         so no one value says what it should hold",
                        name(f),
                        name(s)
                    ));
                }
            }
        }
        Ok((0..bits.len())
            .filter(|&i| !is_shown[i])
            .map(|i| {
                let (k, b) = bits[i];
                ((p.state_keys[k].path.clone(), b), role[i])
            })
            .collect())
    }

    /// The label path of `path` from definition `def` (the root's
    /// definition for a path from the root).
    fn describe(&mut self, def: DefId, path: &NodePath) -> String {
        let mut text = self.describe_from(def, &path.instances);
        let last = self.defs_from(def, &path.instances).unwrap_or(def);
        let name = self.label(last, path.node);
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(&name);
        text
    }

    fn describe_instances(&mut self, path: &[NodeId]) -> String {
        self.describe_from(self.design.root, path)
    }

    fn describe_from(&mut self, def: DefId, path: &[NodeId]) -> String {
        let mut names = Vec::new();
        let mut d = def;
        for &n in path {
            names.push(self.label(d, n));
            match self.design.def(d).ok().and_then(|x| x.nodes.get(n)) {
                Some(node) => match node.kind {
                    NodeKind::Instance(c) => d = c,
                    _ => break,
                },
                None => break,
            }
        }
        names.join("/")
    }

    /// The definition at the end of `path` from `def`.
    fn defs_from(&self, def: DefId, path: &[NodeId]) -> Option<DefId> {
        let mut d = def;
        for &n in path {
            match self.design.def(d).ok()?.nodes.get(n)?.kind {
                NodeKind::Instance(c) => d = c,
                _ => return None,
            }
        }
        Some(d)
    }

    /// How a node of a definition is named: its part's reference.
    fn label(&mut self, def: DefId, node: NodeId) -> String {
        if self.labels.is_empty() {
            let ws = self.ws.clone();
            for (circuit, nodes) in self.map.nodes.clone() {
                let Some(&d) = self.map.defs.get(&circuit) else {
                    continue;
                };
                let doc = if circuit == ws.root.id {
                    &ws.root
                } else if let Some(doc) = ws.definitions.get(&circuit) {
                    &**doc
                } else {
                    continue;
                };
                let reference = self.names(doc).reference.clone();
                for (part, n) in nodes {
                    if let Some(r) = reference.get(&part) {
                        self.labels.insert((d, n), r.clone());
                    }
                }
            }
        }
        self.labels
            .get(&(def, node))
            .cloned()
            .unwrap_or_else(|| "(unnamed)".into())
    }

    // ── State targets: writing and reading ──────────────────────────────

    /// Write `pokes` (each a target and its value), then [`Rig::settle`]:
    /// every net is consistent with the new state, the tick does not
    /// advance, and the next tick commits from that state. Every value is
    /// checked before anything is written.
    pub fn poke(&mut self, pokes: &[(&StateTarget, u64)]) -> Result<(), String> {
        self.write_state(pokes)?;
        self.settle();
        Ok(())
    }

    /// Write words into a memory (`(address, value)` pairs), then
    /// [`Rig::settle`]. Every word is checked before anything is written.
    pub fn poke_words(&mut self, memory: &Memory, words: &[(usize, u64)]) -> Result<(), String> {
        self.write_words(memory, words)?;
        self.settle();
        Ok(())
    }

    /// [`Rig::poke`] without the settle: nets keep their old values until
    /// [`Rig::settle`] or the next tick. For several writes with one
    /// settle.
    pub fn write_state(&mut self, pokes: &[(&StateTarget, u64)]) -> Result<(), String> {
        let mut writes = Vec::new();
        for &(target, value) in pokes {
            target.fits(value)?;
            match &target.stored {
                Stored::Bits(bits) => {
                    for b in bits {
                        let Some(&key) = self.slot_keys.get(&b.path) else {
                            return Err(format!(
                                "\"{}\": its state is not in this simulation",
                                target.text
                            ));
                        };
                        if self.unwritable.contains(&key) {
                            return Err(format!(
                                "\"{}\": the compiled program keeps only a copy of its state, \
                                 which a write would not change; build the rig with other \
                                 compile options",
                                target.text
                            ));
                        }
                        let field = (value >> b.at) & mask(b.len);
                        writes.push(Write::Slot {
                            key,
                            lo: b.lo,
                            len: b.len,
                            value: if b.invert {
                                !field & mask(b.len)
                            } else {
                                field
                            },
                        });
                    }
                }
                Stored::Word { memory, addr } => {
                    writes.push(self.word_write(memory, *addr, value, &target.text)?);
                }
            }
        }
        self.apply(&writes);
        Ok(())
    }

    /// [`Rig::poke_words`] without the settle.
    pub fn write_words(&mut self, memory: &Memory, words: &[(usize, u64)]) -> Result<(), String> {
        let mut writes = Vec::with_capacity(words.len());
        for &(addr, value) in words {
            if value & !mask(memory.width) != 0 {
                return Err(format!(
                    "{value} (0x{value:X}) does not fit a word of the memory ({} bits)",
                    memory.width
                ));
            }
            writes.push(self.word_write(memory, addr, value, "the memory")?);
        }
        self.apply(&writes);
        Ok(())
    }

    fn word_write(
        &self,
        memory: &Memory,
        addr: usize,
        value: u64,
        text: &str,
    ) -> Result<Write, String> {
        if memory.rom {
            return Err(format!(
                "\"{text}\" is a ROM: its words are not state; change them with \"load\""
            ));
        }
        let Some(&index) = self.mem_keys.get(&memory.path) else {
            return Err(format!("\"{text}\" holds no state in this simulation"));
        };
        if addr >= 1usize << memory.addr_bits {
            return Err(format!(
                "\"{text}\": address {addr} (0x{addr:X}) is past the memory's {} words",
                1usize << memory.addr_bits
            ));
        }
        Ok(Write::Mem { index, addr, value })
    }

    fn apply(&mut self, writes: &[Write]) {
        for w in writes {
            match *w {
                Write::Slot {
                    key,
                    lo,
                    len,
                    value,
                } => {
                    let width = self.sim.program().state_keys[key].width;
                    let new = if lo == 0 && len >= width {
                        value
                    } else {
                        let old = self.sim.state(key);
                        (old & !(mask(len) << lo)) | (value << lo)
                    };
                    self.sim.set_state(key, new);
                }
                Write::Mem { index, addr, value } => self.sim.set_mem_word(index, addr, value),
            }
        }
    }

    /// Evaluate every net from the current state without advancing the
    /// tick (after [`Rig::write_state`] or [`Rig::write_words`]). Inputs
    /// set since the last tick still reach the logic at the next tick.
    pub fn settle(&mut self) {
        self.sim.resettle();
    }

    /// The value a target holds now: the state its shown bits hold, or
    /// the memory word.
    pub fn peek(&self, target: &StateTarget) -> u64 {
        match &target.stored {
            Stored::Bits(bits) => bits.iter().filter(|b| b.shown).fold(0, |v, b| {
                let state = self
                    .slot_keys
                    .get(&b.path)
                    .map_or(0, |&k| self.sim.state(k));
                v | (((state >> b.lo) & mask(b.len)) << b.at)
            }),
            Stored::Word { memory, addr } => self
                .mem_keys
                .get(&memory.path)
                .and_then(|&m| self.sim.mem(m).get(*addr).copied())
                .unwrap_or(0),
        }
    }
}

/// `name` or `name.pin`.
fn pin_ref(part: &str, pin: Option<&str>) -> String {
    match pin {
        Some(pin) => format!("{part}.{pin}"),
        None => part.to_string(),
    }
}
