// An assumption in the RTL that cannot be compiled cannot be left out
// either (the model would be weaker than the design says): rejected.
//% clock clk
//% assert -name a1 {q || !q}
//= a1 error
module rtl_assume_unsupported(input logic clk, input logic a, output logic q);
  always_ff @(posedge clk) q <= a;
  m_range: assume property (@(posedge clk) a |-> ##[1:$] q);
endmodule
