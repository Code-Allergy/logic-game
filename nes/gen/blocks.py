"""The reference CPU as blocks: one Verilog module per block, so the
import gives one chip per block in the game, each later replaced by a
gate-level chip with the same ports (docs/nes-plan.md, phase 4).

Called by verilog.py with its namespace `g` (the encodings: ACTS,
ACT_BIT, CW_BITS, ASRC, WSRC, field_slices, SEQUENCES ...).

In a module's text, `@@act:NAME@@` is that micro-operation's control-word
bit, `@@cw:FIELD@@` and `@@ow:FIELD@@` the fields of the control and
operation words, `@@asrc:NAME@@`/`@@wsrc:NAME@@` their codes, `@@CW@@`/`@@OW@@`
the words' top bit index. Next-state logic applies the micro-operations a
block owns in the generator's one global order (verilog.act_order), so
each block keeps the model's step-by-step meaning.

Blocks and the registers they hold (each register its own `ref_regN`
instance, so tests reach it as `cpu/<block>/<name>_reg`):

| block     | registers            | does                                           |
|-----------|----------------------|------------------------------------------------|
| decode    |                      | control ROM {kind, ir, t}, operation ROM {ir}   |
| timing    | t, kind, tk          | sequencer, branch test, ce/rdy/reset gating     |
| interrupt | vec, nmi_prev, nmi_pend, poll | NMI edge, poll latch, vector select    |
| regs      | a, x, y, s           |                                                |
| status    | p                    | flag updates; next P for the poll              |
| pc_unit   | pc                   | increment, loads, branch add and fix           |
| agu       | adl, adh, ba, ci     | address mux, index adds, page fix, SH* value   |
| latches   | ir, dl               |                                                |
| operands  |                      | ALU inputs, operation, destinations; store value |
| alu       |                      | ref_alu                                        |
| bus       |                      | write data, rw, the internal data bus d        |
"""

import re


