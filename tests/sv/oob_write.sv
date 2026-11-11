// IEEE 1800-2017 11.5.1: a write to an out-of-range bit changes nothing
// (it used to wrap the index and write v[0]).
//% clock clk
//% assert -name a_oob {i < 5'd4 || v == 4'b0}
//= a_oob proven
module oob_write(input logic clk, input logic [4:0] i);
  logic [3:0] v;
  always_comb begin v = 4'b0; v[i] = 1'b1; end
endmodule
