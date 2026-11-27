//! What the compiler makes of a circuit (`docs/mcp-server.md`, "What
//! the compiler did").
//!
//! - [`compile`]: a workspace compiled as its root, with build errors
//!   that name parts by label path (`alu/n3`, `fa[2]/h1/x`) instead of
//!   the simulator's node ids. Every place the server builds a circuit
//!   goes through it.
//! - [`insight`] and [`view`]: which chips the function matcher
//!   recognised, and as what, and how large the program is, with and
//!   without the word-level passes (`view {"what": "compiled"}`).
//! - The stored test kinds `compiled` (the compiler's result must stay
//!   as pinned) and `unoptimized` (the chip must behave the same with
//!   function matching, word lifting and word cleanup off), run from
//!   [`super::testing`].

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use logi_sim::compile::func::{self, MatchCache};
use logi_sim::compile::{CompileError, NodePath, Options, Program};
use logi_sim::netlist::{DefId, Design, NodeId, NodeKind, ValidationError};
use logi_sim::sim::Sim;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::batch::{Names, Place, circuit_at, names_of};
use super::testing::{self, Sampling};
use crate::sim::netlist::{DesignMap, design_map_of};
use crate::sim::{CircuitDocument, CircuitLibrary, GateKind, WorkspaceDocument};

/// Parts of one loop named in a build error before the rest are counted.
const LOOP_NAMES_SHOWN: usize = 12;

/// The word-level passes an `unoptimized` test can turn off, by name.
pub const PASSES: [&str; 3] = ["func_match", "lift", "words"];

/// A workspace compiled as its root.
pub struct Built {
    pub design: Design,
    pub map: DesignMap,
    pub program: Program,
}

/// Compile `ws` (its root as the design's root) with `options`. The
/// errors name parts by label path.
pub fn compile(ws: &WorkspaceDocument, options: &Options) -> Result<Built, Vec<String>> {
    let (design, map) = design_map_of(ws).map_err(|e| vec![e])?;
    let program = compile_design(ws, &design, &map, options)?;
    Ok(Built {
        design,
        map,
        program,
    })
}

/// Compile the design of `ws` already made ([`design_map_of`]), naming
/// the parts of any error.
pub fn compile_design(
    ws: &WorkspaceDocument,
    design: &Design,
    map: &DesignMap,
    options: &Options,
) -> Result<Program, Vec<String>> {
    logi_sim::compile::compile(design, options).map_err(|e| named_errors(ws, design, map, &e))
}

/// The build errors of one sentence each, as one line.
pub fn one_line(errors: &[String]) -> String {
    errors.join("; ")
}

/// `errors` of compiling the design of `ws`, with parts named, sorted.
pub fn named_errors(
    ws: &WorkspaceDocument,
    design: &Design,
    map: &DesignMap,
    errors: &[CompileError],
) -> Vec<String> {
    let mut namer = Namer::new(ws, design, map);
    let mut named: Vec<String> = errors
        .iter()
        .map(|e| match e {
            CompileError::CombinationalLoop { nodes } => namer.loop_text(nodes),
            CompileError::Invalid(v) => namer.invalid(v),
        })
        .collect();
    named.sort();
    named
}

/// Names parts of a compiled workspace by label path.
struct Namer<'a> {
    ws: &'a WorkspaceDocument,
    design: &'a Design,
    library: CircuitLibrary,
    /// Definition -> its circuit.
    circuits: HashMap<DefId, Uuid>,
    /// Circuit -> node -> part.
    parts: HashMap<Uuid, HashMap<NodeId, Uuid>>,
    names: HashMap<Uuid, Names>,
}

impl<'a> Namer<'a> {
    fn new(ws: &'a WorkspaceDocument, design: &'a Design, map: &DesignMap) -> Self {
        Namer {
            ws,
            design,
            library: CircuitLibrary {
                definitions: ws.definitions.clone(),
            },
            circuits: map.defs.iter().map(|(c, d)| (*d, *c)).collect(),
            parts: map
                .nodes
                .iter()
                .map(|(c, nodes)| (*c, nodes.iter().map(|(p, n)| (*n, *p)).collect()))
                .collect(),
            names: HashMap::new(),
        }
    }

