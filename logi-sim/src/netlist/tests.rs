use super::*;

fn and1(d: &mut Design, def: DefId) -> NodeId {
    d.add_node(def, NodeKind::And { w: 1 }).unwrap()
}

#[test]
fn add_node_creates_one_net_per_output_pin() {
    let mut d = Design::new("top");
    let r = d.root;
    let add = d.add_node(r, NodeKind::Add { w: 8 }).unwrap();
    let def = d.root_def();
    let node = &def.nodes[add];
    assert_eq!(node.inputs.len(), 3);
    assert!(node.inputs.iter().all(Option::is_none));
    assert_eq!(node.outputs.len(), 2);
    assert_eq!(def.nets[node.outputs[0]].width, 8);
    assert_eq!(def.nets[node.outputs[0]].driver, (add, 0));
    assert_eq!(def.nets[node.outputs[1]].width, 1);
    assert_eq!(def.nets[node.outputs[1]].driver, (add, 1));
    d.validate().unwrap();
}

#[test]
fn add_node_rejects_bad_params_and_instances() {
    let mut d = Design::new("top");
    let r = d.root;
    for kind in [
        NodeKind::And { w: 0 },
        NodeKind::Not { w: 65 },
        NodeKind::Split {
            w: 8,
            lo: 4,
            len: 5,
        },
        NodeKind::Split {
            w: 8,
            lo: 0,
            len: 0,
        },
        NodeKind::Extend {
            from: 8,
            to: 4,
            signed: false,
        },
        NodeKind::Merge {
            widths: smallvec::smallvec![32, 33],
        },
        NodeKind::Clock {
            period: 0,
            duty: 0,
            phase: 0,
        },
        NodeKind::Clock {
            period: 2,
            duty: 3,
            phase: 0,
        },
        NodeKind::Ram {
            addr_bits: 0,
            w: 8,
            init: Default::default(),
        },
        NodeKind::Rom {
            addr_bits: 1,
            w: 8,
            contents: vec![1, 2, 3].into(),
        },
    ] {
        assert!(
            matches!(
                d.add_node(r, kind.clone()),
                Err(NetlistError::BadParams { .. })
            ),
            "{kind:?} accepted"
        );
    }
    let child = d.add_definition("child");
    assert_eq!(
        d.add_node(r, NodeKind::Instance(child)),
        Err(NetlistError::UseAddInstance)
    );
    assert_eq!(d.root_def().revision, 0, "failed adds must not mutate");
}

#[test]
fn port_nodes_join_port_lists_in_creation_order() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let y = d.add_node(r, NodeKind::Output { w: 4 }).unwrap();
    let b = d.add_node(r, NodeKind::Input { w: 8 }).unwrap();
    assert_eq!(d.root_def().inputs, vec![a, b]);
    assert_eq!(d.root_def().outputs, vec![y]);
    let (ins, outs) = d.root_def().signature();
    assert_eq!(ins.as_slice(), &[1, 8]);
    assert_eq!(outs.as_slice(), &[4]);
}

#[test]
fn connect_and_disconnect() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let g = and1(&mut d, r);
    let net = d.root_def().output_net(a, 0).unwrap();
    let rev = d.root_def().revision;

    d.connect(r, net, (g, 1)).unwrap();
    assert!(d.root_def().revision > rev);
    assert_eq!(d.root_def().input_net(g, 0), None);
    assert_eq!(d.root_def().input_net(g, 1), Some(net));
    assert_eq!(d.root_def().sinks(net), vec![(g, 1)]);

    // Fan-out: a second sink on the same net.
    d.connect_pins(r, (a, 0), (g, 0)).unwrap();
    let mut sinks = d.root_def().sinks(net);
    sinks.sort();
    let mut want = vec![(g, 0), (g, 1)];
    want.sort();
    assert_eq!(sinks, want);

    assert_eq!(d.disconnect(r, (g, 1)).unwrap(), Some(net));
    assert_eq!(d.root_def().input_net(g, 1), None);
    assert_eq!(d.disconnect(r, (g, 1)).unwrap(), None);
    d.validate().unwrap();
}

#[test]
fn connect_rejects_width_mismatch_and_bad_pins() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 8 }).unwrap();
    let g = and1(&mut d, r);
    let net = d.root_def().output_net(a, 0).unwrap();
    assert!(matches!(
        d.connect(r, net, (g, 0)),
        Err(NetlistError::WidthMismatch {
            net_width: 8,
            pin_width: 1,
            ..
        })
    ));
    let one = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let one = d.root_def().output_net(one, 0).unwrap();
    assert!(matches!(
        d.connect(r, one, (g, 2)),
        Err(NetlistError::PinOutOfRange {
            pin: 2,
            count: 2,
            ..
        })
    ));
    assert!(matches!(
        d.connect_pins(r, (g, 3), (g, 0)),
        Err(NetlistError::PinOutOfRange { .. })
    ));
}

