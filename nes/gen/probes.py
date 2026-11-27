#!/usr/bin/env python3
"""One-instance wrappers ("probe_<cell>") around each cell, so that
`view {"what": "compiled"}` and `compiled` tests show what the compiler
makes of the cell itself (a chip is only replaced where it is placed; the
circuit compiled as the root never is). Every port is passed through, so
nothing folds to a constant.

    python3 nes/gen/probes.py > nes/build/probes.json
"""

import json
import sys

from cells import EDITS

# What the compiler makes of each cell where it is placed (measured with
# `view {"what": "compiled"}`, 2026-11-22): chips that must match, and the
# most instructions the probe may compile to. Budgets can only go down as
# the compiler improves; tighten them then.
# Pending the matcher fix for state held in child chips (en/d derived from
# ports, multi-bit states from 1-bit ones): reg8, reg8r and dffre do not
# match as Register yet. Add them to "matched" when it lands.
PINS = {
    "half_adder": ([], 2),            # "no cheaper: 2 vs 2"
    "full_adder": (["full_adder"], 2),
    "mux2": (["mux2"], 1),
    "mux2x8": (["mux2", "mux2x8"], 1),
    "mux4x8": (["mux2", "mux2x8"], 5),
    "inc8": (["inc8"], 4),            # as a lookup table, not an Add
    "adder8": (["adder8", "full_adder"], 2),
    "or8": (["or8"], 1),
    "and8": (["and8"], 1),
    "zero8": (["zero8"], 1),
    "eq8": (["and8", "eq8"], 1),
    "decoder2": ([], 15),             # no catalogue function
    "decoder3": ([], 27),
    "dffe": (["dffe", "mux2"], 1),
    "dffre": (["mux2"], 3),
    "reg8": (["dffe", "mux2"], 1),
    "reg8r": (["mux2"], 4),
}


def probe(edit):
    name = edit["circuit"]
    parts = edit["ops"][0]["add"]
    ports = [p for p in parts if p.get("type") in ("Input", "Output")]
    add = [dict(p) for p in ports] + [{"name": "u", "use": name}]
    wire = [[p["name"], f"u.{p['name']}"] if p["type"] == "Input" else [f"u.{p['name']}", p["name"]]
            for p in ports]
    matched, most = PINS[name]
    pin = {"instructions": {"max": most}}
    if matched:
        pin["matched"] = matched
    return {"circuit": f"probe_{name}", "ops": [
        {"add": add}, {"wire": wire}, {"tests": [{"name": "compiled", "compiled": pin}]}]}


if __name__ == "__main__":
    json.dump({"edits": [probe(e) for e in EDITS if e["circuit"] in PINS]}, sys.stdout, indent=1)
    print()
