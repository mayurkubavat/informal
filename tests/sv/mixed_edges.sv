// Every register steps on each model step: blocks on different edges of a
// clock cannot be modelled and must be rejected, not stepped together.
//% clock clk
//% assert -name a1 {a == b}
//= a1 error
module mixed_edges(input logic clk, input logic d);
  logic a, b;
  always_ff @(posedge clk) a <= d;
  always_ff @(negedge clk) b <= d;
endmodule