#[test]
fn unconnected_pin_never_shifts_later_pins() {
    // Regression from the audit: leaving pin 0 open must not move the
    // connection on pin 2 into pin 1 (or anywhere else).
    let mut d = Design::new("top");
    let r = d.root;
    let sel = d.add_node(r, NodeKind::Input { w: 4 }).unwrap();
    let mux = d.add_node(r, NodeKind::Mux { w: 4 }).unwrap();
    let net = d.root_def().output_net(sel, 0).unwrap();
    d.connect(r, net, (mux, 2)).unwrap();
    let node = &d.root_def().nodes[mux];
    assert_eq!(node.inputs.as_slice(), &[None, None, Some(net)]);

    // Disconnecting a lower pin does not shift either.
    let s1 = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let s1 = d.root_def().output_net(s1, 0).unwrap();
    d.connect(r, s1, (mux, 0)).unwrap();
    d.disconnect(r, (mux, 0)).unwrap();
    assert_eq!(
        d.root_def().nodes[mux].inputs.as_slice(),
        &[None, None, Some(net)]
    );
    d.validate().unwrap();
}

#[test]
fn remove_node_disconnects_sinks_and_deletes_nets() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let g = and1(&mut d, r);
    let h = and1(&mut d, r);
    let a_net = d.root_def().output_net(a, 0).unwrap();
    let g_net = d.root_def().output_net(g, 0).unwrap();
    d.connect(r, a_net, (g, 0)).unwrap();
    d.connect(r, g_net, (h, 0)).unwrap();
    d.connect(r, g_net, (h, 1)).unwrap();
    d.set_name(r, g, Some("g".into())).unwrap();
    let rev = d.root_def().revision;

    let removed = d.remove_node(r, g).unwrap();
    assert_eq!(removed.kind, NodeKind::And { w: 1 });
    let def = d.root_def();
    assert!(def.revision > rev);
    assert!(!def.nodes.contains_key(g));
    assert!(!def.nets.contains_key(g_net));
    assert_eq!(def.nodes[h].inputs.as_slice(), &[None, None]);
    assert_eq!(def.find("g"), None, "name side table cleaned up");
    assert!(def.sinks(a_net).is_empty());
    d.validate().unwrap();

    assert_eq!(d.remove_node(r, g), Err(NetlistError::NoSuchNode(g)));
}

#[test]
fn removing_a_port_keeps_remaining_port_order() {
    let mut d = Design::new("top");
    let r = d.root;
    let ins: Vec<_> = (0..4)
        .map(|_| d.add_node(r, NodeKind::Input { w: 1 }).unwrap())
        .collect();
    let outs: Vec<_> = (0..3)
        .map(|_| d.add_node(r, NodeKind::Output { w: 1 }).unwrap())
        .collect();
    d.remove_node(r, ins[1]).unwrap();
    d.remove_node(r, outs[0]).unwrap();
    assert_eq!(d.root_def().inputs, vec![ins[0], ins[2], ins[3]]);
    assert_eq!(d.root_def().outputs, vec![outs[1], outs[2]]);
    // Re-adding appends rather than reusing the old position.
    let again = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    assert_eq!(d.root_def().inputs, vec![ins[0], ins[2], ins[3], again]);
    d.validate().unwrap();
}

/// A child definition with ports `a, b, c -> x, y, z`, instanced in the root
/// with every pin connected.
type InstancedChild = (
    Design,
    DefId,
    NodeId,
    Vec<NodeId>,
    Vec<NodeId>,
    Vec<NetId>,
    Vec<NodeId>,
);

fn instanced_child() -> InstancedChild {
    let mut d = Design::new("top");
    let r = d.root;
    let child = d.add_definition("child");
    let cin: Vec<_> = (0..3)
        .map(|_| d.add_node(child, NodeKind::Input { w: 1 }).unwrap())
        .collect();
    let cout: Vec<_> = (0..3)
        .map(|_| d.add_node(child, NodeKind::Output { w: 1 }).unwrap())
        .collect();
    let inst = d.add_instance(r, child).unwrap();
    let srcs: Vec<NetId> = (0..3)
        .map(|_| {
            let n = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
            d.root_def().output_net(n, 0).unwrap()
        })
        .collect();
    let sinks: Vec<NodeId> = (0..3)
        .map(|_| d.add_node(r, NodeKind::Output { w: 1 }).unwrap())
        .collect();
    for i in 0..3 {
        d.connect(r, srcs[i], (inst, i as u8)).unwrap();
        d.connect_pins(r, (inst, i as u8), (sinks[i], 0)).unwrap();
    }
    d.validate().unwrap();
    (d, child, inst, cin, cout, srcs, sinks)
}

