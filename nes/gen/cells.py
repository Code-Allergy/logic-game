#!/usr/bin/env python3
"""Phase 1 cell library: the gate-level building blocks of the NES.

Writes an `edit` request ({"edits": [...]}) for the gatequest MCP server:

    python3 nes/gen/cells.py > nes/build/cells.json
    then: edit {"file": "nes/build/cells.json"}

Rules (docs/nes-plan.md): 1-bit gates, Const, wiring and `Delay` only;
every flip-flop is `dffe`/`dffre` (a Delay plus gates). Each cell carries a
functional test and an `unoptimized` test (optimised == unoptimised).
`compiled` pins (instruction budgets, which chips match) live in PINS
below and are added once measured.
"""

import json
import sys

EDITS = []


def chip(name, parts, wires, tests):
    EDITS.append({"circuit": name, "ops": [
        {"add": parts},
        {"wire": wires},
        {"tests": tests},
    ]})


def inp(name, w=1):
    return {"name": name, "type": "Input", "w": w}


def out(name, w=1):
    return {"name": name, "type": "Output", "w": w}


def gate(name, kind, count=None):
    g = {"name": name, "type": kind}
    if kind == "Const":
        g["value"] = 0
    if count:
        g["count"] = count
    return g


def use(name, cell, count=None):
    u = {"name": name, "use": cell}
    if count:
        u["count"] = count
    return u


def formula(expect, samples=None, seed=1):
    f = {"expect": expect}
    if samples:
        f["samples"] = samples
        f["seed"] = seed
    return {"name": "function", "formula": f}


def unopt(samples=500):
    return {"name": "unoptimized", "unoptimized": {"samples": samples, "seed": 7}}


def seq(steps):
    return {"name": "function", "sequence": {"steps": steps}}


# ── Combinational cells ─────────────────────────────────────────────────

chip("half_adder",
     [inp("A"), inp("B"), gate("x", "Xor"), gate("a", "And"), out("S"), out("C")],
     [["A", "x.a", "a.a"], ["B", "x.b", "a.b"], ["x", "S"], ["a", "C"]],
     [formula({"S": "A ^ B", "C": "A & B"}), unopt()])

chip("full_adder",
     [inp("A"), inp("B"), inp("Ci"), use("h0", "half_adder"), use("h1", "half_adder"),
      gate("o", "Or"), out("S"), out("Co")],
     [["A", "h0.A"], ["B", "h0.B"], ["h0.S", "h1.A"], ["Ci", "h1.B"],
      ["h0.C", "o.a"], ["h1.C", "o.b"], ["h1.S", "S"], ["o", "Co"]],
     [formula({"S": "(A + B + Ci) & 1", "Co": "(A + B + Ci) >> 1"}), unopt()])

# y = S ? B : A, from AND/OR/NOT.
chip("mux2",
     [inp("A"), inp("B"), inp("S"), gate("ns", "Not"), gate("ga", "And"), gate("gb", "And"),
      gate("o", "Or"), out("Y")],
     [["S", "ns", "gb.b"], ["A", "ga.a"], ["ns", "ga.b"], ["B", "gb.a"],
      ["ga", "o.a"], ["gb", "o.b"], ["o", "Y"]],
     [formula({"Y": "S ? B : A"}), unopt()])

chip("mux2x8",
     [inp("A", 8), inp("B", 8), inp("S"), use("m", "mux2", 8), out("Y", 8)],
     [["A", "m.A"], ["B", "m.B"], ["S", "m.S"], ["m.Y", "Y"]],
     [formula({"Y": "S ? B : A"}, samples=1000), unopt()])

# y = S[1] ? (S[0] ? D : C) : (S[0] ? B : A)
chip("mux4x8",
     [inp("A", 8), inp("B", 8), inp("C", 8), inp("D", 8), inp("S", 2),
      use("lo", "mux2x8"), use("hi", "mux2x8"), use("top", "mux2x8"), out("Y", 8)],
     [["A", "lo.A"], ["B", "lo.B"], ["C", "hi.A"], ["D", "hi.B"],
      ["S:0", "lo.S", "hi.S"], ["S:1", "top.S"], ["lo.Y", "top.A"], ["hi.Y", "top.B"],
      ["top.Y", "Y"]],
     [formula({"Y": "S == 0 ? A : S == 1 ? B : S == 2 ? C : D"}, samples=1000), unopt()])

# Incrementer: S = A + Ci, a chain of half adders.
chip("inc8",
     [inp("A", 8), inp("Ci"), use("h", "half_adder", 8), out("S", 8), out("Co")],
     [["A", "h.A"], ["Ci", "h[0].B"], ["h[0..7].C", "h[1..8].B"], ["h.S", "S"],
      ["h[7].C", "Co"]],
     [formula({"S": "A + Ci", "Co": "(A + Ci) >> 8"}), unopt()])

