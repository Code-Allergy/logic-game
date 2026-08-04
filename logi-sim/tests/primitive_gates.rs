#![allow(clippy::type_complexity)]

use logi_sim::circuit::Circuit;
use logi_sim::prelude::*;

fn run_runner_test<R: Runner>(
    mut runner: R,
    inputs: &[(logi_sim::circuit::NodeId, u8, bool)],
    outputs: &[(logi_sim::circuit::NodeId, u8, bool)],
) {
    for &(node, pin, val) in inputs {
        runner.set_input(node, pin, val);
    }
    runner.tick();
    for &(node, pin, expected) in outputs {
        assert_eq!(
            runner.output(node, pin),
            expected,
            "Runner output mismatch on pin {}",
            pin
        );
    }
}

fn verify_all_runners_truth_table(
    circuit: &Circuit,
    test_cases: Vec<(
        Vec<(logi_sim::circuit::NodeId, u8, bool)>,
        Vec<(logi_sim::circuit::NodeId, u8, bool)>,
    )>,
) {
    for (inputs, outputs) in test_cases {
        let naive = NaiveRunner::new(circuit.clone());
        run_runner_test(naive, &inputs, &outputs);

        let flat = FlatRunner::new(FlattenedCircuit::compile(circuit));
        run_runner_test(flat, &inputs, &outputs);

        let event = EventDrivenRunner::new(EventDrivenCircuit::compile(circuit));
        run_runner_test(event, &inputs, &outputs);

        let simd = SimdRunner::new(BitPackedCircuit::compile(circuit));
        run_runner_test(simd, &inputs, &outputs);
    }
}

#[test]
fn test_nand_full_truth_table() {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let nand = c.nand();
    let out = c.output();

    c.connect((a, 0), (nand, 0));
    c.connect((b, 0), (nand, 1));
    c.connect((nand, 0), (out, 0));

    let cases = vec![
        (vec![(a, 0, false), (b, 0, false)], vec![(out, 0, true)]),
        (vec![(a, 0, false), (b, 0, true)], vec![(out, 0, true)]),
        (vec![(a, 0, true), (b, 0, false)], vec![(out, 0, true)]),
        (vec![(a, 0, true), (b, 0, true)], vec![(out, 0, false)]),
    ];

    verify_all_runners_truth_table(&c, cases);
}

#[test]
fn test_nor_full_truth_table() {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let nor = c.nor();
    let out = c.output();

    c.connect((a, 0), (nor, 0));
    c.connect((b, 0), (nor, 1));
    c.connect((nor, 0), (out, 0));

    let cases = vec![
        (vec![(a, 0, false), (b, 0, false)], vec![(out, 0, true)]),
        (vec![(a, 0, false), (b, 0, true)], vec![(out, 0, false)]),
        (vec![(a, 0, true), (b, 0, false)], vec![(out, 0, false)]),
        (vec![(a, 0, true), (b, 0, true)], vec![(out, 0, false)]),
    ];

    verify_all_runners_truth_table(&c, cases);
}

#[test]
fn test_xnor_full_truth_table() {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let xnor = c.xnor();
    let out = c.output();

    c.connect((a, 0), (xnor, 0));
    c.connect((b, 0), (xnor, 1));
    c.connect((xnor, 0), (out, 0));

    let cases = vec![
        (vec![(a, 0, false), (b, 0, false)], vec![(out, 0, true)]),
        (vec![(a, 0, false), (b, 0, true)], vec![(out, 0, false)]),
        (vec![(a, 0, true), (b, 0, false)], vec![(out, 0, false)]),
        (vec![(a, 0, true), (b, 0, true)], vec![(out, 0, true)]),
    ];

    verify_all_runners_truth_table(&c, cases);
}

#[test]
fn test_mux_full_truth_table() {
    let mut c = Circuit::new();
    let d0 = c.input();
    let d1 = c.input();
    let sel = c.input();
    let mux = c.mux();
    let out = c.output();

    c.connect((d0, 0), (mux, 0));
    c.connect((d1, 0), (mux, 1));
    c.connect((sel, 0), (mux, 2));
    c.connect((mux, 0), (out, 0));

    // Testing all 8 combinations of D0, D1, Sel
    let cases = vec![
        // Sel = 0 -> Output = D0
        (
            vec![(d0, 0, false), (d1, 0, false), (sel, 0, false)],
            vec![(out, 0, false)],
        ),
        (
            vec![(d0, 0, false), (d1, 0, true), (sel, 0, false)],
            vec![(out, 0, false)],
        ),
        (
            vec![(d0, 0, true), (d1, 0, false), (sel, 0, false)],
            vec![(out, 0, true)],
        ),
        (
            vec![(d0, 0, true), (d1, 0, true), (sel, 0, false)],
            vec![(out, 0, true)],
        ),
        // Sel = 1 -> Output = D1
        (
            vec![(d0, 0, false), (d1, 0, false), (sel, 0, true)],
            vec![(out, 0, false)],
        ),
        (
            vec![(d0, 0, false), (d1, 0, true), (sel, 0, true)],
            vec![(out, 0, true)],
        ),
        (
            vec![(d0, 0, true), (d1, 0, false), (sel, 0, true)],
            vec![(out, 0, false)],
        ),
        (
            vec![(d0, 0, true), (d1, 0, true), (sel, 0, true)],
            vec![(out, 0, true)],
        ),
    ];

    verify_all_runners_truth_table(&c, cases);
}

#[test]
fn test_demux_full_truth_table() {
    let mut c = Circuit::new();
    let sig = c.input();
    let sel = c.input();
    let demux = c.demux();
    let out0 = c.output();
    let out1 = c.output();

    c.connect((sig, 0), (demux, 0));
    c.connect((sel, 0), (demux, 1));
    c.connect((demux, 0), (out0, 0));
    c.connect((demux, 1), (out1, 0));

    let cases = vec![
        // Sel = 0 -> Out0 = Sig, Out1 = false
        (
            vec![(sig, 0, false), (sel, 0, false)],
            vec![(out0, 0, false), (out1, 0, false)],
        ),
        (
            vec![(sig, 0, true), (sel, 0, false)],
            vec![(out0, 0, true), (out1, 0, false)],
        ),
        // Sel = 1 -> Out0 = false, Out1 = Sig
        (
            vec![(sig, 0, false), (sel, 0, true)],
            vec![(out0, 0, false), (out1, 0, false)],
        ),
        (
            vec![(sig, 0, true), (sel, 0, true)],
            vec![(out0, 0, false), (out1, 0, true)],
        ),
    ];

    verify_all_runners_truth_table(&c, cases);
}

#[test]
fn test_tristate_buffer_full_truth_table() {
    let mut c = Circuit::new();
    let sig = c.input();
    let enable = c.input();
    let tri = c.tristate();
    let out = c.output();

    c.connect((sig, 0), (tri, 0));
    c.connect((enable, 0), (tri, 1));
    c.connect((tri, 0), (out, 0));

    let cases = vec![
        // Enable = 0 -> Output = false (Z)
        (
            vec![(sig, 0, false), (enable, 0, false)],
            vec![(out, 0, false)],
        ),
        (
            vec![(sig, 0, true), (enable, 0, false)],
            vec![(out, 0, false)],
        ),
        // Enable = 1 -> Output = Signal
        (
            vec![(sig, 0, false), (enable, 0, true)],
            vec![(out, 0, false)],
        ),
        (
            vec![(sig, 0, true), (enable, 0, true)],
            vec![(out, 0, true)],
        ),
    ];

    verify_all_runners_truth_table(&c, cases);
}
