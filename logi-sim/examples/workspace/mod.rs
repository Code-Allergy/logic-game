//! A gatequest workspace (`manifest.json`, `docs/save-format.md`) as a
//! `logi_sim` design, for examples that run without the game crate. It
//! builds the design the game's `sim::netlist::design_of` builds: one
//! definition per circuit, ports in interface order (sorted by label,
//! then id), `Display` nodes as outputs of the root and left out of
//! definitions, every wire a pin-to-pin connection. Also returns names
//! for nets (the label of the node driving the net, or of the output
//! port it feeds), for reports.
//!
//! The JSON reader is minimal: enough for the workspace format.

use std::collections::HashMap;

use logi_sim::netlist::{DefId, Design, NetId, NodeId, NodeKind, ScreenMode, mask};

/// A JSON value.
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    /// Integers are kept exact.
    Int(i128),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i128> {
        match self {
            Json::Int(i) => Some(*i),
            Json::Float(f) if f.fract() == 0.0 => Some(*f as i128),
            _ => None,
        }
    }
    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("JSON: {what} at byte {}", self.at))
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.s.get(self.at) == Some(&c) {
            self.at += 1;
            Ok(())
        } else {
            self.err(&format!("expected {:?}", c as char))
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.s.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut kv = Vec::new();
                self.ws();
                if self.s.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Obj(kv));
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.value()? else {
                        return self.err("expected a key");
                    };
                    self.eat(b':')?;
                    kv.push((k, self.value()?));
                    self.ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Obj(kv));
                        }
                        _ => return self.err("expected , or }"),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value()?);
                    self.ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return self.err("expected , or ]"),
                    }
                }
            }
            Some(b'"') => {
                self.at += 1;
                let mut out = String::new();
                loop {
                    let Some(&c) = self.s.get(self.at) else {
                        return self.err("unterminated string");
                    };
                    self.at += 1;
                    match c {
                        b'"' => return Ok(Json::Str(out)),
                        b'\\' => {
                            let e = self.s.get(self.at).copied().unwrap_or(b'?');
                            self.at += 1;
                            match e {
                                b'n' => out.push('\n'),
                                b't' => out.push('\t'),
                                b'r' => out.push('\r'),
                                b'b' => out.push('\u{8}'),
                                b'f' => out.push('\u{c}'),
                                b'u' => {
                                    let hex = std::str::from_utf8(&self.s[self.at..self.at + 4])
                                        .map_err(|e| e.to_string())?;
                                    self.at += 4;
                                    let cp =
                                        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                }
                                other => out.push(other as char),
                            }
                        }
                        _ => {
                            // Copy a UTF-8 run as is.
                            let start = self.at - 1;
                            while self.at < self.s.len() && !matches!(self.s[self.at], b'"' | b'\\')
                            {
                                self.at += 1;
                            }
                            out.push_str(
                                std::str::from_utf8(&self.s[start..self.at])
                                    .map_err(|e| e.to_string())?,
                            );
                        }
                    }
                }
            }
            Some(b't') if self.s[self.at..].starts_with(b"true") => {
                self.at += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.s[self.at..].starts_with(b"false") => {
                self.at += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.s[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Json::Null)
            }
            Some(_) => {
                let start = self.at;
                while self.at < self.s.len()
                    && matches!(
                        self.s[self.at],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.at += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.at]).map_err(|e| e.to_string())?;
                if let Ok(i) = t.parse::<i128>() {
                    Ok(Json::Int(i))
                } else if let Ok(f) = t.parse::<f64>() {
                    Ok(Json::Float(f))
                } else {
                    self.err("bad value")
                }
            }
            None => self.err("unexpected end"),
        }
    }
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        at: 0,
    };
    p.value()
}

/// A workspace's design and names.
pub struct Loaded {
    pub design: Design,
    /// Circuit name per definition.
    pub def_names: HashMap<DefId, String>,
    /// `(definition, net) -> name`.
    pub net_names: HashMap<(DefId, NetId), String>,
}

fn u8_of(k: &Json, key: &str) -> Result<u8, String> {
    k.get(key)
        .and_then(Json::int)
        .map(|v| v as u8)
        .ok_or_else(|| format!("missing {key}"))
}

fn words(k: &Json, key: &str) -> Vec<u64> {
    k.get(key)
        .map(|a| {
            a.arr()
                .iter()
                .map(|x| x.int().unwrap_or(0) as u64)
                .collect()
        })
        .unwrap_or_default()
}

