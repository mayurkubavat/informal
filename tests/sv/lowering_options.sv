// With the two lowering switches on, a time-zero check of a signal in an
// initial block is left out and $isunknown is 0, so the rest of the
// design is checked. Each is unsupported while its switch is off, which
// is the default (isunknown.sv, initial_check.sv).
//% set_lowering skip_initial_checks on
//% set_lowering isunknown_is_false on
//% clock clk
//% assert -name a_q {q == $past(d)}
//% assert -name a_wrong {q == d}
//= a_q proven
//= a_wrong cex
//= lowering_options.a_known proven
module lowering_options(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
  initial a_init: assert (d == 1'b0);
  a_known: assert property (@(posedge clk) !$isunknown(q));
endmodule
