"""A cycle-level model of the 2A03 CPU that interprets `table.py`.

Control flow comes only from the table (cycles, guards, ends); this file
supplies the datapath: what each micro-operation does to the registers,
and the ALU functions of the operations. The gate-level CPU implements the
same micro-operations, so a disagreement between this model and a test
suite is a table or ALU bug, found before any gates exist.

The bus is a callable pair: `read(addr) -> byte`, `write(addr, byte)`.
Every cycle makes exactly one bus access.
"""

from ops import MAGIC_BLARGG, WRITES, alu, for_mode
from table import INTERRUPT, OPCODES, RESET, SEQUENCES

N, V, U, B, D, I, Z, C_ = 0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01

# The execution fields of every opcode.
OP = {ir: for_mode(*OPCODES[ir]) for ir in range(256)}


class Cpu:
    def __init__(self, read, write, bcd=False, magic=MAGIC_BLARGG):
        """`bcd`: the decimal-mode strap. False is the 2A03 (the D flag is
        kept but ADC and SBC ignore it); True is an NMOS 6502. `magic`: the
        ANE/LXA constant (see ops.py)."""
        self.read, self.write = read, write
        self.bcd = bcd
        self.magic = magic
        self.a = self.x = self.y = 0
        self.s = 0xFD
        self.p = 0x24
        self.pc = 0
        self.ir = 0
        self.adl = self.adh = self.ba = self.dl = 0
        self.ci = 0     # the last index or branch add crossed a page
        self.tk = 0     # the branch is taken
        self.cycles = 0
        self.halted = False
        self.vec = 0xFFFE
        # Interrupt inputs, active high here (the pins are active low):
        # set them between cycles, e.g. from `on_cycle(cpu)`, called
        # before every cycle.
        self.irq = False
        self.nmi = False
        self.on_cycle = None
        self._nmi_prev = False
        self.nmi_pending = False  # the edge detector's output
        self._poll = False        # the poll latch
        self._poll_prev = False   # ... as it stood one cycle earlier
        self._int_next = False    # the next "instruction" is an interrupt
        self._reset_next = False

    def reset(self):
        """Request reset: the next `step` runs the reset sequence."""
        self._reset_next = True

    # ── one instruction ────────────────────────────────────────────────
    def step(self, trace=None):
        """Run one instruction; with `trace`, append (addr, data, 'read'|'write')."""
        if self._reset_next:
            seq, self._reset_next, self._int_next, self._kind = RESET, False, False, "reset"
        elif self._int_next:
            seq, self._int_next, self._kind = INTERRUPT, False, "int"
        else:
            # T0, the opcode fetch, is the same for every opcode.
            seq, self._kind = SEQUENCES[0], "op"
        t = 0
        while True:
            if self.on_cycle:
                self.on_cycle(self)
            cyc = seq[t]
            addr = self._addr(cyc.addr)
            if cyc.rw == "R":
                d = self.read(addr)
                if trace is not None:
                    trace.append((addr, d, "read"))
            else:
                d = self._wdata(cyc.rw[2:])
                self.write(addr, d)
                if trace is not None:
                    trace.append((addr, d, "write"))
            for act in cyc.acts:
                name, _, guard = act.partition("?")
                if guard == "nc" and self.ci:
                    continue
                getattr(self, "_" + _ident(name))(d)
            self.cycles += 1
            # The NMI edge detector, then the poll latch (an edge seen in
            # this cycle is pending for a poll at its end).
            if self.nmi and not self._nmi_prev:
                self.nmi_pending = True
            self._nmi_prev = self.nmi
            self._poll_prev = self._poll
            if cyc.poll:
                self._poll = self.nmi_pending or (self.irq and not self.p & I)
            if t == 0 and self._kind == "op":
                seq = SEQUENCES[self.ir]
            e = cyc.end
            if e == "end" or (e == "nc" and not self.ci) or (e == "nt" and not self.tk):
                self._int_next = self._poll_prev
                return
            if e == "halt":
                self.halted = True
                return
            t += 1

    # ── addresses and write data ───────────────────────────────────────
    def _addr(self, src):
        if src == "PC":
            return self.pc
        if src == "AD":
            return self.adh << 8 | self.adl
        if src == "ZB":
            return self.ba
        if src == "ZB1":
            return (self.ba + 1) & 0xFF
        if src == "ADI":
            return self.adh << 8 | ((self.adl + 1) & 0xFF)
        if src == "SP":
            return 0x100 | self.s
        if src == "VECL":
            return self.vec
        if src == "VECH":
            return self.vec + 1
        if src == "FFFE":
            return 0xFFFE
        if src == "FFFF":
            return 0xFFFF
        raise KeyError(src)

    def _wdata(self, src):
        if src == "STORE":
            return self._reg(self.op.store)
        if src == "DL":
            return self.dl
        if src == "PCH":
            return self.pc >> 8
        if src == "PCL":
            return self.pc & 0xFF
        if src == "PB":
            return self.p | B | U
        if src == "PI":
            return (self.p | U) & ~B
        if src == "A":
            return self.a
        raise KeyError(src)

    @property
    def op(self):
        return OP[self.ir]

    # ── micro-operations ───────────────────────────────────────────────
    def _IR_d(self, d): self.ir = d
    def _IR_0(self, d): self.ir = 0

    def _VSEL(self, d):
        if self._kind == "reset":
            self.vec = 0xFFFC
        elif self.nmi_pending:
            self.vec = 0xFFFA
            self.nmi_pending = False
        else:
            self.vec = 0xFFFE
    def _PC_inc(self, d): self.pc = (self.pc + 1) & 0xFFFF
    def _ADL_d(self, d): self.adl = d
    def _ADH_d(self, d): self.adh = d
    def _ADH_0(self, d): self.adh = 0
    def _BA_d(self, d): self.ba = d
    def _DL_d(self, d): self.dl = d
    def _BA_X(self, d): self.ba = (self.ba + self.x) & 0xFF
    def _ADL_X(self, d): self.adl = (self.adl + self.x) & 0xFF
    def _ADL_Y(self, d): self.adl = (self.adl + self.y) & 0xFF

    def _IDX_X(self, d): self._index(self.x)
    def _IDX_Y(self, d): self._index(self.y)

    def _index(self, r):
        s = self.adl + r
        self.adl, self.ci = s & 0xFF, s >> 8

    def _FIX(self, d): self.adh = (self.adh + self.ci) & 0xFF

    def _SH(self, d):
        op = self.op
        if op.dst == "S":  # TAS: S = A & X first
            self.s = self._reg(op.asel)
        self.dl = self._reg(op.sh) & ((self.adh + 1) & 0xFF)
        if self.ci:
            self.adh = self.dl

    def _S_dec(self, d): self.s = (self.s - 1) & 0xFF
    def _S_inc(self, d): self.s = (self.s + 1) & 0xFF
    def _PCL_d(self, d): self.pc = (self.pc & 0xFF00) | d
    def _PCH_d(self, d): self.pc = (self.pc & 0x00FF) | d << 8
    def _PCL_DL(self, d): self.pc = (self.pc & 0xFF00) | self.dl
    def _PC_AD(self, d): self.pc = self.adh << 8 | self.adl
    def _SEI(self, d): self.p |= I

    def _P_d(self, d):
        # B and bit 5 are not flags; PLP and RTI keep the register's own.
        self.p = (d & ~(B | U)) | (self.p & (B | U))

    def _BRT(self, d):
        # The 6502's encoding: IR bits 7-6 pick N, V, C or Z; bit 5 is the
        # value that takes the branch.
        flag = (N, V, C_, Z)[self.ir >> 6]
        self.tk = 1 if bool(self.p & flag) == bool(self.ir & 0x20) else 0

    def _BRADD(self, d):
        off = self.dl - 256 if self.dl & 0x80 else self.dl
        new = (self.pc + off) & 0xFFFF
        self.ci = 1 if (new ^ self.pc) & 0xFF00 else 0
        self._br_target = new
        self.pc = (self.pc & 0xFF00) | (new & 0xFF)

    def _BRFIX(self, d): self.pc = self._br_target

    # ── operations (fields from ops.py, the ALU from ops.alu) ──────────
    def _reg(self, sel):
        a, x = self.a, self.x
        return {"A": a, "X": x, "Y": self.y, "S": self.s, "DL": self.dl, "AX": a & x,
                "ANE": (a | self.magic) & x, "LXA": a | self.magic, "": 0}[sel]

    def _exec(self, op, b):
        r, f = alu(op.alu, self._reg(op.asel), b, self.p, self.bcd)
        for dst in op.dst:
            if dst == "A": self.a = r
            elif dst == "X": self.x = r
            elif dst == "Y": self.y = r
            elif dst == "S": self.s = r
            elif dst == "M": self.dl = r
        if op.flags:
            mask = WRITES[op.alu]
            self.p = (self.p & ~mask) | (f & mask)

    def _READ(self, d): self._exec(self.op, d)
    def _RMW(self, d): self._exec(self.op, 0)

    def _COMBINE(self, d):
        if self.op.combine:
            self._exec(self.op.combine, self.dl)

    def _IMPL(self, d):
        op = self.op
        if op.flag:
            self.p = (self.p | op.flag) if op.val else (self.p & ~op.flag)
        else:
            self._exec(op, 0)

# Map the table's act names onto the methods above, and check every act
# the table uses exists, at import.
_MAP = {"PC+": "PC_inc", "S-": "S_dec", "S+": "S_inc", "ADL+X": "ADL_X", "ADL+Y": "ADL_Y",
        "BA+X": "BA_X"}


def _ident(name):
    if name in _MAP:
        return _MAP[name]
    return name.replace("<-", "_")


for _seq in list(SEQUENCES.values()) + [INTERRUPT, RESET]:
    for _c in _seq:
        for _a in _c.acts:
            _n = _ident(_a.partition("?")[0])
            assert hasattr(Cpu, "_" + _n), f"act {_a} has no implementation (_{_n})"
