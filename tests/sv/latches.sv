// A latch is transparent within a step: open, it shows its data; closed,
// what it held at the end of the step before. `y` is the latch an
// incomplete `always @*` infers: it holds when sel is 3.
//% clock clk
//% reset -expression {rst}
//% assert -name a_open {en |-> l == d}
//% assert -name a_closed {!en |-> l == $past(l)}
//% assert -name a_wrong {l == d}
//% assert -name a_reg {en |=> q == $past(d)}
//% assert -name a_case {sel == 2'd2 |-> y == (a ^ b)}
//% assert -name a_keep {sel == 2'd3 |-> y == $past(y)}
//% assert -name a_case_wrong {y == a || y == b || y == (a ^ b)}
//% cover -name c_keep {sel == 2'd3 && y != a && y != b && y != (a ^ b)}
//= a_open proven
//= a_closed proven
//= a_wrong cex
//= a_reg proven
//= a_case proven
//= a_keep proven
//= a_case_wrong cex
//= c_keep covered
module latches(input logic clk, input logic rst, input logic en, input logic d,
               input logic [1:0] sel, input logic [3:0] a, input logic [3:0] b);
  logic l, q;
  reg [3:0] y;
  always_latch if (en) l <= d;
  always_ff @(posedge clk) q <= l;
  always @* begin
    case (sel)
      2'd0: y = a;
      2'd1: y = b;
      2'd2: y = a ^ b;
    endcase
  end
endmodule
