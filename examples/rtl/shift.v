// The designs that examples/rtl/shift.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt`; not produced by Yosys).
// Each module is imported on its own (`--top`).

module shl_u(input [7:0] a, input [3:0] b, output [7:0] y);
  assign y = a << b;
endmodule

module shr_u(input [7:0] a, input [3:0] b, output [7:0] y);
  assign y = a >> b;
endmodule

// Arithmetic right shift of a signed value into a wider result.
module sshr_s(input signed [7:0] a, input [3:0] b, output signed [9:0] y);
  assign y = a >>> b;
endmodule

module sshl_s(input signed [5:0] a, input [2:0] b, output signed [7:0] y);
  assign y = a <<< b;
endmodule

// `>>` is a logical shift even of a signed value (after sign extension
// to the result width).
module shr_s(input signed [5:0] a, input [2:0] b, output [7:0] y);
  assign y = a >> b;
endmodule

// A variable part-select becomes $shiftx; bits past the end are x.
module part(input [15:0] a, input [3:0] i, output [3:0] y);
  assign y = a[i +: 4];
endmodule

// With a signed index the shift amount is signed: below 0 reads x too.
module part_s(input [15:0] a, input signed [4:0] i, output [3:0] y);
  assign y = a[i +: 4];
endmodule

// $shift has no Verilog operator of its own (passes such as `peepopt`
// make it): y = a (sign-extended to 10 bits) shifted right by b, or left
// when b is negative, with 0 shifted in.
module shift_s(input signed [7:0] a, input signed [3:0] b, output [9:0] y);
  // assign y = b >= 0 ? a_ext >> b : a_ext << -b;
endmodule

module shl_w(input [31:0] a, input [5:0] b, output [47:0] y);
  assign y = a << b;
endmodule

// Constant shifts are only wiring.
module shift_k(input [7:0] a, output [11:0] y, output [7:0] z);
  assign y = a << 3;
  assign z = a >> 2;
endmodule
