#!/usr/bin/env python3
"""Run Klaus Dormann's 6502 functional test against the model.

    python3 nes/model/klaus.py [--bin ...] [--bcd] [--success 0x3469]

The prebuilt binary is a 64 KiB image loaded at $0000, started at $0400.
It ends in a `jmp *` (PC stays put): at `--success` everything passed;
anywhere else is a failed test, and the listing (`.lst`) at that address
says which. The prebuilt binary includes the decimal-mode tests, so it
needs `--bcd` (the strap the 2A03 ties off).
"""

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cpu import Cpu  # noqa: E402


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(here, "../roms/klaus/bin_files/6502_functional_test.bin"))
    ap.add_argument("--start", type=lambda s: int(s, 0), default=0x400)
    ap.add_argument("--success", type=lambda s: int(s, 0), default=0x3469)
    ap.add_argument("--bcd", action="store_true")
    args = ap.parse_args()
    mem = bytearray(open(args.bin, "rb").read())

    def write(a, v):
        mem[a] = v

    cpu = Cpu(mem.__getitem__, write, bcd=args.bcd)
    cpu.pc = args.start
    t0, n = time.time(), 0
    while True:
        pc = cpu.pc
        cpu.step()
        n += 1
        if cpu.pc == pc:
            break
    ok = pc == args.success
    print(f"{'passed' if ok else 'FAILED'}: trapped at {pc:04X} after {n} instructions, "
          f"{cpu.cycles} cycles ({time.time() - t0:.0f} s); test number $0200 = {mem[0x200]:02X}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
