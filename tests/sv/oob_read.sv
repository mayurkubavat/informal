// An out-of-range read is X: a free value, so this can fail (it used to
// alias v[i % 4] and prove).
//% clock clk
//% assert -name a_oob {i < 5'd4 || v[i] == 1'b0}
//= a_oob cex
module oob_read(input logic clk, input logic [4:0] i);
  logic [3:0] v;
  assign v = 4'b0;
endmodule
