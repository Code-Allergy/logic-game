// The designs that examples/rtl/select.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt`; not produced by Yosys).
// Each module is imported on its own (`--top`).

// A case statement: $eq per case and one $pmux.
module pmux(input [1:0] sel, input [3:0] a, b, c, d, output reg [3:0] y);
  always @*
    case (sel)
      2'd0: y = a;
      2'd1: y = b;
      2'd2: y = c;
      default: y = d;
    endcase
endmodule

// A parallel case on one-hot selects: $pmux straight from s.
module pmux_1h(input [2:0] s, input [3:0] a, input [11:0] b, output reg [3:0] y);
  always @* begin
    y = a;
    (* parallel_case *)
    case (1'b1)
      s[0]: y = b[3:0];
      s[1]: y = b[7:4];
      s[2]: y = b[11:8];
    endcase
  end
endmodule

// $bmux (what `bmuxmap` lowers; Yosys makes it for indexing a packed array).
module bmux(input [31:0] words, input [2:0] sel, output [3:0] y);
  assign y = words[sel * 4 +: 4];
endmodule

// $demux: a into word s of y, the other words 0.
module demux(input [3:0] a, input [1:0] s, output [15:0] y);
  assign y = {12'b0, a} << (s * 4);
endmodule

// $bwmux (a mux per bit), $bweqx (=== per bit) and a 6-input $lut (as
// after `abc -lut 6`): f is 1 when at least three bits of l are 1.
module bitwise(input [3:0] a, b, s, input [5:0] l, output [3:0] y, e, output f);
  assign y = (a & ~s) | (b & s);
  assign e = ~(a ^ b);
  assign f = $countones(l) >= 3;
endmodule
