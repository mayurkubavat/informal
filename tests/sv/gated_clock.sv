// Gated clocks. A computed clock is read once in every model step, like
// any signal, and a register on it takes the data of the step before when
// the clock rose since then.
// `q` is on clk gated by a plain AND with a register: the gate opens with
// the value the enable register takes at that very edge. `r` is on the
// usual clock-gating cell, whose latch holds the enable from before the
// edge. Both copy `p` as it was before the edge, or hold.
//% clock clk
//% reset -expression {rst}
//% assert -name x_copy {$past(en) && !$past(rst) |-> q == $past(p)}
//% assert -name x_hold {!$past(en) && !$past(rst) |-> q == $past(q)}
//% assert -name x_cell_copy {$past(en_q) && !$past(rst) |-> r == $past(p)}
//% assert -name x_cell_hold {!$past(en_q) |-> r == $past(r)}
//% assert -name x_cell_wrong {$past(en) && !$past(rst) |-> r == $past(p)}
//% assert -name x_same {r == q}
//% assert -name x_always {!$past(rst) |-> q == $past(p)}
//% cover -name c_differs {q != p}
//= x_copy proven
//= x_hold proven
//= x_cell_copy proven
//= x_cell_hold proven
//= x_cell_wrong cex
//= x_same cex
//= x_always cex
//= c_differs covered
//= gated_clock.a_cell proven
//= gated_clock.a_cell_wrong cex
//= gated_clock.c_cell covered
module gated_clock(input logic clk, input logic rst, input logic en, input logic d);
  logic en_q, p, q, r;
  logic g, en_l, g_l;
  always_ff @(posedge clk)
    if (rst) begin en_q <= 1'b0; p <= 1'b0; end
    else begin en_q <= en; p <= d; end
  assign g = clk & en_q;
  // The clock-gating cell: the enable is held while clk is high.
  always_latch if (!clk) en_l <= en_q;
  assign g_l = clk & en_l;
  always_ff @(posedge g)   q <= p;
  always_ff @(posedge g_l) r <= p;
  // Properties on the gated clock itself: at each of its rising edges, r
  // still holds what p was at the edge before.
  a_cell:       assert property (@(posedge g_l) r == $past(p));
  a_cell_wrong: assert property (@(posedge g_l) r == p);
  c_cell:       cover property (@(posedge g_l) r && !p ##1 !r && p);
endmodule
