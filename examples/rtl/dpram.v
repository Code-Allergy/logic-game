// The designs that examples/rtl/dpram.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt; memory -nomap`; not
// produced by Yosys). Each module is imported on its own (`--top`). They
// read and write at different addresses, so each becomes a dual-port RAM
// (`Ram2`), except `twowrite`, which has two write ports.

// 1024 words of 16 bits: written at wa, read at ra (at once) and rb (at
// the tick, reading the old word). Contents: mem[i] = (i * 16'h0101) ^ 16'h5A5A.
module dpram(input clk, we, input [9:0] wa, ra, rb, input [15:0] wd,
             output [15:0] rd1, output reg [15:0] rd2);
  reg [15:0] mem [0:1023];
  integer i;
  initial for (i = 0; i < 1024; i = i + 1) mem[i] = (i * 16'h0101) ^ 16'h5A5A;
  always @(posedge clk) if (we) mem[wa] <= wd;
  assign rd1 = mem[ra];
  always @(posedge clk) rd2 <= mem[rb];
endmodule

// 32 words of 16 bits with byte enables and nine read ports (more than one
// Ram2 has, so two copies are written alike). rdK = mem[raK] at once,
// except rd8, which reads at the tick and sees the bytes written in the
// same tick (write first).
module multiread(input clk, input [1:0] we, input [4:0] wa, input [15:0] wd,
                 input [4:0] ra0, ra1, ra2, ra3, ra4, ra5, ra6, ra7, ra8,
                 output [15:0] rd0, rd1, rd2, rd3, rd4, rd5, rd6, rd7,
                 output reg [15:0] rd8);
  reg [15:0] mem [0:31];
  always @(posedge clk) begin
    if (we[0]) mem[wa][7:0] <= wd[7:0];
    if (we[1]) mem[wa][15:8] <= wd[15:8];
  end
  assign rd0 = mem[ra0];
  assign rd1 = mem[ra1];
  assign rd2 = mem[ra2];
  assign rd3 = mem[ra3];
  assign rd4 = mem[ra4];
  assign rd5 = mem[ra5];
  assign rd6 = mem[ra6];
  assign rd7 = mem[ra7];
  always @(posedge clk)
    rd8 <= {we[1] && wa == ra8 ? wd[15:8] : mem[ra8][15:8],
            we[0] && wa == ra8 ? wd[7:0] : mem[ra8][7:0]};
endmodule

// 16 words of 8 bits with two write ports (b wins at the same address),
// r1 read at once and r2 at the tick, seeing both writes (write first).
// Contents: mem[i] = 3 * i.
module twowrite(input clk, wea, input [3:0] waa, input [7:0] wda,
                input web, input [3:0] wab, input [7:0] wdb,
                input [3:0] ra, rb, output [7:0] r1, output reg [7:0] r2);
  reg [7:0] mem [0:15];
  integer i;
  initial for (i = 0; i < 16; i = i + 1) mem[i] = 3 * i;
  always @(posedge clk) begin
    if (wea) mem[waa] <= wda;
    if (web) mem[wab] <= wdb;
  end
  assign r1 = mem[ra];
  always @(posedge clk)
    r2 <= web && wab == rb ? wdb : wea && waa == rb ? wda : mem[rb];
endmodule
