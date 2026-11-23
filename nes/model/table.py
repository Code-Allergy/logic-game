"""The 2A03 CPU as a table: every opcode's bus cycles as micro-operations.

This file is the single source of truth for the CPU (docs/nes-plan.md,
phase 2). The Python model (`cpu.py`) interprets it; later generators turn
the same table into the Verilog reference and the gate-level decode logic.

A **cycle** is `Cycle(addr, rw, acts, end)`:

- `addr`: where the bus points this cycle:
  `PC`; `AD` (ADH:ADL); `ZB` (00:BA) and `ZB1` (00:BA+1, wrapping in page
  zero); `ADI` (ADH:ADL+1, wrapping in the page: JMP indirect); `SP`
  (01:S); `VECL`/`VECH` (the vector being taken); `FFFE`/`FFFF`.
- `rw`: `R`, or `W:<source>` with source `STORE` (the operation's store
  value), `DL`, `PCH`, `PCL`, `PB` (P with B set, for BRK and PHP) or `A`.
- `acts`: register transfers done at the end of the cycle, in order, with
  `d` the byte on the bus. An act may carry a guard: `READ?nc` runs only
  when the index add did not carry into the high byte.
- `end`: `end` finishes the instruction after this cycle; `nc` finishes it
  when there was no page crossing; `nt` when a branch is not taken.
- `poll`: whether the interrupt poll latch is updated at the end of this
  cycle (default yes). An instruction is followed by an interrupt when the
  latch, as it stood at the end of the instruction's next-to-last cycle,
  held a pending interrupt: an NMI edge, or IRQ low with I clear. Only a
  branch's operand cycle does not poll, which is why a taken branch that
  does not cross a page delays an interrupt by one instruction.

Interrupts (`INTERRUPT`, `RESET`) are BRK's sequence with the opcode fetch
discarded (PC not incremented), P pushed with B clear, and for reset the
three stack writes turned into reads. The vector is chosen (`VSEL`) when
P is pushed: an NMI that is pending by then takes it, also from a BRK or
an IRQ that started first.

Every instruction starts with the opcode fetch `T0`. Guards and ends use
two condition bits: `CI` (the last index or branch add crossed a page)
and `TK` (the branch is taken).
"""

from dataclasses import dataclass, field


@dataclass(frozen=True)
class Cycle:
    addr: str
    rw: str = "R"
    acts: tuple = ()
    end: str = ""
    poll: bool = True


def C(addr, rw="R", acts=(), end="", poll=True):
    return Cycle(addr, rw, tuple(acts), end, poll)


T0 = C("PC", acts=["IR<-d", "PC+"])

# ── Opcodes ─────────────────────────────────────────────────────────────
# Mnemonic and addressing mode per opcode, row by row ($x0 .. $xF).

_ROWS = """
BRK impl|ORA izx|JAM -|SLO izx|NOP zp|ORA zp|ASL zp|SLO zp|PHP impl|ORA imm|ASL acc|ANC imm|NOP abs|ORA abs|ASL abs|SLO abs
BPL rel|ORA izy|JAM -|SLO izy|NOP zpx|ORA zpx|ASL zpx|SLO zpx|CLC impl|ORA aby|NOP impl|SLO aby|NOP abx|ORA abx|ASL abx|SLO abx
JSR abs|AND izx|JAM -|RLA izx|BIT zp|AND zp|ROL zp|RLA zp|PLP impl|AND imm|ROL acc|ANC imm|BIT abs|AND abs|ROL abs|RLA abs
BMI rel|AND izy|JAM -|RLA izy|NOP zpx|AND zpx|ROL zpx|RLA zpx|SEC impl|AND aby|NOP impl|RLA aby|NOP abx|AND abx|ROL abx|RLA abx
RTI impl|EOR izx|JAM -|SRE izx|NOP zp|EOR zp|LSR zp|SRE zp|PHA impl|EOR imm|LSR acc|ALR imm|JMP abs|EOR abs|LSR abs|SRE abs
BVC rel|EOR izy|JAM -|SRE izy|NOP zpx|EOR zpx|LSR zpx|SRE zpx|CLI impl|EOR aby|NOP impl|SRE aby|NOP abx|EOR abx|LSR abx|SRE abx
RTS impl|ADC izx|JAM -|RRA izx|NOP zp|ADC zp|ROR zp|RRA zp|PLA impl|ADC imm|ROR acc|ARR imm|JMP ind|ADC abs|ROR abs|RRA abs
BVS rel|ADC izy|JAM -|RRA izy|NOP zpx|ADC zpx|ROR zpx|RRA zpx|SEI impl|ADC aby|NOP impl|RRA aby|NOP abx|ADC abx|ROR abx|RRA abx
NOP imm|STA izx|NOP imm|SAX izx|STY zp|STA zp|STX zp|SAX zp|DEY impl|NOP imm|TXA impl|ANE imm|STY abs|STA abs|STX abs|SAX abs
BCC rel|STA izy|JAM -|SHA izy|STY zpx|STA zpx|STX zpy|SAX zpy|TYA impl|STA aby|TXS impl|TAS aby|SHY abx|STA abx|SHX aby|SHA aby
LDY imm|LDA izx|LDX imm|LAX izx|LDY zp|LDA zp|LDX zp|LAX zp|TAY impl|LDA imm|TAX impl|LXA imm|LDY abs|LDA abs|LDX abs|LAX abs
BCS rel|LDA izy|JAM -|LAX izy|LDY zpx|LDA zpx|LDX zpy|LAX zpy|CLV impl|LDA aby|TSX impl|LAS aby|LDY abx|LDA abx|LDX aby|LAX aby
CPY imm|CMP izx|NOP imm|DCP izx|CPY zp|CMP zp|DEC zp|DCP zp|INY impl|CMP imm|DEX impl|SBX imm|CPY abs|CMP abs|DEC abs|DCP abs
BNE rel|CMP izy|JAM -|DCP izy|NOP zpx|CMP zpx|DEC zpx|DCP zpx|CLD impl|CMP aby|NOP impl|DCP aby|NOP abx|CMP abx|DEC abx|DCP abx
CPX imm|SBC izx|NOP imm|ISC izx|CPX zp|SBC zp|INC zp|ISC zp|INX impl|SBC imm|NOP impl|SBC imm|CPX abs|SBC abs|INC abs|ISC abs
BEQ rel|SBC izy|JAM -|ISC izy|NOP zpx|SBC zpx|INC zpx|ISC zpx|SED impl|SBC aby|NOP impl|ISC aby|NOP abx|SBC abx|INC abx|ISC abx
"""

