// Values are two-state: $isunknown cannot be answered, so it is an error
// (it used to read as 0 and prove !$isunknown(x) vacuously).
//% clock clk
//% assert -name a_known {!$isunknown(d)}
//= a_known error
module isunknown(input logic clk, input logic [3:0] d);
  wire unused = clk;
endmodule