    fn doc(&self, circuit: Uuid) -> Option<&'a CircuitDocument> {
        if circuit == self.ws.root.id {
            Some(&self.ws.root)
        } else {
            self.ws.definitions.get(&circuit).map(|d| &**d)
        }
    }

    /// How `node` of circuit `doc` is referred to, and the part.
    fn part(&mut self, doc: &CircuitDocument, node: NodeId) -> Option<(String, Uuid)> {
        let part = *self.parts.get(&doc.id)?.get(&node)?;
        let library = &self.library;
        let names = self
            .names
            .entry(doc.id)
            .or_insert_with(|| names_of(doc, library));
        Some((names.reference.get(&part)?.clone(), part))
    }

    /// `alu/fa[2]/x`: the label path of a node, from the root down.
    fn path(&mut self, path: &NodePath) -> String {
        let mut doc = &self.ws.root;
        let mut steps = Vec::new();
        for &instance in &path.instances {
            let Some((name, part)) = self.part(doc, instance) else {
                steps.push("?".to_string());
                return steps.join("/");
            };
            steps.push(name);
            let next = match doc.node(part).map(|n| &n.kind) {
                Some(GateKind::SubCircuit { definition }) => self.doc(*definition),
                _ => None,
            };
            let Some(next) = next else {
                steps.push("?".to_string());
                return steps.join("/");
            };
            doc = next;
        }
        steps.push(
            self.part(doc, path.node)
                .map_or_else(|| "?".to_string(), |(name, _)| name),
        );
        steps.join("/")
    }

    /// `a loop of gates with no state in it: a -> b -> a`: along the loop
    /// when it is one simple cycle inside one circuit, else sorted.
    fn loop_text(&mut self, nodes: &[NodePath]) -> String {
        let order = self.cycle_order(nodes);
        let ordered = order.is_some();
        let paths: Vec<&NodePath> = match order {
            Some(order) => order.into_iter().map(|i| &nodes[i]).collect(),
            None => nodes.iter().collect(),
        };
        let mut names: Vec<String> = paths.iter().map(|p| self.path(p)).collect();
        if !ordered {
            names.sort();
        }
        let count = names.len();
        let shown = &names[..count.min(LOOP_NAMES_SHOWN)];
        let more = count - shown.len();
        let list = if ordered {
            let mut text = shown.join(" -> ");
            if more == 0 {
                let _ = write!(text, " -> {}", names[0]);
            } else {
                let _ = write!(text, " -> ... ({more} more on the loop)");
            }
            text
        } else {
            let mut text = shown.join(", ");
            if more > 0 {
                let _ = write!(text, " and {more} more");
            }
            text
        };
        format!(
            "a loop of gates with no state in it: {list} (put a Delay or a Register on the loop)"
        )
    }

    /// The order of `nodes` along their loop, when they all sit in one
    /// circuit and each drives exactly one of the others.
    fn cycle_order(&self, nodes: &[NodePath]) -> Option<Vec<usize>> {
        let first = nodes.first()?;
        if nodes.len() < 2 || nodes.iter().any(|n| n.instances != first.instances) {
            return None;
        }
        let mut def = self.design.root;
        for &instance in &first.instances {
            match self.design.defs.get(def)?.nodes.get(instance)?.kind {
                NodeKind::Instance(child) => def = child,
                _ => return None,
            }
        }
        let def = self.design.defs.get(def)?;
        // Each node's one successor on the loop.
        let mut next = vec![None; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            let outs = &def.nodes.get(n.node)?.outputs;
            let mut found = None;
            for (j, m) in nodes.iter().enumerate() {
                let reads = def
                    .nodes
                    .get(m.node)?
                    .inputs
                    .iter()
                    .flatten()
                    .any(|net| outs.contains(net));
                if reads {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(j);
                }
            }
            next[i] = Some(found?);
        }
        let mut order = vec![0];
        let mut at = next[0]?;
        while at != 0 {
            if order.len() >= nodes.len() {
                return None;
            }
            order.push(at);
            at = next[at]?;
        }
        (order.len() == nodes.len()).then_some(order)
    }

    /// `"alu": part "x"`, for a node of a definition.
    fn at(&mut self, def: DefId, node: NodeId) -> String {
        let doc = self.circuits.get(&def).and_then(|c| self.doc(*c));
        let Some(doc) = doc else {
            return "a part".into();
        };
        // The root's name: the workspace's, or the chip's when a chip is
        // compiled alone.
        let circuit = doc.name.clone();
        match self.part(doc, node) {
            Some((name, _)) => format!("\"{circuit}\": part \"{name}\""),
            None => format!("\"{circuit}\": a part"),
        }
    }

    fn invalid(&mut self, e: &ValidationError) -> String {
        use ValidationError as V;
        match e {
            V::BadParams { def, node, reason } => {
                format!("{}: bad parameters: {reason}", self.at(*def, *node))
            }
            V::PinCount {
                def,
                node,
                expected_inputs,
                inputs,
                expected_outputs,
                outputs,
            } => format!(
                "{}: has {inputs} input and {outputs} output pins; its kind has \
                 {expected_inputs} and {expected_outputs}",
                self.at(*def, *node)
            ),
            V::WidthMismatch {
                def,
                node,
                pin,
                expected,
                actual,
            } => format!(
                "{}: input pin {pin} takes {expected} bits but its wire has {actual}",
                self.at(*def, *node)
            ),
            V::MissingDefinition { def, node, .. } => {
                format!("{}: its chip definition is missing", self.at(*def, *node))
            }
            V::DanglingNet { def, node, pin, .. } => format!(
                "{}: input pin {pin} is wired to a net that is not there",
                self.at(*def, *node)
            ),
            V::BadPort { def, node } => format!("{}: not a valid port", self.at(*def, *node)),
            other => format!("invalid design: {other:?}"),
        }
    }
}