OPCODES = {}
for _hi, _row in enumerate(_ROWS.strip().splitlines()):
    for _lo, _cell in enumerate(_row.split("|")):
        _m, _mode = _cell.split()
        OPCODES[_hi * 16 + _lo] = (_m, _mode)
assert len(OPCODES) == 256

READS = {"ORA", "AND", "EOR", "ADC", "SBC", "CMP", "CPX", "CPY", "BIT", "LDA", "LDX",
         "LDY", "LAX", "NOP", "LAS", "ANC", "ALR", "ARR", "ANE", "LXA", "SBX"}
WRITES = {"STA", "STX", "STY", "SAX"}
# Unstable stores: value = reg & (base high byte + 1), which also replaces
# the high byte of the address when the index crosses a page.
SH_WRITES = {"SHA", "SHX", "SHY", "TAS"}
RMWS = {"ASL", "LSR", "ROL", "ROR", "INC", "DEC", "SLO", "RLA", "SRE", "RRA", "DCP", "ISC"}

# ── Cycle sequences ─────────────────────────────────────────────────────


def _tail(kind, addr="AD"):
    """The data cycles once the effective address is in AD."""
    if kind == "R":
        return [C(addr, acts=["READ"], end="end")]
    if kind == "W":
        return [C(addr, "W:STORE", end="end")]
    return [C(addr, acts=["DL<-d"]), C(addr, "W:DL", ["RMW"]), C(addr, "W:DL", ["COMBINE"], end="end")]


def _indexed_tail(kind, sh=False):
    """After an index add into ADL (carry in CI): the page-crossing cycles."""
    if kind == "R":
        return [C("AD", acts=["READ?nc", "FIX"], end="nc"), C("AD", acts=["READ"], end="end")]
    if kind == "W":
        if sh:
            return [C("AD", acts=["SH"]), C("AD", "W:DL", end="end")]
        return [C("AD", acts=["FIX"]), C("AD", "W:STORE", end="end")]
    return [C("AD", acts=["FIX"])] + _tail("M")


def _kind(m):
    if m in READS:
        return "R"
    if m in WRITES or m in SH_WRITES:
        return "W"
    if m in RMWS:
        return "M"
    raise KeyError(m)


