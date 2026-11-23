// The reference MMC3 (mapper 4: TxROM boards such as SMB3's TSROM), the
// mapper logic only; the cartridge chip `cart_mmc3` around it holds the
// PRG ROM, PRG RAM and CHR memories.
//
// CPU side, writes to $8000-$FFFF by (A14, A13, A0):
//   $8000 bank select (bits 2-0: register; 6: PRG mode; 7: CHR A12 inversion)
//   $8001 bank data, $A000 mirroring (0 vertical, 1 horizontal), $A001 RAM
//   protect (ignored), $C000 IRQ latch, $C001 IRQ reload, $E000 IRQ off
//   and acknowledge, $E001 IRQ on.
// PRG: four 8 KiB windows; $C000 (mode 0) or $8000 (mode 1) is the second
// to last bank, $E000 the last. CHR: two 2 KiB and four 1 KiB windows,
// swapped by the inversion bit. Bank numbers are cut to the board's ROM
// sizes by `prg_mask` (8 KiB banks - 1) and `chr_mask` (1 KiB banks - 1).
// IRQ: the counter is clocked by a rise of PPU A12 after A12 was low for
// at least 8 dots (the board's filter, which lets the sprite fetches of a
// line count once); it reloads from the latch when 0 or asked to, else
// counts down, and raises IRQ when it reaches 0 while enabled.
//
// One tick is one PPU dot; `ce` marks the tick that commits a CPU cycle.
module mmc3_ref (input clk, input ce, input [15:0] a, input rw, input [7:0] d, input [13:0] pa,
                 input [5:0] prg_mask, input [7:0] chr_mask,
                 output [18:0] prg_addr, output [17:0] chr_addr, output ciram_a10,
                 output irq_n, output wram);
  reg [7:0] sel, r0, r1, r2, r3, r4, r5, r6, r7, latch, counter;
  reg mirror, reload, en, pend;
  reg [3:0] low;
  wire wr = ce & ~rw & a[15];
  wire rise = pa[12] & (low >= 4'd8);
  wire [7:0] next_count = (counter == 8'd0 || reload) ? latch : counter - 8'd1;
  always @(posedge clk) begin
    low <= pa[12] ? 4'd0 : (low == 4'd15 ? low : low + 4'd1);
    if (rise) begin
      counter <= next_count;
      reload <= 1'b0;
      if (next_count == 8'd0 && en) pend <= 1'b1;
    end
    if (wr) case ({a[14:13], a[0]})
      3'b000: sel <= d;
      3'b001: case (sel[2:0])
        3'd0: r0 <= d; 3'd1: r1 <= d; 3'd2: r2 <= d; 3'd3: r3 <= d;
        3'd4: r4 <= d; 3'd5: r5 <= d; 3'd6: r6 <= d; default: r7 <= d;
      endcase
      3'b010: mirror <= d[0];
      3'b100: latch <= d;
      3'b101: begin counter <= 8'd0; reload <= 1'b1; end
      3'b110: begin en <= 1'b0; pend <= 1'b0; end
      3'b111: en <= 1'b1;
      default: ;
    endcase
  end
  reg [5:0] pb;
  always @* case (a[14:13])
    2'd0: pb = sel[6] ? 6'h3E : r6[5:0];
    2'd1: pb = r7[5:0];
    2'd2: pb = sel[6] ? r6[5:0] : 6'h3E;
    default: pb = 6'h3F;
  endcase
  assign prg_addr = {pb & prg_mask, a[12:0]};
  wire [2:0] slot = pa[12:10] ^ {sel[7], 2'b00};
  reg [7:0] cb;
  always @* case (slot)
    3'd0: cb = {r0[7:1], 1'b0};
    3'd1: cb = {r0[7:1], 1'b1};
    3'd2: cb = {r1[7:1], 1'b0};
    3'd3: cb = {r1[7:1], 1'b1};
    3'd4: cb = r2;
    3'd5: cb = r3;
    3'd6: cb = r4;
    default: cb = r5;
  endcase
  assign chr_addr = {cb & chr_mask, pa[9:0]};
  assign ciram_a10 = mirror ? pa[11] : pa[10];
  assign irq_n = ~pend;
  assign wram = a[15:13] == 3'b011;
endmodule