#[test]
fn instance_pins_follow_definition_ports() {
    let (d, child, inst, ..) = instanced_child();
    let node = &d.root_def().nodes[inst];
    assert_eq!(node.kind, NodeKind::Instance(child));
    assert_eq!(node.inputs.len(), 3);
    assert_eq!(node.outputs.len(), 3);
}

#[test]
fn removing_a_child_port_keeps_parent_connections_on_the_right_pins() {
    let (mut d, child, inst, cin, cout, srcs, sinks) = instanced_child();
    d.remove_node(child, cin[1]).unwrap();
    d.remove_node(child, cout[0]).unwrap();
    d.validate().unwrap();

    let def = d.root_def();
    let node = &def.nodes[inst];
    assert_eq!(node.inputs.as_slice(), &[Some(srcs[0]), Some(srcs[2])]);
    assert_eq!(node.outputs.len(), 2);
    // Old pin 0's sink is now unconnected; old pins 1 and 2 are now 0 and 1
    // and still drive the same sinks.
    assert_eq!(def.input_net(sinks[0], 0), None);
    assert_eq!(def.input_net(sinks[1], 0), Some(node.outputs[0]));
    assert_eq!(def.input_net(sinks[2], 0), Some(node.outputs[1]));
    assert_eq!(def.nets[node.outputs[0]].driver, (inst, 0));
    assert_eq!(def.nets[node.outputs[1]].driver, (inst, 1));
}

#[test]
fn adding_a_child_port_appends_an_unconnected_instance_pin() {
    let (mut d, child, inst, ..) = instanced_child();
    let rev = d.root_def().revision;
    d.add_node(child, NodeKind::Input { w: 8 }).unwrap();
    d.add_node(child, NodeKind::Output { w: 16 }).unwrap();
    let def = d.root_def();
    assert!(
        def.revision > rev,
        "parent revision bumps on instance pin change"
    );
    let node = &def.nodes[inst];
    assert_eq!(node.inputs.len(), 4);
    assert_eq!(node.inputs[3], None);
    assert_eq!(node.outputs.len(), 4);
    assert_eq!(def.nets[node.outputs[3]].width, 16);
    d.validate().unwrap();
}

#[test]
fn add_and_remove_definition() {
    let (mut d, child, inst, ..) = instanced_child();
    let r = d.root;
    assert_eq!(
        d.remove_definition(r).unwrap_err(),
        NetlistError::RemoveRoot
    );
    assert!(matches!(
        d.remove_definition(child),
        Err(NetlistError::DefinitionInUse { by, .. }) if by == vec![(r, inst)]
    ));
    d.remove_node(r, inst).unwrap();
    let removed = d.remove_definition(child).unwrap();
    assert_eq!(removed.name, "child");
    assert!(d.def(child).is_err());
    d.validate().unwrap();
}

#[test]
fn instance_recursion_is_rejected() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_definition("a");
    let b = d.add_definition("b");
    assert!(matches!(
        d.add_instance(a, a),
        Err(NetlistError::InstanceRecursion { .. })
    ));
    d.add_instance(r, a).unwrap();
    d.add_instance(a, b).unwrap();
    assert!(matches!(
        d.add_instance(b, a),
        Err(NetlistError::InstanceRecursion { .. })
    ));
    assert!(matches!(
        d.add_instance(b, r),
        Err(NetlistError::InstanceRecursion { .. })
    ));
    // Shared definitions (a DAG) are fine.
    d.add_instance(r, b).unwrap();
    d.validate().unwrap();
    assert_eq!(d.defs_bottom_up().last(), Some(&r));
}

#[test]
fn validation_reports_recursion_built_by_hand() {
    let mut d = Design::new("top");
    let a = d.add_definition("a");
    d.add_instance(d.root, a).unwrap();
    // Bypass the API to create a cycle.
    let n = d.defs[a].nodes.insert(Node {
        kind: NodeKind::Instance(d.root),
        inputs: Default::default(),
        outputs: Default::default(),
    });
    let _ = n;
    let errs = d.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InstanceRecursion { cycle } if cycle.len() == 2)),
        "{errs:?}"
    );
}