/// What the function matcher made of one chip definition.
#[derive(Debug, Clone, PartialEq)]
pub enum Match {
    /// Replaced by word primitives; the matcher's description of them.
    Matched(String),
    /// Kept as its gates; why.
    Unmatched(String),
    /// Not looked at: a clock or memory is inside it (or in a chip in it).
    NotAnalysed,
}

/// The size of a compiled program.
#[derive(Debug, Clone, PartialEq)]
pub struct Size {
    /// Instructions, packed lanes counted one each.
    pub instructions: usize,
    pub by_op: BTreeMap<String, usize>,
    /// State words committed each tick.
    pub state_words: usize,
    pub slots: u32,
    pub memories: usize,
    /// Ranks of the settle (its critical path).
    pub depth: u32,
}

impl Size {
    fn of(program: &Program) -> Size {
        let mut by_op: BTreeMap<String, usize> = BTreeMap::new();
        for b in &program.blocks {
            let range = b.start as usize..(b.start + b.len) as usize;
            let n = if b.packed {
                program.packs[range]
                    .iter()
                    .map(|p| p.lanes.count_ones() as usize)
                    .sum()
            } else {
                b.len as usize
            };
            *by_op.entry(format!("{:?}", b.op)).or_default() += n;
        }
        Size {
            instructions: program.work(),
            by_op,
            state_words: program
                .commits
                .iter()
                .filter(|c| matches!(c, logi_sim::compile::Commit::Copy { .. }))
                .count(),
            slots: program.slot_count,
            memories: program.mems.len(),
            depth: program.depth(),
        }
    }
}

/// What the compiler did with one circuit compiled as a root.
#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    /// The circuit (`main` for the root).
    pub circuit: String,
    /// Every chip definition the circuit reaches, by name.
    pub chips: BTreeMap<String, Match>,
    /// With the default options.
    pub size: Size,
    /// Instructions with `func_match`, `lift` and `words` off.
    pub plain_instructions: usize,
}

/// Options with the passes named in `off` turned off.
pub fn options_without(off: &[String]) -> Options {
    let mut options = Options::default();
    for pass in off {
        match pass.as_str() {
            "func_match" => options.func_match = false,
            "lift" => options.lift = false,
            "words" => options.words = false,
            _ => {}
        }
    }
    options
}

fn all_passes() -> Vec<String> {
    PASSES.map(String::from).to_vec()
}

fn circuit_name(ws: &WorkspaceDocument, place: Place) -> String {
    match place {
        Place::Root => "main".into(),
        Place::Definition(_) => circuit_at(ws, place).name.clone(),
    }
}

