#!/usr/bin/env python3
"""Phase 4.1: the gate-level ALU, a drop-in for the reference `ref_alu`.

    python3 nes/gen/alu.py > nes/build/alu.json   (needs the cell library)

Same ports as `ref_alu` (op[5], a, b, p, bcd -> r, f), and the same
function for every input, invalid operations and invalid BCD digits
included, so an `equivalent` test against `ref_alu` covers it and it can
replace `ref_alu` inside the CPU. Operation numbers are those of
`nes/gen/verilog.py` (ALU_OPS):

    0 NONE 1 PASSA 2 PASSB 3 OR 4 AND 5 EOR 6 ADC 7 SBC 8 CMP 9 BIT
    10 INC 11 DEC 12 ASL 13 LSR 14 ROL 15 ROR 16 ANC 17 ALR 18 ARR

Blocks: `alu_ctl` (operation decode into control lines), `bcd_add` and
`bcd_sub` (NMOS decimal adjust) under `bcd`, and `alu` (logic unit, adder
path, shifter wiring, result select, flags).
"""

import json
import sys

import cells
from cells import chip, formula, gate, inp, out, unopt, use

cells.EDITS.clear()


def or_chain(prefix, srcs, sink):
    """2-input OR gates combining `srcs` into `sink`: (parts, wires)."""
    parts, wires = [], []
    acc = srcs[0]
    for i, s in enumerate(srcs[1:]):
        g = f"{prefix}{i}"
        parts.append(gate(g, "Or"))
        wires += [[acc, f"{g}.a"], [s, f"{g}.b"]]
        acc = g
    wires.append([acc, sink])
    return parts, wires


# ── alu_ctl: the operation's one-hot line, then the control lines ────────
CTL_OUT = ["sel_a", "sel_b", "sel_or", "sel_and", "sel_xor", "sel_sum", "sel_shl", "sel_shr",
           "binv", "bkill", "usec", "force1", "andx", "rol", "rotr", "c_add", "c_x7", "c_x0",
           "c_r7", "c_r6", "c_p", "v_add", "bit", "arr", "adc", "sbc"]
GROUPS = {
    "sel_b": [2], "sel_or": [3], "sel_and": [4, 16], "sel_xor": [5],
    "sel_sum": [6, 7, 8, 10, 11], "sel_shl": [12, 14], "sel_shr": [13, 15, 17, 18],
    "binv": [7, 8, 11], "bkill": [10, 11], "usec": [6, 7], "force1": [8, 10],
    "andx": [17, 18], "rol": [14], "rotr": [15, 18],
    "c_add": [6, 7, 8], "c_x7": [12, 14], "c_x0": [13, 15, 17], "c_r7": [16], "c_r6": [18],
    "v_add": [6, 7], "bit": [9], "arr": [18], "adc": [6], "sbc": [7],
}
parts = [inp("op", 5), gate("one", "Const"), use("dec", "decoder5")]
parts[1]["value"] = 1
wires = [["op", "dec.A"], ["one", "dec.En"]]
SRC = {}  # where each control line is driven inside alu_ctl
for name, ops in GROUPS.items():
    srcs = [f"dec.Y:{k}" for k in ops]
    if len(srcs) == 1:
        SRC[name] = srcs[0]
    else:
        p, w = or_chain(f"{name}_or", srcs, None)
        parts += p
        wires += w[:-1]
        SRC[name] = w[-1][0]
# sel_a: no other source selected (NONE, PASSA, BIT and invalid operations
# leave r = a); c_p: no other carry source (C stays). BIT gives C = 0, as
# ref_alu does (its flag mask does not write C, but the block's output is
# matched bit for bit).
for name, others in (("sel_a", ["sel_b", "sel_or", "sel_and", "sel_xor", "sel_sum", "sel_shl", "sel_shr"]),
                     ("c_p", ["c_add", "c_x7", "c_x0", "c_r7", "c_r6", "bit"])):
    p, w = or_chain(f"{name}_any", [SRC[o] for o in others], f"{name}_n")
    parts += p + [gate(f"{name}_n", "Not")]
    wires += w
    SRC[name] = f"{name}_n"
