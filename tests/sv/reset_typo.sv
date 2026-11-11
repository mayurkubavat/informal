// A misspelt reset must be an error, not a free signal (it used to give a
// false cex through a silent Yosys fallback).
//% clock clk
//% reset -expression {!rst_typo}
//! unknown name(s) in module reset_typo: rst_typo
module reset_typo(input logic clk, input logic rst_n, output logic [3:0] q);
  always_ff @(posedge clk) if (!rst_n) q <= 4'd0; else if (q != 4'd9) q <= q + 4'd1;
endmodule
