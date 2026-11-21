// The design that examples/rtl/mac.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt; memory -nomap`; not
// produced by Yosys).
//
// A pipelined multiply-accumulate unit: every tick it multiplies x by the
// next coefficient (a memory of eight signed words, which can be
// rewritten: a dual-port RAM), and one tick later adds the product to the
// accumulator, subtracts it, halves the accumulator or clears it. The reset
// is asynchronous and active low. `over` says the accumulator is past 1000.

module mac(input clk, rst_n, input signed [7:0] x, input [1:0] mode,
           input cwe, input [2:0] caddr, input signed [7:0] cdata,
           output reg signed [19:0] acc, output reg [2:0] idx, output over);
  reg signed [7:0] coef [0:7];
  initial begin
    coef[0] = 1;  coef[1] = 2;  coef[2] = -3; coef[3] = 4;
    coef[4] = 5;  coef[5] = -6; coef[6] = 7;  coef[7] = -8;
  end
  always @(posedge clk) if (cwe) coef[caddr] <= cdata;

  wire signed [7:0] c = coef[idx];
  reg signed [15:0] prod;
  always @(posedge clk, negedge rst_n)
    if (!rst_n) begin
      idx <= 0;
      prod <= 0;
      acc <= 0;
    end else begin
      idx <= idx + 1;
      prod <= x * c;
      case (mode)
        2'd0: acc <= acc + prod;
        2'd1: acc <= acc - prod;
        2'd2: acc <= acc >>> 1;
        default: acc <= 0;
      endcase
    end
  assign over = acc > 20'sd1000;
endmodule
