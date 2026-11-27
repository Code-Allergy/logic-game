#!/usr/bin/env python3
"""`cpu6502`: the CPU of phase 4, a copy of the imported reference
`cpu6502_ref` whose blocks are replaced one by one by gate-level chips
with the same ports. Each replaced block was first proven `equivalent`
to the reference block it replaces; the whole CPU is then checked by the
suites (nes/tests/*_gates.json).

    python3 nes/gen/cpu.py > nes/build/cpu.json
"""

import json
import sys

# Reference block (part label in cpu6502_ref) -> gate-level chip.
REPLACED = {
    "alu": "alu",
}

ops = [{"copy_of": "cpu6502_ref"}]
ops += [{"set": {"part": part, "use": chip}} for part, chip in REPLACED.items()]
json.dump({"edits": [{"circuit": "cpu6502", "ops": ops}]}, sys.stdout, indent=1)
print()
