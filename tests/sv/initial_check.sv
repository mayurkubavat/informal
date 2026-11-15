// A time-zero check of a signal in an initial block is not modelled, and
// is not skipped unless the user asks (set_lowering skip_initial_checks).
//% clock clk
//% assert -name a_q {q == $past(d)}
//= a_q error
module initial_check(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
  initial a_init: assert (d == 1'b0);
endmodule
