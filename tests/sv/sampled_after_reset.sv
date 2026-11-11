// With a reset, the first checked cycle already has one cycle of history
// (the reset cycle), so $rose is checked from there on: a rise right out of
// reset is seen.
//% clock clk
//% reset -expression {rst}
//% assert -name a_rose {$rose(q) |-> $past(d)}
//% assert -name a_never {!$rose(q)}
//= a_rose proven
//= a_never cex
module sampled_after_reset(input logic clk, input logic rst, input logic d);
  logic q;
  always_ff @(posedge clk) q <= rst ? 1'b0 : d;
endmodule