def sequence(op):
    """The cycles of an opcode, T0 included."""
    m, mode = OPCODES[op]
    if m == "JAM":
        return [T0, C("PC"), C("FFFF"), C("FFFE"), C("FFFE")] + [C("FFFF")] * 5 + [C("FFFF", end="halt")]
    if mode in ("impl", "acc"):
        special = {
            "BRK": [C("PC", acts=["PC+"]), C("SP", "W:PCH", ["S-"]), C("SP", "W:PCL", ["S-"]),
                    C("SP", "W:PB", ["S-", "VSEL"]), C("VECL", acts=["PCL<-d", "SEI"]),
                    C("VECH", acts=["PCH<-d"], end="end")],
            "RTI": [C("PC"), C("SP", acts=["S+"]), C("SP", acts=["P<-d", "S+"]),
                    C("SP", acts=["PCL<-d", "S+"]), C("SP", acts=["PCH<-d"], end="end")],
            "RTS": [C("PC"), C("SP", acts=["S+"]), C("SP", acts=["PCL<-d", "S+"]),
                    C("SP", acts=["PCH<-d"]), C("PC", acts=["PC+"], end="end")],
            "PHA": [C("PC"), C("SP", "W:A", ["S-"], end="end")],
            "PHP": [C("PC"), C("SP", "W:PB", ["S-"], end="end")],
            "PLA": [C("PC"), C("SP", acts=["S+"]), C("SP", acts=["READ"], end="end")],
            "PLP": [C("PC"), C("SP", acts=["S+"]), C("SP", acts=["P<-d"], end="end")],
        }
        if m in special:
            return [T0] + special[m]
        return [T0, C("PC", acts=["IMPL"], end="end")]
    if mode == "rel":
        return [T0, C("PC", acts=["DL<-d", "PC+", "BRT"], end="nt", poll=False),
                C("PC", acts=["BRADD"], end="nc"), C("PC", acts=["BRFIX"], end="end")]
    if m == "JSR":
        return [T0, C("PC", acts=["ADL<-d", "PC+"]), C("SP"), C("SP", "W:PCH", ["S-"]),
                C("SP", "W:PCL", ["S-"]), C("PC", acts=["ADH<-d", "PC<-AD"], end="end")]
    if m == "JMP":
        lo = C("PC", acts=["ADL<-d", "PC+"])
        if mode == "abs":
            return [T0, lo, C("PC", acts=["ADH<-d", "PC<-AD"], end="end")]
        return [T0, lo, C("PC", acts=["ADH<-d", "PC+"]), C("AD", acts=["DL<-d"]),
                C("ADI", acts=["PCH<-d", "PCL<-DL"], end="end")]
    kind = _kind(m)
    if mode == "imm":
        return [T0, C("PC", acts=["PC+", "READ"], end="end")]
    if mode == "zp":
        return [T0, C("PC", acts=["ADL<-d", "ADH<-0", "PC+"])] + _tail(kind)
    if mode in ("zpx", "zpy"):
        idx = "ADL+X" if mode == "zpx" else "ADL+Y"
        return [T0, C("PC", acts=["ADL<-d", "ADH<-0", "PC+"]), C("AD", acts=[idx])] + _tail(kind)
    if mode == "abs":
        return [T0, C("PC", acts=["ADL<-d", "PC+"]), C("PC", acts=["ADH<-d", "PC+"])] + _tail(kind)
    if mode in ("abx", "aby"):
        idx = "IDX_X" if mode == "abx" else "IDX_Y"
        return [T0, C("PC", acts=["ADL<-d", "PC+"]), C("PC", acts=["ADH<-d", "PC+", idx])] + \
            _indexed_tail(kind, m in SH_WRITES)
    if mode == "izx":
        return [T0, C("PC", acts=["BA<-d", "PC+"]), C("ZB", acts=["BA+X"]),
                C("ZB", acts=["ADL<-d"]), C("ZB1", acts=["ADH<-d"])] + _tail(kind)
    if mode == "izy":
        return [T0, C("PC", acts=["BA<-d", "PC+"]), C("ZB", acts=["ADL<-d"]),
                C("ZB1", acts=["ADH<-d", "IDX_Y"])] + _indexed_tail(kind, m in SH_WRITES)
    raise ValueError(f"{op:02X}: {m} {mode}")


SEQUENCES = {op: sequence(op) for op in range(256)}

# IRQ and NMI: the fetched opcode is dropped (IR is forced to BRK's 00),
# PC is not incremented, P is pushed with B clear.
INTERRUPT = [C("PC", acts=["IR<-0"]), C("PC"), C("SP", "W:PCH", ["S-"]), C("SP", "W:PCL", ["S-"]),
             C("SP", "W:PI", ["S-", "VSEL"]), C("VECL", acts=["PCL<-d", "SEI"]),
             C("VECH", acts=["PCH<-d"], end="end")]

# Reset: the same, with the stack writes turned into reads.
RESET = [C("PC", acts=["IR<-0"]), C("PC"), C("SP", acts=["S-"]), C("SP", acts=["S-"]),
         C("SP", acts=["S-", "VSEL"]), C("VECL", acts=["PCL<-d", "SEI"]),
         C("VECH", acts=["PCH<-d"], end="end")]