/// The simulator kind of a node kind (`None`: a subcircuit or Display).
fn kind_of(k: &Json) -> Result<Option<NodeKind>, String> {
    let ty = k.get("type").and_then(Json::str).ok_or("node kind")?;
    let w = || u8_of(k, "w");
    Ok(Some(match ty {
        "Input" => NodeKind::Input { w: w()? },
        "Output" | "Display" => NodeKind::Output { w: w()? },
        "Const" => {
            let w = w()?;
            NodeKind::Const {
                w,
                value: k.get("value").and_then(Json::int).unwrap_or(0) as u64 & mask(w),
            }
        }
        "Not" => NodeKind::Not { w: w()? },
        "And" => NodeKind::And { w: w()? },
        "Or" => NodeKind::Or { w: w()? },
        "Xor" => NodeKind::Xor { w: w()? },
        "Nand" => NodeKind::Nand { w: w()? },
        "Nor" => NodeKind::Nor { w: w()? },
        "Xnor" => NodeKind::Xnor { w: w()? },
        "Mux" => NodeKind::Mux { w: w()? },
        "Switch" => NodeKind::Switch { w: w()? },
        "Add" => NodeKind::Add { w: w()? },
        "Eq" => NodeKind::Eq { w: w()? },
        "LtU" => NodeKind::LtU { w: w()? },
        "ReduceOr" => NodeKind::ReduceOr { w: w()? },
        "ReduceAnd" => NodeKind::ReduceAnd { w: w()? },
        "ReduceXor" => NodeKind::ReduceXor { w: w()? },
        "Split" => NodeKind::Split {
            w: w()?,
            lo: u8_of(k, "lo")?,
            len: u8_of(k, "len")?,
        },
        "Merge" => NodeKind::Merge {
            widths: words(k, "widths").into_iter().map(|x| x as u8).collect(),
        },
        "Extend" => NodeKind::Extend {
            from: u8_of(k, "from")?,
            to: u8_of(k, "to")?,
            signed: matches!(k.get("signed"), Some(Json::Bool(true))),
        },
        "Clock" => NodeKind::Clock {
            period: k.get("period").and_then(Json::int).unwrap_or(2) as u32,
            duty: k.get("duty").and_then(Json::int).unwrap_or(1) as u32,
            phase: k.get("phase").and_then(Json::int).unwrap_or(0) as u32,
        },
        "Rom" => NodeKind::Rom {
            addr_bits: u8_of(k, "addr_bits")?,
            w: w()?,
            contents: words(k, "contents").into(),
        },
        "Delay" => NodeKind::Delay { w: w()? },
        "Register" => NodeKind::Register { w: w()? },
        "DLatch" => NodeKind::DLatch { w: w()? },
        "SrLatch" => NodeKind::SrLatch,
        "Ram" => NodeKind::Ram {
            addr_bits: u8_of(k, "addr_bits")?,
            w: w()?,
            init: Vec::new().into(),
        },
        "Ram2" => NodeKind::Ram2 {
            addr_bits: u8_of(k, "addr_bits")?,
            w: w()?,
            reads: u8_of(k, "reads")?,
            mask: matches!(k.get("mask"), Some(Json::Bool(true))),
            init: words(k, "init").into(),
        },
        "Screen" => {
            let side = |key: &str| {
                let s = k.get(key).and_then(Json::int).unwrap_or(0) as u16;
                if s.is_power_of_two() {
                    s.trailing_zeros() as u8
                } else {
                    0
                }
            };
            let mode = k.get("mode").and_then(Json::str).unwrap_or("");
            let bits = match k.get("palette").and_then(Json::str).unwrap_or("") {
                "Mono" => 1,
                "Gray" => 2,
                "Colors16" => 4,
                _ => 8,
            };
            NodeKind::Screen {
                mode: match mode {
                    "Pixel" => ScreenMode::Pixel,
                    "Scanline" => ScreenMode::Scan,
                    _ => ScreenMode::Memory,
                },
                x_bits: side("width"),
                y_bits: side("height"),
                w: if mode == "Text" { 8 } else { bits },
            }
        }
        "SubCircuit" => return Ok(None),
        other => return Err(format!("unknown node kind {other}")),
    }))
}

