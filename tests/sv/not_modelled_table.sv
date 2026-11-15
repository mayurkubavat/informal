// A design that does not lower has no model, so its own properties cannot
// be listed. That must not read as "nothing to prove": elaborate says why,
// and a prove with nothing to run is an error with the same reasons.
// (OpenTitan's prim_count showed this: its assertions vanished behind one
// time-zero check, with no error and exit status 0.)
//% clock clk
//% prove -all
//! nothing was proved: the design cannot be modelled
module not_modelled_table(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
  initial a_init: assert (d == 1'b0);
  a_follows: assert property (@(posedge clk) q == $past(d));
endmodule
