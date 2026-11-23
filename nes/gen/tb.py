#!/usr/bin/env python3
"""Test benches.

    python3 nes/gen/tb.py > nes/build/tb.json

`cpu_tb`: the reference CPU on 64 KiB of RAM (the Harte, nestest and
Klaus configs in nes/tests use it); `cpu_tb_gates`: the same around
`cpu6502`, the CPU whose blocks are being replaced by gates (phase 4).
"""

import json
import sys


def tb(name, cpu):
    return {"circuit": name, "ops": [
        {"add": [
            {"name": "ce", "type": "Input"}, {"name": "rdy", "type": "Input"},
            {"name": "res_n", "type": "Input"}, {"name": "irq_n", "type": "Input"},
            {"name": "nmi_n", "type": "Input"}, {"name": "bcd_en", "type": "Input"},
            {"name": "magic", "type": "Input", "w": 8},
            {"name": "cpu", "use": cpu},
            {"name": "ram", "type": "Ram", "addr_bits": 16, "w": 8},
            {"name": "wr", "type": "Not"}, {"name": "we", "type": "And"},
            {"name": "addr", "type": "Output", "w": 16}, {"name": "sync", "type": "Output"},
            {"name": "rw", "type": "Output"}, {"name": "data", "type": "Output", "w": 8}]},
        {"wire": [
            ["ce", "cpu.ce", "we.b"], ["rdy", "cpu.rdy"], ["res_n", "cpu.res_n"],
            ["irq_n", "cpu.irq_n"], ["nmi_n", "cpu.nmi_n"], ["bcd_en", "cpu.bcd_en"],
            ["magic", "cpu.magic"],
            ["cpu.addr", "ram.addr", "addr"], ["cpu.dout", "ram.wdata"], ["cpu.rw", "wr", "rw"],
            ["wr", "we.a"], ["we", "ram.we"], ["ram.data", "cpu.din", "data"], ["cpu.sync", "sync"]]},
    ]}


if __name__ == "__main__":
    edits = [tb("cpu_tb", "cpu6502_ref")]
    if "--gates" in sys.argv:
        edits.append(tb("cpu_tb_gates", "cpu6502"))
    json.dump({"edits": edits}, sys.stdout, indent=1)
    print()
