use std::fmt::Write;

use crate::prelude::*;

use super::util::*;

pub struct DotBackend;

impl DotBackend {
    pub fn render(circuit: &Circuit) -> String {
        let mut dot = String::new();

        writeln!(&mut dot, "digraph Circuit {{").unwrap();
        writeln!(&mut dot, "    rankdir=LR;").unwrap();
        writeln!(&mut dot, "    splines=ortho;").unwrap();
        writeln!(&mut dot, "    nodesep=0.4;").unwrap();
        writeln!(&mut dot, "    ranksep=0.6;").unwrap();
        writeln!(&mut dot).unwrap();

        writeln!(&mut dot, "    node [shape=record, fontname=\"monospace\"];").unwrap();

        writeln!(&mut dot, "    edge [fontname=\"monospace\"];").unwrap();

        writeln!(&mut dot).unwrap();

        Self::render_nodes(circuit, &mut dot);

        writeln!(&mut dot).unwrap();

        Self::render_wires(circuit, &mut dot);

        writeln!(&mut dot, "}}").unwrap();

        dot
    }

    fn render_nodes(circuit: &Circuit, out: &mut String) {
        for (id, node) in &circuit.nodes {
            let name = node_name(id);

            let inputs = node
                .inputs
                .iter()
                .enumerate()
                .map(|(i, pin)| format!("<i{}> {}", i, escape(&pin.name)))
                .collect::<Vec<_>>()
                .join("|");

            let outputs = node
                .outputs
                .iter()
                .enumerate()
                .map(|(i, pin)| format!("<o{}> {}", i, escape(&pin.name)))
                .collect::<Vec<_>>()
                .join("|");

            let label = format!(
                "{{{{{}}}|{}|{{{}}}}}",
                inputs,
                kind_label(&node.kind),
                outputs,
            );

            writeln!(out, "    {} [label=\"{}\"];", name, label).unwrap();
        }
    }

    fn render_wires(circuit: &Circuit, out: &mut String) {
        for wire in circuit.wires.values() {
            let src = node_name(wire.source.node);

            for sink in &wire.sinks {
                let dst = node_name(sink.node);

                writeln!(
                    out,
                    "    {}:o{} -> {}:i{};",
                    src, wire.source.pin, dst, sink.pin,
                )
                .unwrap();
            }
        }
    }
}
