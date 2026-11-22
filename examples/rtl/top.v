// The design that examples/rtl/top.json stands for. The JSON was written by
// hand in the format of Yosys `write_json` (after `synth -run :fine`, with
// the full adder mapped to gates); it was not produced by running Yosys.

module full_adder(input a, b, cin, output s, cout);
  assign s = a ^ b ^ cin;
  assign cout = (a & b) | ((a ^ b) & cin);
endmodule

module adder4(input [3:0] a, b, input cin, output [3:0] sum, output cout);
  wire [2:0] c;
  full_adder fa0(.a(a[0]), .b(b[0]), .cin(cin),  .s(sum[0]), .cout(c[0]));
  full_adder fa1(.a(a[1]), .b(b[1]), .cin(c[0]), .s(sum[1]), .cout(c[1]));
  full_adder fa2(.a(a[2]), .b(b[2]), .cin(c[1]), .s(sum[2]), .cout(c[2]));
  full_adder fa3(.a(a[3]), .b(b[3]), .cin(c[2]), .s(sum[3]), .cout(cout));
endmodule

// op: 0 add, 1 and, 2 or, 3 not a
module alu4(input [3:0] a, b, input [1:0] op, output [3:0] y);
  assign y = op[1] ? (op[0] ? ~a : a | b) : (op[0] ? a & b : a + b);
endmodule

module counter(input clk, en, output reg [3:0] q, output max);
  always @(posedge clk) if (en) q <= q + 1'b1;
  assign max = q == 4'b1111;
endmodule

(* top *)
module top(input [3:0] a, b, input [1:0] op, input clk, en,
           output [3:0] sum, output cout, output [4:0] total, output [3:0] y,
           output [3:0] q, output max, output msb, output [1:0] hi);
  adder4 u_add(.a(a), .b(b), .cin(1'b0), .sum(sum), .cout(cout));
  alu4 u_alu(.a(a), .b(b), .op(op), .y(y));
  counter u_cnt(.clk(clk), .en(en), .q(q), .max(max));
  assign total = {cout, sum};
  assign msb = a[3] & b[3];
  assign hi = a[3:2];
endmodule