/// Load `folder/manifest.json`.
pub fn load(folder: &std::path::Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(folder.join("manifest.json")).map_err(|e| e.to_string())?;
    let ws = parse(&text)?;
    let circuits: HashMap<String, &Json> = ws
        .get("circuits")
        .ok_or("no circuits")?
        .arr()
        .iter()
        .filter_map(|c| Some((c.get("id")?.str()?.to_string(), c)))
        .collect();
    let root_id = ws
        .get("root")
        .and_then(Json::str)
        .ok_or("no root")?
        .to_string();
    let root = circuits.get(&root_id).ok_or("root circuit missing")?;
    let mut out = Loaded {
        design: Design::new(root.get("name").and_then(Json::str).unwrap_or("root")),
        def_names: HashMap::new(),
        net_names: HashMap::new(),
    };
    let mut defs: HashMap<String, DefId> = HashMap::new();
    let root_def = out.design.root;
    // Bottom-up: definitions before the circuits that instance them.
    let mut order: Vec<String> = Vec::new();
    let mut state: HashMap<String, u8> = HashMap::new();
    let mut stack = vec![(root_id.clone(), false)];
    while let Some((id, done)) = stack.pop() {
        if done {
            order.push(id.clone());
            state.insert(id, 2);
            continue;
        }
        match state.get(&id) {
            Some(2) => continue,
            Some(1) => return Err(format!("circuit {id} contains itself")),
            _ => {}
        }
        state.insert(id.clone(), 1);
        stack.push((id.clone(), true));
        let c = circuits
            .get(&id)
            .ok_or_else(|| format!("circuit {id} missing"))?;
        for n in c.get("nodes").map(Json::arr).unwrap_or(&[]) {
            if let Some(k) = n.get("kind")
                && k.get("type").and_then(Json::str) == Some("SubCircuit")
            {
                let child = k
                    .get("definition")
                    .and_then(Json::str)
                    .ok_or("definition")?;
                if state.get(child) != Some(&2) {
                    stack.push((child.to_string(), false));
                }
            }
        }
    }
    for id in &order {
        let c = circuits[id];
        let name = c.get("name").and_then(Json::str).unwrap_or("").to_string();
        let is_root = *id == root_id;
        let def = if is_root {
            root_def
        } else {
            out.design.add_definition(name.clone())
        };
        out.def_names.insert(def, name.clone());
        defs.insert(id.clone(), def);
        let what = |e: &dyn std::fmt::Display| format!("{name}: {e}");
        let nodes_json = c.get("nodes").map(Json::arr).unwrap_or(&[]);
        let label = |n: &Json| n.get("label").and_then(Json::str).map(str::to_string);
        let node_id = |n: &Json| n.get("id").and_then(Json::str).unwrap_or("").to_string();
        fn ty(n: &Json) -> &str {
            n.get("kind")
                .and_then(|k| k.get("type"))
                .and_then(Json::str)
                .unwrap_or("")
        }
        // Ports in interface order.
        let mut ports: Vec<Vec<&Json>> = vec![Vec::new(), Vec::new()];
        for n in nodes_json {
            match ty(n) {
                "Input" => ports[0].push(n),
                "Output" => ports[1].push(n),
                _ => {}
            }
        }
        for p in &mut ports {
            p.sort_by_key(|n| (label(n).unwrap_or_default(), node_id(n)));
        }
        let mut nodes: HashMap<String, NodeId> = HashMap::new();
        let mut labels: Vec<(NodeId, String, bool)> = Vec::new();
        for n in ports.iter().flatten() {
            let kind = kind_of(n.get("kind").ok_or("kind")?)?.ok_or("bad port")?;
            let node = out.design.add_node(def, kind).map_err(|e| what(&e))?;
            out.design
                .set_name(def, node, label(n))
                .map_err(|e| what(&e))?;
            nodes.insert(node_id(n), node);
            if let Some(l) = label(n) {
                labels.push((node, l, ty(n) == "Output"));
            }
        }
        for n in nodes_json {
            let nid = node_id(n);
            if nodes.contains_key(&nid) {
                continue;
            }
            let k = n.get("kind").ok_or("kind")?;
            let node = match ty(n) {
                "Display" if !is_root => continue,
                "SubCircuit" => {
                    let child = k
                        .get("definition")
                        .and_then(Json::str)
                        .ok_or("definition")?;
                    let child = *defs.get(child).ok_or("child definition not built")?;
                    out.design.add_instance(def, child).map_err(|e| what(&e))?
                }
                t => {
                    let kind = kind_of(k)?.ok_or_else(|| what(&format!("{t} is not simulated")))?;
                    let node = out.design.add_node(def, kind).map_err(|e| what(&e))?;
                    if t == "Display" {
                        out.design
                            .set_name(def, node, label(n))
                            .map_err(|e| what(&e))?;
                    }
                    node
                }
            };
            if let Some(l) = label(n)
                && ty(n) != "SubCircuit"
            {
                labels.push((node, l, ty(n) == "Display"));
            }
            nodes.insert(nid, node);
        }
        for w in c.get("wires").map(Json::arr).unwrap_or(&[]) {
            let end = |k: &str| -> Option<(NodeId, u8)> {
                let e = w.get(k)?;
                let node = *nodes.get(e.get("node")?.str()?)?;
                Some((node, e.get("pin")?.int()? as u8))
            };
            let (Some(src), Some(dst)) = (end("from"), end("to")) else {
                continue;
            };
            out.design
                .connect_pins(def, src, dst)
                .map_err(|e| what(&e))?;
        }
        // Net names: a labelled node's output, or an output port's input.
        let d = out.design.def(def).map_err(|e| what(&e))?;
        for (node, l, sink) in labels {
            let net = if sink {
                d.input_net(node, 0)
            } else {
                d.output_net(node, 0)
            };
            if let Some(net) = net {
                out.net_names.entry((def, net)).or_insert(l);
            }
        }
    }
    Ok(out)
}
