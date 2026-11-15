// A property given by command that names a signal the top module does
// not have is rejected at the command, also when it uses sequence
// operators: a typo must not become a free signal.
//% clock clk
//% assert -name a_typo {req |-> ##[2:3] gnnt throughout req}
//! unknown name(s) in module command_typo: gnnt
module command_typo(input logic clk, input logic req, output logic gnt);
  always_ff @(posedge clk) gnt <= req;
endmodule
