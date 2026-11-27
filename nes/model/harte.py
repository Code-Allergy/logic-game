#!/usr/bin/env python3
"""Run Tom Harte's nes6502 single-step tests against the table-driven model.

    python3 nes/model/harte.py [--dir nes/roms/harte/nes6502/v1] [--only a9,6c] [--limit N]
    python3 nes/model/harte.py --dir nes/roms/harte/6502/v1 --bcd   # decimal mode

Per case: set the registers and RAM, run one instruction, then compare
every bus cycle (address, data, read/write), the registers and the RAM.
Prints one line per failing opcode with its first failing case, and a
total. Exit code 0 when every case passed.
"""

import argparse
import json
import os
import sys
from multiprocessing import Pool

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cpu import Cpu  # noqa: E402
from ops import MAGIC_HARTE  # noqa: E402
from table import OPCODES  # noqa: E402


def run_case(case, bcd=False):
    """None when the case passes, else a description of the first difference."""
    ini, fin = case["initial"], case["final"]
    mem = dict((a, v) for a, v in ini["ram"])
    stray = []

    def read(a):
        if a not in mem:
            stray.append(a)
        return mem.get(a, 0)

    def write(a, v):
        mem[a] = v

    cpu = Cpu(read, write, bcd=bcd, magic=MAGIC_HARTE)
    cpu.pc, cpu.s, cpu.a, cpu.x, cpu.y, cpu.p = (ini[k] for k in ("pc", "s", "a", "x", "y", "p"))
    trace = []
    cpu.step(trace)
    want = [tuple(c) for c in case["cycles"]]
    if trace != want:
        for i, (got, exp) in enumerate(zip(trace, want)):
            if got != exp:
                return f"cycle {i}: got {_cyc(got)}, expected {_cyc(exp)}"
        return f"{len(trace)} cycles, expected {len(want)}"
    for k in ("pc", "s", "a", "x", "y", "p"):
        got = getattr(cpu, k)
        if got != fin[k]:
            return f"{k}: got {got:#04x}, expected {fin[k]:#04x}"
    for a, v in fin["ram"]:
        if mem.get(a) != v:
            return f"ram[{a:#06x}]: got {mem.get(a)}, expected {v}"
    return None


def _cyc(c):
    return f"{c[2]} {c[0]:04X}={c[1]:02X}"


def run_file(args):
    path, limit, bcd = args
    cases = json.load(open(path))
    if limit:
        cases = cases[:limit]
    failed, first = 0, None
    for case in cases:
        r = run_case(case, bcd)
        if r:
            failed += 1
            if first is None:
                first = (case["name"], r)
    return os.path.basename(path), len(cases), failed, first


def main():
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--dir", default=os.path.join(here, "../roms/harte/nes6502/v1"))
    ap.add_argument("--only", default="")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--bcd", action="store_true", help="decimal mode on (the plain 6502 set)")
    args = ap.parse_args()
    names = sorted(f for f in os.listdir(args.dir) if f.endswith(".json"))
    if args.only:
        keep = {o.lower().removesuffix(".json") for o in args.only.split(",")}
        names = [n for n in names if n[:-5] in keep]
    jobs = [(os.path.join(args.dir, n), args.limit, args.bcd) for n in names]
    total = bad = 0
    bad_ops = []
    with Pool() as pool:
        for name, n, failed, first in pool.imap(run_file, jobs):
            total += n
            bad += failed
            if failed:
                op = int(name[:2], 16)
                m, mode = OPCODES[op]
                bad_ops.append(op)
                print(f"{name[:2]} {m} {mode}: {failed}/{n} failed; first {first[0]!r}: {first[1]}")
    print(f"{total - bad} of {total} cases passed; {len(names) - len(bad_ops)} of {len(names)} opcodes clean")
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