/// Compile the circuit at `place` as a root, as tests do, and say what
/// the compiler did.
pub fn insight(ws: &WorkspaceDocument, place: Place) -> Result<Insight, String> {
    let rooted = testing::rooted(ws, place);
    let (design, map) = design_map_of(&rooted)?;
    let fail = |errors: Vec<CompileError>| {
        format!(
            "it does not build: {}",
            one_line(&named_errors(&rooted, &design, &map, &errors))
        )
    };
    let mut cache = MatchCache::new();
    let program =
        logi_sim::compile::compile_cached(&design, &Options::default(), &mut cache, |_| {})
            .map_err(fail)?;
    // The cache holds every analysis the compile made, so this reads it.
    let analysed: HashMap<String, (bool, String)> = func::report(&design, &mut cache)
        .into_iter()
        .map(|(name, matched, note)| (name, (matched, note)))
        .collect();
    let mut chips = BTreeMap::new();
    for def in design.defs_bottom_up() {
        if def == design.root {
            continue;
        }
        let name = design.defs[def].name.clone();
        let found = match analysed.get(&name) {
            Some((true, note)) => Match::Matched(note.clone()),
            Some((false, note)) => Match::Unmatched(note.clone()),
            None => Match::NotAnalysed,
        };
        chips.insert(name, found);
    }
    let plain =
        logi_sim::compile::compile(&design, &options_without(&all_passes())).map_err(fail)?;
    Ok(Insight {
        circuit: circuit_name(ws, place),
        chips,
        size: Size::of(&program),
        plain_instructions: plain.work(),
    })
}

impl Insight {
    /// A `compiled` test that holds for this result: the chips that
    /// matched must match, and the program may not grow.
    pub fn pinning_test(&self) -> Value {
        let matched: Vec<&String> = self
            .chips
            .iter()
            .filter(|(_, m)| matches!(m, Match::Matched(_)))
            .map(|(name, _)| name)
            .collect();
        let mut body = Map::new();
        if !matched.is_empty() {
            body.insert("matched".into(), json!(matched));
        }
        body.insert(
            "instructions".into(),
            json!({"max": self.size.instructions}),
        );
        json!({"name": "compiled", "compiled": body})
    }
}

/// `view {"what": "compiled"}`: the text of [`insight`].
pub fn view(ws: &WorkspaceDocument, place: Place) -> Result<String, String> {
    let i = insight(ws, place)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "compiled \"{}\" as a root ({} chip definition(s) reached; the root itself is never \
         replaced)",
        i.circuit,
        i.chips.len()
    );
    for (name, m) in &i.chips {
        let _ = match m {
            Match::Matched(note) => writeln!(out, "  {name}: matched as {note}"),
            Match::Unmatched(why) => writeln!(out, "  {name}: not matched: {why}"),
            Match::NotAnalysed => writeln!(
                out,
                "  {name}: not analysed (a clock or memory is inside it, or in a chip in it)"
            ),
        };
    }
    let s = &i.size;
    let ops: Vec<String> = s.by_op.iter().map(|(op, n)| format!("{op} {n}")).collect();
    let _ = writeln!(
        out,
        "program: {} instructions ({}), {} state words, {} slots, {} memories, depth {}",
        s.instructions,
        if ops.is_empty() {
            "none".to_string()
        } else {
            ops.join(", ")
        },
        s.state_words,
        s.slots,
        s.memories,
        s.depth
    );
    let _ = writeln!(
        out,
        "without func_match, lift and words: {} instructions",
        i.plain_instructions
    );
    let _ = writeln!(
        out,
        "a test that pins this (edit op \"tests\"):\n  {}",
        i.pinning_test()
    );
    Ok(out)
}

/// What a `compiled` test asks of one chip that must match.
#[derive(Debug, Clone, PartialEq)]
enum Want {
    Any,
    Containing(String),
    Exactly(String),
}

/// A `compiled` test.
#[derive(Debug, Clone, PartialEq)]
struct Spec {
    matched: Vec<(String, Want)>,
    unmatched: Vec<String>,
    min: Option<usize>,
    max: Option<usize>,
    exactly: Option<usize>,
}

const COMPILED_SHAPE: &str = "\"compiled\" takes {\"matched\": [chips] or {chip: text}, \
                              \"unmatched\": [chips], \"instructions\": {\"max\": n}}";

