#!/usr/bin/env python3
"""Generate the reference CPU (`cpu6502_ref`) in Verilog from the model's
tables, for `import_rtl` (docs/nes-plan.md, phases 2 and 3).

    python3 nes/gen/verilog.py > nes/build/cpu6502_ref.v

The sequencing (`nes/model/table.py`) becomes a control ROM addressed by
{kind, opcode, T}; the execution fields (`nes/model/ops.py`) an operation
ROM addressed by the opcode. The ALU and the datapath are written here, by
hand, to mirror `ops.alu()` and the model's micro-operations. Every
architectural register is its own module instance, so it has a stable
path after the import (`cpu/a_reg`, `cpu/pc_reg`, ...) that tests can
read and poke.

Ports: `clk` (unused: the simulator's tick is the clock), `ce` (one CPU
cycle per tick while high), `rdy` (low stalls a read cycle), `res_n`,
`irq_n`, `nmi_n` (active low, as on the chip), `bcd_en` (decimal mode;
tied low in the 2A03), `din`; out `addr`, `dout`, `rw` (1 = read),
`sync` (high in an opcode fetch).
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../model"))
from ops import WRITES, for_mode  # noqa: E402
from table import INTERRUPT, OPCODES, RESET, SEQUENCES  # noqa: E402

# ── Encodings ───────────────────────────────────────────────────────────

ASRC = ["PC", "AD", "ZB", "ZB1", "ADI", "SP", "VECL", "VECH", "FFFE", "FFFF"]
WSRC = ["STORE", "DL", "PCH", "PCL", "PB", "PI", "A"]
ENDS = ["", "end", "nc", "nt", "halt"]
SEL = ["", "A", "X", "Y", "S", "DL", "AX", "ANE", "LXA"]
ALU_OPS = ["NONE", "PASSA", "PASSB", "OR", "AND", "EOR", "ADC", "SBC", "CMP", "BIT", "INC",
           "DEC", "ASL", "LSR", "ROL", "ROR", "ANC", "ALR", "ARR"]
KIND_OP, KIND_INT, KIND_RESET = 0, 1, 2

ALL_SEQS = list(SEQUENCES.values()) + [INTERRUPT, RESET]


def act_order():
    """One order of the micro-operations that agrees with the order inside
    every cycle (a topological sort), so the Verilog can apply them in it."""
    names, after = [], {}
    for seq in ALL_SEQS:
        for cyc in seq:
            for i, a in enumerate(cyc.acts):
                if a not in after:
                    after[a] = set()
                    names.append(a)
                after[a].update(cyc.acts[:i])
    order, done = [], set()
    while len(order) < len(names):
        ready = [a for a in names if a not in done and after[a] <= done]
        if not ready:
            raise SystemExit(f"micro-operations ordered both ways: {sorted(set(names) - done)}")
        order.append(ready[0])
        done.add(ready[0])
    return order


ACTS = act_order()
ACT_BIT = {a: 12 + i for i, a in enumerate(ACTS)}
CW_BITS = 12 + len(ACTS)
assert CW_BITS <= 64


def control_word(cyc):
    w = ASRC.index(cyc.addr)
    if cyc.rw != "R":
        w |= 1 << 4
        w |= WSRC.index(cyc.rw[2:]) << 5
    w |= ENDS.index(cyc.end) << 8
    w |= int(cyc.poll) << 11
    for a in cyc.acts:
        w |= 1 << ACT_BIT[a]
    return w


def control_rom():
    """{address: word}, address = kind << 12 | ir << 4 | t (ir is 0 for
    interrupts and reset)."""
    rom = {}
    for ir, seq in SEQUENCES.items():
        assert len(seq) <= 16
        for t, cyc in enumerate(seq):
            rom[KIND_OP << 12 | ir << 4 | t] = control_word(cyc)
    for kind, seq in ((KIND_INT, INTERRUPT), (KIND_RESET, RESET)):
        for t, cyc in enumerate(seq):
            rom[kind << 12 | t] = control_word(cyc)
    return rom


# Operation word fields: (name, width).
OP_FIELDS = [("asel", 4), ("alu", 5), ("dst", 5), ("fmask", 8), ("calu", 5), ("cdsta", 1),
             ("cmask", 8), ("ssel", 4), ("shsel", 4), ("tas", 1), ("flag", 8), ("fval", 1)]
DST_BITS = {"A": 1, "X": 2, "Y": 4, "S": 8, "M": 16}


def op_word(ir):
    op = for_mode(*OPCODES[ir])
    f = {
        "asel": SEL.index(op.asel),
        "alu": ALU_OPS.index(op.alu),
        "dst": sum(DST_BITS[c] for c in op.dst) if not op.sh else 0,
        "fmask": WRITES[op.alu] if op.flags else 0,
        "calu": 0, "cdsta": 0, "cmask": 0,
        "ssel": SEL.index(op.store), "shsel": SEL.index(op.sh), "tas": int(bool(op.sh) and op.dst == "S"),
        "flag": op.flag, "fval": op.val,
    }
    if op.sh:
        f["asel"] = 0
        f["alu"] = 0
        if f["tas"]:
            f["shsel"] = SEL.index("AX")
    if op.combine:
        f["calu"] = ALU_OPS.index(op.combine.alu)
        f["cdsta"] = int("A" in op.combine.dst)
        f["cmask"] = WRITES[op.combine.alu]
    w, pos = 0, 0
    for name, width in OP_FIELDS:
        assert f[name] < 1 << width, (ir, name, f[name])
        w |= f[name] << pos
        pos += width
    return w


def field_slices():
    out, pos = {}, 0
    for name, width in OP_FIELDS:
        out[name] = f"[{pos + width - 1}:{pos}]" if width > 1 else f"[{pos}]"
        pos += width
    return out, pos


# ── Verilog ─────────────────────────────────────────────────────────────

ALU_ACTS = {"READ", "READ?nc", "RMW", "COMBINE", "IMPL"}
# Registers the ALU reads; no micro-operation before an ALU one in the
# same cycle may write them (the ALU sees the cycle's starting values).
ALU_INPUT_WRITERS = {"IR<-d", "IR<-0", "S-", "S+", "SEI", "P<-d", "DL<-d", "SH", "PC<-AD"}


def check_alu_inputs():
    for seq in ALL_SEQS:
        for cyc in seq:
            seen = set()
            for a in cyc.acts:
                if a in ALU_ACTS and seen & ALU_INPUT_WRITERS:
                    raise SystemExit(f"{cyc}: the ALU would see a value written this cycle")
                seen.add(a)


def regmod(name, w):
    rng = f"[{w - 1}:0] " if w > 1 else ""
    return f"""