def block_modules(g):
    _, op_w = g.field_slices()
    op_sl, _ = g.field_slices()
    cw_fields = {"asrc": "[3:0]", "wr": "[4]", "wsrc": "[7:5]", "end": "[10:8]", "poll": "[11]"}

    # PC<-AD loads {d, ADL}: it always comes with ADH<-d in the same cycle.
    for seq in g.ALL_SEQS:
        for cyc in seq:
            if "PC<-AD" in cyc.acts:
                assert "ADH<-d" in cyc.acts, cyc

    def sub(text):
        def rep(m):
            kind, _, name = m.group(1).partition(":")
            if kind == "act":
                return f"cw[{g.ACT_BIT[name]}]"
            if kind == "cw":
                return "cw" + cw_fields[name]
            if kind == "ow":
                return "ow" + op_sl[name]
            if kind == "asrc":
                return f"4'd{g.ASRC.index(name)}"
            if kind == "wsrc":
                return f"3'd{g.WSRC.index(name)}"
            if kind == "CW":
                return str(g.CW_BITS - 1)
            if kind == "OW":
                return str(op_w - 1)
            raise KeyError(m.group(1))
        return re.sub(r"@@([^@]+)@@", rep, text)

    def ordered(code):
        """`code` ({act: statement}) as statements in the global order."""
        return "\n".join(f"    if (@@act:{a}@@) {code[a]}" for a in g.ACTS if a in code)

    def reg(name, w, en="commit"):
        mod = {16: "ref_reg16", 8: "ref_reg8", 4: "ref_reg4", 2: "ref_reg2", 1: "ref_reg1"}[w]
        return f"  {mod} {name}_reg (.clk(clk), .en({en}), .d({name}_n), .q({name}));"

    pc_code = {
        "PCL<-d": "pc_n[7:0] = d;",
        "PCH<-d": "pc_n[15:8] = d;",
        "PC+": "pc_n = pc_n + 16'd1;",
        "BRADD": "begin {cc, pc_n[7:0]} = {1'b0, pc_n[7:0]} + {1'b0, dl}; br_cross = cc ^ dl[7]; end",
        "BRFIX": "pc_n[15:8] = pc_n[15:8] + (dl[7] ? 8'hFF : 8'h01);",
        "PC<-AD": "pc_n = {d, adl};",
        "PCL<-DL": "pc_n[7:0] = dl;",
    }
    agu_code = {
        "ADL<-d": "adl_n = d;",
        "ADH<-d": "adh_n = d;",
        "ADH<-0": "adh_n = 8'h00;",
        "BA<-d": "ba_n = d;",
        "BA+X": "ba_n = ba_n + x;",
        "ADL+X": "adl_n = adl_n + x;",
        "ADL+Y": "adl_n = adl_n + y;",
        "IDX_X": "{ci_n, adl_n} = {1'b0, adl_n} + {1'b0, x};",
        "IDX_Y": "{ci_n, adl_n} = {1'b0, adl_n} + {1'b0, y};",
        "FIX": "adh_n = adh_n + {7'd0, ci_n};",
        "SH": "if (ci_n) adh_n = sh_v;",
        "BRADD": "ci_n = br_cross;",
    }
    latch_code = {
        "IR<-d": "ir_n = d;",
        "IR<-0": "ir_n = 8'h00;",
        "DL<-d": "dl_n = d;",
        "SH": "dl_n = sh_v;",
    }
    regs_code = {
        "S-": "s_n = s_n - 8'd1;",
        "S+": "s_n = s_n + 8'd1;",
        "SH": "if (@@ow:tas@@) s_n = a & x;",
    }
    status_code = {
        "SEI": "p_n = p_n | 8'h04;",
        "P<-d": "p_n = (d & 8'hCF) | (p_n & 8'h30);",
        "IMPL": "if (@@ow:flag@@ != 8'h00) p_n = @@ow:fval@@ ? (p_n | @@ow:flag@@) : (p_n & ~@@ow:flag@@);",
    }

    mods = []
    mods.append("""
module ref_decode (input [1:0] kind, input [7:0] ir, input [3:0] t,
                   output [@@CW@@:0] cw, output [@@OW@@:0] ow);
  ref_ctl_rom ctl (.addr({kind, kind == 2'd0 ? ir : 8'h00, t}), .data(cw));
  ref_op_rom opd (.addr(ir), .data(ow));
endmodule
""")
    mods.append("""
module ref_timing (input clk, input ce, input rdy, input res_n, input [@@CW@@:0] cw, input [7:0] ir,
                   input [7:0] p, input ci_next, input poll,
                   output [3:0] t, output [1:0] kind, output sync, output commit);
  wire tk;
  reg tk_n;
  reg [3:0] t_n;
  reg [1:0] kind_n;
  wire [2:0] c_end = @@cw:end@@;
  always @* begin
    tk_n = tk; t_n = t + 4'd1; kind_n = kind;
    // The branch test: IR bits 7-6 pick N, V, C or Z; bit 5 is the value.
    if (@@act:BRT@@) tk_n = (ir[7:6] == 2'd0 ? p[7] : ir[7:6] == 2'd1 ? p[6] :
                           ir[7:6] == 2'd2 ? p[0] : p[1]) == ir[5];
    if (c_end == 3'd1 || (c_end == 3'd2 && !ci_next) || (c_end == 3'd3 && !tk_n)) begin
      t_n = 4'd0;
      kind_n = poll ? 2'd1 : 2'd0;   // an interrupt polled at the next-to-last cycle
    end else if (c_end == 3'd4)
      t_n = t;                       // JAM: halted
    if (!res_n) begin
      t_n = 4'd0;
      kind_n = 2'd2;
    end
  end
  // A cycle commits on a clock enable, unless RDY holds a read; reset
  // holds the sequencer at the start of the reset sequence.
  assign commit = ce & res_n & (rdy | @@cw:wr@@);
  wire seq_en = ce & (~res_n | rdy | @@cw:wr@@);
  assign sync = (t == 4'd0) & (kind == 2'd0);
""" + "\n".join([reg("tk", 1), reg("t", 4, "seq_en"), reg("kind", 2, "seq_en")]) + """
endmodule
""")
    mods.append("""
module ref_interrupt (input clk, input commit, input [@@CW@@:0] cw, input [1:0] kind, input nmi_n,
                      input irq_n, input [7:0] p_next, output [7:0] vec, output poll);
  wire nmi_prev, nmi_pend;
  reg [7:0] vec_n;
  reg nmi_pend_n, poll_n;
  wire nmi_prev_n = ~nmi_n;
  always @* begin
    vec_n = vec; nmi_pend_n = nmi_pend; poll_n = poll;
    // The vector, chosen when P is pushed: an NMI pending by then takes it.
    if (@@act:VSEL@@) begin
      if (kind == 2'd2) vec_n = 8'hFC;
      else if (nmi_pend) begin vec_n = 8'hFA; nmi_pend_n = 1'b0; end
      else vec_n = 8'hFE;
    end
    // The NMI edge detector, then the poll latch.
    if (~nmi_n & ~nmi_prev) nmi_pend_n = 1'b1;
    if (@@cw:poll@@) poll_n = nmi_pend_n | (~irq_n & ~p_next[2]);
  end
""" + "\n".join([reg("vec", 8), reg("nmi_prev", 1), reg("nmi_pend", 1), reg("poll", 1)]) + """
endmodule
""")
    mods.append("""
module ref_regs (input clk, input commit, input [@@CW@@:0] cw, input [@@OW@@:0] ow, input [7:0] alu_r,
                 input [4:0] dst, input apply, output [7:0] a, output [7:0] x, output [7:0] y,
                 output [7:0] s);
  reg [7:0] a_n, x_n, y_n, s_n;
  always @* begin
    a_n = a; x_n = x; y_n = y; s_n = s;
""" + ordered(regs_code) + """
    if (apply) begin
      if (dst[0]) a_n = alu_r;
      if (dst[1]) x_n = alu_r;
      if (dst[2]) y_n = alu_r;
      if (dst[3]) s_n = alu_r;
    end
  end
""" + "\n".join([reg("a", 8), reg("x", 8), reg("y", 8), reg("s", 8)]) + """
endmodule
""")
    mods.append("""
module ref_status (input clk, input commit, input [@@CW@@:0] cw, input [@@OW@@:0] ow, input [7:0] d,
                   input [7:0] alu_f, input [7:0] alu_mask, input apply,
                   output [7:0] p, output [7:0] p_next);
  reg [7:0] p_n;
  always @* begin
    p_n = p;
""" + ordered(status_code) + """
    if (apply) p_n = (p_n & ~alu_mask) | (alu_f & alu_mask);
  end
  assign p_next = p_n;
""" + reg("p", 8) + """
endmodule
""")
    mods.append("""
module ref_pc_unit (input clk, input commit, input [@@CW@@:0] cw, input [7:0] d, input [7:0] dl,
                    input [7:0] adl, output [15:0] pc, output reg br_cross);
  reg [15:0] pc_n;
  reg cc;
  always @* begin
    pc_n = pc; cc = 1'b0; br_cross = 1'b0;
""" + ordered(pc_code) + """
  end
""" + reg("pc", 16) + """
endmodule
""")
    mods.append("""
module ref_agu (input clk, input commit, input [@@CW@@:0] cw, input [7:0] d, input [7:0] x,
                input [7:0] y, input [7:0] s, input [7:0] vec, input [7:0] sh_src, input [15:0] pc,
                input br_cross, output reg [15:0] addr, output [7:0] adl, output [7:0] adh,
                output ci, output reg ci_next, output [7:0] sh_v);
  wire [7:0] ba;
  reg [7:0] adl_n, adh_n, ba_n;
  reg ci_n;
  // SHA, SHX, SHY, TAS: the register AND the base address's high byte + 1.
  assign sh_v = sh_src & (adh + 8'd1);
  always @* begin
    adl_n = adl; adh_n = adh; ba_n = ba; ci_n = ci;
""" + ordered(agu_code) + """
    ci_next = ci_n;
  end
  always @* case (@@cw:asrc@@)
    @@asrc:PC@@: addr = pc;
    @@asrc:AD@@: addr = {adh, adl};
    @@asrc:ZB@@: addr = {8'h00, ba};
    @@asrc:ZB1@@: addr = {8'h00, ba + 8'd1};
    @@asrc:ADI@@: addr = {adh, adl + 8'd1};
    @@asrc:SP@@: addr = {8'h01, s};
    @@asrc:VECL@@: addr = {8'hFF, vec};
    @@asrc:VECH@@: addr = {8'hFF, vec | 8'h01};
    @@asrc:FFFE@@: addr = 16'hFFFE;
    default: addr = 16'hFFFF;
  endcase
""" + "\n".join([reg("adl", 8), reg("adh", 8), reg("ba", 8), reg("ci", 1)]) + """
endmodule
""")
    mods.append("""
module ref_latches (input clk, input commit, input [@@CW@@:0] cw, input [7:0] d, input [7:0] alu_r,
                    input dl_alu, input [7:0] sh_v, output [7:0] ir, output [7:0] dl);
  reg [7:0] ir_n, dl_n;
  always @* begin
    ir_n = ir; dl_n = dl;
""" + ordered(latch_code) + """
    if (dl_alu) dl_n = alu_r;
  end
""" + "\n".join([reg("ir", 8), reg("dl", 8)]) + """
endmodule
""")
    mods.append("""
module ref_operands (input [@@CW@@:0] cw, input [@@OW@@:0] ow, input [7:0] a, input [7:0] x,
                     input [7:0] y, input [7:0] s, input [7:0] dl, input [7:0] d, input ci,
                     input [7:0] magic,
                     output [4:0] op, output [7:0] alu_a, output [7:0] alu_b, output [4:0] dst,
                     output [7:0] mask, output apply, output [7:0] store_v, output [7:0] sh_src);
  function [7:0] sel(input [3:0] k);
    case (k)
      4'd1: sel = a;  4'd2: sel = x;  4'd3: sel = y;  4'd4: sel = s;  4'd5: sel = dl;
      4'd6: sel = a & x;  4'd7: sel = (a | magic) & x;  4'd8: sel = a | magic;
      default: sel = 8'h00;
    endcase
  endfunction
  // The combine stage takes A and DL; everything else the operation's
  // register and the bus byte (reads) or nothing.
  wire comb = @@act:COMBINE@@;
  wire rd = @@act:READ@@ | @@act:READ?nc@@;
  assign apply = @@act:READ@@ | (@@act:READ?nc@@ & ~ci) | @@act:RMW@@ | comb |
                 (@@act:IMPL@@ & (@@ow:flag@@ == 8'h00));
  assign op = comb ? @@ow:calu@@ : @@ow:alu@@;
  assign alu_a = comb ? a : sel(@@ow:asel@@);
  assign alu_b = comb ? dl : (rd ? d : 8'h00);
  assign dst = comb ? {4'b0000, @@ow:cdsta@@} : @@ow:dst@@;
  assign mask = comb ? @@ow:cmask@@ : @@ow:fmask@@;
  assign store_v = sel(@@ow:ssel@@);
  assign sh_src = sel(@@ow:shsel@@);
endmodule
""")
    mods.append("""
module ref_bus (input [@@CW@@:0] cw, input res_n, input [7:0] din, input [7:0] store_v,
                input [7:0] dl, input [7:0] a, input [7:0] p, input [15:0] pc,
                output [7:0] d, output reg [7:0] dout, output rw);
  always @* case (@@cw:wsrc@@)
    @@wsrc:STORE@@: dout = store_v;
    @@wsrc:DL@@: dout = dl;
    @@wsrc:PCH@@: dout = pc[15:8];
    @@wsrc:PCL@@: dout = pc[7:0];
    @@wsrc:PB@@: dout = p | 8'h30;
    @@wsrc:PI@@: dout = (p | 8'h20) & 8'hEF;
    default: dout = a;
  endcase
  assign rw = ~(@@cw:wr@@ & res_n);
  // The data bus inside the CPU: what is written, or what is read.
  assign d = @@cw:wr@@ ? dout : din;
endmodule
""")
    mods.append("""
// The 2A03's 6502 core, generated from nes/model (table.py, ops.py).
module cpu6502_ref (input clk, input ce, input rdy, input res_n, input irq_n, input nmi_n,
                    input bcd_en, input [7:0] magic, input [7:0] din,
                    output [15:0] addr, output [7:0] dout, output rw, output sync);
  wire [@@CW@@:0] cw;
  wire [@@OW@@:0] ow;
  wire [15:0] pc;
  wire [7:0] ir, dl, a, x, y, s, p, p_next, adl, adh, vec, d, sh_v, sh_src, store_v;
  wire [7:0] alu_a, alu_b, alu_r, alu_f, alu_mask;
  wire [4:0] alu_op, alu_dst;
  wire [3:0] t;
  wire [1:0] kind;
  wire commit, apply, ci, ci_next, br_cross, poll;
  ref_decode decode (.kind(kind), .ir(ir), .t(t), .cw(cw), .ow(ow));
  ref_timing timing (.clk(clk), .ce(ce), .rdy(rdy), .res_n(res_n), .cw(cw), .ir(ir), .p(p),
                     .ci_next(ci_next), .poll(poll), .t(t), .kind(kind), .sync(sync), .commit(commit));
  ref_interrupt interrupt (.clk(clk), .commit(commit), .cw(cw), .kind(kind), .nmi_n(nmi_n),
                           .irq_n(irq_n), .p_next(p_next), .vec(vec), .poll(poll));
  ref_operands operands (.cw(cw), .ow(ow), .a(a), .x(x), .y(y), .s(s), .dl(dl), .d(d), .ci(ci),
                         .magic(magic), .op(alu_op), .alu_a(alu_a), .alu_b(alu_b), .dst(alu_dst), .mask(alu_mask),
                         .apply(apply), .store_v(store_v), .sh_src(sh_src));
  ref_alu alu (.op(alu_op), .a(alu_a), .b(alu_b), .p(p), .bcd(bcd_en), .r(alu_r), .f(alu_f));
  ref_regs regs (.clk(clk), .commit(commit), .cw(cw), .ow(ow), .alu_r(alu_r), .dst(alu_dst),
                 .apply(apply), .a(a), .x(x), .y(y), .s(s));
  ref_status status (.clk(clk), .commit(commit), .cw(cw), .ow(ow), .d(d), .alu_f(alu_f),
                     .alu_mask(alu_mask), .apply(apply), .p(p), .p_next(p_next));
  ref_pc_unit pc_unit (.clk(clk), .commit(commit), .cw(cw), .d(d), .dl(dl), .adl(adl), .pc(pc),
                       .br_cross(br_cross));
  ref_agu agu (.clk(clk), .commit(commit), .cw(cw), .d(d), .x(x), .y(y), .s(s), .vec(vec),
               .sh_src(sh_src), .pc(pc), .br_cross(br_cross), .addr(addr), .adl(adl), .adh(adh),
               .ci(ci), .ci_next(ci_next), .sh_v(sh_v));
  ref_latches latches (.clk(clk), .commit(commit), .cw(cw), .d(d), .alu_r(alu_r),
                       .dl_alu(apply & alu_dst[4]), .sh_v(sh_v), .ir(ir), .dl(dl));
  ref_bus bus (.cw(cw), .res_n(res_n), .din(din), .store_v(store_v), .dl(dl), .a(a), .p(p),
               .pc(pc), .d(d), .dout(dout), .rw(rw));
endmodule
""")
    return "".join(sub(m) for m in mods)
