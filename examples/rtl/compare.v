// The designs that examples/rtl/compare.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt`; not produced by Yosys).
// Each module is imported on its own (`--top`).

module cmp_u(input [3:0] a, input [4:0] b, output lt, le, gt, ge, eq, ne);
  assign lt = a < b;
  assign le = a <= b;
  assign gt = a > b;
  assign ge = a >= b;
  assign eq = a === b;
  assign ne = a !== b;
endmodule

module cmp_s(input signed [3:0] a, input signed [4:0] b, output lt, le, gt, ge, eq, ne);
  assign lt = a < b;
  assign le = a <= b;
  assign gt = a > b;
  assign ge = a >= b;
  assign eq = a === b;
  assign ne = a !== b;
endmodule

// One unsigned operand makes the comparison unsigned.
module cmp_mix(input signed [3:0] a, input [4:0] b, output lt, ge);
  assign lt = a < b;
  assign ge = a >= b;
endmodule

module cmp_w(input signed [39:0] a, b, output lt, ge);
  assign lt = a < b;
  assign ge = a >= b;
endmodule