chip("adder8",
     [inp("A", 8), inp("B", 8), inp("Ci"), use("fa", "full_adder", 8), out("S", 8), out("Co")],
     [["A", "fa.A"], ["B", "fa.B"], ["Ci", "fa[0].Ci"], ["fa[0..7].Co", "fa[1..8].Ci"],
      ["fa.S", "S"], ["fa[7].Co", "Co"]],
     [formula({"S": "A + B + Ci", "Co": "(A + B + Ci) >> 8"}, samples=2000), unopt()])

# Reductions as balanced trees of 2-input gates.
def tree8(name, kind, invert, expr):
    parts = [inp("A", 8), gate("l0", kind, 4), gate("l1", kind, 2), gate("l2", kind)]
    wires = [[f"A:{2*i}", f"l0[{i}].a"] for i in range(4)] + \
            [[f"A:{2*i+1}", f"l0[{i}].b"] for i in range(4)] + \
            [["l0[0]", "l1[0].a"], ["l0[1]", "l1[0].b"], ["l0[2]", "l1[1].a"], ["l0[3]", "l1[1].b"],
             ["l1[0]", "l2.a"], ["l1[1]", "l2.b"]]
    if invert:
        parts.append(gate("n", "Not"))
        wires += [["l2", "n"], ["n", "Y"]]
    else:
        wires.append(["l2", "Y"])
    parts.append(out("Y"))
    chip(name, parts, wires, [formula({"Y": expr}), unopt()])


tree8("or8", "Or", False, "A != 0")
tree8("and8", "And", False, "A == 255")
tree8("zero8", "Or", True, "A == 0")

chip("eq8",
     [inp("A", 8), inp("B", 8), gate("x", "Xnor", 8), use("all", "and8"), out("Y")],
     [["A", "x.a"], ["B", "x.b"], ["x", "all"], ["all", "Y"]],
     [formula({"Y": "A == B"}, samples=2000), unopt()])

# 2-to-4 decoder with enable: Y bit i = En && A == i.
chip("decoder2",
     [inp("A", 2), inp("En"), gate("n", "Not", 2), gate("p", "And", 4), gate("e", "And", 4),
      out("Y", 4)],
     [["A:0", "n[0]", "p[1].a", "p[3].a"], ["A:1", "n[1]", "p[2].b", "p[3].b"],
      ["n[0]", "p[0].a", "p[2].a"], ["n[1]", "p[0].b", "p[1].b"],
      ["p", "e.a"], ["En", "e.b"], ["e", "Y"]],
     [formula({"Y": "En ? 1 << A : 0"}), unopt()])

# 3-to-8 decoder: two 2-to-4 decoders enabled by A[2].
chip("decoder3",
     [inp("A", 3), inp("En"), gate("n2", "Not"), gate("elo", "And"), gate("ehi", "And"),
      use("d", "decoder2", 2), out("Y", 8)],
     [["A:2", "n2", "ehi.a"], ["n2", "elo.a"], ["En", "elo.b", "ehi.b"],
      ["A:0..2", "d.A"], ["elo", "d[0].En"], ["ehi", "d[1].En"], ["d.Y", "Y"]],
     [formula({"Y": "En ? 1 << A : 0"}), unopt()])

# 5-to-32 decoder: a 2-to-4 decoder on A[4:3] enabling four 3-to-8 ones.
chip("decoder5",
     [inp("A", 5), inp("En"), use("hi", "decoder2"), use("lo", "decoder3", 4), out("Y", 32)],
     [["A:3..5", "hi.A"], ["En", "hi.En"], ["A:0..3", "lo.A"], ["hi.Y", "lo.En"], ["lo.Y", "Y"]],
     [formula({"Y": "En ? 1 << A : 0"}), unopt()])

chip("add4",
     [inp("A", 4), inp("B", 4), inp("Ci"), use("fa", "full_adder", 4), out("S", 4), out("Co")],
     [["A", "fa.A"], ["B", "fa.B"], ["Ci", "fa[0].Ci"], ["fa[0..3].Co", "fa[1..4].Ci"],
      ["fa.S", "S"], ["fa[3].Co", "Co"]],
     [formula({"S": "A + B + Ci", "Co": "(A + B + Ci) >> 4"}), unopt()])

# A 4-bit value is 10 or more (a decimal digit overflowed).
chip("ge10",
     [inp("A", 4), gate("o", "Or"), gate("k", "And"), out("Y")],
     [["A:1", "o.a"], ["A:2", "o.b"], ["A:3", "k.a"], ["o", "k.b"], ["k", "Y"]],
     [formula({"Y": "A >= 10"}), unopt()])

# A word gated by one select line: Y = D AND S (every bit).
chip("gate8",
     [inp("D", 8), inp("S"), gate("g", "And", 8), out("Y", 8)],
     [["D", "g.a"], ["S", "g.b"], ["g", "Y"]],
     [formula({"Y": "S ? D : 0"}), unopt()])

