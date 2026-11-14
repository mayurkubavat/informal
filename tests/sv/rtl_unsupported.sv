// A property in the RTL that uses an operator outside the compiled subset
// is an error on that property, never a guess; the others are still checked.
//% clock clk
//= rtl_unsupported.a_range error
//= rtl_unsupported.a_ok proven
module rtl_unsupported(input logic clk, input logic a, output logic q);
  always_ff @(posedge clk) q <= a;
  a_range: assert property (@(posedge clk) a |-> ##[1:3] q);
  a_ok:    assert property (@(posedge clk) a |=> q);
endmodule
