// A design on both clock edges with a synchronous reset: the reset is held
// for the whole first clock cycle, so the register on the falling edge is
// reset too (x_same would fail at the first check otherwise). Properties
// follow their own clock edge, in the RTL and from commands.
//% clock clk
//% reset -expression {rst}
//% assert -name x_same {n == p}
//% assert -name x_data {!$past(rst) |-> n == $past(d)}
//% assert -name x_late {n == $past(p)}
//% cover -name c_one {n && p}
//= x_same proven
//= x_data proven
//= x_late cex
//= c_one covered
//= both_edges.a_neg proven
//= both_edges.a_neg_wrong cex
//= both_edges.a_pos proven
module both_edges(input logic clk, input logic rst, input logic d);
  logic p, n;
  always_ff @(posedge clk) if (rst) p <= 1'b0; else p <= d;
  always_ff @(negedge clk) if (rst) n <= 1'b0; else n <= p;
  a_neg:       assert property (@(negedge clk) disable iff (rst) n == $past(p));
  a_neg_wrong: assert property (@(negedge clk) disable iff (rst) n == p);
  a_pos:       assert property (@(posedge clk) disable iff (rst) d |=> p ##1 1'b1);
endmodule
