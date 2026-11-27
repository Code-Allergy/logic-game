// The reference OAM DMA unit of the 2A03 ($4014).
//
// A write of page P to $4014 starts it: RDY goes low, so the CPU stops at
// its next read cycle (the instruction after the write starts with one).
// Then one halt cycle, and 256 pairs of cycles: read $PP00 + i, write it
// to $2004. 513 cycles; the extra alignment cycle real hardware adds after
// a write on an odd cycle is not modelled yet. While `own` is high the
// unit drives the system bus in place of the CPU.
//
// One tick is one PPU dot; `ce` marks the tick that commits a CPU cycle.
module oam_dma_ref (input clk, input ce, input [15:0] cpu_addr, input cpu_rw,
                    input [7:0] cpu_dout, input [7:0] din,
                    output own, output [15:0] addr, output rw, output [7:0] dout, output rdy);
  reg active;
  reg [9:0] cnt;
  reg [7:0] page, data;
  wire start = ce & ~cpu_rw & (cpu_addr == 16'h4014) & ~active;
  always @(posedge clk) begin
    if (start) begin
      active <= 1'b1;
      cnt <= 10'd0;
      page <= cpu_dout;
    end else if (active & ce) begin
      if (cnt[0]) data <= din;                     // a read cycle: keep the byte
      if (cnt == 10'd512) active <= 1'b0;
      cnt <= cnt + 10'd1;
    end
  end
  assign own = active & (cnt != 10'd0);
  wire [9:0] i = cnt - 10'd1;
  assign addr = cnt[0] ? {page, i[8:1]} : 16'h2004;
  assign rw = cnt[0];
  assign dout = data;
  assign rdy = ~active;
endmodule
