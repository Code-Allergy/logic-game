"""What each operation does in the datapath: the execution half of the
decode (table.py is the sequencing half).

Every mnemonic is a few hardware fields, so the model, the Verilog
reference and the gate-level decode all read the same thing:

- `asel`: the ALU's A input: `A`, `X`, `Y`, `S`, `DL`; `AX` (A & X);
  `ANE` ((A | magic) & X); `LXA` (A | magic). The B input is the bus byte
  for reads, nothing for implied and read-modify-write operations, and DL
  for the combine stage.
- `alu`: the ALU operation (see `alu()` below).
- `dst`: registers written with the result, a string of `A`, `X`, `Y`,
  `S`, `M` (DL, the memory operand of read-modify-write).
- `flags`: False for operations that leave P alone (TXS, NOP, stores).

Read-modify-write operations have a second stage, `combine`: the
undocumented ones (SLO, RLA, ...) combine A with the new memory value on
the instruction's last cycle, as the chip does. `store` is the register a
store writes (`A`, `X`, `Y`, `AX`); SH* stores take `sh` (the register
ANDed with the address's high byte + 1). Flag instructions take `flag`
(the P bit) and `val`.
"""

from dataclasses import dataclass

# ANE and LXA ("unstable" opcodes) OR A with a constant that comes from a
# conflict on an internal bus, so it differs between real chips. It is the
# CPU's `magic` input (a jumper on the console). $FF: blargg's instr_test
# and most emulators; $EE: the chip Harte's nes6502 tests were recorded on.
MAGIC_BLARGG = 0xFF
MAGIC_HARTE = 0xEE


@dataclass(frozen=True)
class Op:
    asel: str = ""
    alu: str = "NONE"
    dst: str = ""
    flags: bool = True
    combine: "Op" = None
    store: str = ""
    sh: str = ""
    flag: int = 0
    val: int = 0


def _r(alu, asel="", dst="", flags=True):
    return Op(asel=asel, alu=alu, dst=dst, flags=flags)


def _rmw(alu, combine=None):
    return Op(asel="DL", alu=alu, dst="M", combine=combine)


OPS = {
    # Reads (B = the bus byte).
    "LDA": _r("PASSB", dst="A"), "LDX": _r("PASSB", dst="X"), "LDY": _r("PASSB", dst="Y"),
    "LAX": _r("PASSB", dst="AX"), "PLA": _r("PASSB", dst="A"),
    "ORA": _r("OR", "A", "A"), "AND": _r("AND", "A", "A"), "EOR": _r("EOR", "A", "A"),
    "ADC": _r("ADC", "A", "A"), "SBC": _r("SBC", "A", "A"),
    "CMP": _r("CMP", "A"), "CPX": _r("CMP", "X"), "CPY": _r("CMP", "Y"),
    "BIT": _r("BIT", "A"),
    "LAS": _r("AND", "S", "AXS"),
    "ANC": _r("ANC", "A", "A"), "ALR": _r("ALR", "A", "A"), "ARR": _r("ARR", "A", "A"),
    "ANE": _r("AND", "ANE", "A"), "LXA": _r("AND", "LXA", "AX"), "SBX": _r("CMP", "AX", "X"),
    # Implied (no B input).
    "TAX": _r("PASSA", "A", "X"), "TAY": _r("PASSA", "A", "Y"), "TXA": _r("PASSA", "X", "A"),
    "TYA": _r("PASSA", "Y", "A"), "TSX": _r("PASSA", "S", "X"),
    "TXS": _r("PASSA", "X", "S", flags=False),
    "INX": _r("INC", "X", "X"), "INY": _r("INC", "Y", "Y"),
    "DEX": _r("DEC", "X", "X"), "DEY": _r("DEC", "Y", "Y"),
    "NOP": _r("NONE", flags=False),
    "CLC": Op(flag=0x01, val=0), "SEC": Op(flag=0x01, val=1),
    "CLI": Op(flag=0x04, val=0), "SEI": Op(flag=0x04, val=1),
    "CLV": Op(flag=0x40, val=0), "CLD": Op(flag=0x08, val=0), "SED": Op(flag=0x08, val=1),
    # Shifts: on A (accumulator mode) or on DL (memory); see `for_mode`.
    "ASL": _rmw("ASL"), "LSR": _rmw("LSR"), "ROL": _rmw("ROL"), "ROR": _rmw("ROR"),
    "INC": _rmw("INC"), "DEC": _rmw("DEC"),
    "SLO": _rmw("ASL", _r("OR", "A", "A")), "RLA": _rmw("ROL", _r("AND", "A", "A")),
    "SRE": _rmw("LSR", _r("EOR", "A", "A")), "RRA": _rmw("ROR", _r("ADC", "A", "A")),
    "DCP": _rmw("DEC", _r("CMP", "A")), "ISC": _rmw("INC", _r("SBC", "A", "A")),
    # Stores.
    "STA": Op(store="A", flags=False), "STX": Op(store="X", flags=False),
    "STY": Op(store="Y", flags=False), "SAX": Op(store="AX", flags=False),
    "SHA": Op(sh="AX", flags=False), "SHX": Op(sh="X", flags=False),
    "SHY": Op(sh="Y", flags=False), "TAS": Op(sh="S", dst="S", asel="AX", flags=False),
}


