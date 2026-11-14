// As multi_clock, with clk2 one model step (half a period of clk) later:
// its first rising edge then comes with the second rising edge of clk, and
// at a rising edge of clk, s is (f + 1) / 2.
//% clock clk
//% clock clk2 -factor 2 -phase 1
//% reset -expression {rst}
//% assert -name x_ratio {((s - 4'd1) << 1) == (f & 4'hE)}
//% assert -name x_same {s == f}
//% assert -name x_other_phase {(s << 1) == f + {3'b0, f[0]}}
//% cover -name c_reach {s == 4'd3 && f == 4'd4}
//% cover -name c_never {s == 4'd3 && f == 4'd7}
//= x_ratio cex
//= x_same cex
//= x_other_phase proven
//= c_reach unreachable
//= c_never unreachable
//= multi_clock_phase.a_slow proven
//= multi_clock_phase.a_slow_wrong cex
module multi_clock_phase(input logic clk, input logic clk2, input logic rst);
  logic [3:0] f, s;
  always_ff @(posedge clk)  if (rst) f <= 4'd0; else f <= f + 4'd1;
  always_ff @(posedge clk2) if (rst) s <= 4'd0; else s <= s + 4'd1;
  a_slow:       assert property (@(posedge clk2) disable iff (rst) 1'b1 |=> s == $past(s) + 4'd1);
  a_slow_wrong: assert property (@(posedge clk2) disable iff (rst) 1'b1 |=> f == $past(f) + 4'd1);
endmodule
