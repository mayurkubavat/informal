// A cut point that names no signal is an error, not a silent no-op.
//% clock clk
//% stopat u_core.no_such_sig
//! is not a signal of stopat_typo
module stopat_typo(input logic clk, input logic d);
  logic q;
  always_ff @(posedge clk) q <= d;
endmodule
