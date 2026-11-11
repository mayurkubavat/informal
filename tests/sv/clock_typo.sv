//% clock clk_typo
//! unknown name(s) in module clock_typo: clk_typo
module clock_typo(input logic clk, output logic q);
  always_ff @(posedge clk) q <= ~q;
endmodule
