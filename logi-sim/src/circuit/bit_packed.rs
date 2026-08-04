use crate::circuit::{Circuit, FlattenedCircuit, NodeId, NodeKind, WireId};
use smallvec::SmallVec;
use std::collections::{HashMap, VecDeque};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdOp {
    And = 0,
    Or = 1,
    Not = 2,
    Xor = 3,
    Nand = 4,
    Nor = 5,
    Xnor = 6,
    Buffer = 7,
    Mux = 8,
    Demux = 9,
    TriStateBuffer = 10,
}

pub trait SimdIndex: Copy + std::fmt::Debug + Send + Sync + 'static {
    fn from_usize(value: usize) -> Self;
    fn as_usize(self) -> usize;
}

impl SimdIndex for u16 {
    fn from_usize(value: usize) -> Self {
        value as u16
    }

    fn as_usize(self) -> usize {
        self as usize
    }
}

impl SimdIndex for u32 {
    fn from_usize(value: usize) -> Self {
        value as u32
    }

    fn as_usize(self) -> usize {
        self as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdUnaryInstruction<I> {
    pub input: I,
    pub output: I,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdBinaryInstruction<I> {
    pub in0: I,
    pub in1: I,
    pub output: I,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdTernaryInstruction<I> {
    pub in0: I,
    pub in1: I,
    pub in2: I,
    pub output: I,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdDemuxInstruction<I> {
    pub input: I,
    pub select: I,
    pub out_false: I,
    pub out_true: I,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdClock<I> {
    pub period_ticks: u32,
    pub duty_ticks: u32,
    pub phase: u32,
    pub output: I,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SimdBlock {
    pub op: SimdOp,
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Debug)]
pub struct SimdProgram<I> {
    pub blocks: Vec<SimdBlock>,
    pub unary: Vec<SimdUnaryInstruction<I>>,
    pub binary: Vec<SimdBinaryInstruction<I>>,
    pub ternary: Vec<SimdTernaryInstruction<I>>,
    pub demux: Vec<SimdDemuxInstruction<I>>,
    pub clocks: Vec<SimdClock<I>>,
}

impl<I> Default for SimdProgram<I> {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            unary: Vec::new(),
            binary: Vec::new(),
            ternary: Vec::new(),
            demux: Vec::new(),
            clocks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SimdPrograms {
    Compact(SimdProgram<u16>),
    Wide(SimdProgram<u32>),
}

/// Level 3 bit-packed SIMD representation. Most circuits use compact 16-bit
/// wire indices; circuits above that limit transparently use 32-bit records.
#[derive(Clone, Debug)]
pub struct BitPackedCircuit {
    pub programs: SimdPrograms,
    pub wire_count: usize,
    pub node_to_idx: HashMap<NodeId, usize>,
    pub wire_to_idx: HashMap<WireId, usize>,
    pub idx_to_wire: Vec<WireId>,
    pub node_input_pins: HashMap<(NodeId, u8), usize>,
    pub node_output_pins: HashMap<(NodeId, u8), SmallVec<[usize; 2]>>,
}

fn node_op(kind: &NodeKind) -> Option<SimdOp> {
    Some(match kind {
        NodeKind::And => SimdOp::And,
        NodeKind::Or => SimdOp::Or,
        NodeKind::Not => SimdOp::Not,
        NodeKind::Xor => SimdOp::Xor,
        NodeKind::Nand => SimdOp::Nand,
        NodeKind::Nor => SimdOp::Nor,
        NodeKind::Xnor => SimdOp::Xnor,
        NodeKind::Buffer => SimdOp::Buffer,
        NodeKind::Mux => SimdOp::Mux,
        NodeKind::Demux => SimdOp::Demux,
        NodeKind::TriStateBuffer => SimdOp::TriStateBuffer,
        NodeKind::Input | NodeKind::Output | NodeKind::Clock { .. } | NodeKind::SubCircuit(_) => {
            return None;
        }
    })
}

fn push_block(blocks: &mut Vec<SimdBlock>, op: SimdOp, start: usize) {
    if let Some(last) = blocks.last_mut()
        && last.op == op
        && last.start as usize + last.len as usize == start
    {
        last.len += 1;
    } else {
        blocks.push(SimdBlock {
            op,
            start: start as u32,
            len: 1,
        });
    }
}

fn build_program<I: SimdIndex>(
    flat: &FlattenedCircuit,
    topo_order: &[usize],
    wire_listeners: &[Vec<usize>],
) -> SimdProgram<I> {
    let observable_wire_count = flat.idx_to_wire.len();
    let mut fused_not = HashMap::<usize, (SimdOp, usize)>::new();
    let mut skipped_producer = vec![false; flat.nodes.len()];
    let mut producer_by_wire = vec![None; flat.wire_count];
    for (node_idx, node) in flat.nodes.iter().enumerate() {
        for &wire in &node.outputs {
            producer_by_wire[wire] = Some(node_idx);
        }
    }

    // Fuse producer -> NOT only when the intermediate wire is hidden inside a
    // flattened subcircuit and has exactly one consumer. Visible wires must be
    // independently materialized for API/game inspection.
    for (not_idx, node) in flat.nodes.iter().enumerate() {
        if !matches!(node.kind, NodeKind::Not) || node.inputs.len() != 1 {
            continue;
        }
        let wire = node.inputs[0];
        if wire < observable_wire_count || wire_listeners[wire].len() != 1 {
            continue;
        }
        let Some(producer_idx) = producer_by_wire[wire] else {
            continue;
        };
        let producer = &flat.nodes[producer_idx];
        if producer.outputs.as_slice() != [wire] {
            continue;
        }
        let fused_op = match producer.kind {
            NodeKind::And => SimdOp::Nand,
            NodeKind::Or => SimdOp::Nor,
            NodeKind::Xor => SimdOp::Xnor,
            NodeKind::Not => SimdOp::Buffer,
            _ => continue,
        };
        fused_not.insert(not_idx, (fused_op, producer_idx));
        skipped_producer[producer_idx] = true;
    }

    let mut program = SimdProgram::default();
    for &node_idx in topo_order {
        let node = &flat.nodes[node_idx];
        if skipped_producer[node_idx] {
            continue;
        }
        if let NodeKind::Clock {
            period_ticks,
            duty_ticks,
            phase,
        } = node.kind
        {
            for &output in node
                .outputs
                .iter()
                .filter(|&&wire| wire < observable_wire_count || !wire_listeners[wire].is_empty())
            {
                program.clocks.push(SimdClock {
                    period_ticks,
                    duty_ticks,
                    phase,
                    output: I::from_usize(output),
                });
            }
            continue;
        }

        let (op, inputs): (SimdOp, &[usize]) =
            if let Some(&(op, producer_idx)) = fused_not.get(&node_idx) {
                (op, &flat.nodes[producer_idx].inputs)
            } else {
                let Some(op) = node_op(&node.kind) else {
                    continue;
                };
                (op, &node.inputs)
            };

        let outputs: Vec<usize> = node
            .outputs
            .iter()
            .copied()
            .filter(|&wire| wire < observable_wire_count || !wire_listeners[wire].is_empty())
            .collect();
        match op {
            SimdOp::Not | SimdOp::Buffer => {
                for output in outputs {
                    let start = program.unary.len();
                    program.unary.push(SimdUnaryInstruction {
                        input: I::from_usize(inputs[0]),
                        output: I::from_usize(output),
                    });
                    push_block(&mut program.blocks, op, start);
                }
            }
            SimdOp::Mux => {
                for output in outputs {
                    let start = program.ternary.len();
                    program.ternary.push(SimdTernaryInstruction {
                        in0: I::from_usize(inputs[0]),
                        in1: I::from_usize(inputs[1]),
                        in2: I::from_usize(inputs[2]),
                        output: I::from_usize(output),
                    });
                    push_block(&mut program.blocks, op, start);
                }
            }
            SimdOp::Demux => {
                if outputs.len() >= 2 {
                    let start = program.demux.len();
                    program.demux.push(SimdDemuxInstruction {
                        input: I::from_usize(inputs[0]),
                        select: I::from_usize(inputs[1]),
                        out_false: I::from_usize(outputs[0]),
                        out_true: I::from_usize(outputs[1]),
                    });
                    push_block(&mut program.blocks, op, start);
                }
            }
            _ => {
                for output in outputs {
                    let start = program.binary.len();
                    program.binary.push(SimdBinaryInstruction {
                        in0: I::from_usize(inputs[0]),
                        in1: I::from_usize(inputs[1]),
                        output: I::from_usize(output),
                    });
                    push_block(&mut program.blocks, op, start);
                }
            }
        }
    }
    program
}

impl BitPackedCircuit {
    pub fn compile_from_flat(flat: &FlattenedCircuit) -> Self {
        let node_count = flat.nodes.len();
        let wire_count = flat.wire_count;
        let mut wire_listeners: Vec<Vec<usize>> = vec![Vec::new(); wire_count];
        let mut in_degree = vec![0usize; node_count];

        for (node_idx, node) in flat.nodes.iter().enumerate() {
            in_degree[node_idx] = node.inputs.len();
            for &wire in &node.inputs {
                if wire < wire_count {
                    wire_listeners[wire].push(node_idx);
                }
            }
        }

        let mut queue = VecDeque::new();
        let mut visited = vec![false; node_count];
        for (node_idx, node) in flat.nodes.iter().enumerate() {
            if matches!(node.kind, NodeKind::Input) || node.inputs.is_empty() {
                queue.push_back(node_idx);
                visited[node_idx] = true;
            }
        }

        let mut topo_order = Vec::with_capacity(node_count);
        while let Some(node_idx) = queue.pop_front() {
            topo_order.push(node_idx);
            for &wire in &flat.nodes[node_idx].outputs {
                for &listener in &wire_listeners[wire] {
                    if in_degree[listener] > 0 {
                        in_degree[listener] -= 1;
                        if in_degree[listener] == 0 && !visited[listener] {
                            visited[listener] = true;
                            queue.push_back(listener);
                        }
                    }
                }
            }
        }
        for node_idx in 0..node_count {
            if !visited[node_idx] {
                topo_order.push(node_idx);
            }
        }

        let programs = if wire_count <= u16::MAX as usize {
            SimdPrograms::Compact(build_program::<u16>(flat, &topo_order, &wire_listeners))
        } else {
            SimdPrograms::Wide(build_program::<u32>(flat, &topo_order, &wire_listeners))
        };

        Self {
            programs,
            wire_count,
            node_to_idx: flat.node_to_idx.clone(),
            wire_to_idx: flat.wire_to_idx.clone(),
            idx_to_wire: flat.idx_to_wire.clone(),
            node_input_pins: flat.node_input_pins.clone(),
            node_output_pins: flat.node_output_pins.clone(),
        }
    }

    pub fn compile(circuit: &Circuit) -> Self {
        Self::compile_from_flat(&FlattenedCircuit::compile(circuit))
    }
}
