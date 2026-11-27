// The reference 2C02 PPU (NTSC), hand-written, one module per block so
// the import gives one chip per block (docs/nes-plan.md, phase 7).
//
// Stage 1: CPU interface ($2000-$2007), the loopy VRAM address (v, t,
// fine x, w), the background fetch pipeline and shifters, palette RAM,
// vertical blank and NMI. Stage 2: OAM ($2003, $2004; DMA is the
// console's), sprite evaluation (one OAM entry per dot, dots 65-128),
// sprite fetches at dots 257-320 with the real addresses (the MMC3 counts
// A12 rises), eight sprite slots, priority, sprite 0 hit, overflow (without
// the hardware's evaluation bug). The odd-frame dot skip is in
// `ppu_timing` (its `skip` input is `rendering`). Not yet: $2007 access
// glitches during rendering, the $2002 read/vblank-set race, OAM decay.
//
// One tick is one PPU dot. `dot` (0..340) and `line` (0..261; 261 is the
// pre-render line) come from the gate-level `ppu_timing`. `cs` is a CPU
// access to $2000-$3FFF in the tick that commits the CPU cycle (it is
// already ANDed with the CPU's clock enable), so a register's side effect
// happens once. The PPU's own bus (pa, pd) reads combinationally: an
// address put out in a dot is answered in the same dot.

// Every dot and line predicate of the PPU, decoded once: two ROMs, indexed
// by the dot and by the line, whose words hold one flag per predicate (the
// blocks below read single bits instead of comparing `dot` and `line`, so
// the comparisons cost one ROM read per tick, not one operation each).
// The flags, by bit, are defined in `dflags` and `lflags`:
//
//   d[0..4]: fetch window (1-256, 321-336) and dot mod 8 = 1, 3, 5, 7, 0
//            (nametable, attribute, pattern low, pattern high, coarse x)
//   d[5] 256, d[6] 257, d[11] 1, d[13] 64, d[19] 328: that dot
//   d[7]: 280-304 (vertical scroll copy)  d[8]: 2-257 or 322-337 (shift)
//   d[9]: dot mod 8 = 1 in 9-257, or 329, 337 (shifter reload)
//   d[10]: 1-256 (a pixel's dot)  d[12]: 65-128 (sprite evaluation)
//   d[14]: 257-320 (sprite fetches); d[15], d[16], d[17]: and dot mod 8
//          in 5-7 or 0, = 5, = 7  d[18]: 264-327 (line buffer fill)
//   d[20]: 0-8 (left column)  d[21]: dot mod 8 = 7 or 0 (pattern high)
//   d[27:22]: dot - 65 (the OAM entry sprite evaluation reads)
//   d[30:28]: (dot - 257) / 8 mod 8 (the sprite slot being fetched)
//   d[38:31]: dot - 1 (the pixel's column, mod 256)
//   d[40:39]: what a background fetch puts on the bus: 0 nametable (dot
//             mod 8 = 1, 2), 1 attribute (3, 4), 2 pattern low (5, 6),
//             3 pattern high (7, 0)
//   l[0]: 0-239  l[1]: 261  l[2]: 241  l[3]: 0-239 or 261
module ppu_decode (input [8:0] dot, input [8:0] line, output [40:0] d, output [3:0] l);
  function [40:0] dflags(input integer n);
    reg [2:0] f;
    reg fw, sreg;
    begin
      f = n % 8;
      fw = (n >= 1 && n <= 256) || (n >= 321 && n <= 336);
      sreg = n >= 257 && n <= 320;
      dflags = 41'd0;
      dflags[0] = fw && f == 1;
      dflags[1] = fw && f == 3;
      dflags[2] = fw && f == 5;
      dflags[3] = fw && f == 7;
      dflags[4] = fw && f == 0;
      dflags[5] = n == 256;
      dflags[6] = n == 257;
      dflags[7] = n >= 280 && n <= 304;
      dflags[8] = (n >= 2 && n <= 257) || (n >= 322 && n <= 337);
      dflags[9] = f == 1 && ((n >= 9 && n <= 257) || n == 329 || n == 337);
      dflags[10] = n >= 1 && n <= 256;
      dflags[11] = n == 1;
      dflags[12] = n >= 65 && n <= 128;
      dflags[13] = n == 64;
      dflags[14] = sreg;
      dflags[15] = sreg && (f == 5 || f == 6 || f == 7 || f == 0);
      dflags[16] = sreg && f == 5;
      dflags[17] = sreg && f == 7;
      dflags[18] = n >= 264 && n <= 327;
      dflags[19] = n == 328;
      dflags[20] = n <= 8;
      dflags[21] = f == 7 || f == 0;
      dflags[27:22] = n - 65;
      dflags[30:28] = (n - 257) >> 3;
      dflags[38:31] = n - 1;
      dflags[40:39] = (f == 1 || f == 2) ? 2'd0 : (f == 3 || f == 4) ? 2'd1
                    : (f == 5 || f == 6) ? 2'd2 : 2'd3;
    end
  endfunction
  function [3:0] lflags(input integer n);
    begin
      lflags[0] = n < 240;
      lflags[1] = n == 261;
      lflags[2] = n == 241;
      lflags[3] = n < 240 || n == 261;
    end
  endfunction
  reg [40:0] drom [0:511];
  reg [3:0] lrom [0:511];
  integer i;
  initial for (i = 0; i < 512; i = i + 1) begin
    drom[i] = dflags(i);
    lrom[i] = lflags(i);
  end
  assign d = drom[dot];
  assign l = lrom[line];
