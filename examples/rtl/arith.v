// The designs that examples/rtl/arith.json stands for. The JSON was written
// by hand in the format of Yosys `write_json` (after `proc; opt`); it was not
// produced by running Yosys. Each module is imported on its own (`--top`).
// Operation widths are what Yosys gives them: the widest of the operands and
// the result, so `r = a % b` below is an 8-bit $mod cut to 4 bits.

module mul_u(input [5:0] a, b, output [11:0] y);
  assign y = a * b;
endmodule

module mul_s(input signed [4:0] a, input signed [3:0] b, output signed [8:0] y);
  assign y = a * b;
endmodule

module mul_w(input [31:0] a, b, output [31:0] y);
  assign y = a * b;
endmodule

// A constant factor: only the stages for its 1 bits are built.
module mul_k(input [7:0] a, output [9:0] y);
  assign y = a * 3'd5;
endmodule

module divmod_u(input [7:0] a, input [3:0] b, output [7:0] q, output [3:0] r);
  assign q = a / b;
  assign r = a % b;
endmodule

module divmod_s(input signed [5:0] a, input signed [3:0] b,
                output signed [5:0] q, output signed [3:0] r);
  assign q = a / b;
  assign r = a % b;
endmodule

module divmod_w(input [23:0] a, input [11:0] b, output [23:0] q, output [11:0] r);
  assign q = a / b;
  assign r = a % b;
endmodule

module neg_logic(input signed [3:0] a, input [3:0] b,
                 output signed [5:0] n, output land, lor);
  assign n = -a;
  assign land = a && b;
  assign lor = a || b;
endmodule
