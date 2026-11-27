#!/usr/bin/env python3
"""Run nestest in automation mode (PC = $C000) against the model and
compare every instruction with nestest.log: PC, A, X, Y, P, SP and the
cycle count before the instruction executes.

    python3 nes/model/nestest.py [--rom nes/roms/other/nestest.nes] [--log nes/roms/other/nestest.log]

The memory map is the console's CPU side, minus devices: 2 KiB of RAM
mirrored to $1FFF, the 16 KiB PRG ROM at $8000 and $C000 (NROM-128).
Everything else reads 0 and ignores writes; nestest's automation mode does
not depend on the PPU or APU. At the end, nestest's result bytes $02 and
$03 must both be 0.
"""

import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cpu import Cpu  # noqa: E402

LINE = re.compile(r"^([0-9A-F]{4}) .*A:([0-9A-F]{2}) X:([0-9A-F]{2}) Y:([0-9A-F]{2}) "
                  r"P:([0-9A-F]{2}) SP:([0-9A-F]{2}) .*CYC:(\d+)")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--rom", default=os.path.join(here, "../roms/other/nestest.nes"))
    ap.add_argument("--log", default=os.path.join(here, "../roms/other/nestest.log"))
    args = ap.parse_args()

    rom = open(args.rom, "rb").read()
    assert rom[:4] == b"NES\x1a"
    prg = rom[16:16 + rom[4] * 16384]
    ram = bytearray(0x800)

    def read(a):
        if a < 0x2000:
            return ram[a & 0x7FF]
        if a >= 0x8000:
            return prg[(a - 0x8000) % len(prg)]
        return 0

    def write(a, v):
        if a < 0x2000:
            ram[a & 0x7FF] = v

    cpu = Cpu(read, write)
    cpu.pc, cpu.s, cpu.p, cpu.cycles = 0xC000, 0xFD, 0x24, 7
    rows = [LINE.match(l) for l in open(args.log)]
    for n, m in enumerate(rows, 1):
        want = [int(m.group(i), 16) for i in range(1, 7)] + [int(m.group(7))]
        got = [cpu.pc, cpu.a, cpu.x, cpu.y, cpu.p, cpu.s, cpu.cycles]
        if got != want:
            names = ["PC", "A", "X", "Y", "P", "SP", "CYC"]
            diff = ", ".join(f"{k}: got {g:X} expected {w:X}" if k != "CYC" else f"CYC: got {g} expected {w}"
                             for k, g, w in zip(names, got, want) if g != w)
            print(f"line {n}: {diff}\n  {m.group(0)}")
            return 1
        cpu.step()
    print(f"all {len(rows)} instructions match; result bytes $02={ram[2]:02X} $03={ram[3]:02X}")
    return 0 if ram[2] == 0 and ram[3] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