endmodule

// Which dots fetch, shift, reload, scroll; vertical blank.
module ppu_render_ctl (input [40:0] d, input [3:0] l, input [7:0] mask,
                       output fetch_nt, output fetch_at, output fetch_lo, output fetch_hi,
                       output inc_x, output inc_y, output copy_h, output copy_v,
                       output shift, output reload, output pixel_on, output set_vbl,
                       output clr_vbl);
  wire ren = mask[3] | mask[4];
  wire rl = ren & l[3];
  assign fetch_nt = rl & d[0];
  assign fetch_at = rl & d[1];
  assign fetch_lo = rl & d[2];
  assign fetch_hi = rl & d[3];
  assign inc_x = rl & d[4];
  assign inc_y = rl & d[5];
  assign copy_h = rl & d[6];
  assign copy_v = ren & l[1] & d[7];
  assign shift = rl & d[8];
  assign reload = rl & d[9];
  assign pixel_on = l[0] & d[10];
  assign set_vbl = l[2] & d[11];
  assign clr_vbl = l[1] & d[11];
endmodule

// PPUCTRL, PPUMASK, PPUSTATUS, the read buffer and the open-bus latch.
module ppu_regs (input clk, input cs, input [2:0] ra, input rw, input [7:0] wd, input [7:0] pd,
                 input [5:0] pal_rd, input [13:0] v, input set_vbl, input clr_vbl,
                 input [7:0] oam_rd, input set_hit, input set_ovf,
                 output [7:0] ctrl, output [7:0] mask, output reg [7:0] rd, output nmi_n,
                 output w2000, output w2005, output w2006, output r2002, output a2007,
                 output w2003, output w2004, output pwe, output pal_we);
  wire [7:0] latch, buff;
  wire vbl, hit, ovf;
  wire wr = cs & ~rw;
  wire rdc = cs & rw;
  wire pal = v[13:8] == 6'h3F;
  assign w2000 = wr & (ra == 3'd0);
  assign w2005 = wr & (ra == 3'd5);
  assign w2006 = wr & (ra == 3'd6);
  assign r2002 = rdc & (ra == 3'd2);
  assign a2007 = cs & (ra == 3'd7);
  assign w2003 = wr & (ra == 3'd3);
  assign w2004 = wr & (ra == 3'd4);
  assign pwe = wr & (ra == 3'd7) & ~pal;
  assign pal_we = wr & (ra == 3'd7) & pal;
  always @* case (ra)
    3'd2: rd = {vbl, hit, ovf, latch[4:0]};
    3'd4: rd = oam_rd;
    3'd7: rd = pal ? {latch[7:6], pal_rd} : buff;
    default: rd = latch;                          // write-only registers: open bus
  endcase
  wire [7:0] ctrl_n = (wr & (ra == 3'd0)) ? wd : ctrl;
  wire [7:0] mask_n = (wr & (ra == 3'd1)) ? wd : mask;
  wire [7:0] latch_n = wr ? wd : (rdc & (ra == 3'd2 || ra == 3'd4 || ra == 3'd7)) ? rd : latch;
  wire [7:0] buff_n = (rdc & (ra == 3'd7)) ? pd : buff;
  wire vbl_n = r2002 ? 1'b0 : clr_vbl ? 1'b0 : set_vbl ? 1'b1 : vbl;
  wire hit_n = clr_vbl ? 1'b0 : set_hit ? 1'b1 : hit;
  wire ovf_n = clr_vbl ? 1'b0 : set_ovf ? 1'b1 : ovf;
  assign nmi_n = ~(vbl & ctrl[7]);
  ppu_reg8 ctrl_reg (.clk(clk), .en(1'b1), .d(ctrl_n), .q(ctrl));
  ppu_reg8 mask_reg (.clk(clk), .en(1'b1), .d(mask_n), .q(mask));
  ppu_reg8 latch_reg (.clk(clk), .en(1'b1), .d(latch_n), .q(latch));
  ppu_reg8 buff_reg (.clk(clk), .en(1'b1), .d(buff_n), .q(buff));
  ppu_reg1 vbl_reg (.clk(clk), .en(1'b1), .d(vbl_n), .q(vbl));
  ppu_reg1 hit_reg (.clk(clk), .en(1'b1), .d(hit_n), .q(hit));
  ppu_reg1 ovf_reg (.clk(clk), .en(1'b1), .d(ovf_n), .q(ovf));
endmodule

// The loopy registers: v (current VRAM address), t (temporary), fine x, w.
module ppu_loopy (input clk, input [7:0] wd, input [7:0] ctrl, input w2000, input w2005,
                  input w2006, input r2002, input a2007, input inc_x, input inc_y,
                  input copy_h, input copy_v, output [14:0] v, output [2:0] fx);
  wire [14:0] t;
  wire w;
  reg [14:0] v_n, t_n;
  reg [2:0] fx_n;
  reg w_n;
  reg [4:0] cy;
  always @* begin
    v_n = v; t_n = t; fx_n = fx; w_n = w; cy = 5'd0;
    // Rendering: scroll increments and copies from t.
    if (inc_x) begin
      if (v_n[4:0] == 5'd31) begin v_n[4:0] = 5'd0; v_n[10] = ~v_n[10]; end
      else v_n[4:0] = v_n[4:0] + 5'd1;
    end
    if (inc_y) begin
      if (v_n[14:12] != 3'd7) v_n[14:12] = v_n[14:12] + 3'd1;
      else begin
        v_n[14:12] = 3'd0;
        cy = v_n[9:5];
        if (cy == 5'd29) begin cy = 5'd0; v_n[11] = ~v_n[11]; end
        else if (cy == 5'd31) cy = 5'd0;
        else cy = cy + 5'd1;
        v_n[9:5] = cy;
      end
    end
    if (copy_h) begin v_n[10] = t_n[10]; v_n[4:0] = t_n[4:0]; end
    if (copy_v) begin v_n[14:11] = t_n[14:11]; v_n[9:5] = t_n[9:5]; end
    // The CPU.
    if (w2000) t_n[11:10] = wd[1:0];
    if (w2005) begin
      if (!w) begin t_n[4:0] = wd[7:3]; fx_n = wd[2:0]; end
      else begin t_n[14:12] = wd[2:0]; t_n[9:5] = wd[7:3]; end
      w_n = ~w;
    end
    if (w2006) begin
      if (!w) begin t_n[13:8] = wd[5:0]; t_n[14] = 1'b0; end
      else begin t_n[7:0] = wd; v_n = {t_n[14:8], wd}; end
      w_n = ~w;
    end
    if (r2002) w_n = 1'b0;
    if (a2007) v_n = v + (ctrl[2] ? 15'd32 : 15'd1);
  end
  ppu_reg15 v_reg (.clk(clk), .en(1'b1), .d(v_n), .q(v));
  ppu_reg15 t_reg (.clk(clk), .en(1'b1), .d(t_n), .q(t));
  ppu_reg3 fx_reg (.clk(clk), .en(1'b1), .d(fx_n), .q(fx));
  ppu_reg1 w_reg (.clk(clk), .en(1'b1), .d(w_n), .q(w));
endmodule

// The background: fetch addresses, tile latches, shift registers, pixel.
module ppu_bg (input clk, input [40:0] d, input [14:0] v, input [2:0] fx, input [7:0] ctrl,
               input [7:0] mask, input [7:0] pd, input fetch_nt, input fetch_at, input fetch_lo,
               input fetch_hi, input shift, input reload, output reg [13:0] fa,
               output [3:0] pixel);
  wire [7:0] nt, lo, hi;
  wire [1:0] at;
  wire [15:0] sl, sh, al, ah;
  wire [13:0] pt = {1'b0, ctrl[4], nt, 1'b0, v[14:12]};
  // What each dot of an 8-dot fetch group puts on the bus (dot mod 8):
  // 1-2 nametable, 3-4 attribute, 5-6 pattern low, 7-0 pattern high.
  always @* case (d[40:39])
    2'd0: fa = {2'b10, v[11:0]};
    2'd1: fa = {2'b10, v[11:10], 4'b1111, v[9:7], v[4:2]};
    2'd2: fa = pt;
    default: fa = {pt[13:4], 1'b1, pt[2:0]};
  endcase
  wire [7:0] at_byte_sh = pd >> {v[6], v[1], 1'b0};
  wire [7:0] nt_n = fetch_nt ? pd : nt;
  wire [1:0] at_n = fetch_at ? at_byte_sh[1:0] : at;
  wire [7:0] lo_n = fetch_lo ? pd : lo;
  wire [7:0] hi_n = fetch_hi ? pd : hi;
  wire [15:0] sl_s = shift ? {sl[14:0], 1'b0} : sl;
  wire [15:0] sh_s = shift ? {sh[14:0], 1'b0} : sh;
  wire [15:0] al_s = shift ? {al[14:0], 1'b0} : al;
  wire [15:0] ah_s = shift ? {ah[14:0], 1'b0} : ah;
  wire [15:0] sl_n = reload ? {sl_s[15:8], lo} : sl_s;
  wire [15:0] sh_n = reload ? {sh_s[15:8], hi} : sh_s;
  wire [15:0] al_n = reload ? {al_s[15:8], {8{at[0]}}} : al_s;
  wire [15:0] ah_n = reload ? {ah_s[15:8], {8{at[1]}}} : ah_s;
  // The pixel after this dot's shift: bit 15 - fine x of the shifted value.
  wire [15:0] psl = sl_s << fx, psh = sh_s << fx, pal = al_s << fx, pah = ah_s << fx;
  wire clip = ~mask[3] | (d[20] & ~mask[1]);
  assign pixel = clip ? 4'd0 : {pah[15], pal[15], psh[15], psl[15]};
  ppu_reg8 nt_reg (.clk(clk), .en(1'b1), .d(nt_n), .q(nt));
  ppu_reg2 at_reg (.clk(clk), .en(1'b1), .d(at_n), .q(at));
  ppu_reg8 lo_reg (.clk(clk), .en(1'b1), .d(lo_n), .q(lo));
  ppu_reg8 hi_reg (.clk(clk), .en(1'b1), .d(hi_n), .q(hi));
  ppu_reg16 sl_reg (.clk(clk), .en(1'b1), .d(sl_n), .q(sl));
  ppu_reg16 sh_reg (.clk(clk), .en(1'b1), .d(sh_n), .q(sh));
  ppu_reg16 al_reg (.clk(clk), .en(1'b1), .d(al_n), .q(al));
  ppu_reg16 ah_reg (.clk(clk), .en(1'b1), .d(ah_n), .q(ah));
endmodule

// Palette RAM: 32 entries of 6 bits; $3F10/$14/$18/$1C are $3F00/$04/$08/$0C.
module ppu_palette (input clk, input we, input [4:0] waddr, input [7:0] wd, input [4:0] index,
                    input [7:0] mask, output [5:0] colour, output [5:0] cpu_rd);
  reg [5:0] pal [0:31];
  function [4:0] mirror(input [4:0] a);
    mirror = (a[1:0] == 2'd0) ? {1'b0, a[3:0]} : a;
  endfunction
  always @(posedge clk) if (we) pal[mirror(waddr)] <= wd[5:0];
  wire [5:0] c = pal[mirror(index)];
  assign colour = mask[0] ? (c & 6'h30) : c;     // greyscale
  assign cpu_rd = pal[mirror(waddr)];
endmodule


// OAM: 64 sprites of 4 bytes (y, tile, attributes, x) as 32-bit words,
// written a byte at a time through $2004, read a sprite at a time by the
// evaluation. Attribute bits 2-4 do not exist (read 0).
module ppu_oam (input clk, input [7:0] wd, input w2003, input w2004, input clr_addr,
                input [5:0] eval_idx, output [7:0] cpu_rd, output [31:0] eval_word);
  reg [31:0] oam [0:63];
  wire [7:0] addr;
  reg [7:0] addr_n;
  always @* begin
    addr_n = addr;
    if (clr_addr) addr_n = 8'h00;
    if (w2003) addr_n = wd;
    if (w2004) addr_n = addr + 8'd1;
  end
  always @(posedge clk) if (w2004) case (addr[1:0])
    2'd0: oam[addr[7:2]][7:0] <= wd;
    2'd1: oam[addr[7:2]][15:8] <= wd;
    2'd2: oam[addr[7:2]][23:16] <= wd & 8'hE3;
    default: oam[addr[7:2]][31:24] <= wd;
  endcase
  wire [31:0] cpu_word = oam[addr[7:2]];
  assign cpu_rd = cpu_word >> {addr[1:0], 3'b000};
  assign eval_word = oam[eval_idx];
  ppu_reg8 addr_reg (.clk(clk), .en(1'b1), .d(addr_n), .q(addr));
endmodule

// Sprite evaluation into secondary OAM, the sprite fetches, and a line
// buffer that holds the next line's sprite pixels.
//
// Evaluation and the fetches are the 2C02's, dot for dot (the MMC3 counts
// the fetches' A12 rises). Rendering is not: instead of eight sprite units
// that each work out their pixel on every dot, each fetched sprite is
// written into a 256-entry line buffer, one pixel per dot, in the eight
// dots after its pattern-high fetch (sprite k at dots 264 + 8k .. 271 + 8k,
// so 264..327). A pixel is written only where the buffer is still
// transparent, and sprites are written in slot order, so the first opaque
// sprite wins as on the chip. An entry is {sprite 0, priority, palette,
// pattern bits}.
//
// There are two buffers. The fill writes the back one and, once a line's
// fetch has run (dot 328 of a rendered line), it becomes the front one,
// which the next line displays; the back one is cleared while the front
// is displayed (dots 1..256 of lines 0..239 and the pre-render line). A
// line whose fetch does not run (rendering off in its HBlank, as SMB3 does
// at its status bar) leaves the front as it was, so the next line shows
// the last fetched sprites again: the chip's sprite units are not
// reloaded, and nothing shifted them out while rendering was off.
//
// Seen from outside (pixels, priority, sprite 0 hit, overflow) this is the
// 2C02; it costs one buffer read per dot instead of eight comparisons,
// shifts and a priority chain.
module ppu_sprites (input clk, input [8:0] line, input [40:0] d,
                    input [3:0] l, input [7:0] ctrl,
                    input [7:0] mask, input [7:0] pd, input [31:0] eval_word,
                    output [5:0] eval_idx, output [13:0] fa, output fetching, output clr_oam_addr,
                    output [1:0] bits, output [1:0] pal, output prio,
                    output from0, output set_ovf);
  reg [31:0] sec [0:7];
  reg [5:0] buf_ram [0:511];
  wire [3:0] cnt;
  wire s0_next;
  wire ren = mask[3] | mask[4];
  wire vis = l[0];
  wire rl = ren & l[3];
  wire h16 = ctrl[5];
  // Evaluation: OAM entry dot - 65 at dots 65..128 of a visible line.
  assign eval_idx = d[27:22];
  wire evaluating = ren & vis & d[12];
  wire [8:0] diff = line - {1'b0, eval_word[7:0]};
  wire in_range = (line >= {1'b0, eval_word[7:0]}) & (diff < (h16 ? 9'd16 : 9'd8));
  wire take = evaluating & in_range & ~cnt[3];
  assign set_ovf = evaluating & in_range & cnt[3];
  always @(posedge clk) if (take) sec[cnt[2:0]] <= eval_word;
  wire [3:0] cnt_n = d[13] ? 4'd0 : take ? cnt + 4'd1 : cnt;
  wire s0_next_n = d[13] ? 1'b0 : (take & (eval_idx == 6'd0)) ? 1'b1 : s0_next;
  // Fetches: slot k at dots 257 + 8k .. 264 + 8k (dot mod 8: 1-4 garbage
  // nametable and attribute, 5-6 pattern low, 7-0 pattern high).
  wire region = rl & d[14];
  wire [2:0] k = d[30:28];
  wire valid = {1'b0, k} < cnt;
  wire [31:0] e = valid ? sec[k] : 32'hFFFFFFFF;
  wire [8:0] row9 = line - {1'b0, e[7:0]};
  wire [3:0] row = e[23] ? ((h16 ? 4'd15 : 4'd7) - row9[3:0]) : row9[3:0];   // vertical flip
  wire plane = d[21];
  assign fa = h16 ? {1'b0, e[8], e[15:9], row[3], plane, row[2:0]}
                  : {1'b0, ctrl[3], e[15:8], plane, row[2:0]};
  assign fetching = rl & d[15];
  assign clr_oam_addr = region;
  // The fetched sprite, held while its pixels are written: pattern bytes
  // (flipped horizontally if asked, zero for an empty slot), shifted out
  // high bit first, and the buffer column of the next pixel.
  wire [7:0] lo, hi, wattr;
  wire [8:0] wx;
  wire wvalid, ws0, front;
  wire [7:0] rpd = {pd[0], pd[1], pd[2], pd[3], pd[4], pd[5], pd[6], pd[7]};
  wire [7:0] pat = ~valid ? 8'h00 : e[22] ? rpd : pd;
  wire fetch_lo = rl & d[16];
  wire capture = rl & d[17];
  wire filling = rl & d[18];
  wire [7:0] lo_fetched;
  ppu_reg8 lo_fetch_reg (.clk(clk), .en(fetch_lo), .d(pat), .q(lo_fetched));
  wire [7:0] lo_n = capture ? lo_fetched : filling ? {lo[6:0], 1'b0} : lo;
  wire [7:0] hi_n = capture ? pat : filling ? {hi[6:0], 1'b0} : hi;
  wire [8:0] wx_n = capture ? {1'b0, e[31:24]} : filling ? wx + 9'd1 : wx;
  wire [7:0] wattr_n = capture ? e[23:16] : wattr;
  wire wvalid_n = capture ? valid : wvalid;
  wire ws0_n = capture ? ((k == 3'd0) & s0_next) : ws0;
  // The line buffers: a dot of a rendered line reads its column of the
  // front buffer and clears it in the back one; a fill dot writes the held
  // sprite's next pixel into the back one where it is still transparent.
  // The two never fall on the same dot. After a fill, the buffers swap.
  wire [7:0] px = d[38:31];
  wire showing = l[3] & d[10];
  wire [1:0] wbits = {hi[7], lo[7]};
  wire [5:0] under = buf_ram[{~front, wx[7:0]}];
  wire put = filling & wvalid & (wbits != 2'd0) & ~wx[8] & (under[1:0] == 2'd0);
  always @(posedge clk) begin
    if (showing) buf_ram[{~front, px}] <= 6'd0;
    else if (put) buf_ram[{~front, wx[7:0]}] <= {ws0, wattr[5], wattr[1:0], wbits};
  end
  wire front_n = (rl & d[19]) ? ~front : front;
  wire [5:0] got = buf_ram[{front, px}];
  // With sprites off, or in the clipped left column, there is no sprite.
  wire show = vis & mask[4] & ~((px < 8'd8) & ~mask[2]);
  assign bits = show ? got[1:0] : 2'b00;
  assign pal = got[3:2];
  assign prio = got[4];                                      // 1: behind the background
  assign from0 = show & got[5] & (got[1:0] != 2'd0);
  ppu_reg4 cnt_reg (.clk(clk), .en(1'b1), .d(cnt_n), .q(cnt));
  ppu_reg1 s0_next_reg (.clk(clk), .en(1'b1), .d(s0_next_n), .q(s0_next));
  ppu_reg8 lo_reg (.clk(clk), .en(1'b1), .d(lo_n), .q(lo));
  ppu_reg8 hi_reg (.clk(clk), .en(1'b1), .d(hi_n), .q(hi));
  ppu_reg9 wx_reg (.clk(clk), .en(1'b1), .d(wx_n), .q(wx));
  ppu_reg8 wattr_reg (.clk(clk), .en(1'b1), .d(wattr_n), .q(wattr));
  ppu_reg1 wvalid_reg (.clk(clk), .en(1'b1), .d(wvalid_n), .q(wvalid));
  ppu_reg1 ws0_reg (.clk(clk), .en(1'b1), .d(ws0_n), .q(ws0));
  ppu_reg1 front_reg (.clk(clk), .en(1'b1), .d(front_n), .q(front));
endmodule

// Background against sprite: the palette index of the pixel, and sprite 0 hit.
module ppu_mux (input [3:0] bg, input [1:0] sbits, input [1:0] spal, input sprio, input from0,
                input [7:0] mask, input [40:0] d, input pixel_on, output [4:0] index,
                output set_hit);
  wire bg_op = bg[1:0] != 2'd0;
  wire sp_op = sbits != 2'd0;
  assign index = (sp_op & (~bg_op | ~sprio)) ? {1'b1, spal, sbits} : bg_op ? {1'b0, bg} : 5'd0;
  assign set_hit = pixel_on & from0 & sp_op & bg_op & ~d[5] & mask[3] & mask[4];
endmodule

module ppu_ref (input clk, input [8:0] dot, input [8:0] line,
                input cs, input [2:0] ra, input rw, input [7:0] cpu_wd, output [7:0] cpu_rd,
                output nmi_n, output [13:0] pa, input [7:0] pd, output [7:0] pwd, output pwe,
                output [5:0] colour, output rendering);
  wire [7:0] ctrl, mask;
  wire [14:0] v;
  wire [2:0] fx;
  wire [13:0] fa;
  wire [3:0] pixel;
  wire [5:0] pal_rd;
  wire fetch_nt, fetch_at, fetch_lo, fetch_hi, inc_x, inc_y, copy_h, copy_v, shift, reload;
  wire pixel_on, set_vbl, clr_vbl, w2000, w2005, w2006, r2002, a2007, pal_we;
  wire w2003, w2004, set_hit, set_ovf, clr_oam_addr, spr_fetching, sprio, from0;
  wire [7:0] oam_rd;
  wire [31:0] eval_word;
  wire [5:0] eval_idx;
  wire [13:0] spr_fa;
  wire [1:0] sbits, spal;
  wire [4:0] index;
  wire [40:0] d;
  wire [3:0] l;
  assign rendering = mask[3] | mask[4];
  ppu_decode decode (.dot(dot), .line(line), .d(d), .l(l));
  ppu_render_ctl render (.d(d), .l(l), .mask(mask), .fetch_nt(fetch_nt),
                         .fetch_at(fetch_at), .fetch_lo(fetch_lo), .fetch_hi(fetch_hi),
                         .inc_x(inc_x), .inc_y(inc_y), .copy_h(copy_h), .copy_v(copy_v),
                         .shift(shift), .reload(reload), .pixel_on(pixel_on),
                         .set_vbl(set_vbl), .clr_vbl(clr_vbl));
  ppu_regs regs (.clk(clk), .cs(cs), .ra(ra), .rw(rw), .wd(cpu_wd), .pd(pd), .pal_rd(pal_rd),
                 .v(v[13:0]), .set_vbl(set_vbl), .clr_vbl(clr_vbl), .oam_rd(oam_rd),
                 .set_hit(set_hit), .set_ovf(set_ovf), .ctrl(ctrl), .mask(mask),
                 .rd(cpu_rd), .nmi_n(nmi_n), .w2000(w2000), .w2005(w2005), .w2006(w2006),
                 .r2002(r2002), .a2007(a2007), .w2003(w2003), .w2004(w2004), .pwe(pwe),
                 .pal_we(pal_we));
  ppu_oam oam (.clk(clk), .wd(cpu_wd), .w2003(w2003), .w2004(w2004), .clr_addr(clr_oam_addr),
               .eval_idx(eval_idx), .cpu_rd(oam_rd), .eval_word(eval_word));
  ppu_sprites sprites (.clk(clk), .line(line), .d(d), .l(l), .ctrl(ctrl), .mask(mask), .pd(pd),
                       .eval_word(eval_word), .eval_idx(eval_idx), .fa(spr_fa),
                       .fetching(spr_fetching), .clr_oam_addr(clr_oam_addr), .bits(sbits),
                       .pal(spal), .prio(sprio), .from0(from0), .set_ovf(set_ovf));
  ppu_mux mux (.bg(pixel), .sbits(sbits), .spal(spal), .sprio(sprio), .from0(from0),
               .mask(mask), .d(d), .pixel_on(pixel_on), .index(index), .set_hit(set_hit));
  ppu_loopy loopy (.clk(clk), .wd(cpu_wd), .ctrl(ctrl), .w2000(w2000), .w2005(w2005),
                   .w2006(w2006), .r2002(r2002), .a2007(a2007), .inc_x(inc_x), .inc_y(inc_y),
                   .copy_h(copy_h), .copy_v(copy_v), .v(v), .fx(fx));
  ppu_bg bg (.clk(clk), .d(d), .v(v), .fx(fx), .ctrl(ctrl), .mask(mask), .pd(pd),
             .fetch_nt(fetch_nt), .fetch_at(fetch_at), .fetch_lo(fetch_lo), .fetch_hi(fetch_hi),
             .shift(shift), .reload(reload), .fa(fa), .pixel(pixel));
  ppu_palette palette (.clk(clk), .we(pal_we), .waddr(v[4:0]), .wd(cpu_wd), .index(index),
                       .mask(mask), .colour(colour), .cpu_rd(pal_rd));
  // The PPU's bus: the CPU's $2007 access when there is one, else the
  // sprite pattern fetch (dots 257-320), else the background's.
  assign pa = a2007 ? v[13:0] : spr_fetching ? spr_fa : fa;
  assign pwd = cpu_wd;
endmodule

module ppu_reg9 (input clk, input en, input [8:0] d, output reg [8:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg4 (input clk, input en, input [3:0] d, output reg [3:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg16 (input clk, input en, input [15:0] d, output reg [15:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg15 (input clk, input en, input [14:0] d, output reg [14:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg8 (input clk, input en, input [7:0] d, output reg [7:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg3 (input clk, input en, input [2:0] d, output reg [2:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg2 (input clk, input en, input [1:0] d, output reg [1:0] q);
  always @(posedge clk) if (en) q <= d;
endmodule
module ppu_reg1 (input clk, input en, input d, output reg q);
  always @(posedge clk) if (en) q <= d;
endmodule