parts += [out(n) for n in CTL_OUT]
wires += [[SRC[n], n] for n in CTL_OUT]
ctl_tests = {
    "sel_a": "op == 0 || op == 1 || op == 9 || op > 18",
    "sel_sum": "op == 6 || op == 7 || op == 8 || op == 10 || op == 11",
    "sel_shr": "op == 13 || op == 15 || op == 17 || op == 18",
    "binv": "op == 7 || op == 8 || op == 11",
    "c_p": "!(op == 6 || op == 7 || op == 8 || op == 9 || (op >= 12 && op <= 18))",
}
chip("alu_ctl", parts, wires, [formula(ctl_tests), unopt()])

# ── Decimal adjust (NMOS) ────────────────────────────────────────────────
# ADC: low digit a+b+c, +6 when 10 or more (carrying 1); high digit
# a+b+that carry, +6 when 10 or more (C). N and V from the high digit
# before its adjustment.
chip("bcd_add",
     [inp("A", 8), inp("B", 8), inp("Ci"), use("lo", "add4"), use("g1", "ge10"), gate("lc", "Or"),
      use("loadj", "add4"), use("hi", "add4"), use("g2", "ge10"), gate("hc", "Or"),
      use("hiadj", "add4"), gate("zero", "Const"), gate("vs", "Xnor"), gate("vd", "Xor"),
      gate("vk", "And"), out("R", 8), out("N"), out("V"), out("C")],
     [["A:0..4", "lo.A"], ["B:0..4", "lo.B"], ["Ci", "lo.Ci"], ["lo.S", "g1.A", "loadj.A"],
      ["lo.Co", "lc.a"], ["g1.Y", "lc.b"],
      ["zero", "loadj.B:0", "loadj.B:3", "loadj.Ci", "hiadj.B:0", "hiadj.B:3", "hiadj.Ci"],
      ["lc", "loadj.B:1", "loadj.B:2", "hi.Ci"],
      ["A:4..8", "hi.A"], ["B:4..8", "hi.B"], ["hi.S", "g2.A", "hiadj.A"],
      ["hi.Co", "hc.a"], ["g2.Y", "hc.b"], ["hc", "hiadj.B:1", "hiadj.B:2", "C"],
      ["loadj.S", "R:0..4"], ["hiadj.S", "R:4..8"],
      ["hi.S:3", "N", "vd.b"], ["A:7", "vs.a", "vd.a"], ["B:7", "vs.b"], ["vs", "vk.a"],
      ["vd", "vk.b"], ["vk", "V"]],
     [formula({
         # The NMOS algorithm, as ops.py's _decimal() writes it.
         "R": "(((A & 15) + (B & 15) + Ci >= 10 ? (((A & 15) + (B & 15) + Ci + 6) & 15) : "
              "(((A & 15) + (B & 15) + Ci) & 15))) | (((((A >> 4) + (B >> 4) + "
              "((A & 15) + (B & 15) + Ci >= 10)) >= 10) ? ((A >> 4) + (B >> 4) + "
              "((A & 15) + (B & 15) + Ci >= 10) + 6) : ((A >> 4) + (B >> 4) + "
              "((A & 15) + (B & 15) + Ci >= 10))) & 15) << 4",
         "C": "((A >> 4) + (B >> 4) + ((A & 15) + (B & 15) + Ci >= 10)) >= 10",
     }, samples=4000), unopt()])

# SBC: the binary subtraction's digits, -6 (+10) for a digit that borrowed.
_L = "((A & 15) + 15 - (B & 15) + Ci)"           # lo + 16; lo < 0 when < 16
_BL = f"({_L} < 16)"
_H = f"((A >> 4) + 16 - (B >> 4) - {_BL})"        # high digit + 16
SBC_R = f"((({_BL} ? {_L} + 10 : {_L}) & 15) | ((({_H} < 16) ? {_H} + 10 : {_H}) & 15) << 4)"
chip("bcd_sub",
     [inp("A", 8), inp("B", 8), inp("Ci"), gate("nb", "Not", 8), use("lo", "add4"), use("hi", "add4"),
      gate("nlo", "Not"), gate("nhi", "Not"), use("loadj", "add4"), use("hiadj", "add4"),
      gate("zero", "Const"), out("R", 8)],
     [["B", "nb"], ["A:0..4", "lo.A"], ["nb[0..4]", "lo.B"], ["Ci", "lo.Ci"],
      ["A:4..8", "hi.A"], ["nb[4..8]", "hi.B"], ["lo.Co", "hi.Ci", "nlo"], ["hi.Co", "nhi"],
      ["lo.S", "loadj.A"], ["hi.S", "hiadj.A"],
      ["zero", "loadj.B:0", "loadj.B:2", "loadj.Ci", "hiadj.B:0", "hiadj.B:2", "hiadj.Ci"],
      ["nlo", "loadj.B:1", "loadj.B:3"], ["nhi", "hiadj.B:1", "hiadj.B:3"],
      ["loadj.S", "R:0..4"], ["hiadj.S", "R:4..8"]],
     [formula({
         # ops.py's _decimal() SBC with every intermediate offset by +16 so
         # it stays non-negative: L = lo + 16, H = high digit + 16.
         "R": SBC_R,
     }, samples=4000), unopt()])