fn spec(body: &Value) -> Result<Spec, String> {
    let Value::Object(body) = body else {
        return Err(COMPILED_SHAPE.into());
    };
    if let Some(k) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "matched" | "unmatched" | "instructions"))
    {
        return Err(format!(
            "unknown field \"{k}\" (compiled has matched, unmatched, instructions)"
        ));
    }
    if body.is_empty() {
        return Err(COMPILED_SHAPE.into());
    }
    let names = |key: &str, v: &Value| -> Result<Vec<String>, String> {
        match v {
            Value::Array(items) => items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(String::from)
                        .ok_or_else(|| format!("\"{key}\" lists chip names (strings)"))
                })
                .collect(),
            _ => Err(format!("\"{key}\" lists chip names (strings)")),
        }
    };
    let matched = match body.get("matched") {
        None => Vec::new(),
        Some(Value::Object(chips)) => chips
            .iter()
            .map(|(name, want)| {
                let want = match want {
                    Value::Bool(true) => Want::Any,
                    Value::String(s) if s.is_empty() => Want::Any,
                    Value::String(s) => Want::Containing(s.clone()),
                    Value::Object(o) if o.len() == 1 && o.contains_key("exactly") => {
                        match &o["exactly"] {
                            Value::String(s) => Want::Exactly(s.clone()),
                            _ => return Err("\"exactly\" takes the text of the match".into()),
                        }
                    }
                    _ => {
                        return Err(format!(
                            "\"matched\": \"{name}\" takes true, a text the match must contain, \
                             or {{\"exactly\": text}}"
                        ));
                    }
                };
                Ok((name.clone(), want))
            })
            .collect::<Result<_, String>>()?,
        Some(v) => names("matched", v)?
            .into_iter()
            .map(|n| (n, Want::Any))
            .collect(),
    };
    let unmatched = match body.get("unmatched") {
        None => Vec::new(),
        Some(v) => names("unmatched", v)?,
    };
    if let Some((name, _)) = matched.iter().find(|(n, _)| unmatched.contains(n)) {
        return Err(format!("\"{name}\" is both matched and unmatched"));
    }
    let (mut min, mut max, mut exactly) = (None, None, None);
    match body.get("instructions") {
        None => {}
        Some(Value::Object(bounds)) if !bounds.is_empty() => {
            for (k, v) in bounds {
                let n = v
                    .as_u64()
                    .ok_or_else(|| format!("\"instructions\": \"{k}\" must be a whole number"))?
                    as usize;
                match k.as_str() {
                    "min" => min = Some(n),
                    "max" => max = Some(n),
                    "exactly" => exactly = Some(n),
                    _ => {
                        return Err(format!(
                            "\"instructions\": unknown field \"{k}\" (it has max, min, exactly)"
                        ));
                    }
                }
            }
        }
        Some(_) => {
            return Err(
                "\"instructions\" takes {\"max\": n}, {\"min\": n} or {\"exactly\": n}".into(),
            );
        }
    }
    Ok(Spec {
        matched,
        unmatched,
        min,
        max,
        exactly,
    })
}

/// Check a `compiled` test: its shape, and that the chips it names are
/// chips the circuit at `place` reaches.
pub fn validate_compiled(ws: &WorkspaceDocument, place: Place, body: &Value) -> Result<(), String> {
    let spec = spec(body)?;
    let doc = circuit_at(ws, place);
    let library = CircuitLibrary {
        definitions: ws.definitions.clone(),
    };
    let reached: Vec<String> = library
        .reachable_from(doc)
        .values()
        .map(|d| d.name.clone())
        .collect();
    let named = spec.matched.iter().map(|(n, _)| n).chain(&spec.unmatched);
    for name in named {
        if !reached.contains(name) {
            return Err(if *name == doc.name {
                format!(
                    "\"{name}\" is this circuit: it is compiled as the root, which is never \
                     replaced; test it from a circuit that places it"
                )
            } else {
                format!("this circuit places no chip \"{name}\" (at any depth)")
            });
        }
    }
    Ok(())
}

