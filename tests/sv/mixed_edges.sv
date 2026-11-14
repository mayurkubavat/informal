// Blocks on both edges of the clock: one model step is half a clock cycle.
// `a` takes `d` at the rising edge and `b` at the falling edge, and `d` may
// change in between, so they can differ; `c` follows `a` half a cycle
// later, so from the second rising edge on they agree (no reset here: at
// the first one neither holds a sampled value yet).
//% clock clk
//% assert -name a1 {a == b}
//% assert -name a2 {1'b1 |=> a == c}
//% assert -name a3 {a == $past(d)}
//= a1 cex
//= a2 proven
//= a3 proven
module mixed_edges(input logic clk, input logic d);
  logic a, b, c;
  always_ff @(posedge clk) a <= d;
  always_ff @(negedge clk) b <= d;
  always_ff @(negedge clk) c <= a;
endmodule
