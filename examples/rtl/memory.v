// The designs that examples/rtl/memory.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt; memory -nomap`; not
// produced by Yosys). Each module is imported on its own (`--top`).

// No write port: a ROM (one per read port). d1 reads at once, d2 at the
// tick. Contents: mem[i] = 37 * i + 5 (8 bits).
module rom(input clk, input [3:0] a1, a2, output [7:0] d1, output reg [7:0] d2);
  reg [7:0] mem [0:15];
  integer i;
  initial for (i = 0; i < 16; i = i + 1) mem[i] = 37 * i + 5;
  assign d1 = mem[a1];
  always @(posedge clk) d2 <= mem[a2];
endmodule

// One address for reading and writing: a RAM. The read is clocked and reads
// the old word (read first). Contents: mem[i] = 8'hA0 + i.
module spram(input clk, we, input [3:0] addr, input [7:0] din, output reg [7:0] dout);
  reg [7:0] mem [0:15];
  integer i;
  initial for (i = 0; i < 16; i = i + 1) mem[i] = 8'hA0 + i;
  always @(posedge clk) begin
    if (we) mem[addr] <= din;
    dout <= mem[addr];
  end
endmodule

// Write first (a transparent read port), as the older $mem cell.
module spram_wf(input clk, we, input [2:0] addr, input [7:0] din, output reg [7:0] dout);
  reg [7:0] mem [0:7];
  always @(posedge clk)
    if (we) begin
      mem[addr] <= din;
      dout <= din;
    end else
      dout <= mem[addr];
endmodule

// Byte enables (a per-bit write enable), read at once.
module bytes(input clk, input [1:0] we, input [2:0] addr, input [15:0] din,
             output [15:0] dout);
  reg [15:0] mem [0:7];
  always @(posedge clk) begin
    if (we[0]) mem[addr][7:0] <= din[7:0];
    if (we[1]) mem[addr][15:8] <= din[15:8];
  end
  assign dout = mem[addr];
endmodule

// Reads at other addresses than the write: built from registers. r1 reads
// at once; r2 at the tick, seeing a word written in the same tick (write
// first). Contents: mem[i] = 8'h11 * i.
module regfile(input clk, we, input [2:0] wa, ra, rb, input [7:0] wd,
               output [7:0] r1, output reg [7:0] r2);
  reg [7:0] mem [0:7];
  integer i;
  initial for (i = 0; i < 8; i = i + 1) mem[i] = 8'h11 * i;
  always @(posedge clk) if (we) mem[wa] <= wd;
  assign r1 = mem[ra];
  always @(posedge clk) r2 <= (we && wa == rb) ? wd : mem[rb];
endmodule