def for_mode(m, mode):
    """The operation of mnemonic `m` in addressing mode `mode`."""
    op = OPS.get(m, Op(flags=False))
    if mode == "acc":
        return Op(asel="A", alu=op.alu, dst="A")
    return op


# ── The ALU ─────────────────────────────────────────────────────────────

N, V, D, Z, C = 0x80, 0x40, 0x08, 0x02, 0x01

# Flags each operation defines (N and Z come from the result unless noted).
WRITES = {
    "NONE": 0, "PASSA": N | Z, "PASSB": N | Z, "OR": N | Z, "AND": N | Z, "EOR": N | Z,
    "ADC": N | Z | C | V, "SBC": N | Z | C | V, "CMP": N | Z | C, "BIT": N | Z | V,
    "INC": N | Z, "DEC": N | Z, "ASL": N | Z | C, "LSR": N | Z | C, "ROL": N | Z | C,
    "ROR": N | Z | C, "ANC": N | Z | C, "ALR": N | Z | C, "ARR": N | Z | C | V,
}


def alu(op, a, b, p, bcd):
    """(result, new flag bits) of `op` on 8-bit `a`, `b` with flags `p`.
    Only the bits in WRITES[op] of the returned flags are meaningful."""
    c = p & C
    v = 0
    if op == "NONE":
        return a, 0
    if op == "PASSA":
        r = a
    elif op == "PASSB":
        r = b
    elif op == "OR":
        r = a | b
    elif op == "AND":
        r = a & b
    elif op == "EOR":
        r = a ^ b
    elif op in ("ADC", "SBC"):
        if bcd and p & D:
            return _decimal(op, a, b, c)
        bb = b if op == "ADC" else b ^ 0xFF
        s = a + bb + c
        v = V if ~(a ^ bb) & (a ^ s) & 0x80 else 0
        r, c = s & 0xFF, s >> 8
    elif op == "CMP":
        s = a + (b ^ 0xFF) + 1
        r, c = s & 0xFF, s >> 8
    elif op == "BIT":
        return a, (b & (N | V)) | (0 if a & b else Z)
    elif op == "INC":
        r = (a + 1) & 0xFF
    elif op == "DEC":
        r = (a - 1) & 0xFF
    elif op == "ASL":
        r, c = (a << 1) & 0xFF, a >> 7
    elif op == "LSR":
        r, c = a >> 1, a & 1
    elif op == "ROL":
        r, c = ((a << 1) | c) & 0xFF, a >> 7
    elif op == "ROR":
        r, c = (a >> 1) | (c << 7), a & 1
    elif op == "ANC":
        r = a & b
        c = r >> 7
    elif op == "ALR":
        t = a & b
        r, c = t >> 1, t & 1
    elif op == "ARR":
        r = ((a & b) >> 1) | (c << 7)
        c = (r >> 6) & 1
        v = V if ((r >> 6) ^ (r >> 5)) & 1 else 0
    else:
        raise KeyError(op)
    return r, (r & N) | (0 if r else Z) | (C if c else 0) | v


def _decimal(op, a, b, c):
    """NMOS decimal ADC/SBC."""
    if op == "ADC":
        lo = (a & 0x0F) + (b & 0x0F) + c
        if lo >= 0x0A:
            lo = ((lo + 0x06) & 0x0F) + 0x10
        r = (a & 0xF0) + (b & 0xF0) + lo
        z = (a + b + c) & 0xFF == 0
        v = ~(a ^ b) & (a ^ r) & 0x80
        n = r & 0x80
        if r >= 0xA0:
            r += 0x60
        return r & 0xFF, (N if n else 0) | (V if v else 0) | (Z if z else 0) | (C if r >= 0x100 else 0)
    # SBC: every flag from the binary subtraction; A decimal-adjusted.
    _, flags = alu("SBC", a, b, c, False)
    lo = (a & 0x0F) - (b & 0x0F) + c - 1
    if lo < 0:
        lo = ((lo - 0x06) & 0x0F) - 0x10
    r = (a & 0xF0) - (b & 0xF0) + lo
    if r < 0:
        r -= 0x60
    return r & 0xFF, flags