chip("bcd",
     [inp("A", 8), inp("B", 8), inp("Ci"), inp("Sub"), use("add", "bcd_add"), use("sub", "bcd_sub"),
      use("m", "mux2x8"), out("R", 8), out("N"), out("V"), out("C")],
     [["A", "add.A", "sub.A"], ["B", "add.B", "sub.B"], ["Ci", "add.Ci", "sub.Ci"],
      ["add.R", "m.A"], ["sub.R", "m.B"], ["Sub", "m.S"], ["m.Y", "R"],
      ["add.N", "N"], ["add.V", "V"], ["add.C", "C"]],
     [unopt(2000)])

# ── alu ──────────────────────────────────────────────────────────────────
parts = [
    inp("op", 5), inp("a", 8), inp("b", 8), inp("p", 8), inp("bcd"),
    use("ctl", "alu_ctl"),
    # Logic unit.
    gate("land", "And", 8), gate("lor", "Or", 8), gate("lxor", "Xor", 8),
    # Adder path: b' = (b AND NOT kill) XOR invert; cin = (usec AND C) OR force1.
    gate("nkill", "Not"), gate("bk", "And", 8), gate("bx", "Xor", 8),
    gate("cu", "And"), gate("cin", "Or"), use("add", "adder8"),
    gate("ovs", "Xnor"), gate("ovd", "Xor"), gate("ov", "And"),
    # Shifter: its input is a, or a AND b (ALR, ARR); shifted-in bits.
    use("sx", "mux2x8"), gate("lin", "And"), gate("rin", "And"),
    # Result select.
    use("rs", "sel8x8"),
    # Flags of the binary result.
    use("nm", "mux2"), use("zm", "mux2x8"), use("zr", "zero8"),
    gate("ca", "And"), gate("c7", "And"), gate("c0", "And"), gate("cr7", "And"), gate("cr6", "And"),
    gate("cp", "And"), gate("co0", "Or"), gate("co1", "Or"), gate("co2", "Or"), gate("co3", "Or"),
    gate("co4", "Or"),
    gate("va", "And"), gate("vb", "And"), gate("v65", "Xor"), gate("vr", "And"),
    gate("vo0", "Or"), gate("vo1", "Or"),
    # Decimal mode: bcd AND D AND (ADC or SBC).
    use("dec", "bcd"), gate("dm", "And"), gate("dadc", "And"), gate("dsbc", "And"), gate("dany", "Or"),
    use("rf", "mux2x8"), use("nf", "mux2"), use("vf", "mux2"), use("cf", "mux2"),
    gate("zero", "Const"),
    out("r", 8), out("f", 8),
]
wires = [
    ["op", "ctl.op"],
    ["a", "land.a", "lor.a", "lxor.a", "add.A", "sx.A", "rs.D0", "dec.A"],
    ["b", "land.b", "lor.b", "lxor.b", "bk.a", "rs.D1", "dec.B"],
    ["land", "rs.D3", "sx.B", "zm.B"], ["lor", "rs.D2"], ["lxor", "rs.D4"],
    ["ctl.bkill", "nkill"], ["nkill", "bk.b"], ["bk", "bx.a"], ["ctl.binv", "bx.b"],
    ["bx", "add.B"], ["ctl.usec", "cu.a"], ["p:0", "cu.b", "lin.b", "rin.b", "dec.Ci", "cp.b"],
    ["cu", "cin.a"], ["ctl.force1", "cin.b"], ["cin", "add.Ci"], ["add.S", "rs.D5"],
    ["a:7", "ovs.a", "ovd.a"], ["bx[7]", "ovs.b"], ["add.S:7", "ovd.b"], ["ovs", "ov.a"], ["ovd", "ov.b"],
    ["ctl.andx", "sx.S"], ["ctl.rol", "lin.a"], ["ctl.rotr", "rin.a"],
    ["lin", "rs.D6:0"], ["sx.Y:0..7", "rs.D6:1..8"],
    ["sx.Y:1..8", "rs.D7:0..7"], ["rin", "rs.D7:7"],
    ["ctl.sel_a", "rs.S0"], ["ctl.sel_b", "rs.S1"], ["ctl.sel_or", "rs.S2"], ["ctl.sel_and", "rs.S3"],
    ["ctl.sel_xor", "rs.S4"], ["ctl.sel_sum", "rs.S5"], ["ctl.sel_shl", "rs.S6"], ["ctl.sel_shr", "rs.S7"],
    # N: b7 for BIT, else r7. Z: (a AND b) = 0 for BIT, else r = 0.
    ["rs.Y:7", "nm.A", "cr7.b"], ["b:7", "nm.B"], ["ctl.bit", "nm.S", "zm.S", "vb.a"],
    ["rs.Y", "zm.A", "rf.A"], ["zm.Y", "zr.A"],
    # C: one of the adder, the shifted-out bits, r7 (ANC), r6 (ARR), or C.
    ["ctl.c_add", "ca.a"], ["add.Co", "ca.b"], ["ctl.c_x7", "c7.a"], ["sx.Y:7", "c7.b"],
    ["ctl.c_x0", "c0.a"], ["sx.Y:0", "c0.b"], ["ctl.c_r7", "cr7.a"], ["ctl.c_r6", "cr6.a"],
    ["rs.Y:6", "cr6.b", "v65.a"], ["ctl.c_p", "cp.a"],
    ["ca", "co0.a"], ["c7", "co0.b"], ["co0", "co1.a"], ["c0", "co1.b"], ["co1", "co2.a"],
    ["cr7", "co2.b"], ["co2", "co3.a"], ["cr6", "co3.b"], ["co3", "co4.a"], ["cp", "co4.b"],
    # V: the adder's overflow (ADC, SBC), b6 (BIT), r6 XOR r5 (ARR).
    ["ctl.v_add", "va.a"], ["ov", "va.b"], ["b:6", "vb.b"], ["rs.Y:5", "v65.b"],
    ["ctl.arr", "vr.a"], ["v65", "vr.b"], ["va", "vo0.a"], ["vb", "vo0.b"], ["vo0", "vo1.a"],
    ["vr", "vo1.b"],
    # Decimal override: ADC takes R, N, V and C from the decimal unit; SBC
    # only R (its flags are the binary subtraction's). Z stays binary.
    ["bcd", "dm.a"], ["p:3", "dm.b"], ["dm", "dadc.a", "dsbc.a"], ["ctl.adc", "dadc.b"],
    ["ctl.sbc", "dsbc.b", "dec.Sub"], ["dadc", "dany.a", "nf.S", "vf.S", "cf.S"], ["dsbc", "dany.b"],
    ["dec.R", "rf.B"], ["dany", "rf.S"], ["rf.Y", "r"],
    ["nm.Y", "nf.A"], ["dec.N", "nf.B"], ["vo1", "vf.A"], ["dec.V", "vf.B"],
    ["co4", "cf.A"], ["dec.C", "cf.B"],
    # f = {N, V, 0, 0, 0, 0, Z, C}.
    ["cf.Y", "f:0"], ["zr.Y", "f:1"], ["zero", "f:2", "f:3", "f:4", "f:5"], ["vf.Y", "f:6"],
    ["nf.Y", "f:7"],
]
chip("alu", parts, wires,
     [{"name": "same as ref_alu", "equivalent": {"to": "ref_alu", "samples": 200000, "seed": 3}},
      unopt(5000)])

if __name__ == "__main__":
    cells.emit(cells.EDITS)