#[test]
fn validation_reports_structural_errors() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 8 }).unwrap();
    let g = and1(&mut d, r);
    let a_net = d.root_def().output_net(a, 0).unwrap();
    let def = d.defs.get_mut(r).unwrap();
    // Width mismatch written directly.
    def.nodes[g].inputs[0] = Some(a_net);
    // Pin count wrong.
    def.nodes[g].inputs.push(None);
    let errs = d.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::PinCount { .. })),
        "{errs:?}"
    );

    d.defs[r].nodes[g].inputs.pop();
    let errs = d.validate().unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::WidthMismatch {
                pin: 0,
                expected: 1,
                actual: 8,
                ..
            }
        )),
        "{errs:?}"
    );

    // Dangling net.
    let ghost = d.defs[r].nets.insert(Net {
        width: 1,
        driver: (g, 7),
    });
    d.defs[r].nodes[g].inputs[0] = Some(ghost);
    d.defs[r].nets.remove(ghost);
    let errs = d.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DanglingNet { .. })),
        "{errs:?}"
    );
    d.defs[r].nodes[g].inputs[0] = None;
    d.validate().unwrap();

    // Port missing from its list.
    d.defs[r].inputs.clear();
    let errs = d.validate().unwrap_err();
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadPort { node, .. } if *node == a))
    );
}

#[test]
fn validation_reports_stale_instance_pins() {
    let (mut d, child, inst, ..) = instanced_child();
    // Bypass the API: add a child port without updating instances.
    d.defs[child].insert_node(NodeKind::Input { w: 1 }, 0, &[1]);
    let errs = d.validate().unwrap_err();
    assert!(errs.iter().any(|e| matches!(
        e,
        ValidationError::PinCount { node, expected_inputs: 4, inputs: 3, .. } if *node == inst
    )));
}

#[test]
fn names_live_in_a_side_table() {
    let mut d = Design::new("top");
    let r = d.root;
    let a = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    let y = d.add_node(r, NodeKind::Output { w: 1 }).unwrap();
    d.set_name(r, a, Some("A".into())).unwrap();
    d.set_name(r, y, Some("Y".into())).unwrap();
    assert_eq!(d.root_def().input_port("A"), Some(0));
    assert_eq!(d.root_def().output_port("Y"), Some(0));
    assert_eq!(d.root_def().find("A"), Some(a));
    d.set_name(r, a, None).unwrap();
    assert_eq!(d.root_def().input_port("A"), None);
}

#[test]
fn every_mutation_bumps_revision() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut last = d.root_def().revision;
    let mut bumped = |d: &Design| {
        let now = d.root_def().revision;
        assert!(now > last);
        last = now;
    };
    let a = d.add_node(r, NodeKind::Input { w: 1 }).unwrap();
    bumped(&d);
    let g = and1(&mut d, r);
    bumped(&d);
    d.connect_pins(r, (a, 0), (g, 0)).unwrap();
    bumped(&d);
    d.disconnect(r, (g, 0)).unwrap();
    bumped(&d);
    d.set_name(r, g, Some("g".into())).unwrap();
    bumped(&d);
    let c = d.add_definition("c");
    d.add_instance(r, c).unwrap();
    bumped(&d);
    d.remove_node(r, g).unwrap();
    bumped(&d);
}

#[test]
fn builder_composites_validate() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let s = b.input("s", 2);
    let en = b.input("en", 1);
    let rst = b.input("rst", 1);
    let bits = b.bus_expand(x);
    assert_eq!(bits.len(), 8);
    let all = b.and_n(&bits);
    let any = b.or_n(&bits);
    let par = b.xor_n(&bits);
    let back = b.bus_collect(&bits);
    assert_eq!(b.width(back), 8);
    let dec = b.decoder(s);
    assert_eq!(dec.len(), 4);
    let dm = b.demux(s, x);
    assert_eq!(dm.len(), 4);
    let picked = b.mux_n(s, &dm[..3]);
    assert_eq!(b.width(picked), 8);
    let cnt = b.counter(16, en, rst);
    assert_eq!(b.width(cnt), 16);
    let wide = b.sext(x, 64);
    let narrow = b.split(wide, 60, 4);
    let flags = b.merge(&[all, any, par, narrow]);
    assert_eq!(b.width(flags), 7);
    b.output("flags", flags);
    b.output("picked", picked);
    b.output("cnt", cnt);
    d.validate().unwrap();
}
