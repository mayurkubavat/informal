// Two declared clocks: clk2 has twice the period of clk, and both start
// low and rise together for the first time. The model drives both, the
// reset is held until each has had a whole period, and each register
// follows its own clock: the slow counter runs at half the rate.
// At a rising edge of clk, s is f / 2 + 1 (written so that it also holds
// when the counters wrap).
//% clock clk
//% clock clk2 -factor 2
//% reset -expression {rst}
//% assert -name x_ratio {((s - 4'd1) << 1) == (f & 4'hE)}
//% assert -name x_same {s == f}
//% assert -name x_other_phase {(s << 1) == f + {3'b0, f[0]}}
//% cover -name c_reach {s == 4'd3 && f == 4'd4}
//% cover -name c_never {s == 4'd3 && f == 4'd7}
//= x_ratio proven
//= x_same cex
//= x_other_phase cex
//= c_reach covered
//= c_never unreachable
//= multi_clock.a_slow proven
//= multi_clock.a_slow_wrong cex
module multi_clock(input logic clk, input logic clk2, input logic rst);
  logic [3:0] f, s;
  always_ff @(posedge clk)  if (rst) f <= 4'd0; else f <= f + 4'd1;
  always_ff @(posedge clk2) if (rst) s <= 4'd0; else s <= s + 4'd1;
  a_slow:       assert property (@(posedge clk2) disable iff (rst) 1'b1 |=> s == $past(s) + 4'd1);
  a_slow_wrong: assert property (@(posedge clk2) disable iff (rst) 1'b1 |=> f == $past(f) + 4'd1);
endmodule