chip("or8w",
     [inp("A", 8), inp("B", 8), gate("o", "Or", 8), out("Y", 8)],
     [["A", "o.a"], ["B", "o.b"], ["o", "Y"]],
     [formula({"Y": "A | B"}, samples=1000), unopt()])

# One of eight 8-bit words by one-hot selects: Y = OR of (Di AND Si), a
# tree of or8w over eight gate8.
chip("sel8x8",
     [inp(f"D{i}", 8) for i in range(8)] + [inp(f"S{i}") for i in range(8)] +
     [use("g", "gate8", 8), use("o", "or8w", 4), use("p", "or8w", 2), use("q", "or8w"), out("Y", 8)],
     [[f"D{i}", f"g[{i}].D"] for i in range(8)] + [[f"S{i}", f"g[{i}].S"] for i in range(8)] +
     [[f"g[{2 * i}].Y", f"o[{i}].A"] for i in range(4)] + [[f"g[{2 * i + 1}].Y", f"o[{i}].B"] for i in range(4)] +
     [["o[0].Y", "p[0].A"], ["o[1].Y", "p[0].B"], ["o[2].Y", "p[1].A"], ["o[3].Y", "p[1].B"],
      ["p[0].Y", "q.A"], ["p[1].Y", "q.B"], ["q.Y", "Y"]],
     [formula({"Y": " | ".join(f"(S{i} ? D{i} : 0)" for i in range(8))}, samples=3000), unopt()])

# ── State cells ──────────────────────────────────────────────────────────
# Timing (docs/sim-v2-design.md 3.2): an input set before a step reaches
# state at the second tick, so each step runs 2 ticks.

chip("dffe",
     [inp("D"), inp("En"), use("m", "mux2"), gate("ff", "Delay"), out("Q")],
     [["ff", "m.A", "Q"], ["D", "m.B"], ["En", "m.S"], ["m.Y", "ff"]],
     [seq([{"set": {"D": 1, "En": 0}, "ticks": 2, "expect": {"Q": 0}},
           {"set": {"En": 1}, "ticks": 2, "expect": {"Q": 1}},
           {"set": {"D": 0, "En": 0}, "ticks": 2, "expect": {"Q": 1}},
           {"set": {"En": 1}, "ticks": 2, "expect": {"Q": 0}}]),
      unopt(1000)])

# Synchronous reset wins over enable.
chip("dffre",
     [inp("D"), inp("En"), inp("Rst"), use("m", "mux2"), gate("nr", "Not"), gate("k", "And"),
      gate("ff", "Delay"), out("Q")],
     [["ff", "m.A", "Q"], ["D", "m.B"], ["En", "m.S"], ["m.Y", "k.a"], ["Rst", "nr"],
      ["nr", "k.b"], ["k", "ff"]],
     [seq([{"set": {"D": 1, "En": 1}, "ticks": 2, "expect": {"Q": 1}},
           {"set": {"En": 0, "D": 0}, "ticks": 2, "expect": {"Q": 1}},
           {"set": {"Rst": 1}, "ticks": 2, "expect": {"Q": 0}},
           {"set": {"D": 1, "En": 1}, "ticks": 2, "expect": {"Q": 0}},
           {"set": {"Rst": 0}, "ticks": 2, "expect": {"Q": 1}}]),
      unopt(1000)])

chip("reg8",
     [inp("D", 8), inp("En"), use("b", "dffe", 8), out("Q", 8)],
     [["D", "b.D"], ["En", "b.En"], ["b.Q", "Q"]],
     [seq([{"set": {"D": 0x5A, "En": 0}, "ticks": 2, "expect": {"Q": 0}},
           {"set": {"En": 1}, "ticks": 2, "expect": {"Q": 0x5A}},
           {"set": {"D": 0xC3, "En": 0}, "ticks": 2, "expect": {"Q": 0x5A}},
           {"set": {"En": 1}, "ticks": 2, "expect": {"Q": 0xC3}}]),
      unopt(1000)])

chip("reg8r",
     [inp("D", 8), inp("En"), inp("Rst"), use("b", "dffre", 8), out("Q", 8)],
     [["D", "b.D"], ["En", "b.En"], ["Rst", "b.Rst"], ["b.Q", "Q"]],
     [seq([{"set": {"D": 0x5A, "En": 1}, "ticks": 2, "expect": {"Q": 0x5A}},
           {"set": {"Rst": 1}, "ticks": 2, "expect": {"Q": 0}},
           {"set": {"Rst": 0, "D": 0xFF}, "ticks": 2, "expect": {"Q": 0xFF}}]),
      unopt(1000)])

def emit(edits):
    """Write the request; `--only a,b` keeps those chips (to add new cells
    to a workspace that has the others)."""
    only = None
    if len(sys.argv) > 2 and sys.argv[1] == "--only":
        only = set(sys.argv[2].split(","))
    if only:
        edits = [e for e in edits if e["circuit"] in only]
    json.dump({"edits": edits}, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    emit(EDITS)