/// Run a `compiled` test: the checks it made, or every one that failed.
pub fn run_compiled(ws: &WorkspaceDocument, place: Place, body: &Value) -> Result<u64, String> {
    let spec = spec(body)?;
    let i = insight(ws, place)?;
    let mut wrong = Vec::new();
    let mut checks = 0;
    for (name, want) in &spec.matched {
        checks += 1;
        match (i.chips.get(name), want) {
            (None, _) => wrong.push(format!("\"{name}\" is not reached any more")),
            (Some(Match::Matched(_)), Want::Any) => {}
            (Some(Match::Matched(note)), Want::Containing(text)) => {
                if !note.contains(text.as_str()) {
                    wrong.push(format!(
                        "\"{name}\" matched as {note}, which does not contain \"{text}\""
                    ));
                }
            }
            (Some(Match::Matched(note)), Want::Exactly(text)) => {
                if note != text {
                    wrong.push(format!(
                        "\"{name}\" matched as {note}, not exactly as {text}"
                    ));
                }
            }
            (Some(Match::Unmatched(why)), _) => {
                wrong.push(format!("\"{name}\" did not match: {why}"));
            }
            (Some(Match::NotAnalysed), _) => wrong.push(format!(
                "\"{name}\" was not analysed: a clock or memory is inside it"
            )),
        }
    }
    for name in &spec.unmatched {
        checks += 1;
        match i.chips.get(name) {
            None => wrong.push(format!("\"{name}\" is not reached any more")),
            Some(Match::Matched(note)) => {
                wrong.push(format!("\"{name}\" must not match, but matched as {note}"));
            }
            Some(_) => {}
        }
    }
    let n = i.size.instructions;
    if let Some(max) = spec.max {
        checks += 1;
        if n > max {
            wrong.push(format!("{n} instructions, more than the {max} allowed"));
        }
    }
    if let Some(min) = spec.min {
        checks += 1;
        if n < min {
            wrong.push(format!("{n} instructions, fewer than the {min} expected"));
        }
    }
    if let Some(exactly) = spec.exactly {
        checks += 1;
        if n != exactly {
            wrong.push(format!("{n} instructions, not exactly {exactly}"));
        }
    }
    if wrong.is_empty() {
        Ok(checks)
    } else {
        Err(wrong.join("; "))
    }
}

/// An `unoptimized` test.
struct Unoptimized {
    sampling: Sampling,
    off: Vec<String>,
}

fn unoptimized(body: &Value) -> Result<Unoptimized, String> {
    let Value::Object(body) = body else {
        return Err(
            "\"unoptimized\" takes {\"samples\": n, \"seed\": s, \"hold\": k, \"off\": [passes]}"
                .into(),
        );
    };
    if let Some(k) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "samples" | "seed" | "hold" | "off"))
    {
        return Err(format!(
            "unknown field \"{k}\" (unoptimized has samples, seed, hold, off)"
        ));
    }
    let off = match body.get("off") {
        None => all_passes(),
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut off = Vec::new();
            for item in items {
                match item.as_str() {
                    Some(p) if PASSES.contains(&p) => {
                        if !off.iter().any(|o| o == p) {
                            off.push(p.to_string());
                        }
                    }
                    _ => {
                        return Err(format!(
                            "\"off\" lists passes: {} (not {item})",
                            PASSES.join(", ")
                        ));
                    }
                }
            }
            off
        }
        Some(_) => {
            return Err(format!(
                "\"off\" lists passes to turn off: some of {}",
                PASSES.join(", ")
            ));
        }
    };
    Ok(Unoptimized {
        sampling: testing::sampling(body)?,
        off,
    })
}

/// Check an `unoptimized` test's shape.
pub fn validate_unoptimized(body: &Value) -> Result<(), String> {
    unoptimized(body).map(|_| ())
}

fn sim_of(ws: &WorkspaceDocument, options: &Options) -> Result<Sim, String> {
    let built = compile(ws, options).map_err(|e| format!("it does not build: {}", one_line(&e)))?;
    Sim::try_new(Arc::new(built.program)).map_err(|e| e.0)
}

/// Run an `unoptimized` test: the chip compiled as usual against the same
/// chip with the named passes off, on the same random inputs.
pub fn run_unoptimized(ws: &WorkspaceDocument, place: Place, body: &Value) -> Result<u64, String> {
    let u = unoptimized(body)?;
    let rooted = testing::rooted(ws, place);
    let (inputs, outputs) = testing::ports(&rooted.root);
    let mut a = sim_of(&rooted, &Options::default())?;
    let mut b = sim_of(&rooted, &options_without(&u.off))?;
    let without = format!("with {} off", u.off.join(", "));
    testing::compare(
        &mut a,
        &mut b,
        &inputs,
        &outputs,
        &u.sampling,
        ("as compiled", &without),
    )
}
