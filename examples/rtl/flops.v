// The designs that examples/rtl/flops.json stands for (hand-written in the
// format of Yosys `write_json` after `proc; opt`; not produced by Yosys).
// Each module is imported on its own (`--top`).
//
// The simulator has one clock, the tick: every flip-flop takes D once per
// tick and its clock pin is ignored. An asynchronous reset, set or load
// shows on the output at once and is stored at the next tick.

module adff(input clk, rst, input [3:0] d, output reg [3:0] q);
  always @(posedge clk, posedge rst)
    if (rst) q <= 4'd9; else q <= d;
endmodule

module adffe(input clk, rst_n, en, input [3:0] d, output reg [3:0] q);
  always @(posedge clk, negedge rst_n)
    if (!rst_n) q <= 4'd6; else if (en) q <= d;
endmodule

module sdff(input clk, rst, input [3:0] d, output reg [3:0] q);
  always @(posedge clk)
    if (rst) q <= 4'd5; else q <= d;
endmodule

// The reset wins over the enable ($sdffe) ...
module sdffe(input clk, rst, en, input [3:0] d, output reg [3:0] q);
  always @(posedge clk)
    if (rst) q <= 4'd5; else if (en) q <= d;
endmodule

// ... or only acts while enabled ($sdffce); here active low.
module sdffce(input clk, rst_n, en, input [3:0] d, output reg [3:0] q);
  always @(posedge clk)
    if (en) begin
      if (!rst_n) q <= 4'd10; else q <= d;
    end
endmodule

// Per-bit asynchronous set and clear; clear wins. (Yosys makes $dffsr from
// one such always block per bit.)
module dffsr(input clk, input [3:0] set, clr, d, output reg [3:0] q);
  // always @(posedge clk, posedge set[i], posedge clr[i])
  //   if (clr[i]) q[i] <= 0; else if (set[i]) q[i] <= 1; else q[i] <= d[i];
endmodule

module dffsre(input clk, en, input [3:0] set, clr_n, d, output reg [3:0] q);
  // as dffsr, with an enable and an active-low clear
endmodule

// Asynchronous load of ad.
module aldff(input clk, load, input [3:0] ad, d, output reg [3:0] q);
  always @(posedge clk, posedge load)
    if (load) q <= ad; else q <= d;
endmodule

// A flip-flop on the global clock ($ff, as `clk2fflogic` makes).
module ff(input [3:0] d, output [3:0] q);
endmodule

// Gate-level flip-flops, as after `dfflegalize` or `abc`: one per bit of q.
//   q[0] $_DFF_PN0_      async reset to 0, active low (r)
//   q[1] $_DFF_PP1_      async reset to 1 (r)
//   q[2] $_DFFE_PN0P_    async reset to 0, active low (r); enable e
//   q[3] $_SDFF_PP0_     sync reset to 0 (r)
//   q[4] $_SDFFE_PN1P_   sync reset to 1, active low (r), over enable e
//   q[5] $_SDFFCE_PP1N_  sync reset to 1 (r), only while e is low
//   q[6] $_DFFSR_PNP_    async set, active low (s); async clear (r)
//   q[7] $_DFFSRE_PPNP_  async set (s), clear active low (r); enable e
//   q[8] $_ALDFF_PP_     async load of ad (l)
//   q[9] $_ALDFFE_PNP_   async load of ad, active low (l); enable e
module gates(input c, r, s, e, l, ad, input [9:0] d, output [9:0] q);
endmodule

// Latches: transparent while enabled.
//   q   $dlatch: always @* if (e) q = d;
//   qa  $adlatch: reset (r) to 4'b1010
//   qs  $sr: per-bit set (s) and clear (k), clear wins
//   qd  $dlatchsr: a latch with per-bit set (s) and clear (k)
//   qg  gate level: qg[0] $_DLATCH_P_ (e), qg[1] $_DLATCH_NP0_ (enable
//       active low on e, reset r to 0), qg[2] $_SR_PN_ (set s[0], clear
//       active low on k[0]), qg[3] $_DLATCHSR_PPP_ (e, set s[1], clear k[1])
module latches(input e, r, input [3:0] d, input [1:0] s, k,
               output [3:0] q, qa, output [1:0] qs, qd, output [3:0] qg);
endmodule
