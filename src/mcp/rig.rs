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
//! `Screen`).

use std::collections::HashMap;
use std::sync::Arc;

use logi_sim::compile::{NodePath, Options, Program};
use logi_sim::netlist::{Design, NetId, NodeId};
use logi_sim::sim::Sim;
use uuid::Uuid;

use super::batch::{Names, Place, circuit_at, names_of};
use super::memfile;
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
}

/// A resolved signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub text: String,
    pub source: Source,
    pub width: u8,
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
    /// Inputs as last set, to carry over a recompile.
    inputs: HashMap<String, u64>,
    names: HashMap<Uuid, Names>,
}

fn compile(design: &Design, options: &Options) -> Result<Program, String> {
    logi_sim::compile::compile(design, options).map_err(|errors| {
        let loops = errors
            .iter()
            .any(|e| matches!(e, logi_sim::compile::CompileError::CombinationalLoop { .. }));
        if loops {
            "the circuit does not build: it has a loop of gates with no state in it".to_string()
        } else {
            format!(
                "the circuit does not build: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    })
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

    /// Compile `ws`, its root the circuit under test.
    pub fn of(ws: WorkspaceDocument) -> Result<Rig, String> {
        let (design, map) = design_map_of(&ws)?;
        let program = compile(&design, &Options::default())?;
        let sim = Sim::try_new(Arc::new(program)).map_err(|e| e.0)?;
        Ok(Rig {
            library: CircuitLibrary {
                definitions: ws.definitions.clone(),
            },
            ws,
            design,
            map,
            sim,
            probes: Vec::new(),
            debug: false,
            inputs: HashMap::new(),
            names: HashMap::new(),
        })
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

    /// Set a root input (seen by the logic at the next tick).
    pub fn set(&mut self, name: &str, value: u64) -> Result<(), String> {
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
        self.sim.set_input(port, value);
        self.inputs.insert(name.to_string(), value);
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

    pub fn snapshot(&self) -> logi_sim::sim::StateSnapshot {
        self.sim.export_state()
    }

    pub fn restore(&mut self, snapshot: &logi_sim::sim::StateSnapshot) {
        self.sim.import_state(snapshot);
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

    fn resolve_inner(&mut self, text: &str) -> Result<Signal, String> {
        let ws = self.ws.clone();
        let mut doc: &CircuitDocument = &ws.root;
        let mut path: Vec<NodeId> = Vec::new();
        let mut steps: Vec<&str> = text.split('/').map(str::trim).collect();
        let last = steps.pop().unwrap_or_default();
        for step in steps {
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
            Some(_) => return Err("name one element of the array".into()),
            None => r.node.to_string(),
        };
        let ids = self.part(doc, &whole)?;
        let [id] = ids.as_slice() else {
            return Err(format!(
                "\"{}\" is an array of {} parts; name one element, e.g. \"{}[0]\"",
                r.node,
                ids.len(),
                r.node
            ));
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
            (definition.input_net(node, pin as u8), ins[pin])
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
            let mut doc: &CircuitDocument = &ws.root;
            let mut path = Vec::new();
            let mut steps: Vec<&str> = text.split('/').map(str::trim).collect();
            let last = steps.pop().unwrap_or_default();
            for step in steps {
                let ids = rig.part(doc, step)?;
                let [id] = ids.as_slice() else {
                    return Err(format!("\"{step}\" names {} parts; pick one", ids.len()));
                };
                let GateKind::SubCircuit { definition } = doc.node(*id).expect("named").kind else {
                    return Err(format!("\"{step}\" is not a chip"));
                };
                path.push(rig.node(doc, *id)?);
                doc = ws
                    .definitions
                    .get(&definition)
                    .ok_or_else(|| format!("the definition of \"{step}\" is missing"))?;
            }
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
        let options = Options {
            debug: self.debug,
            probes: self.probes.clone(),
            ..Options::default()
        };
        let program = compile(&self.design, &options)?;
        let mut sim = Sim::try_new(Arc::new(program)).map_err(|e| e.0)?;
        for (name, value) in &self.inputs {
            if let Some(port) = sim.program().ports.input(name) {
                sim.set_input(port, *value);
            }
        }
        sim.import_state(&state);
        self.sim = sim;
        Ok(())
    }

    /// The value of a watched signal after the last tick.
    pub fn value(&self, signal: &Signal) -> u64 {
        match &signal.source {
            Source::Zero => 0,
            Source::Net { path, net, lo, len } => {
                (self.sim.read_net(path, *net).unwrap_or(0) >> lo) & mask(*len)
            }
            Source::Word { memory, addr } => {
                self.words(memory).get(*addr).copied().unwrap_or(0) & mask(memory.width)
            }
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
        self.sim.import_state(&state);
        Ok(())
    }
}

/// `name` or `name.pin`.
fn pin_ref(part: &str, pin: Option<&str>) -> String {
    match pin {
        Some(pin) => format!("{part}.{pin}"),
        None => part.to_string(),
    }
}