module {name} (input clk, input en, input {rng}d, output reg {rng}q);
  always @(posedge clk) if (en) q <= d;
endmodule
"""


ALU_V = """
// The ALU: ops.alu() of the model. f holds N (7), V (6), Z (1), C (0);
// only the bits of the operation's flag mask are used.
module ref_alu (input [4:0] op, input [7:0] a, input [7:0] b, input [7:0] p, input bcd,
                output reg [7:0] r, output reg [7:0] f);
  integer s, lo, rr, bb;
  reg c, v, z, n;
  always @* begin
    c = p[0]; v = 1'b0; r = a; f = 8'h00;
    s = 0; lo = 0; rr = 0; bb = 0; z = 1'b0; n = 1'b0;
    case (op)
      5'd1: r = a;                          // PASSA
      5'd2: r = b;                          // PASSB
      5'd3: r = a | b;                      // OR
      5'd4: r = a & b;                      // AND
      5'd5: r = a ^ b;                      // EOR
      5'd6, 5'd7: begin                     // ADC, SBC
        bb = (op == 5'd6) ? b : (b ^ 8'hFF);
        s = a + bb + p[0];
        v = (~(a ^ bb[7:0]) & (a ^ s[7:0]) & 8'h80) != 0;
        r = s[7:0];
        c = s[8];
      end
      5'd8: begin s = a + (b ^ 8'hFF) + 1; r = s[7:0]; c = s[8]; end   // CMP
      5'd10: r = a + 8'd1;                  // INC
      5'd11: r = a - 8'd1;                  // DEC
      5'd12: begin r = {a[6:0], 1'b0}; c = a[7]; end              // ASL
      5'd13: begin r = {1'b0, a[7:1]}; c = a[0]; end              // LSR
      5'd14: begin r = {a[6:0], p[0]}; c = a[7]; end              // ROL
      5'd15: begin r = {p[0], a[7:1]}; c = a[0]; end              // ROR
      5'd16: begin r = a & b; c = r[7]; end                       // ANC
      5'd17: begin r = {1'b0, a[7:1] & b[7:1]}; c = a[0] & b[0]; end   // ALR
      5'd18: begin r = {p[0], a[7:1] & b[7:1]}; c = r[6]; v = r[6] ^ r[5]; end  // ARR
      default: ;
    endcase
    f = {r[7], v, 4'b0000, r == 8'h00, c};
    if (op == 5'd9)                         // BIT
      f = {b[7], b[6], 4'b0000, (a & b) == 8'h00, 1'b0};
    if (bcd && p[3] && op == 5'd6) begin    // decimal ADC (NMOS)
      lo = a[3:0] + b[3:0] + p[0];
      if (lo >= 10) lo = ((lo + 6) & 15) + 16;
      rr = (a & 8'hF0) + (b & 8'hF0) + lo;
      z = ((a + b + p[0]) & 255) == 0;
      v = ((~(a ^ b)) & (a ^ rr) & 128) != 0;
      n = rr[7];
      if (rr >= 160) rr = rr + 96;
      r = rr[7:0];
      f = {n, v, 4'b0000, z, rr >= 256};
    end
    if (bcd && p[3] && op == 5'd7) begin    // decimal SBC: binary flags, A adjusted
      lo = a[3:0] - b[3:0] + p[0] - 1;
      if (lo < 0) lo = ((lo - 6) & 15) - 16;
      rr = (a & 8'hF0) - (b & 8'hF0) + lo;
      if (rr < 0) rr = rr - 96;
      r = rr[7:0];
    end
  end
endmodule
"""


def rom_module(name, addr_bits, width, words):
    # Words not set here are 0 (the importer reads undefined memory as 0).
    lines = [f"module {name} (input [{addr_bits - 1}:0] addr, output [{width - 1}:0] data);",
             f"  reg [{width - 1}:0] rom [0:{(1 << addr_bits) - 1}];",
             "  initial begin"]
    for a in sorted(words):
        if words[a]:
            lines.append(f"    rom[{a}] = {width}'h{words[a]:x};")
    lines += ["  end", "  assign data = rom[addr];", "endmodule"]
    return "\n".join(lines) + "\n"


from blocks import block_modules  # noqa: E402  (the CPU, one module per block)


def main():
    check_alu_inputs()
    ctl = control_rom()
    ops = {ir: op_word(ir) for ir in range(256)}
    _, op_w = field_slices()
    out = [f"// Generated by nes/gen/verilog.py from nes/model; do not edit.\n"
           f"// Micro-operation order: {', '.join(ACTS)}\n"]
    for name, w in (("ref_reg16", 16), ("ref_reg8", 8), ("ref_reg4", 4), ("ref_reg2", 2), ("ref_reg1", 1)):
        out.append(regmod(name, w))
    out.append(ALU_V)
    out.append(rom_module("ref_ctl_rom", 14, CW_BITS, ctl))
    out.append(rom_module("ref_op_rom", 8, op_w, ops))
    out.append(block_modules(sys.modules[__name__]))
    sys.stdout.write("".join(out))


if __name__ == "__main__":
    main()
